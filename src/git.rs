//! Git operations bound to a concrete working tree, independent of the Pulse UI.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Conflict,
    Worktree,
    Index,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub x: u8,
    pub y: u8,
    pub group: Group,
    pub stat: String,
}
impl File {
    pub fn label(&self) -> String {
        let path = display_path(&self.path);
        self.old_path.as_ref().map_or(path.clone(), |old| {
            format!("{} → {path}", display_path(old))
        })
    }
    pub fn untracked(&self) -> bool {
        self.x == b'?'
    }
}
pub fn display_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub files: Vec<File>,
    pub head: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Commit {
    pub id: String,
    pub short: String,
    pub date: String,
    pub author: String,
    pub subject: String,
    pub refs: String,
}
pub const LOG_PAGE_SIZE: usize = 50;
#[derive(Clone, Debug)]
pub struct History {
    pub commits: Vec<Commit>,
    pub page: usize,
    pub has_more: bool,
    pub query: String,
}
#[derive(Clone, Debug)]
pub struct Hunk {
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug)]
pub struct Diff {
    pub file: File,
    pub raw: Vec<u8>,
    pub lines: Vec<String>,
    pub hunks: Vec<Hunk>,
    pub fingerprint: Vec<u8>,
    pub reason: Option<String>,
}
impl Diff {
    pub fn patch(&self, hunk: usize) -> Result<Vec<u8>> {
        ensure!(
            self.reason.is_none(),
            "This change supports whole-file operations only"
        );
        let selected = self.hunks.get(hunk).context("Select a difference block")?;
        let lines: Vec<&[u8]> = self.raw.split_inclusive(|b| *b == b'\n').collect();
        let first = self.hunks.first().context("No difference blocks")?.start;
        Ok(lines[..first]
            .iter()
            .chain(lines[selected.start..selected.end].iter())
            .flat_map(|l| l.iter().copied())
            .collect())
    }
}
#[derive(Clone, Debug)]
pub enum Action {
    Stage(File),
    Unstage(File),
    StageAll,
    UnstageAll,
    Discard(Diff),
    Hunk {
        diff: Diff,
        hunk: usize,
        discard: bool,
    },
    Commit {
        message: String,
        head: Option<String>,
        index: Vec<u8>,
    },
    Switch(String),
    Create(String),
    Track {
        local: String,
        remote: String,
    },
    Fetch(String),
    Pull,
    Push {
        remote: Option<String>,
    },
}

fn os(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes.to_vec())
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).as_ref())
    }
}
fn args(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

// Drain both pipes concurrently, including hooks and network commands. No command
// can hold the UI thread or wait for a terminal password prompt.
fn run(root: &Path, parts: Vec<OsString>, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(root)
        .args(parts)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    if parts_are_network(&command) {
        let ssh = std::env::var("GIT_SSH_COMMAND")
            .ok()
            .or_else(|| text(root, &["config", "--get", "core.sshCommand"]).ok())
            .or_else(|| {
                std::env::var("GIT_SSH")
                    .ok()
                    .map(|path| format!("'{}'", path.replace('\'', "'\\''")))
            })
            .unwrap_or_else(|| "ssh".into());
        let variant = std::env::var("GIT_SSH_VARIANT")
            .ok()
            .or_else(|| text(root, &["config", "--get", "ssh.variant"]).ok());
        let batch = if matches!(
            variant.as_deref(),
            Some("plink" | "putty" | "tortoiseplink")
        ) || ssh.contains("plink")
        {
            " -batch"
        } else if variant.as_deref() == Some("simple") {
            ""
        } else {
            " -oBatchMode=yes"
        };
        command.env("GIT_SSH_COMMAND", format!("{ssh}{batch}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .context("Cannot start Git; install git and check PATH")?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let (send, receive) = std::sync::mpsc::channel();
    for (stdout, pipe) in [
        (
            true,
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        ),
        (
            false,
            Box::new(child.stderr.take().unwrap()) as Box<dyn Read + Send>,
        ),
    ] {
        let send = send.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut kept = Vec::new();
            let mut buffer = [0u8; 8192];
            let result = loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break Ok(kept),
                    Ok(n) => {
                        if kept.len() < 16 * 1024 * 1024 {
                            kept.extend_from_slice(&buffer[..n.min(16 * 1024 * 1024 - kept.len())]);
                        }
                    }
                    Err(e) => break Err(e),
                }
            };
            let _ = send.send((stdout, result));
        });
    }
    if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            bail!("Git timed out after 120s; inspect repository state before retrying");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    for _ in 0..2 {
        let (out, result) = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("Git output did not close; inspect hooks or credential helper")?;
        if out {
            stdout = result?;
        } else {
            stderr = result?;
        }
    }
    ensure!(
        stdout.len() < 16 * 1024 * 1024,
        "Git output is too large; narrow the selection"
    );
    ensure!(
        status.success(),
        "{}",
        String::from_utf8_lossy(if stderr.is_empty() { &stdout } else { &stderr }).trim()
    );
    Ok(stdout)
}
fn parts_are_network(command: &Command) -> bool {
    command
        .get_args()
        .nth(4)
        .is_some_and(|arg| matches!(arg.to_str(), Some("fetch" | "pull" | "push")))
}
fn git(root: &Path, parts: &[&str]) -> Result<Vec<u8>> {
    run(root, args(parts), None)
}
fn text(root: &Path, parts: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(root, parts)?)
        .trim_end_matches(['\r', '\n'])
        .into())
}

