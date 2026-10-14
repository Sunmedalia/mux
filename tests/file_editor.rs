//! Exercise the real plugin entrypoint without a live Herdr session or interactive shell.
#[cfg(unix)]
#[test]
fn editor_entrypoint_passes_literal_filename_then_returns_to_shell() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("中文 ' \" $(touch injected) ; file");
    std::fs::write(&file, b"literal file content").unwrap();
    let shell = temp.path().join("shell");
    std::fs::write(
        &shell,
        "#!/bin/sh\nprintf 'returned to shell: %s\\n' \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = assert_cmd::Command::cargo_bin("mux")
        .unwrap()
        .arg("file-edit")
        .env("MUX_FILE_PATH", &file)
        .env("VISUAL", "/bin/cat -n")
        .env("EDITOR", "not-a-real-editor")
        .env_remove("MUX_FILE_EDITOR")
        .env("SHELL", &shell)
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("literal file content"));
    assert!(stdout.contains("returned to shell: -i"));
    assert!(!temp.path().join("injected").exists());
    let failed = assert_cmd::Command::cargo_bin("mux")
        .unwrap()
        .arg("file-edit")
        .env("MUX_FILE_PATH", &file)
        .env("MUX_FILE_EDITOR", "not-a-real-editor")
        .env("SHELL", &shell)
        .output()
        .unwrap();
    assert!(failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Editor exited"));
    assert!(String::from_utf8_lossy(&failed.stdout).contains("returned to shell"));
}
