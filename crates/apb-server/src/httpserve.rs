//! A thin replacement for `axum::serve` that sets an http1 header-read timeout.
//!
//! `axum::serve` uses hyper's defaults, which impose no deadline on reading a
//! request head: a slowloris client that dribbles header bytes holds a socket
//! and a task open indefinitely. axum 0.8 exposes no hook to configure the
//! underlying hyper connection builder, so both listeners drive the connection
//! loop here instead, with `Http1Builder::header_read_timeout` set. WebSocket
//! upgrades and graceful shutdown are preserved.
//!
//! A concurrent-connection cap is deliberately NOT added in-process: the
//! supported topology puts a TLS-terminating reverse proxy in front of both
//! listeners (see docs/DEPLOYMENT.md), which is where a connection limit
//! belongs. The header-read timeout is added here anyway because it costs
//! nothing and closes the one slowloris vector a direct non-loopback bind would
//! otherwise expose.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::Service;

/// The header-read deadline both listeners use: generous for a real client on a
/// slow link, far short of "forever".
pub(crate) const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// How long in-flight connections are given to drain after a shutdown signal.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves `app` on `listener` with an http1 header-read timeout and WebSocket
/// upgrades. When `shutdown` resolves the accept loop stops and in-flight
/// connections are given [`DRAIN_TIMEOUT`] to finish. A listener that never
/// shuts down passes `std::future::pending()`. `header_timeout` is a parameter
/// so a test can drive a short deadline; production passes
/// [`HEADER_READ_TIMEOUT`].
pub(crate) async fn serve_with_header_timeout<F>(
    listener: TcpListener,
    app: Router,
    header_timeout: Duration,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let mut make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => accepted?,
            _ = &mut shutdown => break,
        };
        // `IntoMakeServiceWithConnectInfo` is always ready and infallible; the
        // service it yields bakes `ConnectInfo(peer)` in for the handlers, which
        // both listeners rely on for their rate-limit keying and logging.
        let tower_service = make_service
            .call(peer)
            .await
            .expect("into_make_service_with_connect_info is infallible");
        let hyper_service = TowerToHyperService::new(tower_service);
        let io = TokioIo::new(stream);
        let mut builder = Builder::new(TokioExecutor::new());
        // A timer is required for `header_read_timeout` to be enforceable;
        // without one hyper rejects the connection instead of applying it.
        builder
            .http1()
            .timer(TokioTimer::new())
            .header_read_timeout(header_timeout);
        let conn = builder
            .serve_connection_with_upgrades(io, hyper_service)
            .into_owned();
        let watched = graceful.watch(conn);
        tokio::spawn(async move {
            let _ = watched.await;
        });
    }

    // Stop accepting and let live connections drain, bounded so a stuck peer
    // cannot hold shutdown open.
    drop(listener);
    tokio::select! {
        _ = graceful.shutdown() => {}
        _ = tokio::time::sleep(DRAIN_TIMEOUT) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    async fn spawn_server(header_timeout: Duration) -> (SocketAddr, oneshot::Sender<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/", get(|| async { "ok" }));
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = serve_with_header_timeout(listener, app, header_timeout, async {
                let _ = rx.await;
            })
            .await;
        });
        // Give the accept loop a moment to start.
        tokio::time::sleep(Duration::from_millis(50)).await;
        (addr, tx)
    }

    #[tokio::test]
    async fn a_complete_request_is_served() {
        let (addr, _stop) = spawn_server(HEADER_READ_TIMEOUT).await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
            .await
            .expect("the server answered a complete request")
            .unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 200"), "response was: {text}");
    }

    #[tokio::test]
    async fn a_slowloris_partial_header_is_closed_at_the_deadline() {
        // A short deadline so the test is fast; production uses HEADER_READ_TIMEOUT.
        let (addr, _stop) = spawn_server(Duration::from_millis(300)).await;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // A request line but never the blank line that ends the head: hyper's
        // default would hold this forever.
        stream.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();

        // The server must close the connection near the deadline. read_to_end
        // returns once the peer half-closes; we bound the wait well above the
        // deadline and well below hyper's would-be-forever default.
        let mut buf = Vec::new();
        let closed =
            tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
        assert!(
            closed.is_ok(),
            "the header-read timeout must close a stalled connection"
        );
    }
}
