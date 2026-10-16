//! Native CLI continuation and cross-client handoff, shared by both session browsers.
use super::Session;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Client {
    Claude,
    Pi,
    Codex,
    Grok,
}
impl Client {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Pi, Self::Codex, Self::Grok];
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Pi => "Pi",
            Self::Codex => "Codex",
            Self::Grok => "Grok",
        }
    }
    pub fn program(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Pi => "pi",
            Self::Codex => "codex",
            Self::Grok => "grok",
        }
    }
    pub fn executable(self) -> Result<PathBuf> {
        #[cfg(windows)]
        {
            crate::platform::resolve_program(self.program().as_ref())
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(self.program()))
                .find(|p| {
                    p.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
                .map(std::path::absolute)
                .transpose()?
                .with_context(|| format!("{} is not installed in PATH", self.program()))
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Spec {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
}

pub struct Prepared {
    _lock: File,
    directory: tempfile::TempDir,
    spec: Spec,
    pub source: Session,
    pub target: Client,
}

fn arguments(source: &Session, target: Client, transcript: Option<&Path>) -> Result<Vec<String>> {
    if let Some(path) = transcript {
        let prompt = format!(
            "Continue the task from a previous {} conversation in this project. First read the full local handoff transcript at {} using your file tools (in chunks if needed). It contains historical messages and tool results. Establish the current task and progress, then continue any unfinished user request. Respect completed work and do not repeat completed actions. If nothing remains unfinished, acknowledge the handoff and wait for the user's next instruction. Use your own tools and permissions. Do not assume any prior tool call is still running.",
            source.client,
            path.display()
        );
        return Ok(vec![prompt]);
    }
    Ok(match target {
        Client::Claude | Client::Grok => vec!["--resume".into(), source.id.clone()],
        Client::Codex => vec!["resume".into(), source.id.clone()],
        Client::Pi => vec![
            "--session".into(),
            source
                .sources
                .first()
                .context("Pi session source is unavailable")?
                .to_str()
                .context("Session path is not UTF-8")?
                .into(),
        ],
    })
}

/// Clean crashed launches only when no preparation/runner holds the directory lock.
fn cleanup(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("handoff-") {
            continue;
        }
        let path = entry.path();
        // A just-created directory may not have its lock yet.
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 86400);
        if !old {
            continue;
        }
        if let Ok(lock) = File::open(path.join("lock"))
            && lock.try_lock_exclusive().is_ok()
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}

pub fn prepare(source: Session, target: Client, state: &Path) -> Result<Prepared> {
    ensure!(!source.child, "Child-agent sessions cannot be taken over");
    ensure!(
        !source.id.is_empty() && !source.id.starts_with('-'),
        "Invalid session ID"
    );
    let cwd = PathBuf::from(&source.project);
    ensure!(
        cwd.is_absolute() && cwd.is_dir(),
        "Project directory is unavailable: {}",
        cwd.display()
    );
    ensure!(
        !source.sources.is_empty() && source.sources.iter().all(|p| p.is_file()),
        "Session log is unavailable; refresh Sessions"
    );
    let program = target.executable()?;
    // Check the actual installed CLI instead of relying on version-number guesses.
    let mut help = Command::new(&program);
    if target == Client::Codex && source.client == target.name() {
        help.arg("resume");
    }
    let output = crate::managed_process::output_timeout(
        help.arg("--help"),
        std::time::Duration::from_secs(10),
    )?;
    ensure!(
        output.status.success(),
        "Cannot inspect {} CLI capabilities",
        target.name()
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let same = source.client == target.name();
    let supported = if same {
        match target {
            Client::Claude | Client::Grok => text.contains("--resume"),
            Client::Pi => text.contains("--session"),
            Client::Codex => text.contains("SESSION_ID"),
        }
    } else {
        text.contains("[prompt]") || text.contains("[PROMPT]") || text.contains("[messages...]")
    };
    ensure!(
        supported,
        "Installed {} CLI does not advertise the required continuation arguments; update the CLI",
        target.name()
    );
    let root = state.join("session-handoffs");
    fs::create_dir_all(&root)?;
    cleanup(&root);
    let directory = tempfile::Builder::new()
        .prefix("handoff-")
        .tempdir_in(root)?;
    let lock = File::create(directory.path().join("lock"))?;
    lock.lock_exclusive()?;
    let transcript = if same {
        None
    } else {
        let path = directory.path().join("conversation.md");
        let mut file = File::create(&path)?;
        super::handoff::transcript(&source, &mut file)?;
        file.flush()?;
        Some(path)
    };
    let spec = Spec {
        program,
        args: arguments(&source, target, transcript.as_deref())?,
        cwd,
    };
    fs::write(
        directory.path().join("launch.json"),
        serde_json::to_vec(&spec)?,
    )?;
    Ok(Prepared {
        directory,
        _lock: lock,
        spec,
        source,
        target,
    })
}

fn herdr(args: &[&str]) -> Result<serde_json::Value> {
    let mut cmd =
        Command::new(std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()));
    let out =
        crate::managed_process::output_timeout(cmd.args(args), std::time::Duration::from_secs(15))?;
    ensure!(
        out.status.success(),
        "Herdr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if out.stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    ensure!(
        value.get("error").is_none_or(serde_json::Value::is_null),
        "Herdr: {}",
        value["error"]
    );
    Ok(value)
}

fn open_tab(
    prepared: &Prepared,
    workspace: &str,
    mut execute: impl FnMut(&[&str]) -> Result<serde_json::Value>,
) -> Result<bool> {
    if prepared.source.client == prepared.target.name() {
        let list = execute(&["pane", "list", "--workspace", workspace])?;
        if let Some(panes) = list["result"]["panes"].as_array() {
            let matching: Vec<_> = panes
                .iter()
                .filter(|p| {
                    let agent = &p["agent_session"];
                    let mut identity = match agent["kind"].as_str() {
                        Some("id") => agent["value"].as_str().map(str::to_owned),
                        Some("path") => agent["value"].as_str().and_then(|p| {
                            super::id_from_log(Path::new(p)).or_else(|| {
                                (prepared.target == Client::Claude)
                                    .then(|| Path::new(p).file_stem()?.to_str().map(str::to_owned))
                                    .flatten()
                            })
                        }),
                        _ => None,
                    };
                    if prepared.target == Client::Codex
                        && let (Some(title), Some(cwd), Ok(home)) = (
                            p["terminal_title_stripped"].as_str(),
                            p["foreground_cwd"].as_str().or(p["cwd"].as_str()),
                            crate::codex::home(),
                        )
                        && let Some(id) = super::codex_id_from_title(&home, title, cwd)
                    {
                        identity = Some(id);
                    }
                    agent["agent"].as_str() == Some(prepared.target.program())
                        && identity.as_deref() == Some(prepared.source.id.as_str())
                })
                .collect();
            if matching.len() == 1 {
                let pane = matching[0];
                execute(&[
                    "tab",
                    "focus",
                    pane["tab_id"].as_str().context("Missing tab ID")?,
                ])?;
                execute(&[
                    "pane",
                    "focus",
                    pane["pane_id"].as_str().context("Missing pane ID")?,
                ])?;
                return Ok(false);
            }
        }
    }
    let opened = execute(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        "mux",
        "--entrypoint",
        "session",
        "--placement",
        "tab",
        "--workspace",
        workspace,
        "--cwd",
        prepared
            .spec
            .cwd
            .to_str()
            .context("Project path is not UTF-8")?,
        "--env",
        &format!(
            "MUX_SESSION_LAUNCH={}",
            prepared.directory.path().join("launch.json").display()
        ),
        "--no-focus",
    ])?;
    // The runner now owns this directory, even if subsequent focus calls fail.
    let pane = &opened["result"]["plugin_pane"]["pane"];
    let tab = pane["tab_id"]
        .as_str()
        .context("Session tab opened but no tab ID returned")?;
    let id = pane["pane_id"]
        .as_str()
        .context("Session tab opened but no pane ID returned")?;
    execute(&["tab", "focus", tab])?;
    execute(&["plugin", "pane", "focus", id])?;
    Ok(true)
}

impl Prepared {
    pub fn in_herdr() -> bool {
        std::env::var("HERDR_ENV").as_deref() == Ok("1")
    }
    fn launch_herdr(
        self,
        workspace: &str,
        mut execute: impl FnMut(&[&str]) -> Result<serde_json::Value>,
    ) -> Result<()> {
        let mut transferred = false;
        let result = open_tab(&self, workspace, |args| {
            let value = execute(args)?;
            if args.starts_with(&["plugin", "pane", "open"]) {
                transferred = true;
            }
            Ok(value)
        });
        // Once a pane has opened, the runner owns cleanup even if focus fails.
        // Failed creation and existing-pane focus keep normal TempDir cleanup.
        if transferred {
            let _ = self.directory.keep();
        }
        result.map(|_| ())
    }
    pub fn launch(self) -> Result<()> {
        if Self::in_herdr() {
            let workspace = std::env::var("MUX_MONITOR_WORKSPACE")
                .or_else(|_| std::env::var("HERDR_WORKSPACE_ID"))
                .context("Missing Herdr workspace")?;
            self.launch_herdr(&workspace, herdr)
        } else {
            run_cli(&self.spec)?;
            Ok(())
        }
    }
}

// Ctrl+C belongs to the foreground AI while Mux waits. Caught Unix signal
// dispositions reset on exec, so the child still receives the normal interrupt.
fn run_cli(spec: &Spec) -> Result<()> {
    let _interrupts = interrupts::Guard::new()?;
    let status = Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .status()
        .context("Could not launch AI CLI")?;
    ensure!(status.success(), "AI CLI exited with {status}");
    Ok(())
}

#[cfg(unix)]
mod interrupts {
    use anyhow::{Context, Result};
    pub struct Guard {
        previous: Vec<(i32, libc::sigaction)>,
    }
    extern "C" fn wait_for_child(_: i32) {}
    impl Guard {
        pub fn new() -> Result<Self> {
            let mut guard = Self {
                previous: Vec::new(),
            };
            for signal in [libc::SIGINT, libc::SIGQUIT] {
                // SAFETY: sigaction structures are initialized, and the handler
                // touches no memory. Drop restores the exact prior disposition.
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    let mut previous = std::mem::zeroed();
                    action.sa_sigaction = wait_for_child as *const () as usize;
                    libc::sigemptyset(&mut action.sa_mask);
                    action.sa_flags = libc::SA_RESTART;
                    if libc::sigaction(signal, &action, &mut previous) != 0 {
                        return Err(std::io::Error::last_os_error())
                            .context("Cannot suspend Mux interrupt handling");
                    }
                    guard.previous.push((signal, previous));
                }
            }
            Ok(guard)
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            for (signal, previous) in &self.previous {
                // SAFETY: the prior actions were returned by sigaction above.
                unsafe {
                    libc::sigaction(*signal, previous, std::ptr::null_mut());
                }
            }
        }
    }
}
#[cfg(windows)]
mod interrupts {
    use anyhow::Result;
    use windows::{
        Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler},
        core::BOOL,
    };
    pub struct Guard;
    unsafe extern "system" fn wait_for_child(event: u32) -> BOOL {
        BOOL::from(event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT)
    }
    impl Guard {
        pub fn new() -> Result<Self> {
            // SAFETY: this process-local handler is removed when the child exits.
            unsafe {
                SetConsoleCtrlHandler(Some(wait_for_child), true)?;
            }
            Ok(Self)
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: remove the same handler registered in new().
            let _ = unsafe { SetConsoleCtrlHandler(Some(wait_for_child), false) };
        }
    }
}

