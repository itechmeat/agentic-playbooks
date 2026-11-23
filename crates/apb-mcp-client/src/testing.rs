//! A scripted stdio MCP server for the client, engine and CLI tests.
//!
//! One POSIX `sh` script, so no extra binary is built. The script reads
//! its behaviour from files in its directory on every request, so a test
//! changes the server between two calls by rewriting a file.

use std::path::{Path, PathBuf};

/// A scripted stdio MCP server for tests. `write` puts an executable
/// `stub-mcp.sh` in `dir`. The script reads its behaviour from files in
/// `dir` on every request, so a test changes the server between two calls
/// by rewriting a file.
pub struct StubServer {
    pub dir: PathBuf,
}

/// The revision the stub speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubEra {
    /// Answers `server/discover` (2026-07-28).
    Modern,
    /// Answers `server/discover` with `-32601` and serves `initialize`.
    Legacy,
    /// Never answers `server/discover` and serves `initialize`.
    LegacySilent,
}

/// What the stub does with a `tools/call`.
#[derive(Debug, Clone)]
pub enum StubCall {
    /// Answer with this JSON object as the `result` member, after
    /// appending the request line to `calls.log`.
    Result(serde_json::Value),
    /// Answer with this JSON-RPC error (after logging the line).
    Error { code: i64, message: String },
    /// Log the line, then exit 1 without answering.
    ExitAfterRead,
    /// Log the line, then never answer.
    Hang,
    /// Log the line, then answer one result line of about this many bytes
    /// (a text block of `a` characters), for the line cap.
    Oversize(usize),
    /// Log the line, then answer with this text as the `result` member,
    /// byte for byte (a test writes its own escaping or a large dropped
    /// block).
    RawResult(String),
}

/// The stub server's script. Every answer is printed with `printf` and
/// `cat` of the files the test wrote, so no JSON tool is needed.
///
/// Started from its descriptor (Linux), the script's `$0` is
/// `/proc/self/fd/<n>`; it finds its directory through the link that
/// names in the shell's own `/proc/<pid>/fd`, which resolves to the
/// approved file (a replaced file reads `<path> (deleted)`, whose
/// directory is the same).
pub const STUB_SH: &str = r#"#!/bin/sh
case "$0" in
  /proc/self/fd/*) P=$(readlink "/proc/$$/fd/${0##*/}") ;;
  *) P=$0 ;;