pub fn discover(cwd: &Path) -> Result<Option<PathBuf>> {
    // Distinguish a normal non-repository directory from Git/access failures.
    let mut directory = Some(cwd);
    while let Some(path) = directory {
        if path.join(".git").exists() {
            let raw = git(cwd, &["rev-parse", "--show-toplevel"])?;
            let trimmed = raw.strip_suffix(b"\n").unwrap_or(&raw);
            return Ok(Some(PathBuf::from(os(trimmed))));
        }
        directory = path.parent();
    }
    ensure!(cwd.is_dir(), "Project directory is unavailable");
    Ok(None)
}
fn parse_files(raw: &[u8]) -> Result<Vec<File>> {
    let mut records = raw.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        ensure!(
            record.len() >= 4 && record[2] == b' ',
            "Invalid Git status record"
        );
        let (x, y) = (record[0], record[1]);
        let path = PathBuf::from(os(&record[3..]));
        let old_path = if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            Some(PathBuf::from(os(records
                .next()
                .context("Missing rename source")?)))
        } else {
            None
        };
        let conflict =
            x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D');
        for group in if conflict {
            vec![Group::Conflict]
        } else {
            let mut groups = Vec::new();
            if y != b' ' && y != b'!' {
                groups.push(Group::Worktree);
            }
            if x != b' ' && x != b'?' && x != b'!' {
                groups.push(Group::Index);
            }
            groups
        } {
            files.push(File {
                path: path.clone(),
                old_path: old_path.clone(),
                x,
                y,
                group,
                stat: String::new(),
            });
        }
    }
    files.sort_by(|a, b| (group_order(a.group), &a.path).cmp(&(group_order(b.group), &b.path)));
    Ok(files)
}
fn group_order(group: Group) -> u8 {
    match group {
        Group::Conflict => 0,
        Group::Worktree => 1,
        Group::Index => 2,
    }
}
fn diff_args(file: &File, stat: bool) -> Vec<OsString> {
    let mut parts = args(&["diff", "--no-ext-diff", "--no-textconv", "--no-color"]);
    parts.extend(args(if stat {
        &["--numstat", "-z"][..]
    } else {
        &[
            "--patch",
            "--full-index",
            "--no-renames",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ][..]
    }));
    if file.group == Group::Index {
        parts.push("--cached".into());
    }
    parts.push("--".into());
    parts.push(file.path.as_os_str().into());
    if let Some(old) = &file.old_path {
        parts.push(old.as_os_str().into());
    }
    parts
}
pub fn snapshot(root: &Path) -> Result<Snapshot> {
    let mut files = parse_files(&git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?)?;
    // Two aggregate numstat calls avoid spawning one process per modified file.
    for group in [Group::Worktree, Group::Index] {
        let mut parts = args(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--numstat",
            "-z",
        ]);
        if group == Group::Index {
            parts.push("--cached".into());
        }
        for record in run(root, parts, None)?
            .split(|b| *b == 0)
            .filter(|r| !r.is_empty())
        {
            let mut columns = record.splitn(3, |b| *b == b'\t');
            if let (Some(add), Some(del), Some(path)) =
                (columns.next(), columns.next(), columns.next())
            {
                let path = os(path);
                for file in files
                    .iter_mut()
                    .filter(|f| f.group == group && f.path.as_os_str() == path)
                {
                    file.stat = if add == b"-" {
                        "binary".into()
                    } else {
                        format!(
                            "+{} -{}",
                            String::from_utf8_lossy(add),
                            String::from_utf8_lossy(del)
                        )
                    };
                }
            }
        }
    }
    let head = text(root, &["rev-parse", "--verify", "HEAD"]).ok();
    let branch = text(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_else(|_| {
        format!(
            "detached {}",
            head.as_deref()
                .unwrap_or("unknown")
                .chars()
                .take(8)
                .collect::<String>()
        )
    });
    let upstream = text(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok();
    let (ahead, behind) = if upstream.is_some() {
        let counts = text(
            root,
            &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )?;
        let mut values = counts
            .split_whitespace()
            .map(|v| v.parse::<u64>().unwrap_or(0));
        (values.next().unwrap_or(0), values.next().unwrap_or(0))
    } else {
        (0, 0)
    };
    Ok(Snapshot {
        root: root.into(),
        branch,
        upstream,
        ahead,
        behind,
        files,
        head,
    })
}
fn fingerprint(root: &Path, file: &File, raw: &[u8]) -> Result<Vec<u8>> {
    let mut hash = Sha256::new();
    hash.update(raw);
    hash.update(git(root, &["ls-files", "--stage", "-z"])?);
    for path in std::iter::once(&file.path).chain(file.old_path.iter()) {
        let path = root.join(path);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                hash.update(os_bytes(std::fs::read_link(path)?.as_os_str()));
            }
            Ok(meta) if meta.is_file() => {
                let mut stream = std::fs::File::open(path)?;
                let mut buffer = [0; 8192];
                loop {
                    let n = stream.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buffer[..n]);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    hash.update(meta.permissions().mode().to_le_bytes());
                }
            }
            Ok(_) => hash.update(b"directory"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash.update(b"missing"),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(hash.finalize().to_vec())
}
fn os_bytes(value: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}
pub fn diff(root: &Path, file: &File) -> Result<Diff> {
    let raw = if file.untracked() {
        // Read-only preview: never use intent-to-add to inspect untracked content.
        let path = root.join(&file.path);
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            format!("Symlink → {}", display_path(&std::fs::read_link(&path)?)).into_bytes()
        } else {
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 2 * 1024 * 1024 {
                bytes.truncate(2 * 1024 * 1024);
                bytes.extend_from_slice(
                    b"\n[Preview truncated after 2 MiB; whole-file operations remain available]\n",
                );
            }
            bytes
        }
    } else {
        run(root, diff_args(file, false), None)?
    };
    let mut lines: Vec<String> = String::from_utf8_lossy(&raw)
        .lines()
        .map(str::to_owned)
        .collect();
    let reason = if file.group == Group::Conflict {
        Some("Resolve conflict externally, then stage the file")
    } else if file.untracked() {
        Some("New file: whole-file operations only")
    } else if file.old_path.is_some() || file.x == b'A' || file.x == b'D' || file.y == b'D' {
        Some("Added/deleted/renamed file: whole-file operations only")
    } else if raw.contains(&0)
        || lines
            .iter()
            .any(|l| l.starts_with("Binary files ") || l.starts_with("GIT binary patch"))
    {
        Some("Binary file: whole-file operations only")
    } else if lines.iter().any(|l| {
        l.starts_with("old mode ")
            || l.contains("Subproject commit")
            || l.starts_with("new file mode ")
            || (l.starts_with("index ") && (l.ends_with(" 120000") || l.ends_with(" 160000")))
    }) {
        Some("Mode/submodule change: whole-file operations only")
    } else {
        None
    }
    .map(str::to_owned);
    if raw.contains(&0) {
        lines = vec!["Binary file; no text preview".into()];
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    if !file.untracked() {
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with("@@ ") {
                if let Some(last) = hunks.last_mut() {
                    last.end = i;
                }
                hunks.push(Hunk {
                    start: i,
                    end: lines.len(),
                });
            }
        }
    }
    let fingerprint = fingerprint(root, file, &raw)?;
    Ok(Diff {
        file: file.clone(),
        raw,
        lines,
        hunks,
        fingerprint,
        reason,
    })
}
pub fn index_stamp(root: &Path) -> Result<Vec<u8>> {
    git(root, &["ls-files", "--stage", "-z"])
}
pub fn history(root: &Path, page: usize, query: &str) -> Result<History> {
    let mut history = History {
        commits: vec![],
        page,
        has_more: false,
        query: query.into(),
    };
    if snapshot(root)?.head.is_none() {
        return Ok(history);
    }
    let skip = page
        .checked_mul(LOG_PAGE_SIZE)
        .context("History page is too large")?;
    let mut parts = args(&[
        "log",
        "-z",
        "--date-order",
        "--decorate=short",
        "--format=%H%x00%h%x00%aI%x00%an%x00%s%x00%D",
        &format!("--skip={skip}"),
        &format!("--max-count={}", LOG_PAGE_SIZE + 1),
    ]);
    if !query.is_empty() {
        parts.extend(args(&["--fixed-strings", "--regexp-ignore-case"]));
        parts.push(format!("--grep={query}").into());
    }
    parts.extend(args(&["HEAD", "--"]));
    let output = run(root, parts, None)?;
    for fields in output
        .split(|b| *b == 0)
        .collect::<Vec<_>>()
        .chunks_exact(6)
    {
        let value = |i| String::from_utf8_lossy(fields[i]).into_owned();
        history.commits.push(Commit {
            id: value(0),
            short: value(1),
            date: value(2),
            author: value(3),
            subject: value(4),
            refs: value(5),
        });
    }
    history.has_more = history.commits.len() > LOG_PAGE_SIZE;
    history.commits.truncate(LOG_PAGE_SIZE);
    Ok(history)
}
pub fn show_commit(root: &Path, id: &str) -> Result<String> {
    ensure!(
        matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid commit id"
    );
    text(
        root,
        &[
            "show",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--format=fuller",
            "--stat",
            "--patch",
            "--find-renames",
            id,
            "--",
        ],
    )
}
pub fn branches(root: &Path) -> Result<Vec<String>> {
    Ok(text(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ],
    )?
    .lines()
    .filter(|s| !s.ends_with("/HEAD"))
    .filter_map(|s| {
        s.strip_prefix("refs/heads/")
            .map(str::to_owned)
            .or_else(|| {
                s.strip_prefix("refs/remotes/")
                    .map(|s| format!("remote: {s}"))
            })
    })
    .collect())
}
pub fn remotes(root: &Path) -> Result<Vec<String>> {
    Ok(text(root, &["remote"])?
        .lines()
        .map(str::to_owned)
        .collect())
}
fn file_command(root: &Path, parts: &[&str], file: &File) -> Result<()> {
    let mut parts = args(parts);
    parts.push("--".into());
    parts.push(file.path.as_os_str().into());
    if let Some(old) = &file.old_path {
        parts.push(old.as_os_str().into());
    }
    run(root, parts, None)?;
    Ok(())
}
fn check_diff(root: &Path, expected: &Diff) -> Result<()> {
    let current = diff(root, &expected.file)?;
    ensure!(
        current.raw == expected.raw && current.fingerprint == expected.fingerprint,
        "File or index changed; refresh and select the change again"
    );
    Ok(())
}
pub fn perform(root: &Path, action: Action) -> Result<String> {
    let current = snapshot(root)?;
    let mut output = String::new();
    match action {
        Action::Stage(file) => file_command(root, &["add"], &file)?,
        Action::Unstage(file) => file_command(
            root,
            if current.head.is_some() {
                &["restore", "--staged"]
            } else {
                &["rm", "--cached", "--force"]
            },
            &file,
        )?,
        Action::StageAll => {
            git(root, &["add", "-A", "--", "."])?;
            output = "Staged all changes".into();
        }
        Action::UnstageAll => {
            if current.head.is_some() {
                git(root, &["restore", "--staged", "--", "."])?;
            } else {
                for file in current.files.iter().filter(|f| f.group == Group::Index) {
                    file_command(root, &["rm", "--cached", "--force"], file)?;
                }
            }
        }
        Action::Discard(expected) => {
            ensure!(
                expected.file.group == Group::Worktree,
                "Only unstaged changes can be discarded"
            );
            check_diff(root, &expected)?;
            if expected.file.untracked() {
                ensure!(
                    !root.join(&expected.file.path).is_dir(),
                    "Refusing to delete a directory"
                );
                std::fs::remove_file(root.join(&expected.file.path))?;
            } else {
                file_command(root, &["restore", "--worktree"], &expected.file)?;
            }
        }
        Action::Hunk {
            diff: expected,
            hunk,
            discard,
        } => {
            check_diff(root, &expected)?;
            ensure!(
                !discard || expected.file.group == Group::Worktree,
                "Only unstaged changes can be discarded"
            );
            let patch = expected.patch(hunk)?;
            let mut parts = args(&["apply", "--whitespace=nowarn"]);
            if !discard {
                parts.push("--cached".into());
            }
            if discard || expected.file.group == Group::Index {
                parts.push("--reverse".into());
            }
            let mut check = parts.clone();
            check.push("--check".into());
            run(root, check, Some(patch.clone()))?;
            run(root, parts, Some(patch))?;
        }
        Action::Commit {
            message,
            head,
            index,
        } => {
            ensure!(
                !message.lines().next().unwrap_or("").trim().is_empty(),
                "Commit title is required"
            );
            ensure!(
                current.files.iter().any(|f| f.group == Group::Index),
                "No staged changes"
            );
            ensure!(
                !current.files.iter().any(|f| f.group == Group::Conflict),
                "Resolve conflicts before committing"
            );
            ensure!(
                current.head == head && index_stamp(root)? == index,
                "HEAD or index changed; review staged files and reopen Commit"
            );
            output = String::from_utf8_lossy(&run(
                root,
                args(&["commit", "-F", "-"]),
                Some(message.into_bytes()),
            )?)
            .into();
        }
        Action::Switch(branch) => {
            ensure!(!branch.starts_with('-'), "Invalid branch");
            output = text(root, &["switch", &branch])?;
        }
        Action::Create(branch) => {
            git(root, &["check-ref-format", "--branch", &branch])?;
            output = text(root, &["switch", "-c", &branch])?;
        }
        Action::Track { local, remote } => {
            git(root, &["check-ref-format", "--branch", &local])?;
            ensure!(!remote.starts_with('-'), "Invalid remote branch");
            output = text(root, &["switch", "-c", &local, "--track", &remote])?;
        }
        Action::Fetch(remote) => {
            ensure!(remotes(root)?.contains(&remote), "Remote no longer exists");
            output = text(root, &["fetch", "--", &remote])?;
        }
        Action::Pull => {
            ensure!(
                current.upstream.is_some(),
                "Set an upstream branch before pulling"
            );
            ensure!(
                current.files.is_empty(),
                "Commit or discard local changes before pulling"
            );
            output = text(root, &["pull", "--ff-only", "--no-autostash"])?;
        }
        Action::Push { remote } => {
            ensure!(
                current.head.is_some() && !current.branch.starts_with("detached "),
                "Push requires a branch with commits"
            );
            output = if let Some(remote) = remote {
                ensure!(remotes(root)?.contains(&remote), "Remote no longer exists");
                text(
                    root,
                    &[
                        "push",
                        "--set-upstream",
                        "--",
                        &remote,
                        &format!("HEAD:refs/heads/{}", current.branch),
                    ],
                )?
            } else {
                ensure!(
                    current.upstream.is_some(),
                    "Select a remote for the first push"
                );
                let remote = text(
                    root,
                    &[
                        "config",
                        "--get",
                        &format!("branch.{}.remote", current.branch),
                    ],
                )?;
                let target = text(
                    root,
                    &[
                        "config",
                        "--get",
                        &format!("branch.{}.merge", current.branch),
                    ],
                )?;
                ensure!(
                    target.starts_with("refs/heads/"),
                    "Upstream must be a branch"
                );
                // Explicit refspecs prevent push.default=matching or remote.push
                // from silently pushing additional branches.
                text(root, &["push", "--", &remote, &format!("HEAD:{target}")])?
            };
        }
    }
    Ok(if output.trim().is_empty() {
        "Git operation completed".into()
    } else {
        output.trim().into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]).unwrap();
        for (name, value) in [
            ("user.name", "Mux Test"),
            ("user.email", "mux@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            git(dir.path(), &["config", name, value]).unwrap();
        }
        let hooks = dir.path().join(".git/empty-hooks");
        std::fs::create_dir(&hooks).unwrap();
        git(
            dir.path(),
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        )
        .unwrap();
        dir
    }
    fn write(root: &Path, path: &str, content: &str) {
        std::fs::write(root.join(path), content).unwrap();
    }
    fn commit(root: &Path) {
        git(root, &["add", "-A"]).unwrap();
        git(root, &["commit", "-m", "fixture"]).unwrap();
    }
    fn file(root: &Path, group: Group) -> File {
        snapshot(root)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.group == group)
            .unwrap()
    }

    #[test]
    fn stage_all_includes_modified_deleted_and_new_files() {
        let dir = repo();
        let root = dir.path();
        write(root, "modified", "before\n");
        write(root, "deleted", "before\n");
        commit(root);
        write(root, "modified", "after\n");
        std::fs::remove_file(root.join("deleted")).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        write(root, "sub/new", "new\n");
        assert_eq!(
            perform(root, Action::StageAll).unwrap(),
            "Staged all changes"
        );
        let s = snapshot(root).unwrap();
        assert_eq!(s.files.len(), 3);
        assert!(s.files.iter().all(|file| file.group == Group::Index));
        assert!(
            s.files
                .iter()
                .any(|f| f.path == Path::new("deleted") && f.x == b'D')
        );
    }

    #[test]
    fn history_pages_and_details_preserve_worktree_and_index() {
        let dir = repo();
        let root = dir.path();
        assert!(history(root, 0, "").unwrap().commits.is_empty());
        write(root, "tracked", "initial\n");
        commit(root);
        for i in 0..LOG_PAGE_SIZE {
            git(
                root,
                &[
                    "commit",
                    "--allow-empty",
                    "-m",
                    &format!("history {i} 中文"),
                    "-m",
                    "Full commit body [literal]",
                ],
            )
            .unwrap();
        }
        write(root, "tracked", "unstaged\n");
        let index = index_stamp(root).unwrap();
        let first = history(root, 0, "").unwrap();
        assert_eq!(first.commits.len(), LOG_PAGE_SIZE);
        assert!(first.has_more);
        assert_eq!(first.commits[0].subject, "history 49 中文");
        assert_eq!(first.commits[0].author, "Mux Test");
        assert!(first.commits[0].refs.contains("HEAD -> main"));
        assert!(first.commits[1].refs.is_empty());
        let second = history(root, 1, "").unwrap();
        assert_eq!(second.commits.len(), 1);
        assert!(!second.has_more);
        let filtered = history(root, 0, "fixture").unwrap();
        assert_eq!(filtered.commits.len(), 1);
        assert_eq!(filtered.commits[0].id, second.commits[0].id);
        assert_eq!(filtered.query, "fixture");
        let body = history(root, 0, "full COMMIT body [literal]").unwrap();
        assert_eq!(body.commits.len(), LOG_PAGE_SIZE);
        assert!(!body.has_more);
        assert!(history(root, 0, "--all").unwrap().commits.is_empty());
        assert!(
            history(root, 0, "no such message")
                .unwrap()
                .commits
                .is_empty()
        );
        let details = show_commit(root, &first.commits[0].id).unwrap();
        assert!(details.contains("Full commit body"));
        let initial = show_commit(root, &second.commits[0].id).unwrap();
        assert!(initial.contains("diff --git a/tracked b/tracked"));
        assert!(initial.contains("+initial"));
        assert!(show_commit(root, "--all").is_err());
        assert_eq!(index_stamp(root).unwrap(), index);
        assert_eq!(
            std::fs::read_to_string(root.join("tracked")).unwrap(),
            "unstaged\n"
        );
    }

    #[test]
    fn hunk_staging_unstaging_and_discard_preserve_other_changes() {
        let dir = repo();
        let root = dir.path();
        let original = (0..30).map(|i| format!("line {i}\n")).collect::<String>();
        write(root, "sample.txt", &original);
        commit(root);
        let modified = original
            .replace("line 2\n", "first change\n")
            .replace("line 25\n", "second change\n");
        write(root, "sample.txt", &modified);
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        assert_eq!(d.hunks.len(), 2);
        perform(
            root,
            Action::Hunk {
                diff: d,
                hunk: 0,
                discard: false,
            },
        )
        .unwrap();
        let staged = text(root, &["show", ":sample.txt"]).unwrap();
        assert!(staged.contains("first change"));
        assert!(!staged.contains("second change"));
        let d = diff(root, &file(root, Group::Index)).unwrap();
        perform(
            root,
            Action::Hunk {
                diff: d,
                hunk: 0,
                discard: false,
            },
        )
        .unwrap();
        assert!(
            snapshot(root)
                .unwrap()
                .files
                .iter()
                .all(|f| f.group != Group::Index)
        );
        // Stage the second hunk to exercise patches whose offsets are not at line one.
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        perform(
            root,
            Action::Hunk {
                diff: d,
                hunk: 1,
                discard: false,
            },
        )
        .unwrap();
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        perform(
            root,
            Action::Hunk {
                diff: d,
                hunk: 0,
                discard: true,
            },
        )
        .unwrap();
        let actual = std::fs::read_to_string(root.join("sample.txt")).unwrap();
        assert!(actual.contains("line 2"));
        assert!(actual.contains("second change"));
        assert!(
            !snapshot(root)
                .unwrap()
                .files
                .iter()
                .any(|f| f.group == Group::Worktree)
        );
    }
    #[test]
    fn discard_rejects_stale_content_and_preserves_index() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        write(root, "file", "staged\n");
        perform(root, Action::StageAll).unwrap();
        write(root, "file", "unstaged\n");
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        write(root, "file", "new agent edit\n");
        assert!(
            perform(root, Action::Discard(d))
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        perform(root, Action::Discard(d)).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file")).unwrap(),
            "staged\n"
        );
    }
    #[test]
    fn unborn_unstage_commit_and_index_review() {
        let dir = repo();
        let root = dir.path();
        write(root, "new", "initial\n");
        let before = index_stamp(root).unwrap();
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        assert!(d.reason.is_some());
        assert_eq!(before, index_stamp(root).unwrap());
        perform(root, Action::StageAll).unwrap();
        perform(root, Action::UnstageAll).unwrap();
        assert!(root.join("new").exists());
        assert_eq!(snapshot(root).unwrap().files[0].group, Group::Worktree);
        perform(root, Action::StageAll).unwrap();
        let index = index_stamp(root).unwrap();
        write(root, "other", "another\n");
        perform(root, Action::StageAll).unwrap();
        assert!(
            perform(
                root,
                Action::Commit {
                    message: "First\n\nBody".into(),
                    head: None,
                    index
                }
            )
            .is_err()
        );
        perform(
            root,
            Action::Commit {
                message: "First\n\nBody".into(),
                head: None,
                index: index_stamp(root).unwrap(),
            },
        )
        .unwrap();
        assert!(snapshot(root).unwrap().files.is_empty());
        assert!(
            text(root, &["log", "-1", "--format=%B"])
                .unwrap()
                .contains("Body")
        );
    }
    #[test]
    fn special_names_rename_delete_binary_and_no_newline() {
        let dir = repo();
        let root = dir.path();
        #[cfg(unix)]
        let name = "中文 \t\n--$(literal).txt";
        #[cfg(not(unix))]
        let name = "中文 --$(literal).txt";
        write(root, name, "no newline");
        write(root, "binary", "text\n");
        commit(root);
        write(root, name, "changed without newline");
        let snapshot = snapshot(root).unwrap();
        let f = snapshot
            .files
            .iter()
            .find(|f| f.path == Path::new(name))
            .unwrap();
        #[cfg(unix)]
        assert!(f.label().contains("\\n"));
        let d = diff(root, f).unwrap();
        perform(
            root,
            Action::Hunk {
                diff: d,
                hunk: 0,
                discard: false,
            },
        )
        .unwrap();
        std::fs::write(root.join("binary"), b"\0binary").unwrap();
        let f = snapshot_files(root)
            .into_iter()
            .find(|f| f.path == Path::new("binary"))
            .unwrap();
        assert!(diff(root, &f).unwrap().reason.is_some());
        git(root, &["commit", "-m", "changed name fixture"]).unwrap();
        git(root, &["mv", "--", name, "renamed"]).unwrap();
        let f = file(root, Group::Index);
        assert!(f.old_path.is_some());
        assert!(diff(root, &f).unwrap().reason.is_some());
        perform(root, Action::Unstage(f)).unwrap();
        assert!(root.join("renamed").exists());
        std::fs::remove_file(root.join("binary")).unwrap();
        let f = snapshot_files(root)
            .into_iter()
            .find(|f| f.path == Path::new("binary"))
            .unwrap();
        let d = diff(root, &f).unwrap();
        assert!(d.reason.is_some());
        perform(root, Action::Discard(d)).unwrap();
        assert!(root.join("binary").exists());
    }
    fn snapshot_files(root: &Path) -> Vec<File> {
        snapshot(root).unwrap().files
    }
    #[test]
    fn worktree_uses_own_index_and_branch() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("linked");
        let mut parts = args(&["worktree", "add", "-b", "side"]);
        parts.push(path.as_os_str().into());
        run(root, parts, None).unwrap();
        write(&path, "file", "side\n");
        perform(&path, Action::StageAll).unwrap();
        assert_eq!(
            discover(&path).unwrap(),
            Some(std::fs::canonicalize(&path).unwrap())
        );
        assert_eq!(snapshot(&path).unwrap().branch, "side");
        assert!(snapshot(root).unwrap().files.is_empty());
    }
    #[test]
    fn conflict_blocks_commit_and_hunks() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        perform(root, Action::Create("side".into())).unwrap();
        write(root, "file", "side\n");
        commit(root);
        perform(root, Action::Switch("main".into())).unwrap();
        write(root, "file", "main\n");
        commit(root);
        assert!(git(root, &["merge", "side"]).is_err());
        let s = snapshot(root).unwrap();
        assert_eq!(s.files[0].group, Group::Conflict);
        assert!(diff(root, &s.files[0]).unwrap().reason.is_some());
        assert!(
            perform(
                root,
                Action::Commit {
                    message: "bad".into(),
                    head: s.head,
                    index: index_stamp(root).unwrap()
                }
            )
            .is_err()
        );
        write(root, "file", "resolved\n");
        perform(root, Action::Stage(s.files[0].clone())).unwrap();
        assert!(
            !snapshot(root)
                .unwrap()
                .files
                .iter()
                .any(|f| f.group == Group::Conflict)
        );
    }
    #[test]
    fn local_remote_fetch_pull_push_and_divergence() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]).unwrap();
        git(
            root,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        )
        .unwrap();
        perform(
            root,
            Action::Push {
                remote: Some("origin".into()),
            },
        )
        .unwrap();
        assert_eq!(
            snapshot(root).unwrap().upstream.as_deref(),
            Some("origin/main")
        );
        let other = repo();
        git(
            other.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        )
        .unwrap();
        perform(other.path(), Action::Fetch("origin".into())).unwrap();
        perform(
            other.path(),
            Action::Track {
                local: "copy".into(),
                remote: "origin/main".into(),
            },
        )
        .unwrap();
        write(other.path(), "upstream", "remote\n");
        commit(other.path());
        git(other.path(), &["push", "origin", "HEAD:main"]).unwrap();
        perform(root, Action::Fetch("origin".into())).unwrap();
        assert_eq!(snapshot(root).unwrap().behind, 1);
        perform(root, Action::Pull).unwrap();
        assert!(root.join("upstream").exists());
        write(root, "local", "local\n");
        commit(root);
        write(other.path(), "another", "remote\n");
        commit(other.path());
        git(other.path(), &["push", "origin", "HEAD:main"]).unwrap();
        let head = snapshot(root).unwrap().head;
        assert!(perform(root, Action::Pull).is_err());
        assert_eq!(snapshot(root).unwrap().head, head);
    }
    #[test]
    fn commit_hook_failure_is_reported_and_does_not_commit() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = repo();
            let root = dir.path();
            write(root, "file", "base\n");
            let hook = root.join(".git/empty-hooks/pre-commit");
            std::fs::write(&hook, "#!/bin/sh\necho rejected-by-test-hook >&2\nexit 1\n").unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            perform(root, Action::StageAll).unwrap();
            let error = perform(
                root,
                Action::Commit {
                    message: "test".into(),
                    head: None,
                    index: index_stamp(root).unwrap(),
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("rejected-by-test-hook"));
            assert!(snapshot(root).unwrap().head.is_none());
        }
    }
    #[test]
    fn untracked_delete_uses_full_content_fingerprint_even_when_preview_is_truncated() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("large"), vec![b'x'; 2 * 1024 * 1024 + 100]).unwrap();
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        assert!(d.lines.last().unwrap().contains("truncated"));
        let mut stream = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("large"))
            .unwrap();
        stream.write_all(b"changed tail").unwrap();
        assert!(perform(root, Action::Discard(d)).is_err());
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        perform(root, Action::Discard(d)).unwrap();
        assert!(!root.join("large").exists());
    }
    #[test]
    fn index_changes_invalidate_selected_hunk_and_detached_head_is_visible() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        write(root, "file", "change\n");
        let d = diff(root, &file(root, Group::Worktree)).unwrap();
        write(root, "other", "other\n");
        git(root, &["add", "other"]).unwrap();
        assert!(
            perform(
                root,
                Action::Hunk {
                    diff: d,
                    hunk: 0,
                    discard: false
                }
            )
            .is_err()
        );
        git(root, &["switch", "--detach"]).unwrap();
        assert!(snapshot(root).unwrap().branch.starts_with("detached "));
    }
    #[cfg(unix)]
    #[test]
    fn symlink_and_non_utf8_paths_use_whole_file_and_literal_paths() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let dir = repo();
        let root = dir.path();
        // APFS requires valid Unicode filenames; Linux permits arbitrary bytes.
        let name = if cfg!(target_os = "macos") {
            PathBuf::from("中文-name")
        } else {
            PathBuf::from(OsString::from_vec(b"invalid-\xff-name".to_vec()))
        };
        std::fs::write(root.join(&name), b"base\n").unwrap();
        symlink("first-target", root.join("link")).unwrap();
        commit(root);
        std::fs::remove_file(root.join("link")).unwrap();
        symlink("second-target", root.join("link")).unwrap();
        let f = snapshot(root)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == Path::new("link"))
            .unwrap();
        let d = diff(root, &f).unwrap();
        assert!(d.reason.is_some());
        perform(root, Action::Discard(d)).unwrap();
        assert_eq!(
            std::fs::read_link(root.join("link")).unwrap(),
            Path::new("first-target")
        );
        std::fs::write(root.join(&name), b"modified\n").unwrap();
        let f = file(root, Group::Worktree);
        assert_eq!(f.path, name);
        perform(root, Action::Stage(f)).unwrap();
        assert_eq!(file(root, Group::Index).path, name);
    }
    #[cfg(unix)]
    #[test]
    fn porcelain_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let files = parse_files(b" M invalid-\xff-name\0").unwrap();
        assert_eq!(files[0].path.as_os_str().as_bytes(), b"invalid-\xff-name");
    }
    #[test]
    fn push_only_updates_current_upstream_even_with_matching_configuration() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]).unwrap();
        git(
            root,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        )
        .unwrap();
        perform(
            root,
            Action::Push {
                remote: Some("origin".into()),
            },
        )
        .unwrap();
        perform(root, Action::Create("extra".into())).unwrap();
        git(root, &["push", "origin", "extra"]).unwrap();
        let extra = text(remote.path(), &["rev-parse", "refs/heads/extra"]).unwrap();
        write(root, "extra", "extra\n");
        commit(root);
        perform(root, Action::Switch("main".into())).unwrap();
        git(root, &["config", "push.default", "matching"]).unwrap();
        perform(root, Action::Push { remote: None }).unwrap();
        assert_eq!(
            text(remote.path(), &["rev-parse", "refs/heads/extra"]).unwrap(),
            extra
        );
    }
}
