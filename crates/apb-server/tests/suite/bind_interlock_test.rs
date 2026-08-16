//! The startup interlock: binding anywhere but loopback requires at least one
//! issued API key. Pure precondition, so it is checked without opening a
//! socket.

use apb_server::check_bind_allowed;
use std::net::{IpAddr, Ipv4Addr};

#[test]
fn loopback_needs_no_keys() {
    assert!(check_bind_allowed(IpAddr::V4(Ipv4Addr::LOCALHOST), 0).is_ok());
    assert!(check_bind_allowed("::1".parse().unwrap(), 0).is_ok());
}

#[test]
fn non_loopback_without_keys_is_refused() {
    let err = check_bind_allowed(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap_err();
    assert!(
        err.contains("0.0.0.0"),
        "the error names the address: {err}"
    );
    assert!(
        err.contains("apb server key issue"),
        "the error names the remedy: {err}"
    );
    assert!(!err.contains('!'), "no exclamation marks: {err}");
    assert!(!err.contains('\u{2014}'), "no em-dashes: {err}");

    let err = check_bind_allowed("10.0.0.5".parse().unwrap(), 0).unwrap_err();
    assert!(err.contains("10.0.0.5"), "{err}");
}

#[test]
fn non_loopback_with_a_key_is_allowed() {
    assert!(check_bind_allowed(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1).is_ok());
    assert!(check_bind_allowed("10.0.0.5".parse().unwrap(), 2).is_ok());
}
