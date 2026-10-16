//! Real terminal transitions with isolated fake agents; never calls model APIs.
#[cfg(unix)]
#[test]
fn native_session_takeover_restores_terminal_in_both_browsers() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/session_takeover_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_mux"))
        .output()
        .expect("Python 3 is required for the PTY regression fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