esac
D=${P%/*}
echo start >> "$D/starts.log"
printf '%s\n' "$0" > "$D/argv0.log"
if [ -f "$D/fork.flag" ]; then
  sh -c 'echo $$ > "$1/worker.pid"; exec sleep 300' sh "$D" </dev/null >/dev/null 2>&1 &
fi
echo $$ > "$D/pid.log"
pwd > "$D/cwd.log"
ls -A > "$D/cwd_entries.log"
if [ -f "$D/fd1.flag" ]; then L=$(readlink "/proc/$$/fd/1"); printf '%s\n' "$L" > "$D/stdout.link"; fi
env | sed 's/=.*//' > "$D/env.log"
env > "$D/env.values"
: > "$D/args.log"
for a in "$@"; do printf '%s\n' "$a" >> "$D/args.log"; done
if [ -n "${STUB_STDERR+x}" ]; then printf '%s\n' "$STUB_STDERR" >&2; fi
ERA=$(cat "$D/era")
# The orphan a close test needs: a writer of its own session, started now,
# before the first request is read, and waited for until its pid is
# recorded. A writer forked only at end of input is still in the server's
# group until its `setsid` runs, so the close's group kill could take it
# with the server and the test would time a close against nothing. It
# holds stdout from here on and writes nothing until the stub touches
# `orphan.go` at end of input; it gives up after about 30 seconds without
# the go, so a test that never closes leaves no writer behind for long.
# `orphan_writer` keeps the pipe open with a sleep, `orphan_flow` fills it
# with bytes that carry no newline and never pause.
orphan_mode=
if [ -f "$D/orphan_writer.flag" ]; then orphan_mode=sleep; fi
if [ -f "$D/orphan_flow.flag" ]; then orphan_mode=flow; fi
if [ -n "$orphan_mode" ]; then
  rm -f "$D/orphan.pid" "$D/orphan.pid.tmp" "$D/orphan.go" "$D/orphan.running"
  setsid sh -c '
    echo $$ > "$1/orphan.pid.tmp" && mv "$1/orphan.pid.tmp" "$1/orphan.pid"
    i=0
    while [ ! -f "$1/orphan.go" ]; do
      i=$((i + 1)); [ "$i" -gt 3000 ] && exit 0; sleep 0.01
    done
    : > "$1/orphan.running"
    if [ "$2" = sleep ]; then exec sleep 30; fi
    while :; do printf "%s" "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; done
  ' sh "$D" "$orphan_mode" </dev/null &
  i=0
  while [ ! -f "$D/orphan.pid" ] && [ "$i" -lt 3000 ]; do i=$((i + 1)); sleep 0.01; done
fi
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"server/discover"'*) m=server/discover ;;
    *'"method":"notifications/initialized"'*) m=notifications/initialized ;;
    *'"method":"initialize"'*) m=initialize ;;
    *'"method":"tools/list"'*) m=tools/list ;;
    *'"method":"tools/call"'*) m=tools/call ;;
    *'"method":"'*) m=$(printf '%s\n' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p') ;;
    *) m=- ;;
  esac
  printf '%s\n' "$m" >> "$D/methods.log"
  if [ -z "$id" ]; then continue; fi
  case "$m" in
    server/discover)
      case "$ERA" in
        modern)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"stub","version":"1"}},"ttlMs":0,"cacheScope":"private"}}\n' "$id" ;;
        legacy)
          printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
        *) : ;;
      esac ;;
    initialize)
      if [ "$ERA" = modern ]; then
        printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id"
      else
        pv=2025-11-25
        if [ -f "$D/protocol_version" ]; then pv=$(cat "$D/protocol_version"); fi
        printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{}},"serverInfo":{"name":"stub","version":"1"}}}\n' "$id" "$pv"
      fi ;;
    tools/list)
      # Exiting after the list: stdin is closed first, so a request
      # written after this answer finds no reader whatever the timing.
      if [ -f "$D/exit_after_list.flag" ]; then exit_after=1; exec 0<&-; fi
      # Raw frames around the answer, `@ID@` the request's id.
      if [ -f "$D/list_before" ]; then sed "s/@ID@/$id/g" "$D/list_before"; fi
      extra=
      if [ "$ERA" = modern ]; then extra=',"resultType":"complete","ttlMs":0,"cacheScope":"private"'; fi
      if [ -f "$D/tools.pages" ]; then
        pages=$(cat "$D/tools.pages")
        n=$(printf '%s\n' "$line" | sed -n 's/.*"[c]ursor":"p\([0-9][0-9]*\)".*/\1/p')
        if [ -z "$n" ]; then n=0; fi
        next=
        if [ -f "$D/next_cursor" ]; then
          next=",\"nextCursor\":\"$(cat "$D/next_cursor")\""
        elif [ $((n + 1)) -lt "$pages" ]; then
          next=",\"nextCursor\":\"p$((n + 1))\""
        fi
        # The page is read whole before anything is written, so a page a
        # test blocks (a FIFO) holds the stub before its answer starts.
        page=$(cat "$D/tools.$n.json")
        printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":%s%s%s}}\n' "$id" "$page" "$next" "$extra"
      else
        printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":' "$id"
        if [ -f "$D/tools.json" ]; then cat "$D/tools.json"; else printf '[]'; fi
        printf '%s}}\n' "$extra"
      fi
      if [ -n "${exit_after:-}" ]; then echo listed > "$D/exited.log"; exit 0; fi
      # Stops reading while the flag exists, so the pipe fills up and the
      # client's writer is held.
      while [ -f "$D/stall_after_list.flag" ]; do sleep 0.1; done ;;
    tools/call)
      printf '%s\n' "$line" >> "$D/calls.log"
      suffix=
      if [ -f "$D/call_suffix" ]; then suffix=$(cat "$D/call_suffix"); fi
      # Raw bytes written right before the answer, on its line.
      if [ -f "$D/call_prefix" ]; then cat "$D/call_prefix"; fi
      mode=result
      if [ -f "$D/call.mode" ]; then mode=$(cat "$D/call.mode"); fi
      case "$mode" in
        result)
          printf '{"jsonrpc":"2.0","id":%s,"result":' "$id"
          if [ -f "$D/call.json" ]; then cat "$D/call.json"; else printf '{"content":[{"type":"text","text":"ok"}]}'; fi
          printf '}\n' ;;
        error)
          printf '{"jsonrpc":"2.0","id":%s,"error":' "$id"
          cat "$D/call.json"
          printf '}\n' ;;
        exit) exit 1 ;;
        hang) cat > /dev/null ;;
        oversize)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"' "$id"
          head -c "$(cat "$D/call.json")" /dev/zero | tr '\0' a
          printf '"}]}}\n' ;;
        raw)
          printf '{"jsonrpc":"2.0","id":%s,"result":' "$id"
          cat "$D/call.raw"
          printf '}\n' ;;
      esac
      # A line written right after the answer, in a write of its own
      # (read before the answer, so nothing starts between the two).
      # The first answer only: the file is gone afterwards.
      if [ -n "$suffix" ]; then printf '%s\n' "$suffix"; rm -f "$D/call_suffix"; fi
      # The half line the server writes after its answer and never
      # completes: the reader holds it as a line without its newline.
      if [ -f "$D/call_half_line.flag" ]; then printf '%s' "$(cat "$D/call_half_line.flag")"; fi ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
  esac