/// Isolated Herdr entrypoint; only the runner owns the exported transcript lifetime.
pub fn run() -> Result<()> {
    let path = PathBuf::from(
        std::env::var_os("MUX_SESSION_LAUNCH").context("Missing session launch specification")?,
    );
    let directory = path.parent().context("Invalid launch path")?;
    let lock = File::open(directory.join("lock"))?;
    lock.lock_exclusive()?;
    let result = (|| -> Result<()> {
        let spec: Spec = serde_json::from_slice(&fs::read(&path)?)?;
        run_cli(&spec)?;
        Ok(())
    })();
    drop(lock);
    let _ = fs::remove_dir_all(directory);
    if let Err(error) = result {
        eprintln!("{error:#}");
    }
    // Keep the tab useful after the CLI exits, matching the file-editor tab.
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| {
        if cfg!(windows) {
            "cmd.exe".into()
        } else {
            "/bin/sh".into()
        }
    });
    let mut cmd = Command::new(shell);
    if !cfg!(windows) {
        cmd.arg("-i");
    }
    cmd.status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn prepared(client: Client, target: Client) -> Prepared {
        let directory = tempfile::tempdir().unwrap();
        let lock = File::create(directory.path().join("lock")).unwrap();
        Prepared {
            directory,
            _lock: lock,
            spec: Spec {
                program: "unused".into(),
                args: vec![],
                cwd: "/project 中文 ' $(literal)".into(),
            },
            source: Session {
                client: client.name(),
                id: "exact-id".into(),
                sources: vec!["/pi session.jsonl".into()],
                ..Default::default()
            },
            target,
        }
    }
    #[test]
    fn native_resume_arguments_and_all_twelve_cross_client_pairs() {
        for source in Client::ALL {
            let p = prepared(source, source);
            let args = arguments(&p.source, source, None).unwrap();
            let expected = match source {
                Client::Claude | Client::Grok => vec!["--resume", "exact-id"],
                Client::Codex => vec!["resume", "exact-id"],
                Client::Pi => vec!["--session", "/pi session.jsonl"],
            };
            assert_eq!(args, expected);
            for target in Client::ALL {
                if target == source {
                    continue;
                }
                let args = arguments(
                    &p.source,
                    target,
                    Some(Path::new("/context 中文 ' $(literal).md")),
                )
                .unwrap();
                assert_eq!(args.len(), 1);
                assert!(args[0].contains("/context 中文 ' $(literal).md"));
                assert!(args[0].contains(source.name()));
                assert!(!args.iter().any(|a| a == "--resume"));
            }
        }
    }
    #[test]
    fn exact_existing_session_is_focused_and_cross_client_always_opens_tab() {
        for target in [Client::Claude, Client::Pi, Client::Codex, Client::Grok] {
            let p = prepared(target, target);
            let mut calls = Vec::new();
            let opened = open_tab(&p, "workspace", |args| {
                calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
                Ok(if args[0] == "pane" && args[1] == "list" { json!({"result":{"panes":[{"tab_id":"tab-existing","pane_id":"existing","agent_session":{"agent":target.program(),"kind":"id","value":"exact-id"}}]}}) } else { json!(null) })
            }).unwrap();
            assert!(!opened);
            assert_eq!(calls[1], ["tab", "focus", "tab-existing"]);
            assert_eq!(calls[2], ["pane", "focus", "existing"]);
        }
        let p = prepared(Client::Claude, Client::Codex);
        let mut calls = Vec::new();
        assert!(open_tab(&p, "workspace", |args| {
            calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            Ok(json!({"result":{"plugin_pane":{"pane":{"tab_id":"new-tab","pane_id":"new-pane"}}}}))
        }).unwrap());
        assert_eq!(calls[0][0..3], ["plugin", "pane", "open"]);
        assert!(calls[0].iter().any(|s| s == "/project 中文 ' $(literal)"));
        assert_eq!(calls[1], ["tab", "focus", "new-tab"]);
        assert_eq!(calls[2], ["plugin", "pane", "focus", "new-pane"]);
    }
    #[test]
    fn ambiguous_live_sessions_open_a_new_tab_and_errors_propagate() {
        let p = prepared(Client::Claude, Client::Claude);
        let pane = json!({"tab_id":"tab","pane_id":"pane","agent_session":{"agent":"claude","kind":"id","value":"exact-id"}});
        let mut opened = false;
        assert!(
            open_tab(&p, "workspace", |args| {
                if args[0] == "pane" && args[1] == "list" {
                    return Ok(json!({"result":{"panes":[pane.clone(),pane.clone()]}}));
                }
                opened = true;
                anyhow::bail!("cannot create tab")
            })
            .is_err()
        );
        assert!(opened);
    }
    #[test]
    fn tab_creation_failure_cleans_snapshot_but_focus_failure_preserves_runner_files() {
        let p = prepared(Client::Claude, Client::Codex);
        let path = p.directory.path().to_path_buf();
        assert!(
            p.launch_herdr("workspace", |_| anyhow::bail!("creation failed"))
                .is_err()
        );
        assert!(!path.exists());
        let p = prepared(Client::Claude, Client::Codex);
        let path = p.directory.path().to_path_buf();
        assert!(
            p.launch_herdr("workspace", |args| {
                if args.starts_with(&["plugin", "pane", "open"]) {
                    Ok(json!({"result":{"plugin_pane":{"pane":{"tab_id":"tab","pane_id":"pane"}}}}))
                } else {
                    anyhow::bail!("focus failed")
                }
            })
            .is_err()
        );
        assert!(path.exists());
        fs::remove_dir_all(path).unwrap();
    }
}
