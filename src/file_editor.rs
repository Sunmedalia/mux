//! Dedicated plugin tab entrypoint for editing one native path.
use anyhow::{Context, Result, ensure};
use std::{ffi::OsString, path::Path, process::Command};

pub(crate) fn editor() -> Result<OsString> {
    for key in ["VISUAL", "EDITOR"] {
        if let Some(value) = std::env::var_os(key).filter(|v| !v.is_empty()) {
            return Ok(value);
        }
    }
    for name in ["nvim", "vim", "vi"] {
        if std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|path| {
                let path = path.join(name);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    path.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                }
                #[cfg(not(unix))]
                {
                    path.is_file()
                }
            })
        }) {
            return Ok(name.into());
        }
    }
    anyhow::bail!("No terminal editor found; configure VISUAL or EDITOR")
}

fn edit_command(editor: &std::ffi::OsStr, path: &Path) -> Command {
    // Editor configuration is shell syntax, while the filename is always a positional argument.
    let mut script = OsString::from("exec ");
    script.push(editor);
    script.push(" \"$1\"");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("mux-file-editor")
        .arg(path);
    command
}

pub(crate) fn run() -> Result<()> {
    let result = (|| -> Result<()> {
        let path = std::env::var_os("MUX_FILE_PATH").context("Missing MUX_FILE_PATH")?;
        let path = Path::new(&path);
        ensure!(
            path.is_absolute() && path.is_file(),
            "File is unavailable: {}",
            path.display()
        );
        let editor = std::env::var_os("MUX_FILE_EDITOR")
            .map(Ok)
            .unwrap_or_else(editor)?;
        let status = edit_command(&editor, path)
            .status()
            .context("Could not launch editor")?;
        ensure!(
            status.success(),
            "Editor exited with {status}; configure VISUAL or EDITOR to change it"
        );
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("{error:#}");
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(Command::new(shell).arg("-i").exec().into())
    }
    #[cfg(not(unix))]
    {
        Command::new(shell).status()?;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn path_is_passed_literally_and_editor_accepts_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("中文 ' \" $(touch injected) ; file");
        std::fs::write(&path, b"literal").unwrap();
        let output = edit_command(std::ffi::OsStr::new("cat -n"), &path)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("literal"));
        assert!(!temp.path().join("injected").exists());
    }
}