done
if [ -f "$D/ignore_eof.flag" ]; then
  trap '' TERM
  while :; do sleep 1; done
fi
# The close's drain test: bytes written only at end of input, after the
# client cancelled the service and its reader is gone.
if [ -f "$D/eof_bytes.flag" ]; then cat "$D/eof_bytes.flag"; fi
if [ -f "$D/eof_log.flag" ]; then
  printf '%s\n' "$(cat "$D/eof_log.flag")"
fi
# The orphan started above begins at end of input, after the client
# cancelled the service and its reader is gone: it keeps the pipe open, so
# the drain meets no end of input. The stub ends only once the orphan says
# it runs, so the close always meets a writer that holds the pipe.
if [ -n "$orphan_mode" ]; then
  : > "$D/orphan.go"
  i=0
  while [ ! -f "$D/orphan.running" ] && [ "$i" -lt 3000 ]; do i=$((i + 1)); sleep 0.01; done
fi
exit 0
"#;

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

impl StubServer {
    /// Writes an executable `stub-mcp.sh` speaking `era` into `dir`.
    pub fn write(dir: &Path, era: StubEra) -> std::io::Result<StubServer> {
        std::fs::create_dir_all(dir)?;
        let era = match era {
            StubEra::Modern => "modern",
            StubEra::Legacy => "legacy",
            StubEra::LegacySilent => "legacy_silent",
        };
        std::fs::write(dir.join("era"), era)?;
        let program = dir.join("stub-mcp.sh");
        std::fs::write(&program, STUB_SH)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(StubServer {
            dir: dir.to_path_buf(),
        })
    }

    /// `dir/stub-mcp.sh`
    pub fn program(&self) -> PathBuf {
        self.dir.join("stub-mcp.sh")
    }

    /// Writes `dir/tools.json`, the JSON array `tools/list` answers. An
    /// array of arrays is served as pages: page `i` with `"nextCursor":
    /// "p<i+1>"` until the last, a request with `"cursor":"pN"` gets page
    /// `N` (each page is also written as `tools.<i>.json`, with the page
    /// count in `tools.pages`, which the script reads).
    pub fn set_tools(&self, tools: &serde_json::Value) -> std::io::Result<()> {
        // Stale pages of an earlier listing never answer this one.
        let _ = std::fs::remove_file(self.dir.join("tools.pages"));
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("tools.") && name.ends_with(".json") && name != "tools.json" {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        std::fs::write(self.dir.join("tools.json"), tools.to_string())?;
        if let Some(pages) = tools.as_array()
            && !pages.is_empty()
            && pages.iter().all(serde_json::Value::is_array)
        {
            for (i, page) in pages.iter().enumerate() {
                std::fs::write(self.dir.join(format!("tools.{i}.json")), page.to_string())?;
            }
            std::fs::write(self.dir.join("tools.pages"), pages.len().to_string())?;
        }
        Ok(())
    }

    /// Makes every page of a paged listing answer `cursor` as its
    /// `nextCursor` (a list that never ends), or restores the numbered
    /// cursors with `None`.
    pub fn set_next_cursor(&self, cursor: Option<&str>) -> std::io::Result<()> {
        let path = self.dir.join("next_cursor");
        match cursor {
            Some(c) => std::fs::write(path, c),
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }

    /// Writes `dir/call.mode` and `dir/call.json` (`dir/call.raw` for
    /// `RawResult`).
    pub fn set_call(&self, call: StubCall) -> std::io::Result<()> {
        let (mode, json, raw) = match call {
            StubCall::Result(v) => ("result", Some(v.to_string()), None),
            StubCall::Error { code, message } => (
                "error",
                Some(serde_json::json!({ "code": code, "message": message }).to_string()),
                None,
            ),
            StubCall::ExitAfterRead => ("exit", None, None),
            StubCall::Hang => ("hang", None, None),
            StubCall::Oversize(n) => ("oversize", Some(n.to_string()), None),
            StubCall::RawResult(text) => ("raw", None, Some(text)),
        };
        if let Some(json) = json {
            std::fs::write(self.dir.join("call.json"), json)?;
        }
        if let Some(raw) = raw {
            std::fs::write(self.dir.join("call.raw"), raw)?;
        }
        std::fs::write(self.dir.join("call.mode"), mode)
    }

    /// Lines of `dir/calls.log` (one per tools/call request received).
    pub fn calls(&self) -> Vec<String> {
        lines_of(&self.dir.join("calls.log"))
    }

    /// Variable names the last start saw (`dir/env.log`).
    pub fn env_seen(&self) -> Vec<String> {
        lines_of(&self.dir.join("env.log"))
    }

    /// The value the last start saw for `name` (`dir/env.values`, the
    /// whole `env` output; test values only, never a real secret).
    pub fn env_value_seen(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}=");
        lines_of(&self.dir.join("env.values"))
            .into_iter()
            .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
    }

    /// Arguments the last start got (`dir/args.log`), one per line.
    pub fn args_seen(&self) -> Vec<String> {
        lines_of(&self.dir.join("args.log"))
    }

    /// Lines of `dir/starts.log` (one per server start); empty when absent.
    pub fn starts(&self) -> Vec<String> {
        lines_of(&self.dir.join("starts.log"))
    }

    /// Lines of `dir/methods.log` (one per JSON-RPC message received).
    pub fn methods(&self) -> Vec<String> {
        lines_of(&self.dir.join("methods.log"))
    }

    /// The working directory of the last start (`dir/cwd.log`).
    pub fn cwd_seen(&self) -> Option<PathBuf> {
        lines_of(&self.dir.join("cwd.log"))
            .into_iter()
            .next()
            .map(PathBuf::from)
    }

    /// The pid of the last start (`dir/pid.log`), for a liveness or group
    /// check in tests only. Never used to send a signal.
    pub fn last_pid(&self) -> Option<u32> {
        lines_of(&self.dir.join("pid.log"))
            .into_iter()
            .next()
            .and_then(|l| l.trim().parse().ok())
    }

    /// Whether `pid` (one of `last_pid`'s) still exists: `kill -0`, the
    /// null signal, which checks existence and sends nothing.
    pub fn is_alive(pid: u32) -> bool {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill -0 {pid} 2>/dev/null"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// The process group of `pid`: `ps -o pgid= -p <pid>`.
    pub fn process_group(pid: u32) -> Option<u32> {
        let out = std::process::Command::new("ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    /// Creates or removes `dir/fork.flag`: with it, each start leaves a
    /// worker in its process group that sleeps and outlives the server
    /// (its pid in `dir/worker.pid`).
    pub fn set_fork_worker(&self, on: bool) -> std::io::Result<()> {
        let path = self.dir.join("fork.flag");
        if on {
            std::fs::write(path, "")
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// The pid of the worker the last start left (`dir/worker.pid`), for
    /// a liveness check in tests only.
    pub fn worker_pid(&self) -> Option<u32> {
        lines_of(&self.dir.join("worker.pid"))
            .into_iter()
            .next()
            .and_then(|l| l.trim().parse().ok())
    }

    /// The `$0` of the last start (`dir/argv0.log`).
    pub fn argv0_seen(&self) -> Option<String> {
        lines_of(&self.dir.join("argv0.log")).into_iter().next()
    }

    /// Creates or removes `dir/exit_after_list.flag`: with it, the stub
    /// closes its stdin, answers a `tools/list` and exits
    /// (`dir/exited.log`), so no later request reaches it.
    pub fn set_exit_after_list(&self, on: bool) -> std::io::Result<()> {
        let path = self.dir.join("exit_after_list.flag");
        if on {
            std::fs::write(path, "")
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// The entries the working directory held at the last start
    /// (`dir/cwd_entries.log`, `ls -A`).
    pub fn cwd_entries_seen(&self) -> Vec<String> {
        lines_of(&self.dir.join("cwd_entries.log"))
    }

    /// Whether `pid` has exited (gone, or a zombie nobody reaped yet):
    /// `ps -o stat= -p <pid>`. For tests only; sends nothing.
    pub fn has_exited(pid: u32) -> bool {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        let stat = String::from_utf8_lossy(&out.stdout);
        let stat = stat.trim();
        stat.is_empty() || stat.starts_with('Z')
    }

    /// Writes `dir/protocol_version`, the `protocolVersion` a legacy
    /// `initialize` answers with (`2025-11-25` without it). Written into
    /// the JSON as is: no quote or backslash.
    pub fn set_protocol_version(&self, version: &str) -> std::io::Result<()> {
        std::fs::write(self.dir.join("protocol_version"), version)
    }

    /// Writes `dir/call_prefix`: raw bytes the stub writes right before
    /// each `tools/call` answer, on the answer's own line.
    pub fn set_call_prefix(&self, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(self.dir.join("call_prefix"), bytes)
    }

    /// Writes `dir/call_suffix`: one line (without its newline) the stub
    /// writes right after its first `tools/call` answer, in a write of its
    /// own. The file is gone afterwards, so the later answers write none.
    pub fn set_call_suffix(&self, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(self.dir.join("call_suffix"), bytes)
    }

    /// Writes `dir/call_half_line.flag`: the text the stub writes to its
    /// stdout right after its `tools/call` answer, in a write of its own,
    /// and never completes: the reader holds it as a line without its
    /// newline. `None` removes the flag.
    pub fn set_call_half_line(&self, text: Option<&str>) -> std::io::Result<()> {
        let path = self.dir.join("call_half_line.flag");
        match text {
            Some(text) => std::fs::write(path, text),
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }

    /// Writes `dir/eof_bytes.flag`: the bytes the stub writes to its
    /// stdout when it reads end of input, after the client cancelled the
    /// service and its reader is gone. They are written as they are.
    pub fn set_eof_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(self.dir.join("eof_bytes.flag"), bytes)
    }

    /// Writes `dir/list_before`: raw lines the stub writes before each
    /// `tools/list` answer, `@ID@` replaced by the request's id.
    pub fn set_list_before(&self, lines: impl AsRef<[u8]>) -> std::io::Result<()> {
        std::fs::write(self.dir.join("list_before"), lines)
    }

    /// Writes `dir/tools.json` as given, bytes the stub answers with
    /// unparsed (a member named twice, say).
    pub fn set_tools_raw(&self, json: &str) -> std::io::Result<()> {
        // A paged listing set earlier would answer instead of these bytes.
        match std::fs::remove_file(self.dir.join("tools.pages")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::write(self.dir.join("tools.json"), json)
    }

    /// Creates or removes `dir/stall_after_list.flag`: while it exists the
    /// stub reads nothing more after a `tools/list` answer.
    pub fn set_stall_after_list(&self, on: bool) -> std::io::Result<()> {
        let path = self.dir.join("stall_after_list.flag");
        if on {
            std::fs::write(path, "")
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// Creates or removes `dir/ignore_eof.flag`.
    pub fn set_ignore_eof(&self, on: bool) -> std::io::Result<()> {
        let path = self.dir.join("ignore_eof.flag");
        if on {
            std::fs::write(path, "")
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// Writes `dir/eof_log.flag`: one line (without its newline) the stub
    /// writes to its stdout only when it reads end of input, after the
    /// client cancelled the service. `None` removes the flag.
    pub fn set_eof_log(&self, line: Option<&str>) -> std::io::Result<()> {
        let path = self.dir.join("eof_log.flag");
        match line {
            Some(text) => std::fs::write(path, text),
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }

    /// Creates or removes `dir/orphan_writer.flag`: with it, the stub
    /// leaves behind a `setsid sleep 30` of its own session that holds its
    /// stdout, so the pipe never ends its output. Its pid is in
    /// `dir/orphan.pid`, so a test can kill it.
    pub fn set_orphan_writer(&self, on: bool) -> std::io::Result<()> {
        let path = self.dir.join("orphan_writer.flag");
        if on {
            std::fs::write(path, "")
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// Creates or removes `dir/orphan_flow.flag`: with it, the stub leaves
    /// behind a writer of its own session that fills the pipe with bytes
    /// that carry no newline and never pause. Its pid is in
    /// `dir/orphan.pid`, so a test can kill it.
    pub fn set_orphan_flow(&self, on: bool) -> std::io::Result<()> {
        let path = self.dir.join("orphan_flow.flag");
        if on {
            std::fs::write(path, "")
        } else {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// The pid of the orphan the last start left (`dir/orphan.pid`), for a
    /// test that kills it. Nothing is sent to it here.
    pub fn orphan_pid(&self) -> Option<u32> {
        lines_of(&self.dir.join("orphan.pid"))
            .into_iter()
            .next()
            .and_then(|l| l.trim().parse().ok())
    }

    /// Makes the next start record the link of its stdout
    /// (`dir/stdout.link`, `pipe:[inode]`), for a descriptor-inheritance
    /// check in tests only.
    pub fn record_stdout_link(&self) -> std::io::Result<()> {
        std::fs::write(self.dir.join("fd1.flag"), "")
    }

    /// The recorded link of the last start's stdout (`dir/stdout.link`).
    /// No link for an empty recording: an empty link is a substring of
    /// every string, so a missing recording would read as a match.
    pub fn stdout_link(&self) -> Option<String> {
        lines_of(&self.dir.join("stdout.link"))
            .into_iter()
            .find(|l| !l.is_empty())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Runs the stub once with `lines` on stdin, waits for its exit and
    /// answers the JSON lines it printed.
    fn drive(stub: &StubServer, args: &[&str], lines: &[Value]) -> Vec<Value> {
        let mut child = Command::new(stub.program())
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("STUB_VAR", "stub value")
            .current_dir(&stub.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stub starts");
        {
            let mut stdin = child.stdin.take().expect("stdin");
            for l in lines {
                writeln!(stdin, "{l}").expect("a line is written");
            }
        }
        let out = child.wait_with_output().expect("the stub exits");
        assert!(out.status.success(), "the stub exited with {}", out.status);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).expect("a JSON line"))
            .collect()
    }

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    #[test]
    fn the_stub_answers_discover_in_the_modern_era() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_tools(&json!([{ "name": "t", "inputSchema": { "type": "object" } }]))
            .unwrap();
        let out = drive(
            &stub,
            &[],
            &[
                request(0, "server/discover", json!({})),
                request(1, "tools/list", json!({})),
            ],
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], 0);
        assert_eq!(out[0]["result"]["supportedVersions"], json!(["2026-07-28"]));
        assert_eq!(out[1]["id"], 1);
        assert_eq!(out[1]["result"]["tools"][0]["name"], "t");
        assert_eq!(out[1]["result"]["resultType"], "complete");
        assert_eq!(stub.methods(), vec!["server/discover", "tools/list"]);
    }

    #[test]
    fn the_stub_answers_initialize_in_the_legacy_era() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Legacy).unwrap();
        let out = drive(
            &stub,
            &[],
            &[
                request(0, "server/discover", json!({})),
                request(1, "initialize", json!({})),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                request(2, "tools/list", json!({})),
            ],
        );
        assert_eq!(out.len(), 3, "the notification gets no answer: {out:?}");
        assert_eq!(out[0]["error"]["code"], -32601);
        assert_eq!(out[1]["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(out[2]["result"]["tools"], json!([]));
        assert!(out[2]["result"].get("resultType").is_none());

        let silent = tempfile::tempdir().unwrap();
        let stub = StubServer::write(silent.path(), StubEra::LegacySilent).unwrap();
        let out = drive(
            &stub,
            &[],
            &[
                request(0, "server/discover", json!({})),
                request(1, "initialize", json!({})),
            ],
        );
        assert_eq!(out.len(), 1, "discover gets no answer: {out:?}");
        assert_eq!(out[0]["id"], 1);
    }

    #[test]
    fn the_stub_logs_calls_env_args_and_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_call(StubCall::Result(
            json!({ "content": [{ "type": "text", "text": "done" }] }),
        ))
        .unwrap();
        let call = request(3, "tools/call", json!({ "name": "t", "arguments": {} }));
        let out = drive(&stub, &["--one", "two words"], std::slice::from_ref(&call));
        assert_eq!(out[0]["result"]["content"][0]["text"], "done");
        assert_eq!(stub.calls(), vec![call.to_string()]);
        assert_eq!(stub.args_seen(), vec!["--one", "two words"]);
        assert_eq!(stub.starts().len(), 1);
        assert!(stub.env_seen().contains(&"STUB_VAR".to_string()));
        assert_eq!(
            stub.env_value_seen("STUB_VAR").as_deref(),
            Some("stub value")
        );
        assert_eq!(
            stub.cwd_seen().map(|p| p.canonicalize().unwrap()),
            Some(tmp.path().canonicalize().unwrap())
        );
        let pid = stub.last_pid().expect("a pid");
        assert!(!StubServer::is_alive(pid), "the stub was reaped");

        stub.set_call(StubCall::Error {
            code: -32000,
            message: "nope".into(),
        })
        .unwrap();
        let out = drive(&stub, &[], &[request(4, "tools/call", json!({}))]);
        assert_eq!(out[0]["error"]["code"], -32000);
        assert_eq!(stub.starts().len(), 2);

        stub.set_call(StubCall::RawResult(r#"{"content":[],"x":1}"#.into()))
            .unwrap();
        let out = drive(&stub, &[], &[request(5, "tools/call", json!({}))]);
        assert_eq!(out[0]["result"]["x"], 1);

        stub.set_call(StubCall::Oversize(100)).unwrap();
        let out = drive(&stub, &[], &[request(6, "tools/call", json!({}))]);
        assert_eq!(out[0]["result"]["content"][0]["text"], "a".repeat(100));
        assert_eq!(stub.calls().len(), 4);
    }

    /// A recording of nothing is no link: an empty link would be a
    /// substring of every string, so a missing recording would read as a
    /// match instead of a failure.
    #[test]
    fn an_empty_recording_is_no_link() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.record_stdout_link().unwrap();
        std::fs::write(stub.dir.join("stdout.link"), "\n").unwrap();
        assert_eq!(stub.stdout_link(), None, "an empty recording is no link");
    }

    #[test]
    fn the_stub_serves_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = StubServer::write(tmp.path(), StubEra::Modern).unwrap();
        stub.set_tools(&json!([[{ "name": "a" }], [{ "name": "b" }], [{ "name": "c" }]]))
            .unwrap();
        let out = drive(
            &stub,
            &[],
            &[
                request(1, "tools/list", json!({})),
                request(2, "tools/list", json!({ "cursor": "p1" })),
                request(3, "tools/list", json!({ "cursor": "p2" })),
            ],
        );
        assert_eq!(out[0]["result"]["tools"][0]["name"], "a");
        assert_eq!(out[0]["result"]["nextCursor"], "p1");
        assert_eq!(out[1]["result"]["tools"][0]["name"], "b");
        assert_eq!(out[1]["result"]["nextCursor"], "p2");
        assert_eq!(out[2]["result"]["tools"][0]["name"], "c");
        assert!(out[2]["result"].get("nextCursor").is_none());

        stub.set_next_cursor(Some("again")).unwrap();
        let out = drive(&stub, &[], &[request(1, "tools/list", json!({}))]);
        assert_eq!(out[0]["result"]["nextCursor"], "again");

        // A flat list is one page again.
        stub.set_tools(&json!([{ "name": "z" }])).unwrap();
        let out = drive(&stub, &[], &[request(1, "tools/list", json!({}))]);
        assert_eq!(out[0]["result"]["tools"], json!([{ "name": "z" }]));
        assert!(out[0]["result"].get("nextCursor").is_none());
    }
}
