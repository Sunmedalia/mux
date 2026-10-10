//! Side-pane Git UI. The worker owns all subprocesses; the UI only sends requests.
use super::*;
use crate::git::{self as service, Action, Diff, Group, Snapshot};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
enum MenuKind {
    Operations,
    Branches,
    Fetch,
    Push,
}
enum FormKind {
    Commit {
        head: Option<String>,
        index: Vec<u8>,
    },
    Create,
    Track(String),
    Search,
}
enum Modal {
    Menu {
        kind: MenuKind,
        items: Vec<String>,
        selected: usize,
        filter: String,
    },
    Form {
        kind: FormKind,
        text: String,
    },
    Confirm {
        action: Action,
        description: String,
    },
    Message(String),
}
enum Request {
    Refresh,
    Diff(PathBuf, service::File, bool),
    Menu(PathBuf, MenuKind),
    Commit(PathBuf),
    Execute(PathBuf, Action),
    History(PathBuf, usize, String),
    Show(PathBuf, String),
}
enum Response {
    Snapshot(std::result::Result<Option<Snapshot>, String>),
    Diff(std::result::Result<Diff, String>, bool),
    Menu(MenuKind, std::result::Result<Vec<String>, String>),
    Commit(std::result::Result<(Snapshot, Vec<u8>), String>),
    Done(std::result::Result<String, String>),
    History(std::result::Result<service::History, String>),
    Show(std::result::Result<String, String>),
}
#[derive(Default)]
pub(super) struct GitPane {
    snapshot: Option<Snapshot>,
    diff: Option<Diff>,
    history: Option<service::History>,
    history_selected: usize,
    selected: usize,
    hunk: usize,
    scroll: u16,
    horizontal: u16,
    nowrap: bool,
    diff_rows: Vec<usize>,
    hunk_rows: Vec<u16>,
    diff_anchor: Option<usize>,
    modal: Option<Modal>,
    busy: bool,
    notice: String,
    error: Option<String>,
    refreshed: Option<Instant>,
    requests: Option<mpsc::SyncSender<Request>>,
    updates: Option<mpsc::Receiver<Response>>,
    pinned: Arc<AtomicBool>,
    body: Rect,
    screen: Rect,
    branch_button: Option<Rect>,
    rows: Vec<(u16, usize)>,
    menu_rows: Vec<(u16, usize)>,
    limit: u16,
}

// Unlike agent usage tracking, Git follows regular shell panes too. Only panes
// in the originating tab are candidates; focusing Pulse retains the last cwd.
fn project_cwd(
    panes: &serde_json::Value,
    tab: &str,
    source: &str,
    own: &str,
    initial: bool,
) -> Option<PathBuf> {
    let panes = panes["result"]["panes"].as_array()?;
    let eligible = |pane: &&serde_json::Value| {
        pane["tab_id"].as_str() == Some(tab)
            && pane["pane_id"].as_str() != Some(own)
            && pane["label"] != LABEL
    };
    let pane = panes
        .iter()
        .filter(eligible)
        .find(|p| p["focused"] == true)
        .or_else(|| {
            initial
                .then(|| {
                    panes
                        .iter()
                        .filter(eligible)
                        .find(|p| p["pane_id"].as_str() == Some(source))
                })
                .flatten()
        })?;
    pane["foreground_cwd"]
        .as_str()
        .filter(|p| !p.is_empty())
        .or_else(|| pane["cwd"].as_str().filter(|p| !p.is_empty()))
        .map(PathBuf::from)
}
fn watch_focus(dirty: Arc<AtomicBool>, stopped: Arc<AtomicBool>) {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return;
    }
    let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") else {
        return;
    };
    let Ok(mut socket) = focus_event_socket(std::path::Path::new(&path)) else {
        return;
    };
    let request = serde_json::json!({"id": "mux_git_focus", "method": "events.subscribe", "params": {"subscriptions": [
        {"type": "pane.focused"}, {"type": "pane.updated"}, {"type": "pane.closed"}, {"type": "pane.moved"}
    ]}});
    if writeln!(socket, "{request}").is_err() {
        return;
    }
    let mut reader = BufReader::new(socket);
    if read_focus_subscription_response(&mut reader).is_err() {
        return;
    }
    while !stopped.load(Ordering::Relaxed) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                dirty.store(true, Ordering::Relaxed);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => break,
        }
    }
}
impl GitPane {
    pub(super) fn can_switch_workspace(&self) -> bool {
        !self.busy && self.modal.is_none()
    }
    pub(super) fn start(&mut self) {
        let (send, requests) = mpsc::sync_channel(8);
        let (updates, receive) = mpsc::channel();
        self.requests = Some(send);
        self.updates = Some(receive);
        let pinned = self.pinned.clone();
        let initial = std::env::current_dir().unwrap_or_default();
        let workspace = std::env::var("MUX_MONITOR_WORKSPACE")
            .or_else(|_| std::env::var("HERDR_WORKSPACE_ID"))
            .ok();
        let tab = std::env::var("MUX_MONITOR_TAB")
            .or_else(|_| std::env::var("HERDR_TAB_ID"))
            .ok();
        let source = std::env::var("MUX_MONITOR_SOURCE_PANE").unwrap_or_default();
        let needs_source = !source.is_empty() && std::env::var("HERDR_ENV").as_deref() == Ok("1");
        let own = std::env::var("HERDR_PANE_ID").unwrap_or_default();
        let focus_dirty = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let focus_stopped = stopped.clone();
        let events = focus_dirty.clone();
        std::thread::spawn(move || {
            watch_focus(events, focus_stopped);
        });
        std::thread::spawn(move || {
            let mut cwd = initial;
            let mut pending_cwd = None;
            let mut first = true;
            let mut context_known = !needs_source;
            let mut checked = Instant::now() - Duration::from_secs(3);
            loop {
                if checked.elapsed() >= Duration::from_secs(2)
                    || (checked.elapsed() >= Duration::from_millis(150)
                        && focus_dirty.load(Ordering::Relaxed))
                {
                    focus_dirty.store(false, Ordering::Relaxed);
                    if std::env::var("HERDR_ENV").as_deref() == Ok("1")
                        && let (Some(workspace), Some(tab)) = (&workspace, &tab)
                        && let Ok(panes) = herdr(&["pane", "list", "--workspace", workspace])
                        && let Some(path) = project_cwd(&panes, tab, &source, &own, first)
                    {
                        pending_cwd = Some(path);
                        first = false;
                    }
                    if !pinned.load(Ordering::Relaxed)
                        && let Some(path) = pending_cwd.take()
                    {
                        cwd = path;
                        context_known = true;
                    }
                    let result = if context_known {
                        service::discover(&cwd)
                            .and_then(|root| root.map(|root| service::snapshot(&root)).transpose())
                            .map_err(|e| e.to_string())
                    } else {
                        Ok(None)
                    };
                    if updates.send(Response::Snapshot(result)).is_err() {
                        break;
                    }
                    checked = Instant::now();
                }
                let request = match requests.recv_timeout(Duration::from_millis(250)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let response = match request {
                    Request::Refresh => {
                        checked = Instant::now() - Duration::from_secs(3);
                        continue;
                    }
                    Request::Diff(root, file, discard) => Response::Diff(
                        service::diff(&root, &file).map_err(|e| e.to_string()),
                        discard,
                    ),
                    Request::Menu(root, kind) => {
                        let items = match kind {
                            MenuKind::Branches => service::branches(&root),
                            _ => service::remotes(&root),
                        };
                        Response::Menu(kind, items.map_err(|e| e.to_string()))
                    }
                    Request::Commit(root) => Response::Commit(
                        service::index_stamp(&root)
                            .and_then(|index| {
                                let s = service::snapshot(&root)?;
                                anyhow::ensure!(
                                    !s.files.iter().any(|f| f.group == Group::Conflict),
                                    "Resolve conflicts before committing"
                                );
                                anyhow::ensure!(
                                    s.files.iter().any(|f| f.group == Group::Index),
                                    "No staged changes"
                                );
                                anyhow::ensure!(
                                    index == service::index_stamp(&root)?,
                                    "Index changed; review and reopen Commit"
                                );
                                Ok((s, index))
                            })
                            .map_err(|e| e.to_string()),
                    ),
                    Request::Execute(root, action) => {
                        let result = service::perform(&root, action).map_err(|e| e.to_string());
                        checked = Instant::now() - Duration::from_secs(3);
                        Response::Done(result)
                    }
                    Request::History(root, page, query) => Response::History(
                        service::history(&root, page, &query).map_err(|e| e.to_string()),
                    ),
                    Request::Show(root, id) => {
                        Response::Show(service::show_commit(&root, &id).map_err(|e| e.to_string()))
                    }
                };
                if updates.send(response).is_err() {
                    break;
                }
            }
            stopped.store(true, Ordering::Relaxed);
        });
    }
    fn pin(&self) {
        self.pinned
            .store(self.busy || self.modal.is_some(), Ordering::Relaxed);
    }
    fn request(&mut self, request: Request) {
        if self.busy {
            return;
        }
        if self
            .requests
            .as_ref()
            .is_some_and(|s| s.try_send(request).is_ok())
        {
            self.busy = true;
            self.notice = "Working…".into();
            self.error = None;
            self.pin();
        } else {
            self.error = Some("Git worker unavailable; reopen Pulse".into());
        }
    }
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(response) = self.updates.as_ref().and_then(|u| u.try_recv().ok()) {
            changed = true;
            match response {
                Response::Snapshot(Ok(snapshot)) => {
                    if !self.busy && self.modal.is_none() {
                        let root_changed = self.snapshot.as_ref().map(|s| &s.root)
                            != snapshot.as_ref().map(|s| &s.root);
                        if root_changed {
                            self.diff = None;
                            self.history = None;
                            self.selected = 0;
                            self.scroll = 0;
                            self.notice.clear();
                        }
                        let selected = self
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.files.get(self.selected))
                            .map(|f| (f.path.clone(), f.group));
                        if let (Some((path, group)), Some(snapshot)) = (selected, &snapshot) {
                            self.selected = snapshot
                                .files
                                .iter()
                                .position(|f| f.path == path && f.group == group)
                                .unwrap_or(self.selected);
                        }
                        self.snapshot = snapshot;
                        self.selected = self.selected.min(
                            self.snapshot
                                .as_ref()
                                .map_or(0, |s| s.files.len().saturating_sub(1)),
                        );
                        self.error = None;
                        self.refreshed = Some(Instant::now());
                    }
                }
                Response::Snapshot(Err(error)) => self.error = Some(format!("STALE · {error}")),
                Response::Diff(result, discard) => {
                    self.busy = false;
                    match result {
                        Ok(diff) if discard => self.confirm_discard(diff, false),
                        Ok(diff) => {
                            if let Some(snapshot) = &self.snapshot
                                && let Some(index) = snapshot.files.iter().position(|f| {
                                    f.path == diff.file.path && f.group == diff.file.group
                                })
                            {
                                self.selected = index;
                            }
                            self.diff = Some(diff);
                            self.diff_anchor = None;
                            self.hunk = 0;
                            self.scroll = 0;
                            self.horizontal = 0;
                            self.notice.clear();
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                Response::Menu(kind, result) => {
                    self.busy = false;
                    match result {
                        Ok(items) if items.is_empty() => {
                            self.report_error("No branches/remotes available".into())
                        }
                        Ok(items) => {
                            self.modal = Some(Modal::Menu {
                                kind,
                                items,
                                selected: 0,
                                filter: String::new(),
                            })
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                Response::Commit(result) => {
                    self.busy = false;
                    match result {
                        Ok((snapshot, index)) => {
                            let head = snapshot.head.clone();
                            self.snapshot = Some(snapshot);
                            self.modal = Some(Modal::Form {
                                kind: FormKind::Commit { head, index },
                                text: String::new(),
                            })
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                Response::Done(result) => {
                    self.busy = false;
                    self.diff = None;
                    self.scroll = 0;
                    match result {
                        Ok(message) => {
                            self.notice = message;
                            self.error = None;
                        }
                        Err(error) => {
                            self.error = Some(error.clone());
                            self.modal = Some(Modal::Message(error));
                        }
                    }
                }
                Response::History(result) => {
                    self.busy = false;
                    match result {
                        Ok(history) => {
                            self.history = Some(history);
                            self.diff = None;
                            self.history_selected = 0;
                            self.scroll = 0;
                            self.notice.clear();
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                Response::Show(result) => {
                    self.busy = false;
                    self.scroll = 0;
                    self.notice.clear();
                    match result {
                        Ok(message) => self.modal = Some(Modal::Message(message)),
                        Err(error) => self.report_error(error),
                    }
                }
            }
            self.pin();
        }
        changed
    }
    fn report_error(&mut self, error: String) {
        self.error = Some(error.clone());
        self.modal = Some(Modal::Message(error));
        self.scroll = 0;
    }
    fn root(&self) -> Option<PathBuf> {
        self.snapshot.as_ref().map(|s| s.root.clone())
    }
    fn file(&self) -> Option<service::File> {
        if self.history.is_some() {
            return None;
        }
        if let Some(diff) = &self.diff {
            return Some(diff.file.clone());
        }
        self.snapshot.as_ref()?.files.get(self.selected).cloned()
    }
    fn diff_position(&self) -> Option<(usize, usize)> {
        let snapshot = self.snapshot.as_ref()?;
        let file = &self.diff.as_ref()?.file;
        let index = snapshot
            .files
            .iter()
            .position(|f| f.path == file.path && f.group == file.group)?;
        Some((index, snapshot.files.len()))
    }
    fn switch_diff_file(&mut self, next: bool) {
        let Some((index, count)) = self.diff_position() else {
            return;
        };
        let target = if next {
            (index + 1).min(count.saturating_sub(1))
        } else {
            index.saturating_sub(1)
        };
        if target == index {
            return;
        }
        if let Some(snapshot) = &self.snapshot {
            let root = snapshot.root.clone();
            let file = snapshot.files[target].clone();
            self.request(Request::Diff(root, file, false));
        }
    }
    fn execute(&mut self, action: Action) {
        if let Some(root) = self.root() {
            self.modal = None;
            self.request(Request::Execute(root, action));
        }
    }
    fn confirm(&mut self, action: Action, description: String) {
        let description = self
            .snapshot
            .as_ref()
            .map_or(description.clone(), |snapshot| {
                format!(
                    "{}\nBranch: {}\n\n{description}",
                    service::display_path(&snapshot.root),
                    snapshot.branch
                )
            });
        self.modal = Some(Modal::Confirm {
            action,
            description,
        });
        self.scroll = 0;
        self.pin();
    }
    fn confirm_discard(&mut self, diff: Diff, hunk: bool) {
        if diff.file.group != Group::Worktree {
            self.error = Some("Only unstaged changes can be discarded".into());
            return;
        }
        let description = format!(
            "{} {}?\n{}\nStaged changes are preserved.",
            if diff.file.untracked() {
                "Delete untracked file"
            } else {
                "Discard unstaged"
            },
            if hunk {
                "difference block"
            } else {
                "file changes"
            },
            diff.file.label()
        );
        let action = if hunk {
            Action::Hunk {
                diff,
                hunk: self.hunk,
                discard: true,
            }
        } else {
            Action::Discard(diff)
        };
        self.confirm(action, description);
    }
    fn operations(&mut self) {
        if self.snapshot.is_none() {
            return;
        }
        self.scroll = 0;
        self.modal = Some(Modal::Menu {
            kind: MenuKind::Operations,
            items: [
                "Stage file",
                "Unstage file",
                "Discard file",
                "Stage all",
                "Unstage all",
                "Commit",
                "Branches",
                "Create branch",
                "Fetch",
                "Pull (fast-forward)",
                "Push",
                "Git log",
                "Stage selected block",
            ]
            .iter()
            .map(|s| (*s).into())
            .collect(),
            selected: 0,
            filter: String::new(),
        });
        self.pin();
    }
    fn operation(&mut self, index: usize) {
        match index {
            0 => {
                if let Some(file) = self.file() {
                    self.execute(Action::Stage(file));
                }
            }
            1 => {
                if let Some(file) = self.file() {
                    self.execute(Action::Unstage(file));
                }
            }
            2 => {
                if let (Some(root), Some(file)) = (self.root(), self.file()) {
                    self.request(Request::Diff(root, file, true));
                }
            }
            3 => self.execute(Action::StageAll),
            4 => self.execute(Action::UnstageAll),
            5 => {
                if let Some(root) = self.root() {
                    self.request(Request::Commit(root));
                }
            }
            6 => {
                if let Some(root) = self.root() {
                    self.request(Request::Menu(root, MenuKind::Branches));
                }
            }
            7 => {
                self.modal = Some(Modal::Form {
                    kind: FormKind::Create,
                    text: String::new(),
                });
            }
            8 => {
                if let Some(root) = self.root() {
                    self.request(Request::Menu(root, MenuKind::Fetch));
                }
            }
            9 => self.confirm(
                Action::Pull,
                "Pull current upstream using fast-forward only?".into(),
            ),
            10 => {
                if self.snapshot.as_ref().is_some_and(|s| s.upstream.is_some()) {
                    self.confirm(
                        Action::Push { remote: None },
                        "Push current branch to its upstream?".into(),
                    );
                } else if let Some(root) = self.root() {
                    self.request(Request::Menu(root, MenuKind::Push));
                }
            }
            11 => self.open_history(0),
            12 => {
                if let Some(diff) = self.diff.clone()
                    && diff.file.group == Group::Worktree
                    && diff.reason.is_none()
                    && !diff.hunks.is_empty()
                {
                    self.execute(Action::Hunk {
                        diff,
                        hunk: self.hunk,
                        discard: false,
                    });
                } else {
                    self.error = Some("Open an unstaged text Diff and select a block".into());
                }
            }
            _ => {}
        }
        self.pin();
    }
    fn menu_select(&mut self, kind: MenuKind, item: String, index: usize) {
        self.modal = None;
        self.scroll = 0;
        match kind {
            MenuKind::Operations => self.operation(index),
            MenuKind::Fetch => self.execute(Action::Fetch(item)),
            MenuKind::Push => self.confirm(
                Action::Push {
                    remote: Some(item.clone()),
                },
                format!("Push current branch to {item} and set upstream?"),
            ),
            MenuKind::Branches => {
                if self.snapshot.as_ref().is_some_and(|s| s.branch == item) {
                    return;
                }
                // Exact local ref lookup, rather than interpreting slashes in a branch name.
                let local = self.local_branches_contains(&item);
                if local {
                    self.confirm(
                        Action::Switch(item.clone()),
                        format!("Switch to {item}? Local changes follow Git's normal checks."),
                    );
                } else {
                    let remote = item.strip_prefix("remote: ").unwrap_or(&item);
                    let name = remote
                        .split_once('/')
                        .map_or(remote, |(_, name)| name)
                        .to_string();
                    self.modal = Some(Modal::Form {
                        kind: FormKind::Track(item),
                        text: name,
                    });
                }
            }
        }
        self.pin();
    }
    fn local_branches_contains(&self, item: &str) -> bool {
        // Branch lists prefix remote entries explicitly; local names remain untouched.
        !item.starts_with("remote: ")
    }
    fn open_history(&mut self, page: usize) {
        if let Some(root) = self.root() {
            let query = self
                .history
                .as_ref()
                .map_or(String::new(), |h| h.query.clone());
            self.request(Request::History(root, page, query));
        }
    }
    fn search_history(&mut self) {
        self.modal = Some(Modal::Form {
            kind: FormKind::Search,
            text: self
                .history
                .as_ref()
                .map_or(String::new(), |h| h.query.clone()),
        });
        self.scroll = 0;
        self.pin();
    }
    fn history_key(&mut self, key: KeyEvent) -> bool {
        let history = self.history.as_ref().unwrap();
        let last = history.commits.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('T' | 'q') => return false,
            KeyCode::Char('g') => {}
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Esc | KeyCode::Char('L') => {
                self.history = None;
                self.scroll = 0;
            }
            KeyCode::Char('r') => self.open_history(history.page),
            KeyCode::Char('/') => self.search_history(),
            KeyCode::Char(',') if history.page > 0 => self.open_history(history.page - 1),
            KeyCode::Char('.') if history.has_more => self.open_history(history.page + 1),
            KeyCode::Enter => {
                if let (Some(root), Some(commit)) =
                    (self.root(), history.commits.get(self.history_selected))
                {
                    self.request(Request::Show(root, commit.id.clone()));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.history_selected = self.history_selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.history_selected = (self.history_selected + 1).min(last)
            }
            KeyCode::Home => {
                self.history_selected = 0;
                self.scroll = 0;
            }
            KeyCode::End => {
                self.history_selected = last;
                self.scroll = self.limit;
            }
            KeyCode::PageDown => {
                self.scroll_to(self.scroll.saturating_add(self.body.height.max(1)))
            }
            KeyCode::PageUp => self.scroll_to(self.scroll.saturating_sub(self.body.height.max(1))),
            KeyCode::Char('o') => self.operations(),
            KeyCode::Char('b') => self.operation(6),
            KeyCode::Char('?') => {
                self.scroll = 0;
                self.modal = Some(Modal::Message("GIT LOG\nCurrent branch history, newest first.\n↑↓ / j/k: select commit · Enter: full message, stats and Diff\n, / .: newer / older page (50 commits per page)\n/: search commit title and body · Enter applies\nPgUp / PgDn: scroll · r: refresh\nEsc / Files: working tree · g: Git · T: Token · Alt+1/2: Token/Git\nCommit details wrap automatically; Esc returns to the selected commit.".into()));
            }
            _ => {}
        }
        self.pin();
        true
    }
    fn scroll_to(&mut self, target: u16) {
        self.scroll = target.min(self.limit);
        let visible_end = self.scroll.saturating_add(self.body.height.max(1));
        if self.modal.is_none() && self.diff.is_none() {
            let selected = if self.history.is_some() {
                self.history_selected
            } else {
                self.selected
            };
            if let Some((row, _)) = self.rows.iter().find(|(_, index)| *index == selected)
                && (*row < self.scroll || *row >= visible_end)
                && let Some((_, index)) = self
                    .rows
                    .iter()
                    .find(|(row, _)| *row >= self.scroll)
                    .or_else(|| self.rows.last())
            {
                if self.history.is_some() {
                    self.history_selected = *index;
                } else {
                    self.selected = *index;
                }
            }
        } else if let Some(Modal::Menu { selected, .. }) = &mut self.modal
            && let Some((row, _)) = self.menu_rows.iter().find(|(_, index)| index == selected)
            && (*row < self.scroll || *row >= visible_end)
            && let Some((_, index)) = self
                .menu_rows
                .iter()
                .find(|(row, _)| *row >= self.scroll)
                .or_else(|| self.menu_rows.last())
        {
            *selected = *index;
        }
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if self.busy {
            return true;
        }
        if let Some(modal) = self.modal.take() {
            if !matches!(modal, Modal::Menu { .. }) {
                match key.code {
                    KeyCode::PageDown => {
                        self.scroll = self
                            .scroll
                            .saturating_add(self.body.height.max(1))
                            .min(self.limit);
                        self.modal = Some(modal);
                        return true;
                    }
                    KeyCode::PageUp => {
                        self.scroll_to(self.scroll.saturating_sub(self.body.height.max(1)));
                        self.modal = Some(modal);
                        return true;
                    }
                    _ => {}
                }
            }
            match modal {
                Modal::Message(message) => match key.code {
                    KeyCode::Esc | KeyCode::Enter => {}
                    KeyCode::Down => {
                        self.scroll = (self.scroll + 1).min(self.limit);
                        self.modal = Some(Modal::Message(message));
                    }
                    KeyCode::Up => {
                        self.scroll = self.scroll.saturating_sub(1);
                        self.modal = Some(Modal::Message(message));
                    }
                    _ => self.modal = Some(Modal::Message(message)),
                },
                Modal::Confirm {
                    action,
                    description,
                } => match key.code {
                    KeyCode::Enter | KeyCode::Char('y') => self.execute(action),
                    KeyCode::Esc | KeyCode::Char('n') => {
                        if let Action::Commit {
                            message,
                            head,
                            index,
                        } = action
                        {
                            self.modal = Some(Modal::Form {
                                kind: FormKind::Commit { head, index },
                                text: message,
                            });
                        }
                        self.scroll = 0;
                    }
                    _ => {
                        self.modal = Some(Modal::Confirm {
                            action,
                            description,
                        })
                    }
                },
                Modal::Form { kind, mut text } => {
                    let submit = key.code == KeyCode::Enter
                        && (!matches!(kind, FormKind::Commit { .. })
                            || key.modifiers.contains(KeyModifiers::CONTROL));
                    if key.code == KeyCode::Esc {
                    } else if submit && matches!(kind, FormKind::Search) {
                        if let Some(root) = self.root() {
                            self.request(Request::History(root, 0, text.trim().into()));
                        }
                    } else if submit {
                        if text.lines().next().unwrap_or("").trim().is_empty() {
                            self.error = Some("A title/name is required".into());
                            self.modal = Some(Modal::Form { kind, text });
                        } else {
                            let action = match kind {
                                FormKind::Commit { head, index } => Action::Commit {
                                    message: text.clone(),
                                    head,
                                    index,
                                },
                                FormKind::Create => Action::Create(text.trim().into()),
                                FormKind::Track(remote) => Action::Track {
                                    local: text.trim().into(),
                                    remote: remote
                                        .strip_prefix("remote: ")
                                        .unwrap_or(&remote)
                                        .into(),
                                },
                                FormKind::Search => unreachable!(),
                            };
                            self.confirm(
                                action,
                                format!(
                                    "Confirm {}?\n{}",
                                    if text.contains('\n') {
                                        "commit"
                                    } else {
                                        "operation"
                                    },
                                    text
                                ),
                            );
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('u')
                                if matches!(kind, FormKind::Search)
                                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                text.clear()
                            }
                            KeyCode::Backspace => {
                                text.pop();
                            }
                            KeyCode::Enter if matches!(kind, FormKind::Commit { .. }) => {
                                text.push('\n')
                            }
                            KeyCode::Char(c)
                                if !key
                                    .modifiers
                                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                text.push(c)
                            }
                            _ => {}
                        }
                        self.modal = Some(Modal::Form { kind, text });
                        self.scroll = u16::MAX;
                    }
                }
                Modal::Menu {
                    kind,
                    items,
                    mut selected,
                    mut filter,
                } => {
                    let visible: Vec<_> = items
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.to_lowercase().contains(&filter.to_lowercase()))
                        .collect();
                    match key.code {
                        KeyCode::Esc => {}
                        KeyCode::Enter => {
                            if let Some((i, item)) = visible.get(selected) {
                                self.menu_select(kind, (*item).clone(), *i);
                            } else {
                                self.modal = Some(Modal::Menu {
                                    kind,
                                    items,
                                    selected,
                                    filter,
                                });
                            }
                        }
                        _ => {
                            match key.code {
                                KeyCode::Down => {
                                    selected = (selected + 1).min(visible.len().saturating_sub(1))
                                }
                                KeyCode::Up => selected = selected.saturating_sub(1),
                                KeyCode::Home => selected = 0,
                                KeyCode::End => selected = visible.len().saturating_sub(1),
                                KeyCode::Backspace => {
                                    filter.pop();
                                    selected = 0;
                                }
                                KeyCode::Char(c)
                                    if !key
                                        .modifiers
                                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                                {
                                    filter.push(c);
                                    selected = 0;
                                }
                                _ => {}
                            }
                            self.modal = Some(Modal::Menu {
                                kind,
                                items,
                                selected,
                                filter,
                            });
                        }
                    }
                }
            }
            self.pin();
            return true;
        }
        if self.history.is_some() {
            return self.history_key(key);
        }
        match key.code {
            KeyCode::Char('T') => return false,
            KeyCode::Char('g') => {}
            KeyCode::Char('q') => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char(',') if self.diff.is_some() => self.switch_diff_file(false),
            KeyCode::Char('.') if self.diff.is_some() => self.switch_diff_file(true),
            KeyCode::Char('w') if self.diff.is_some() => {
                self.diff_anchor = self.diff_rows.get(usize::from(self.scroll)).copied();
                self.nowrap = !self.nowrap;
                self.horizontal = 0;
            }
            KeyCode::Char('o') => self.operations(),
            KeyCode::Char('?') => {
                self.scroll = 0;
                self.modal = Some(Modal::Message("GIT HELP\nEnter: open selected file Diff\nEsc / Files: return to file list\n, / .: previous / next file in Diff\nw: toggle line wrapping (on by default)\na / A: stage all changes\ns: stage selected file (whole file)\nu: unstage file or selected hunk\no: stage one file / selected block\nd: discard file or selected hunk\nD: discard whole unstaged file\nA / U: stage / unstage all\nn: commit staged files\nb: search branches\nL: Git log (current branch)\no: all operations and remotes\n[ / ]: previous / next hunk\n← / →: pan Diff when wrapping is off\nr: refresh\nl: operation output / error details\nEsc: back · g: Git · T: Token · Alt+1/2: Token/Git · q: close Pulse\nCommit: Enter adds a line; Ctrl+Enter or Review opens confirmation.\nPgUp / PgDn: scroll forms and confirmation.\nBinary, new, deleted, renamed and mode changes use whole-file operations.\nResolve conflicts externally, then stage. Pull only fast-forwards.".into()));
            }
            KeyCode::Char('l') => {
                self.scroll = 0;
                self.modal = Some(Modal::Message(
                    self.error.clone().unwrap_or_else(|| self.notice.clone()),
                ));
            }
            KeyCode::Char('r') => {
                if let Some(diff) = &self.diff {
                    if let Some(root) = self.root() {
                        self.request(Request::Diff(root, diff.file.clone(), false));
                    }
                } else if let Some(send) = &self.requests {
                    let _ = send.try_send(Request::Refresh);
                }
            }
            KeyCode::Char('n') => self.operation(5),
            KeyCode::Char('b') => self.operation(6),
            KeyCode::Char('A') => self.execute(Action::StageAll),
            KeyCode::Char('a') => self.execute(Action::StageAll),
            KeyCode::Char('s') => {
                if self.file().is_some_and(|f| f.group != Group::Index) {
                    self.operation(0);
                } else {
                    self.error = Some("Select an unstaged file".into());
                }
            }
            KeyCode::Char('L') => self.open_history(0),
            KeyCode::Char('U') => self.execute(Action::UnstageAll),
            KeyCode::Char('D') => {
                if let Some(diff) = self.diff.clone() {
                    self.confirm_discard(diff, false);
                } else {
                    self.operation(2);
                }
            }
            KeyCode::Char('u' | 'd') => {
                if let Some(diff) = self.diff.clone() {
                    if key.code == KeyCode::Char('d') {
                        if diff.reason.is_none() && !diff.hunks.is_empty() {
                            self.confirm_discard(diff, true);
                        } else {
                            self.confirm_discard(diff, false);
                        }
                    } else {
                        let index = key.code == KeyCode::Char('u');
                        if (diff.file.group == Group::Index) != index
                            && diff.file.group != Group::Conflict
                        {
                            self.error = Some(
                                if index {
                                    "Select a staged file"
                                } else {
                                    "Select an unstaged file"
                                }
                                .into(),
                            );
                        } else if diff.reason.is_none() && !diff.hunks.is_empty() {
                            self.execute(Action::Hunk {
                                diff,
                                hunk: self.hunk,
                                discard: false,
                            });
                        } else {
                            self.execute(if index {
                                Action::Unstage(diff.file)
                            } else {
                                Action::Stage(diff.file)
                            });
                        }
                    }
                } else {
                    self.operation(match key.code {
                        KeyCode::Char('a') => 0,
                        KeyCode::Char('u') => 1,
                        _ => 2,
                    });
                }
            }
            KeyCode::Enter if self.diff.is_none() => {
                if let (Some(root), Some(file)) = (self.root(), self.file()) {
                    self.request(Request::Diff(root, file, false));
                }
            }
            KeyCode::Esc => {
                if self.diff.take().is_some() {
                    self.scroll = 0;
                    self.horizontal = 0;
                } else {
                    return false;
                }
            }
            KeyCode::Char('[' | ']') if self.diff.is_some() => {
                let diff = self.diff.as_ref().unwrap();
                if key.code == KeyCode::Char(']') {
                    self.hunk = (self.hunk + 1).min(diff.hunks.len().saturating_sub(1));
                } else {
                    self.hunk = self.hunk.saturating_sub(1);
                }
                if let Some(hunk) = diff.hunks.get(self.hunk) {
                    self.scroll = self
                        .hunk_rows
                        .get(self.hunk)
                        .copied()
                        .unwrap_or(hunk.start as u16)
                        .min(self.limit);
                }
            }
            KeyCode::Left if self.nowrap => self.horizontal = self.horizontal.saturating_sub(8),
            KeyCode::Right if self.diff.is_some() && self.nowrap => {
                self.horizontal = self.horizontal.saturating_add(8)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.diff.is_some() {
                    self.scroll = self.scroll.saturating_sub(1);
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.diff.is_some() {
                    self.scroll = (self.scroll + 1).min(self.limit);
                } else {
                    self.selected = (self.selected + 1).min(
                        self.snapshot
                            .as_ref()
                            .map_or(0, |s| s.files.len().saturating_sub(1)),
                    );
                }
            }
            KeyCode::PageDown => {
                self.scroll_to(self.scroll.saturating_add(self.body.height.max(1)))
            }
            KeyCode::PageUp => self.scroll_to(self.scroll.saturating_sub(self.body.height.max(1))),
            KeyCode::Home => {
                self.scroll = 0;
                if self.diff.is_none() {
                    self.selected = 0;
                }
            }
            KeyCode::End => {
                self.scroll = self.limit;
                if self.diff.is_none() {
                    self.selected = self
                        .snapshot
                        .as_ref()
                        .map_or(0, |s| s.files.len().saturating_sub(1));
                }
            }
            _ => {}
        }
        self.pin();
        true
    }
    pub(super) fn input(&mut self, event: &Event) -> bool {
        match event {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => self.key(*key),
            Event::Paste(text) => {
                if let Some(Modal::Form { text: field, .. }) = &mut self.modal {
                    field.push_str(text);
                    self.scroll = u16::MAX;
                }
                true
            }
            Event::Mouse(mouse) => {
                if self.busy {
                    return true;
                }
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                ) {
                    if self.modal.is_some() {
                        return self.key(KeyEvent::new(
                            if mouse.kind == MouseEventKind::ScrollDown {
                                KeyCode::Down
                            } else {
                                KeyCode::Up
                            },
                            KeyModifiers::NONE,
                        ));
                    }
                    let target = if mouse.kind == MouseEventKind::ScrollDown {
                        self.scroll.saturating_add(3).min(self.limit)
                    } else {
                        self.scroll.saturating_sub(3)
                    };
                    self.scroll_to(target);
                    return true;
                }
                if matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Drag(MouseButton::Left)
                ) && let Some(target) = scrollbar_target(
                    self.body,
                    self.body.right(),
                    mouse.column,
                    mouse.row,
                    self.limit,
                ) {
                    self.scroll_to(target);
                    return true;
                }
                if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                    return true;
                }
                if mouse.row == 0 {
                    let (area, reserve) = pulse_header_area(self.screen);
                    let tabs = page_tab_rects(area, reserve);
                    if self.modal.is_none() && contains(tabs[0], mouse.column, mouse.row) {
                        return false;
                    }
                    return true;
                }
                if self.modal.is_none()
                    && self
                        .branch_button
                        .is_some_and(|rect| contains(rect, mouse.column, mouse.row))
                {
                    return self.key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
                }
                for navigation in [false, true] {
                    if navigation && self.screen.height < 7 {
                        continue;
                    }
                    let row = self
                        .screen
                        .bottom()
                        .saturating_sub(if navigation { 2 } else { 1 });
                    if mouse.row != row {
                        continue;
                    }
                    let controls = self.controls(navigation);
                    if let Some(index) = control_rects(self.screen, row, controls.len())
                        .iter()
                        .position(|rect| contains(*rect, mouse.column, mouse.row))
                        && let Some(control) = controls.get(index).filter(|control| control.enabled)
                    {
                        return self.key(control.key);
                    }
                    return true;
                }
                if !contains(self.body, mouse.column, mouse.row) {
                    return true;
                }
                let row = mouse
                    .row
                    .saturating_sub(self.body.y)
                    .saturating_add(self.scroll);
                if let Some(Modal::Menu { selected, .. }) = &mut self.modal {
                    if let Some((_, index)) = self.menu_rows.iter().find(|(r, _)| *r == row) {
                        *selected = *index;
                    }
                } else if let Some(diff) = &self.diff {
                    let Some(&line) = self.diff_rows.get(usize::from(row)) else {
                        return true;
                    };
                    if let Some(index) = diff
                        .hunks
                        .iter()
                        .position(|h| line >= h.start && line < h.end)
                    {
                        self.hunk = index;
                    }
                } else if let Some((_, index)) = self.rows.iter().find(|(r, _)| *r == row) {
                    let selected = if self.history.is_some() {
                        &mut self.history_selected
                    } else {
                        &mut self.selected
                    };
                    if *selected == *index {
                        return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                    *selected = *index;
                }
                true
            }
            _ => true,
        }
    }
    fn lines(&mut self) -> Vec<Line<'static>> {
        self.rows.clear();
        self.menu_rows.clear();
        if let Some(modal) = &self.modal {
            return match modal {
                Modal::Message(message) => std::iter::once(line("GIT · DETAILS", BLUE))
                    .chain(message.lines().map(|s| {
                        line(
                            safe_text(s),
                            if s.starts_with('+') && !s.starts_with("+++") {
                                GREEN
                            } else if s.starts_with('-') && !s.starts_with("---") {
                                RED
                            } else if s.starts_with("@@") || s.starts_with("diff --git") {
                                BLUE
                            } else {
                                INK
                            },
                        )
                    }))
                    .collect(),
                Modal::Confirm {
                    action,
                    description,
                } => std::iter::once(line("CONFIRM", GOLD))
                    .chain(description.lines().map(|s| line(s.to_owned(), INK)))
                    .chain([line(
                        if matches!(action, Action::Commit { .. }) {
                            "Enter / y: Commit · Esc / n: Edit message"
                        } else {
                            "Enter / y: Confirm · Esc / n: Cancel"
                        },
                        GOLD,
                    )])
                    .collect(),
                Modal::Form { kind, text } => {
                    let mut lines = vec![line(
                        match kind {
                            FormKind::Commit { .. } => "COMMIT · staged changes only",
                            FormKind::Create => "CREATE AND SWITCH BRANCH",
                            FormKind::Track(_) => "LOCAL TRACKING BRANCH NAME",
                            FormKind::Search => "SEARCH COMMIT MESSAGES",
                        },
                        BLUE,
                    )];
                    if matches!(kind, FormKind::Commit { .. }) {
                        if let Some(snapshot) = &self.snapshot {
                            lines.push(line(
                                format!(
                                    "{} · {}",
                                    service::display_path(&snapshot.root),
                                    snapshot.branch
                                ),
                                SOFT,
                            ));
                            for file in snapshot.files.iter().filter(|f| f.group == Group::Index) {
                                lines
                                    .push(line(format!("  {} {}", file.label(), file.stat), GREEN));
                            }
                        }
                        lines.push(line("MESSAGE (title, then body)", BLUE));
                    }
                    lines.extend(text.lines().map(|s| line(s.to_owned(), INK)));
                    if text.is_empty() || text.ends_with('\n') {
                        lines.push(Line::default());
                    }
                    if let Some(last) = lines.last_mut() {
                        last.spans
                            .push(Span::styled("▏", Style::default().fg(GOLD)));
                    }
                    if matches!(kind, FormKind::Search) {
                        lines.push(line("Title + body · literal text, case insensitive", SOFT));
                        lines.push(line("Enter: search · Ctrl+U: clear · empty: all", SOFT));
                    } else if matches!(kind, FormKind::Commit { .. }) {
                        lines.push(line("Ctrl+Enter / Review: confirm · Esc: Cancel", SOFT));
                    } else {
                        lines.push(line("Enter / Review: confirm · Esc: Cancel", SOFT));
                    }
                    lines
                }
                Modal::Menu {
                    kind,
                    items,
                    selected,
                    filter,
                } => {
                    let mut out = vec![
                        line(
                            match kind {
                                MenuKind::Operations => "OPERATIONS",
                                MenuKind::Branches => "BRANCHES",
                                MenuKind::Fetch => "FETCH FROM",
                                MenuKind::Push => "PUSH TO",
                            },
                            BLUE,
                        ),
                        line(format!("Search: {filter}▏"), SOFT),
                    ];
                    for (i, item) in items
                        .iter()
                        .filter(|s| s.to_lowercase().contains(&filter.to_lowercase()))
                        .enumerate()
                    {
                        self.menu_rows.push((out.len() as u16, i));
                        out.push(line(
                            format!("{} {item}", if *selected == i { "›" } else { " " }),
                            if *selected == i { GOLD } else { INK },
                        ));
                    }
                    out
                }
            };
        }
        if self.diff.is_some() {
            return self.diff_content();
        }
        if let Some(history) = &self.history {
            let mut out = vec![];
            if history.commits.is_empty() {
                out.push(line(
                    if !history.query.is_empty() {
                        "No matching commits · /: change search"
                    } else if history.page == 0 {
                        "No commits yet"
                    } else {
                        "No commits on this page · r: refresh"
                    },
                    SOFT,
                ));
            }
            for (i, commit) in history.commits.iter().enumerate() {
                self.rows.push((out.len() as u16, i));
                let selected = self.history_selected == i;
                let width = usize::from(self.body.width.max(1));
                out.extend(wrap_line(
                    line(
                        format!(
                            "{} {} {}",
                            if selected { "›" } else { " " },
                            commit.short,
                            safe_text(&commit.subject)
                        ),
                        if selected { GOLD } else { INK },
                    ),
                    width,
                ));
                out.extend(wrap_line(
                    line(
                        format!(
                            "  {} · {}",
                            commit.date.get(..10).unwrap_or(&commit.date),
                            safe_text(&commit.author)
                        ),
                        SOFT,
                    ),
                    width,
                ));
                if !commit.refs.is_empty() {
                    out.extend(wrap_line(
                        line(format!("  {}", safe_text(&commit.refs)), BLUE),
                        width,
                    ));
                }
                out.push(Line::default());
            }
            return out;
        }
        let Some(snapshot) = &self.snapshot else {
            return vec![
                line("No Git repository", SOFT),
                line("Focus a project terminal in this tab.", SOFT),
            ];
        };
        let mut out = vec![];
        if snapshot.files.is_empty() {
            out.push(line("✓ Working tree clean", GREEN));
            out.push(line("New changes will appear here.", SOFT));
        }
        let mut previous = None;
        for (i, file) in snapshot.files.iter().enumerate() {
            if previous != Some(file.group) {
                if previous.is_some() {
                    out.push(Line::default());
                }
                let count = snapshot
                    .files
                    .iter()
                    .filter(|f| f.group == file.group)
                    .count();
                let mut heading = section(
                    &format!(
                        "{}  {count}",
                        match file.group {
                            Group::Conflict => "CONFLICTS",
                            Group::Worktree => "UNSTAGED",
                            Group::Index => "STAGED",
                        }
                    ),
                    self.body.width,
                );
                heading.spans[0].style = Style::default()
                    .fg(group_color(file.group))
                    .add_modifier(Modifier::BOLD);
                out.push(heading);
                previous = Some(file.group);
            }
            for row in file_lines(
                file,
                self.selected == i,
                self.body.width,
                self.screen.width < 32 || self.screen.height < 12,
            ) {
                self.rows.push((out.len().min(u16::MAX as usize) as u16, i));
                out.push(row);
            }
        }
        out
    }
    fn controls(&self, navigation: bool) -> Vec<Control> {
        let control = |label, compact, code| Control::new(label, compact, code);
        let mut controls = if navigation {
            if let Some(Modal::Menu {
                items,
                selected,
                filter,
                ..
            }) = &self.modal
            {
                let count = items
                    .iter()
                    .filter(|s| s.to_lowercase().contains(&filter.to_lowercase()))
                    .count();
                let mut previous = control("↑ Previous", "↑", KeyCode::Up);
                previous.enabled = *selected > 0 && count > 0;
                let mut next = control("↓ Next", "↓", KeyCode::Down);
                next.enabled = *selected + 1 < count;
                vec![previous, next]
            } else if self.modal.is_some() {
                let mut previous = control("Page up", "PgUp", KeyCode::PageUp);
                previous.enabled = self.scroll > 0;
                let mut next = control("Page down", "PgDn", KeyCode::PageDown);
                next.enabled = self.scroll < self.limit;
                vec![previous, next]
            } else if let Some(history) = &self.history {
                let mut prev = control("Newer(,)", ", Newer", KeyCode::Char(','));
                prev.enabled = history.page > 0;
                let mut next = control("Older(.)", ". Older", KeyCode::Char('.'));
                next.enabled = history.has_more;
                let mut show = control("Details(Enter)", "↵ Show", KeyCode::Enter);
                show.enabled = !history.commits.is_empty();
                vec![
                    prev,
                    next,
                    show,
                    control("Files(Esc)", "Esc List", KeyCode::Esc),
                ]
            } else if self.diff.is_some() {
                let position = self.diff_position();
                let mut prev = control("Prev(,)", ", Prev", KeyCode::Char(','));
                prev.enabled = position.is_some_and(|(i, _)| i > 0);
                let mut next = control("Next(.)", ". Next", KeyCode::Char('.'));
                next.enabled = position.is_some_and(|(i, n)| i + 1 < n);
                vec![
                    prev,
                    next,
                    control(
                        if self.nowrap {
                            "Wrap OFF(w)"
                        } else {
                            "Wrap ON(w)"
                        },
                        "w Wrap",
                        KeyCode::Char('w'),
                    ),
                    control("Files(Esc)", "Esc List", KeyCode::Esc),
                ]
            } else {
                let mut open = control("Diff(Enter)", "↵ Diff", KeyCode::Enter);
                open.enabled = self.file().is_some();
                let mut commit = control("Commit(n)", "n Commit", KeyCode::Char('n'));
                commit.enabled = self.snapshot.as_ref().is_some_and(|s| {
                    s.files.iter().any(|f| f.group == Group::Index)
                        && !s.files.iter().any(|f| f.group == Group::Conflict)
                });
                let mut log = control("Log(L)", "L Log", KeyCode::Char('L'));
                log.enabled = self.snapshot.is_some();
                let mut branches = control("Branch(b)", "b Branch", KeyCode::Char('b'));
                branches.enabled = self.snapshot.is_some();
                vec![open, commit, log, branches]
            }
        } else {
            match &self.modal {
                Some(Modal::Message(_)) => vec![control(
                    if self.history.is_some() {
                        "Log(Esc)"
                    } else if self.diff.is_some() {
                        "Diff(Esc)"
                    } else {
                        "Files(Esc)"
                    },
                    "Esc Back",
                    KeyCode::Esc,
                )],
                Some(Modal::Confirm { action, .. }) => vec![
                    control(
                        if matches!(action, Action::Commit { .. }) {
                            "Commit(Enter)"
                        } else {
                            "Confirm(Enter)"
                        },
                        if matches!(action, Action::Commit { .. }) {
                            "↵ Commit"
                        } else {
                            "↵ Confirm"
                        },
                        KeyCode::Enter,
                    ),
                    control(
                        if matches!(action, Action::Commit { .. }) {
                            "Edit(Esc)"
                        } else {
                            "Cancel(Esc)"
                        },
                        "Esc Back",
                        KeyCode::Esc,
                    ),
                ],
                Some(Modal::Form {
                    kind: FormKind::Search,
                    text,
                }) => {
                    let mut clear = control("Clear(Ctrl+U)", "^U Clear", KeyCode::Char('u'));
                    clear.key.modifiers = KeyModifiers::CONTROL;
                    clear.enabled = !text.is_empty();
                    vec![
                        control(
                            if text.trim().is_empty() {
                                "All commits(Enter)"
                            } else {
                                "Search(Enter)"
                            },
                            if text.trim().is_empty() {
                                "↵ All"
                            } else {
                                "↵ Search"
                            },
                            KeyCode::Enter,
                        ),
                        clear,
                        control("Cancel(Esc)", "Esc Cancel", KeyCode::Esc),
                    ]
                }
                Some(Modal::Form { kind, text }) => {
                    let commit = matches!(kind, FormKind::Commit { .. });
                    let mut submit = control(
                        if commit {
                            "Review(Ctrl+Enter)"
                        } else {
                            "Review(Enter)"
                        },
                        if commit { "^↵ Review" } else { "↵ Review" },
                        KeyCode::Enter,
                    );
                    if commit {
                        submit.key.modifiers = KeyModifiers::CONTROL;
                    }
                    submit.enabled = !text.lines().next().unwrap_or("").trim().is_empty();
                    vec![submit, control("Cancel(Esc)", "Esc Cancel", KeyCode::Esc)]
                }
                Some(Modal::Menu {
                    items,
                    selected,
                    filter,
                    ..
                }) => {
                    let mut select = control("Select(Enter)", "↵ Select", KeyCode::Enter);
                    select.enabled = items
                        .iter()
                        .filter(|s| s.to_lowercase().contains(&filter.to_lowercase()))
                        .nth(*selected)
                        .is_some();
                    vec![select, control("Back(Esc)", "Esc Back", KeyCode::Esc)]
                }
                None => {
                    if self.history.is_some() {
                        return vec![
                            control("Refresh(r)", "r Refresh", KeyCode::Char('r')),
                            control("Search(/)", "/ Search", KeyCode::Char('/')),
                            control("Ops(o)", "o Ops", KeyCode::Char('o')),
                            control("Help(?)", "? Help", KeyCode::Char('?')),
                        ]
                        .into_iter()
                        .map(|mut c| {
                            c.enabled = !self.busy;
                            c
                        })
                        .collect();
                    }
                    let file = self.file();
                    let index = file.as_ref().is_some_and(|f| f.group == Group::Index);
                    let block = self
                        .diff
                        .as_ref()
                        .is_some_and(|d| d.reason.is_none() && !d.hunks.is_empty());
                    let mut single = control(
                        if index && block {
                            "Unstage block(u)"
                        } else if index {
                            "Unstage file(u)"
                        } else {
                            "Stage file(s)"
                        },
                        if index && block {
                            "u Block"
                        } else if index {
                            "u File"
                        } else {
                            "Stage(s)"
                        },
                        KeyCode::Char(if index { 'u' } else { 's' }),
                    );
                    single.enabled = file.is_some();
                    let mut stage = control("Stage all(a)", "All(a)", KeyCode::Char('a'));
                    stage.enabled = self
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| s.files.iter().any(|f| f.group != Group::Index));
                    let second = if file.as_ref().is_some_and(|f| f.group == Group::Worktree) {
                        control(
                            if block {
                                "Discard block(d)"
                            } else {
                                "Discard file(d)"
                            },
                            if block { "d Block" } else { "d File" },
                            KeyCode::Char('d'),
                        )
                    } else {
                        control("Refresh(r)", "r Refresh", KeyCode::Char('r'))
                    };
                    let mut menu = control("Ops(o)", "o Ops", KeyCode::Char('o'));
                    menu.enabled = self.snapshot.is_some();
                    vec![
                        single,
                        stage,
                        second,
                        menu,
                        control("Help(?)", "? Help", KeyCode::Char('?')),
                    ]
                }
            }
        };
        if self.busy {
            for control in &mut controls {
                control.enabled = false;
            }
        }
        controls
    }
    fn diff_header(&self, width: u16) -> Vec<Line<'static>> {
        let Some(diff) = &self.diff else {
            return vec![];
        };
        let mut header: Vec<_> = vec![line("DIFF · Files(Esc)", BLUE)];
        header.extend(
            wrap_line(line(diff.file.label(), BLUE), usize::from(width.max(1)))
                .into_iter()
                .take(2),
        );
        let position = self.diff_position();
        let counter = position.map_or("File".into(), |(i, n)| {
            if width >= 40 {
                format!("File {}/{n}", i + 1)
            } else {
                format!("{}/{n}", i + 1)
            }
        });
        let group = match diff.file.group {
            Group::Index => "Staged",
            Group::Worktree => "Unstaged",
            Group::Conflict => "Conflict",
        };
        let block = if diff.hunks.is_empty() {
            0
        } else {
            self.hunk + 1
        };
        header.push(line(
            format!("{counter} · {group} · Block {block}/{}", diff.hunks.len()),
            SOFT,
        ));
        let hint = if width >= 44 {
            "Esc: files · ,/.: prev/next · [/]: block"
        } else {
            "Esc: files · ,/.: prev/next"
        };
        header.extend(wrap_line(line(hint, GOLD), usize::from(width.max(1))));
        if let Some(reason) = &diff.reason {
            header.extend(wrap_line(
                line(reason.clone(), SOFT),
                usize::from(width.max(1)),
            ));
        }
        header
    }
    fn repository_branch_label(&self, width: u16) -> String {
        let Some(snapshot) = &self.snapshot else {
            return String::new();
        };
        let available = width / 2;
        if available < 4 {
            return clipped(&snapshot.branch, usize::from(available));
        }
        format!(
            "{} ▾",
            clipped(
                &format!("⑂ {}", snapshot.branch),
                usize::from(available.saturating_sub(2))
            )
        )
    }
    fn repository_header(&self, width: u16) -> Vec<Line<'static>> {
        let Some(snapshot) = &self.snapshot else {
            return vec![];
        };
        let counts = [Group::Worktree, Group::Index, Group::Conflict]
            .map(|group| snapshot.files.iter().filter(|f| f.group == group).count());
        let compact = self.screen.width < 32 || self.screen.height < 16;
        let mut summary = Vec::new();
        for (i, (group, count)) in [Group::Worktree, Group::Index, Group::Conflict]
            .into_iter()
            .zip(counts)
            .enumerate()
        {
            if i == 2 && count == 0 {
                continue;
            }
            if !summary.is_empty() {
                summary.push(Span::styled(" · ", Style::default().fg(SOFT)));
            }
            let label = match (group, compact || width < 42) {
                (Group::Worktree, true) => "U",
                (Group::Index, true) => "S",
                (Group::Conflict, true) => "!",
                (Group::Worktree, false) => "Unstaged",
                (Group::Index, false) => "Staged",
                (Group::Conflict, false) => "Conflicts",
            };
            summary.push(Span::styled(
                format!("{label} {count}"),
                Style::default().fg(group_color(group)),
            ));
        }
        let branch = self.repository_branch_label(width);
        let path = clipped(
            &service::display_path(&snapshot.root),
            usize::from(width).saturating_sub(branch.width() + 1),
        );
        let mut identity = pair(&path, branch, width, SOFT);
        identity.spans[0].style = Style::default().fg(INK);
        identity.spans[2].style = Style::default().fg(SOFT);
        if compact {
            return vec![identity, Line::from(summary)];
        }
        let upstream = snapshot
            .upstream
            .as_ref()
            .map_or("No upstream · o: remote operations".into(), |upstream| {
                format!("{upstream} · ↑{} ↓{}", snapshot.ahead, snapshot.behind)
            });
        vec![
            identity,
            line(clipped(&upstream, usize::from(width)), SOFT),
            Line::from(summary),
            Line::default(),
        ]
    }
    fn diff_content(&mut self) -> Vec<Line<'static>> {
        self.diff_rows.clear();
        self.hunk_rows.clear();
        let Some(diff) = &self.diff else {
            return vec![];
        };
        self.hunk_rows.resize(diff.hunks.len(), 0);
        let width = usize::from(self.body.width.max(1));
        let digits = diff
            .lines
            .iter()
            .filter(|s| s.starts_with("@@ "))
            .flat_map(|s| s.split_whitespace().skip(1).take(2))
            .map(range_end)
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(2);
        let numbered = width >= digits * 2 + 10;
        let gutter_width = if numbered { digits * 2 + 3 } else { 1 };
        let mut old = 0usize;
        let mut new = 0usize;
        let mut out = Vec::new();
        for (i, text) in diff.lines.iter().enumerate() {
            if !diff.file.untracked()
                && (text.starts_with("diff --git ")
                    || text.starts_with("index ")
                    || text.starts_with("--- ")
                    || text.starts_with("+++ "))
            {
                continue;
            }
            if text.starts_with("@@ ") && !diff.file.untracked() {
                let fields: Vec<_> = text.split_whitespace().collect();
                old = fields.get(1).map_or(0, |s| range_start(s));
                new = fields.get(2).map_or(0, |s| range_start(s));
            }
            if let Some(h) = diff.hunks.iter().position(|h| h.start == i) {
                self.hunk_rows[h] = out.len().min(u16::MAX as usize) as u16;
            }
            let selected = diff
                .hunks
                .get(self.hunk)
                .is_some_and(|h| i >= h.start && i < h.end);
            let (old_number, new_number, color) = if diff.file.untracked() {
                new += 1;
                (String::new(), new.to_string(), INK)
            } else if text.starts_with('+') {
                let n = new;
                new += 1;
                (String::new(), n.to_string(), GREEN)
            } else if text.starts_with('-') {
                let n = old;
                old += 1;
                (n.to_string(), String::new(), RED)
            } else if text.starts_with(' ') {
                let pair = (old.to_string(), new.to_string(), INK);
                old += 1;
                new += 1;
                pair
            } else {
                (String::new(), String::new(), BLUE)
            };
            let gutter = if numbered {
                format!(
                    "{}{:>digits$} {:>digits$} ",
                    if selected { "▎" } else { " " },
                    old_number,
                    new_number
                )
            } else {
                if selected { "▎" } else { " " }.into()
            };
            let safe = safe_text(text);
            let style = Style::default()
                .fg(color)
                .bg(if selected { RAIL } else { BG });
            let chunks = if self.nowrap {
                vec![safe]
            } else {
                split_display(&safe, width.saturating_sub(gutter_width).max(1))
            };
            for (row, chunk) in chunks.into_iter().enumerate() {
                let prefix = if row == 0 {
                    gutter.clone()
                } else if gutter_width == 1 {
                    "↳".into()
                } else {
                    format!(
                        "{}{}↳",
                        if selected { "▎" } else { " " },
                        " ".repeat(gutter_width.saturating_sub(2))
                    )
                };
                self.diff_rows.push(i);
                out.push(Line::from(Span::styled(format!("{prefix}{chunk}"), style)));
            }
        }
        out
    }
    pub(super) fn draw(&mut self, frame: &mut ratatui::Frame) {
        let screen = frame.area();
        self.screen = screen;
        self.branch_button = None;
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(INK)),
            screen,
        );
        if screen.width == 0 || screen.height == 0 {
            return;
        }
        let margin = if screen.width >= 32 { 1 } else { 0 };
        let area = screen.inner(Margin::new(margin, 0));
        let mini = screen.height < 7;
        let showing_diff = self.diff.is_some() && self.modal.is_none();
        let showing_history = self.history.is_some() && self.modal.is_none();
        let footer_height = if mini { 1 } else { 3 };
        let mut headers = if showing_diff {
            self.diff_header(area.width.saturating_sub(1))
        } else if showing_history {
            let history = self.history.as_ref().unwrap();
            let mut header = vec![line("GIT LOG · Files(Esc)", BLUE)];
            if let Some(snapshot) = &self.snapshot {
                header.extend(wrap_line(
                    line(
                        format!(
                            "{} · {}",
                            service::display_path(&snapshot.root),
                            snapshot.branch
                        ),
                        BLUE,
                    ),
                    usize::from(area.width.saturating_sub(1).max(1)),
                ));
            }
            header.push(line(
                format!(
                    "Page {} · {} commits",
                    history.page + 1,
                    history.commits.len()
                ),
                SOFT,
            ));
            if !history.query.is_empty() {
                header.extend(
                    wrap_line(
                        line(format!("Search: {}", safe_text(&history.query)), GOLD),
                        usize::from(area.width.saturating_sub(1).max(1)),
                    )
                    .into_iter()
                    .take(2),
                );
            }
            header.extend(wrap_line(
                line("Enter: details · ,/.: pages · Esc: files", GOLD),
                usize::from(area.width.saturating_sub(1).max(1)),
            ));
            header
        } else if self.modal.is_none() {
            self.repository_header(area.width.saturating_sub(1))
        } else {
            vec![]
        };
        headers.truncate(usize::from(area.height.saturating_sub(footer_height + 2)));
        let header_height = headers.len() as u16;
        let (header_area, reserve) = pulse_header_area(screen);
        draw_page_tabs(
            frame,
            header_area,
            reserve,
            true,
            self.modal.is_none() && !self.busy,
        );
        frame.render_widget(
            Paragraph::new(headers),
            Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width.saturating_sub(1),
                header_height,
            ),
        );
        if self.modal.is_none() && !showing_diff && !showing_history && header_height > 0 {
            let width = area.width.saturating_sub(1);
            let label = self.repository_branch_label(width);
            let button_width = label.width().min(usize::from(width)) as u16;
            if button_width > 0 {
                let x = area.x + width - button_width;
                let rect = Rect::new(x, area.y.saturating_add(1), button_width, 1);
                self.branch_button = Some(rect);
            }
        }
        let previous_width = self.body.width;
        self.body = Rect::new(
            area.x,
            area.y.saturating_add(1 + header_height),
            area.width.saturating_sub(1),
            area.height
                .saturating_sub(1 + header_height + footer_height),
        );
        if showing_diff && previous_width != self.body.width && self.diff_anchor.is_none() {
            self.diff_anchor = self.diff_rows.get(usize::from(self.scroll)).copied();
        }
        let mut content = self.lines();
        if self
            .modal
            .as_ref()
            .is_some_and(|m| !matches!(m, Modal::Menu { .. }))
        {
            content = content
                .into_iter()
                .flat_map(|l| wrap_line(l, usize::from(self.body.width.max(1))))
                .collect();
        }
        self.limit = (content.len().min(u16::MAX as usize) as u16).saturating_sub(self.body.height);
        if showing_diff && let Some(anchor) = self.diff_anchor.take() {
            self.scroll = self
                .diff_rows
                .iter()
                .position(|i| *i >= anchor)
                .unwrap_or(0)
                .min(u16::MAX as usize) as u16;
        }
        if self.modal.is_none() && self.diff.is_none() {
            let selected = if showing_history {
                self.history_selected
            } else {
                self.selected
            };
            if let Some((row, _)) = self.rows.iter().find(|(_, i)| *i == selected) {
                let last = self
                    .rows
                    .iter()
                    .rfind(|(_, i)| *i == selected)
                    .map_or(*row, |(row, _)| *row);
                if last < self.scroll {
                    self.scroll = *row;
                } else if *row >= self.scroll.saturating_add(self.body.height) {
                    self.scroll = row.saturating_add(1).saturating_sub(self.body.height);
                }
            }
        } else if let Some(Modal::Menu { selected, .. }) = &self.modal
            && let Some((row, _)) = self.menu_rows.iter().find(|(_, i)| i == selected)
        {
            if *row < self.scroll {
                self.scroll = *row;
            } else if *row >= self.scroll.saturating_add(self.body.height) {
                self.scroll = row.saturating_add(1).saturating_sub(self.body.height);
            }
        }
        self.scroll = self.scroll.min(self.limit);
        let horizontal = if showing_diff && self.nowrap {
            self.horizontal
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(content).scroll((self.scroll, horizontal)),
            self.body,
        );
        if self.limit > 0 {
            let mut state = scroll_state(
                usize::from(self.limit) + usize::from(self.body.height),
                usize::from(self.scroll),
                usize::from(self.body.height),
            );
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(Style::default().fg(BLUE))
                    .track_style(Style::default().fg(RAIL)),
                Rect::new(self.body.right(), self.body.y, 1, self.body.height),
                &mut state,
            );
        }
        if !mini {
            let status = if self.busy {
                "Working… (up to 120s)".into()
            } else if let Some(error) = &self.error {
                if self.modal.is_some() {
                    format!("! {error}")
                } else {
                    format!("! {error} · l: details")
                }
            } else if let Some(modal) = &self.modal {
                match modal {
                    Modal::Menu { .. } => "Type to filter · ↑↓ select · Enter open · Esc back",
                    Modal::Form {
                        kind: FormKind::Search,
                        ..
                    } => "Enter search · Ctrl+U clear · empty search: all commits",
                    Modal::Form {
                        kind: FormKind::Commit { .. },
                        ..
                    } => "Enter newline · Ctrl+Enter review · Esc cancel",
                    Modal::Form { .. } => "Enter review · Esc cancel",
                    Modal::Confirm {
                        action: Action::Commit { .. },
                        ..
                    } => "Enter commit · Esc edit message",
                    Modal::Confirm { .. } => "Enter confirm · Esc cancel",
                    Modal::Message(_) => "↑↓ / PgUp / PgDn scroll · Esc back",
                }
                .into()
            } else if !self.notice.is_empty() {
                self.notice.clone()
            } else if showing_history {
                "↑↓: select · Enter: details · / search · r refresh".into()
            } else if showing_diff {
                if self
                    .diff
                    .as_ref()
                    .is_some_and(|d| d.reason.is_none() && !d.hunks.is_empty())
                {
                    if self
                        .diff
                        .as_ref()
                        .is_some_and(|d| d.file.group == Group::Index)
                    {
                        "u: unstage block · [/]: select · r refresh".into()
                    } else {
                        "a: stage all · d: block · [/]: select · r refresh".into()
                    }
                } else {
                    "↑↓ scroll · whole-file actions · r refresh".into()
                }
            } else {
                self.refreshed.map_or("Reading Git…".into(), |time| {
                    format!(
                        "Updated {}s ago · ↑↓ select · r refresh",
                        time.elapsed().as_secs()
                    )
                })
            };
            frame.render_widget(
                Paragraph::new(clipped(&status, usize::from(area.width)))
                    .style(Style::default().fg(if self.error.is_some() { RED } else { SOFT })),
                Rect::new(area.x, screen.bottom() - 3, area.width, 1),
            );
        }
        for navigation in [true, false] {
            if navigation && mini {
                continue;
            }
            let row = screen
                .bottom()
                .saturating_sub(if navigation { 2 } else { 1 });
            let controls = self.controls(navigation);
            for (control, rect) in controls
                .iter()
                .zip(control_rects(screen, row, controls.len()))
            {
                frame.render_widget(
                    Paragraph::new(control.label(rect.width))
                        .alignment(Alignment::Center)
                        .style(
                            Style::default()
                                .fg(if control.enabled { BLUE } else { SOFT })
                                .bg(if control.enabled { RAIL } else { BG }),
                        ),
                    rect,
                );
            }
        }
    }
}
fn group_color(group: Group) -> Color {
    match group {
        Group::Worktree => GOLD,
        Group::Index => GREEN,
        Group::Conflict => RED,
    }
}
fn file_lines(
    file: &service::File,
    selected: bool,
    width: u16,
    compact: bool,
) -> Vec<Line<'static>> {
    let status = if file.untracked() {
        '?'
    } else if file.group == Group::Index {
        file.x as char
    } else {
        file.y as char
    };
    let color = if file.group == Group::Conflict || status == 'D' {
        RED
    } else if status == 'A' {
        GREEN
    } else {
        group_color(file.group)
    };
    let marker = if selected { "▎" } else { " " };
    let selection = Style::default().bg(if selected { RAIL } else { BG });
    if compact {
        return wrap_line(
            Line::from(vec![
                Span::styled(format!("{marker} {status} "), Style::default().fg(color)),
                Span::styled(
                    format!("{} {}", file.label(), file.stat),
                    Style::default().fg(INK),
                ),
            ])
            .style(selection),
            usize::from(width.max(1)),
        )
        .into_iter()
        .map(|row| row.style(selection))
        .collect();
    }
    let name = file
        .path
        .file_name()
        .map_or_else(|| file.label(), |s| safe_text(&s.to_string_lossy()));
    let available = usize::from(width).saturating_sub(5 + file.stat.width());
    let leaf = clipped(&name, available);
    let gap = usize::from(width).saturating_sub(4 + leaf.width() + file.stat.width());
    let mut spans = vec![
        Span::styled(format!("{marker} "), Style::default().fg(GOLD)),
        Span::styled(
            format!("{status} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            leaf,
            Style::default().fg(INK).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::raw(" ".repeat(gap)),
    ];
    for (i, stat) in file.stat.split_whitespace().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            stat.to_owned(),
            Style::default().fg(if stat.starts_with('+') {
                GREEN
            } else if stat.starts_with('-') {
                RED
            } else {
                SOFT
            }),
        ));
    }
    let mut out = vec![Line::from(spans).style(selection)];
    let context = if file.old_path.is_some() || name.width() > available {
        file.label()
    } else {
        file.path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or(String::new(), |p| format!("{}/", service::display_path(p)))
    };
    if !context.is_empty() {
        for part in split_display(&context, usize::from(width.saturating_sub(4).max(1))) {
            out.push(line(format!("{marker}   {part}"), SOFT).style(selection));
        }
    }
    out
}
struct Control {
    label: &'static str,
    compact: &'static str,
    key: KeyEvent,
    enabled: bool,
}
impl Control {
    fn new(label: &'static str, compact: &'static str, code: KeyCode) -> Self {
        Self {
            label,
            compact,
            key: KeyEvent::new(code, KeyModifiers::NONE),
            enabled: true,
        }
    }
    fn label(&self, width: u16) -> String {
        if self.label.width() <= usize::from(width) {
            self.label.into()
        } else if self.compact.width() <= usize::from(width) {
            self.compact.into()
        } else {
            let key = match self.key.code {
                KeyCode::Char(c) => c.to_string(),
                KeyCode::Esc => "Esc".into(),
                KeyCode::Enter => "↵".into(),
                KeyCode::Up => "↑".into(),
                KeyCode::Down => "↓".into(),
                KeyCode::PageUp => "PgUp".into(),
                KeyCode::PageDown => "PgDn".into(),
                _ => String::new(),
            };
            clipped(&key, usize::from(width))
        }
    }
}
fn control_rects(screen: Rect, row: u16, count: usize) -> Vec<Rect> {
    if count == 0 {
        return vec![];
    }
    Layout::horizontal(vec![Constraint::Ratio(1, count as u32); count])
        .split(Rect::new(screen.x, row, screen.width, 1))
        .to_vec()
}
fn range_start(value: &str) -> usize {
    value
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
fn range_end(value: &str) -> usize {
    let mut range = value.trim_start_matches(['-', '+']).split(',');
    range
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .saturating_add(range.next().and_then(|s| s.parse().ok()).unwrap_or(1))
}
fn safe_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c == '\t' {
                vec![' '; 4]
            } else if c.is_control() {
                c.escape_default().collect()
            } else {
                vec![c]
            }
        })
        .collect()
}
fn split_display(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for c in text.chars() {
        let size = c.width().unwrap_or(0);
        if used + size > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(c);
        used += size;
    }
    out.push(current);
    out
}
fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let style = line.spans.first().map_or(Style::default(), |s| s.style);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    let safe: String = line
        .to_string()
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect();
    for c in safe.chars() {
        let size = c.width().unwrap_or(0);
        if used + size > width && !current.is_empty() {
            out.push(Line::from(Span::styled(
                std::mem::take(&mut current),
                style,
            )));
            used = 0;
        }
        current.push(c);
        used += size;
    }
    out.push(Line::from(Span::styled(current, style)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use serde_json::json;
    fn snapshot(root: &str) -> Snapshot {
        Snapshot {
            root: root.into(),
            branch: "feature/git".into(),
            upstream: Some("origin/feature/git".into()),
            ahead: 2,
            behind: 1,
            head: Some("abc".into()),
            files: vec![
                service::File {
                    path: "src/中文.rs".into(),
                    old_path: None,
                    x: b' ',
                    y: b'M',
                    group: Group::Worktree,
                    stat: "+3 -1".into(),
                },
                service::File {
                    path: "staged.rs".into(),
                    old_path: None,
                    x: b'M',
                    y: b' ',
                    group: Group::Index,
                    stat: "+2 -0".into(),
                },
            ],
        }
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn render(pane: &mut GitPane, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| pane.draw(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn follows_regular_terminal_in_own_tab_and_retains_cwd_on_pulse_focus() {
        let panes = json!({"result":{"panes":[
            {"pane_id":"source","tab_id":"tab","cwd":"/project","foreground_cwd":"/project/sub","focused":true},
            {"pane_id":"pulse","tab_id":"tab","label":LABEL,"cwd":"/plugin"},
            {"pane_id":"other","tab_id":"other-tab","cwd":"/other","focused":true}
        ]}});
        assert_eq!(
            project_cwd(&panes, "tab", "source", "pulse", false),
            Some("/project/sub".into())
        );
        let mut panes = panes;
        panes["result"]["panes"][0]["focused"] = json!(false);
        panes["result"]["panes"][1]["focused"] = json!(true);
        assert_eq!(project_cwd(&panes, "tab", "source", "pulse", false), None);
        assert_eq!(
            project_cwd(&panes, "tab", "source", "pulse", true),
            Some("/project/sub".into())
        );
    }
    #[test]
    fn sidebar_and_tiny_layouts_keep_git_navigation() {
        for (width, height) in [(48, 30), (32, 12), (20, 8), (10, 4), (1, 1)] {
            let mut pane = GitPane {
                snapshot: Some(snapshot("/project")),
                ..Default::default()
            };
            let text = render(&mut pane, width, height);
            if width >= 20 {
                assert!(text.contains("GIT"));
                if width >= 32 {
                    assert!(text.contains("Ops"));
                }
            }
            if width >= 32 && height >= 12 {
                assert!(text.contains("UNSTAGED"));
                assert!(text.contains("STAGED"));
            }
            assert!(pane.key(key(KeyCode::Char('g'))));
            assert!(!pane.key(key(KeyCode::Char('T'))));
        }
    }
    #[test]
    fn repository_overview_stays_visible_when_scrolling_files() {
        let mut data = snapshot("/workspace/project");
        data.branch = "main".into();
        let template = data.files[0].clone();
        let staged = data.files[1].clone();
        data.files = (0..60)
            .map(|i| service::File {
                path: format!("folder/file-{i:02}.rs").into(),
                ..template.clone()
            })
            .collect();
        data.files.push(staged);
        let mut pane = GitPane {
            snapshot: Some(data),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(48, 30)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let first: String = (0..48)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect();
        assert!(first.contains("/workspace/project") && first.contains("main"));
        let branch = pane.branch_button.unwrap();
        let cell = &terminal.backend().buffer()[(branch.x, branch.y)];
        assert_eq!(cell.fg, SOFT);
        assert_eq!(cell.bg, BG);
        assert!(!cell.modifier.contains(Modifier::BOLD));
        let header_bottom = pane.body.y;
        let header = |terminal: &Terminal<TestBackend>| {
            (1..header_bottom)
                .map(|y| {
                    (0..48)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        let before = header(&terminal);
        assert!(before.join(" ").contains("Unstaged 60"));
        assert!(before.join(" ").contains("Staged 1"));
        pane.key(key(KeyCode::End));
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert!(pane.scroll > 0);
        assert_eq!(header(&terminal), before);
        assert_eq!(pane.file().unwrap().path, PathBuf::from("staged.rs"));
    }
    #[test]
    fn branch_name_button_opens_selection_and_switch_still_requires_confirmation() {
        for (width, height) in [(20, 8), (32, 12), (48, 30), (100, 40)] {
            let mut pane = GitPane {
                snapshot: Some(snapshot("/project")),
                ..Default::default()
            };
            let (send, receive) = mpsc::sync_channel(2);
            pane.requests = Some(send);
            let (updates, responses) = mpsc::channel();
            pane.updates = Some(responses);
            render(&mut pane, width, height);
            let rect = pane.branch_button.expect("Visible branch button");
            assert_eq!(rect.y, 1);
            assert!(rect.right() <= width);
            assert!(rect.bottom() <= pane.body.y);
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.right() - 1,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(
                matches!(receive.try_recv().unwrap(), Request::Menu(root, MenuKind::Branches) if root == std::path::Path::new("/project"))
            );
            assert!(pane.busy);
            updates
                .send(Response::Menu(
                    MenuKind::Branches,
                    Ok(vec!["feature/git".into(), "topic".into()]),
                ))
                .unwrap();
            pane.poll();
            assert!(matches!(
                pane.modal,
                Some(Modal::Menu {
                    kind: MenuKind::Branches,
                    ..
                })
            ));
            pane.key(key(KeyCode::Down));
            pane.key(key(KeyCode::Enter));
            assert!(
                matches!(&pane.modal, Some(Modal::Confirm { action: Action::Switch(name), .. }) if name == "topic")
            );
            assert!(receive.try_recv().is_err());
            pane.key(key(KeyCode::Esc));
            assert!(pane.modal.is_none());
            assert_eq!(pane.snapshot.as_ref().unwrap().branch, "feature/git");
        }
    }
    #[test]
    fn branch_button_hit_area_tracks_layout_and_is_inactive_outside_overview() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            ..Default::default()
        };
        pane.snapshot.as_mut().unwrap().branch = "feature/非常长的分支名称-with-a-long-name".into();
        let (send, receive) = mpsc::sync_channel(2);
        pane.requests = Some(send);
        render(&mut pane, 100, 40);
        let wide = pane.branch_button.unwrap();
        let text = render(&mut pane, 20, 8);
        let compact = pane.branch_button.unwrap();
        assert!(text.contains('▾'));
        assert!(text.contains('…'));
        assert_ne!(wide, compact);
        let click = |column| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: compact.y,
                modifiers: KeyModifiers::NONE,
            })
        };
        pane.input(&click(compact.right()));
        assert!(receive.try_recv().is_err());
        pane.busy = true;
        pane.input(&click(compact.x));
        assert!(receive.try_recv().is_err());
        pane.busy = false;
        pane.modal = Some(Modal::Form {
            kind: FormKind::Create,
            text: "draft".into(),
        });
        render(&mut pane, 20, 8);
        assert!(pane.branch_button.is_none());
        pane.input(&click(compact.x));
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "draft"));
        assert!(receive.try_recv().is_err());
        let mut diff = diff_pane();
        render(&mut diff, 48, 30);
        assert!(diff.branch_button.is_none());
        pane.modal = None;
        pane.snapshot = None;
        render(&mut pane, 48, 30);
        assert!(pane.branch_button.is_none());
    }
    #[test]
    fn long_file_context_is_visible_and_clicking_it_selects_the_correct_file() {
        let mut data = snapshot("/project");
        let path = PathBuf::from(
            "very/long/目录/with/nested/folders/第二个文件-with-a-much-longer-file-name.rs",
        );
        data.files[1].path = path.clone();
        let mut pane = GitPane {
            snapshot: Some(data),
            ..Default::default()
        };
        let (send, receive) = mpsc::sync_channel(2);
        pane.requests = Some(send);
        render(&mut pane, 48, 30);
        let mapped: Vec<_> = pane.rows.iter().filter(|(_, i)| *i == 1).collect();
        assert!(mapped.len() > 1);
        let row = mapped[1].0 + pane.body.y;
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x + 4,
            row,
            modifiers: KeyModifiers::NONE,
        });
        pane.input(&click);
        assert_eq!(pane.selected, 1);
        pane.input(&click);
        assert!(
            matches!(receive.try_recv().unwrap(), Request::Diff(_, file, false) if file.path == path && file.group == Group::Index)
        );
        let file = &pane.snapshot.as_ref().unwrap().files[1];
        let lines = file_lines(file, true, 45, false);
        let joined = lines
            .iter()
            .skip(1)
            .map(ToString::to_string)
            .map(|s| s.trim_start_matches(['▎', ' ']).to_owned())
            .collect::<String>();
        assert_eq!(joined, service::display_path(&path));
        assert!(lines.iter().all(|line| line.width() <= 45));
        assert!(lines.iter().all(|line| line.style.bg == Some(RAIL)));
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "+2" && span.style.fg == Some(GREEN))
        );
    }
    #[test]
    fn wrapped_file_can_scroll_through_its_continuation_rows() {
        let mut data = snapshot("/project");
        data.files.truncate(1);
        data.files[0].path = format!("{}/file.rs", "nested/目录/".repeat(50)).into();
        let mut pane = GitPane {
            snapshot: Some(data),
            ..Default::default()
        };
        render(&mut pane, 32, 12);
        let before = pane.scroll;
        pane.key(key(KeyCode::PageDown));
        let target = pane.scroll;
        assert!(target > before);
        render(&mut pane, 32, 12);
        assert_eq!(pane.scroll, target);
        assert_eq!(pane.selected, 0);
    }
    #[test]
    fn mouse_selects_file_and_menu_and_can_submit_without_keyboard() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            ..Default::default()
        };
        render(&mut pane, 48, 30);
        let row = pane.rows.iter().find(|(_, i)| *i == 1).unwrap().0 + pane.body.y;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x + 1,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(pane.selected, 1);
        pane.operations();
        render(&mut pane, 48, 30);
        let row = pane.menu_rows.iter().find(|(_, i)| *i == 7).unwrap().0 + pane.body.y;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x + 1,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 29,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(
            pane.modal,
            Some(Modal::Form {
                kind: FormKind::Create,
                ..
            })
        ));
    }
    #[test]
    fn commit_text_keeps_shortcuts_and_requires_review() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            modal: Some(Modal::Form {
                kind: FormKind::Commit {
                    head: None,
                    index: vec![],
                },
                text: String::new(),
            }),
            ..Default::default()
        };
        for c in "git changes".chars() {
            pane.key(key(KeyCode::Char(c)));
        }
        pane.key(key(KeyCode::Enter));
        pane.input(&Event::Paste("Detailed body".into()));
        assert!(
            matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "git changes\nDetailed body")
        );
        pane.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(
            matches!(&pane.modal, Some(Modal::Confirm { action: Action::Commit { message, .. }, .. }) if message == "git changes\nDetailed body")
        );
        assert!(pane.pinned.load(Ordering::Relaxed));
        pane.key(key(KeyCode::Esc));
        assert!(
            matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "git changes\nDetailed body")
        );
        pane.key(key(KeyCode::Esc));
        assert!(!pane.pinned.load(Ordering::Relaxed));
    }
    #[test]
    fn in_flight_and_modal_keep_repository_bound_and_old_diff_is_cleared_on_change() {
        let (send, receive) = mpsc::channel();
        let mut pane = GitPane {
            snapshot: Some(snapshot("/old")),
            updates: Some(receive),
            modal: Some(Modal::Form {
                kind: FormKind::Create,
                text: "new".into(),
            }),
            ..Default::default()
        };
        send.send(Response::Snapshot(Ok(Some(snapshot("/other")))))
            .unwrap();
        pane.poll();
        assert_eq!(pane.root(), Some("/old".into()));
        pane.modal = None;
        pane.busy = true;
        send.send(Response::Snapshot(Ok(Some(snapshot("/other")))))
            .unwrap();
        pane.poll();
        assert_eq!(pane.root(), Some("/old".into()));
        pane.busy = false;
        pane.diff = Some(Diff {
            file: pane.file().unwrap(),
            raw: vec![],
            lines: vec![],
            hunks: vec![],
            fingerprint: vec![],
            reason: None,
        });
        send.send(Response::Snapshot(Ok(Some(snapshot("/other")))))
            .unwrap();
        pane.poll();
        assert_eq!(pane.root(), Some("/other".into()));
        assert!(pane.diff.is_none());
    }
    #[test]
    fn long_forms_messages_and_diff_pan_are_reviewable() {
        let mut pane = GitPane {
            modal: Some(Modal::Message("中文 filename ".repeat(100))),
            ..Default::default()
        };
        render(&mut pane, 24, 10);
        assert!(pane.limit > 0);
        pane.key(key(KeyCode::PageDown));
        assert!(pane.scroll > 0);
        pane.key(key(KeyCode::Esc));
        pane.snapshot = Some(snapshot("/project"));
        pane.diff = Some(Diff {
            file: pane.file().unwrap(),
            raw: vec![],
            lines: vec!["+long diff line".into()],
            hunks: vec![],
            fingerprint: vec![],
            reason: None,
        });
        pane.key(key(KeyCode::Right));
        assert_eq!(pane.horizontal, 0);
        pane.key(key(KeyCode::Char('w')));
        pane.key(key(KeyCode::Right));
        assert_eq!(pane.horizontal, 8);
        pane.key(key(KeyCode::Esc));
        assert!(pane.diff.is_none());
        assert_eq!(pane.horizontal, 0);
    }
    #[test]
    fn branch_search_preserves_local_slashes_and_tracks_remote() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            ..Default::default()
        };
        pane.menu_select(MenuKind::Branches, "feature/local".into(), 0);
        assert!(
            matches!(&pane.modal, Some(Modal::Confirm { action: Action::Switch(name), .. }) if name == "feature/local")
        );
        pane.menu_select(
            MenuKind::Branches,
            "remote: origin/feature/remote".into(),
            0,
        );
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "feature/remote"));
        pane.key(key(KeyCode::Enter));
        assert!(
            matches!(&pane.modal, Some(Modal::Confirm { action: Action::Track { remote, local }, .. }) if remote == "origin/feature/remote" && local == "feature/remote")
        );
    }
    #[test]
    fn file_wheel_and_scrollbar_reach_later_files() {
        let mut data = snapshot("/project");
        data.files = (0..60)
            .map(|i| service::File {
                path: format!("file-{i:02}").into(),
                old_path: None,
                x: b' ',
                y: b'M',
                group: Group::Worktree,
                stat: String::new(),
            })
            .collect();
        let mut pane = GitPane {
            snapshot: Some(data),
            ..Default::default()
        };
        render(&mut pane, 32, 12);
        for _ in 0..12 {
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 2,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }));
            render(&mut pane, 32, 12);
        }
        assert!(pane.selected > 10);
        assert!(pane.scroll > 10);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: pane.body.right(),
            row: pane.body.bottom() - 1,
            modifiers: KeyModifiers::NONE,
        }));
        render(&mut pane, 32, 12);
        assert_eq!(pane.scroll, pane.limit);
    }
    #[test]
    fn git_start_page_survives_configuration_round_trip_and_hot_reload() {
        let preferences = config::UiPreferences {
            pulse_start_page: config::PulseStartPage::Git,
            ..Default::default()
        };
        let encoded = toml::to_string(&preferences).unwrap();
        assert!(encoded.contains("pulse_start_page = \"git\""));
        let decoded = toml::from_str(&encoded).unwrap();
        let mut monitor = Monitor::default();
        monitor.apply_preferences(decoded);
        monitor.apply_start_page();
        assert_eq!(monitor.page, config::PulseStartPage::Git);
        monitor.refresh_preferences(config::UiPreferences {
            pulse_visual: true,
            ..Default::default()
        });
        assert_eq!(monitor.page, config::PulseStartPage::Git);
    }
    fn diff_pane() -> GitPane {
        let snapshot = snapshot("/project");
        let lines = vec![
            "diff --git a/file b/file".into(),
            "index old..new 100644".into(),
            "--- a/file".into(),
            "+++ b/file".into(),
            "@@ -1,2 +1,2 @@".into(),
            "-old".into(),
            format!("+{}FIRST_TAIL", "中文\tlong ".repeat(16)),
            " context".into(),
            "@@ -90,1 +90,1 @@".into(),
            "-other".into(),
            format!("+{}SECOND_TAIL", "新内容 ".repeat(15)),
        ];
        let raw = (lines.join("\n") + "\n").into_bytes();
        let diff = Diff {
            file: snapshot.files[0].clone(),
            raw,
            lines,
            hunks: vec![
                service::Hunk { start: 4, end: 8 },
                service::Hunk { start: 8, end: 11 },
            ],
            fingerprint: vec![],
            reason: None,
        };
        GitPane {
            snapshot: Some(snapshot),
            diff: Some(diff),
            ..Default::default()
        }
    }
    #[test]
    fn wrapped_diff_keeps_unicode_tail_colors_and_sticky_file_navigation() {
        let mut pane = diff_pane();
        let raw = pane.diff.as_ref().unwrap().raw.clone();
        for (width, height) in [(48, 20), (32, 12), (20, 10)] {
            render(&mut pane, width, height);
            let content = pane.diff_content();
            assert!(
                content
                    .iter()
                    .all(|line| line.width() <= usize::from(pane.body.width))
            );
            let reconstructed: String = content
                .iter()
                .zip(&pane.diff_rows)
                .filter(|(_, original_line)| **original_line == 6)
                .flat_map(|(line, _)| line.to_string().chars().skip(7).collect::<Vec<_>>())
                .collect();
            assert_eq!(
                reconstructed,
                safe_text(&pane.diff.as_ref().unwrap().lines[6])
            );
            assert!(content.iter().any(|line| line.to_string().contains('↳')));
            assert!(content.iter().any(
                |line| line.to_string().contains('↳') && line.spans[0].style.fg == Some(GREEN)
            ));
            pane.scroll = pane.limit;
            let text = render(&mut pane, width, height);
            assert!(text.contains("DIFF"));
            // TestBackend includes padding cells after double-width glyphs.
            assert!(text.replace(' ', "").contains("src/中文.rs"));
            if width >= 32 {
                assert!(text.contains("Esc: files"));
            }
            assert_eq!(pane.diff.as_ref().unwrap().raw, raw);
        }
    }
    #[test]
    fn wrapped_continuation_selects_and_stages_its_original_hunk() {
        let mut pane = diff_pane();
        render(&mut pane, 32, 12);
        let target = pane.diff_rows.iter().position(|row| *row == 10).unwrap() + 1;
        pane.scroll = (target as u16).min(pane.limit);
        render(&mut pane, 32, 12);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x + 3,
            row: pane.body.y + target as u16 - pane.scroll,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(pane.hunk, 1);
        let original = pane.diff.as_ref().unwrap().patch(1).unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        pane.requests = Some(send);
        pane.operation(12);
        let Request::Execute(
            _,
            Action::Hunk {
                diff,
                hunk,
                discard,
            },
        ) = receive.try_recv().unwrap()
        else {
            panic!("Expected original hunk staging request");
        };
        assert_eq!(hunk, 1);
        assert!(!discard);
        assert_eq!(diff.patch(hunk).unwrap(), original);
    }
    #[test]
    fn wrap_button_and_hunk_navigation_use_visual_rows() {
        let mut pane = diff_pane();
        render(&mut pane, 48, 20);
        pane.key(key(KeyCode::Char(']')));
        assert_eq!(pane.hunk, 1);
        assert_eq!(pane.scroll, pane.hunk_rows[1].min(pane.limit));
        let anchor = pane.diff_rows[usize::from(pane.scroll)];
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 18,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(pane.nowrap);
        render(&mut pane, 48, 20);
        assert!(pane.diff_rows[usize::from(pane.scroll)] <= anchor);
        pane.key(key(KeyCode::Right));
        assert_eq!(pane.horizontal, 8);
        pane.key(key(KeyCode::Char('w')));
        render(&mut pane, 48, 20);
        assert!(!pane.nowrap);
        assert_eq!(pane.horizontal, 0);
    }
    #[test]
    fn next_file_requests_correct_path_and_group_and_back_retains_selection() {
        let mut pane = diff_pane();
        let (requests, receive) = mpsc::sync_channel(2);
        pane.requests = Some(requests);
        let (updates, responses) = mpsc::channel();
        pane.updates = Some(responses);
        pane.key(key(KeyCode::Char('.')));
        let Request::Diff(root, file, discard) = receive.try_recv().unwrap() else {
            panic!("Expected next file preview request");
        };
        assert_eq!(root, PathBuf::from("/project"));
        assert_eq!(file.path, PathBuf::from("staged.rs"));
        assert_eq!(file.group, Group::Index);
        assert!(!discard);
        let mut diff = pane.diff.clone().unwrap();
        diff.file = file;
        updates.send(Response::Diff(Ok(diff), false)).unwrap();
        pane.poll();
        assert_eq!(pane.selected, 1);
        let text = render(&mut pane, 48, 20);
        assert!(text.contains("File 2/2"));
        assert!(text.contains("u Block"));
        assert!(!text.contains("Discard(d)"));
        let controls = pane.controls(true);
        assert!(controls[0].enabled);
        assert!(!controls[1].enabled);
        pane.key(key(KeyCode::Esc));
        assert!(pane.diff.is_none());
        assert_eq!(pane.selected, 1);
        pane.key(key(KeyCode::Enter));
        let Request::Diff(_, file, _) = receive.try_recv().unwrap() else {
            panic!("Expected selected file preview");
        };
        assert_eq!(file.path, PathBuf::from("staged.rs"));
    }
    #[test]
    fn controls_fit_narrow_panes_and_mouse_uses_the_displayed_action() {
        let mut pane = diff_pane();
        for width in [20, 32, 48] {
            render(&mut pane, width, 20);
            for navigation in [false, true] {
                let controls = pane.controls(navigation);
                for (control, rect) in
                    controls
                        .iter()
                        .zip(control_rects(pane.screen, 0, controls.len()))
                {
                    assert!(control.label(rect.width).width() <= usize::from(rect.width));
                }
            }
        }
        render(&mut pane, 48, 20);
        assert!(!pane.controls(true)[0].enabled);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 42,
            row: 18,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(pane.diff.is_none());
        render(&mut pane, 48, 20);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 42,
            row: 19,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(pane.modal, Some(Modal::Message(_))));
    }
    #[test]
    fn stage_button_and_shortcut_stage_all_from_files_and_diff() {
        for diff in [false, true] {
            let mut pane = diff_pane();
            if !diff {
                pane.diff = None;
            }
            let (send, receive) = mpsc::sync_channel(2);
            pane.requests = Some(send);
            render(&mut pane, 48, 20);
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 14,
                row: 19,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(matches!(
                receive.try_recv().unwrap(),
                Request::Execute(_, Action::StageAll)
            ));
            pane.busy = false;
            pane.key(key(KeyCode::Char('a')));
            assert!(matches!(
                receive.try_recv().unwrap(),
                Request::Execute(_, Action::StageAll)
            ));
        }
    }
    #[test]
    fn status_is_above_both_button_rows() {
        let mut pane = diff_pane();
        pane.notice = "Staged all changes".into();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(48, 20)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let row = |y| {
            (0..48)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(row(17).contains("Staged all changes"));
        assert!(row(18).contains("Wrap"));
        assert!(row(19).contains("All(a)"));
    }
    #[test]
    fn token_tab_returns_home_without_losing_diff_and_git_tab_keeps_context() {
        for (width, height) in [(20, 10), (32, 12), (48, 20), (100, 40)] {
            let mut pane = diff_pane();
            let text = render(&mut pane, width, height);
            let (area, reserve) = pulse_header_area(pane.screen);
            let tabs = page_tab_rects(area, reserve);
            assert_eq!(
                Monitor::default().header_git_rect(area, reserve == 4),
                tabs[1]
            );
            assert!(text.contains("TOKEN"));
            assert!(!text.contains("TOKEN(g)"));
            let click = |column| {
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                })
            };
            assert!(pane.input(&click(tabs[1].x)));
            assert!(pane.diff.is_some());
            assert!(!pane.input(&click(tabs[0].x)));
            assert!(pane.diff.is_some());
            assert!(
                !pane
                    .controls(true)
                    .iter()
                    .any(|c| c.key.code == KeyCode::Char('g'))
            );
            pane.modal = Some(Modal::Message("Details".into()));
            assert!(pane.input(&click(tabs[0].x)));
            assert!(pane.modal.is_some());
        }
    }
    fn click_footer(pane: &mut GitPane, navigation: bool, index: usize) {
        let row = pane.screen.bottom() - if navigation { 2 } else { 1 };
        let rect = control_rects(pane.screen, row, pane.controls(navigation).len())[index];
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }
    #[test]
    fn form_buttons_review_with_the_correct_key_and_commit_back_keeps_draft() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            ..Default::default()
        };
        for kind in [
            FormKind::Create,
            FormKind::Track("origin/topic".into()),
            FormKind::Commit {
                head: None,
                index: vec![],
            },
        ] {
            let commit = matches!(kind, FormKind::Commit { .. });
            pane.modal = Some(Modal::Form {
                kind,
                text: String::new(),
            });
            render(&mut pane, 80, 20);
            assert!(!pane.controls(false)[0].enabled);
            click_footer(&mut pane, false, 0);
            assert!(matches!(pane.modal, Some(Modal::Form { .. })));
            pane.input(&Event::Paste("topic".into()));
            let text = render(&mut pane, 80, 20);
            assert!(text.contains(if commit {
                "Review(Ctrl+Enter)"
            } else {
                "Review(Enter)"
            }));
            assert_eq!(
                pane.controls(false)[0]
                    .key
                    .modifiers
                    .contains(KeyModifiers::CONTROL),
                commit
            );
            click_footer(&mut pane, false, 0);
            assert!(matches!(pane.modal, Some(Modal::Confirm { .. })));
            let text = render(&mut pane, 80, 20);
            assert!(text.contains(if commit { "Edit(Esc)" } else { "Cancel(Esc)" }));
            click_footer(&mut pane, false, 1);
            if commit {
                assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "topic"));
            } else {
                assert!(pane.modal.is_none());
            }
        }
    }
    #[test]
    fn search_clear_button_restores_all_only_after_submit() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            modal: Some(Modal::Form {
                kind: FormKind::Search,
                text: "topic".into(),
            }),
            ..Default::default()
        };
        let (send, receive) = mpsc::sync_channel(2);
        pane.requests = Some(send);
        render(&mut pane, 80, 20);
        click_footer(&mut pane, false, 1);
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text.is_empty()));
        assert!(receive.try_recv().is_err());
        let text = render(&mut pane, 80, 20);
        assert!(text.contains("All commits(Enter)"));
        assert!(!pane.controls(false)[1].enabled);
        click_footer(&mut pane, false, 0);
        assert!(
            matches!(receive.try_recv().unwrap(), Request::History(_, 0, query) if query.is_empty())
        );
    }
    #[test]
    fn details_return_to_their_page_and_footer_never_contains_home() {
        let mut pane = diff_pane();
        for page in 0..3 {
            if page == 1 {
                pane.diff = None;
            }
            if page == 2 {
                pane.history = Some(service::History {
                    commits: vec![],
                    page: 0,
                    has_more: false,
                    query: String::new(),
                });
            }
            render(&mut pane, 80, 20);
            for navigation in [true, false] {
                assert!(
                    !pane
                        .controls(navigation)
                        .iter()
                        .any(|c| c.key.code == KeyCode::Char('g') || c.label.contains("Home"))
                );
            }
            pane.modal = Some(Modal::Message("Details".into()));
            let text = render(&mut pane, 80, 20);
            assert!(text.contains(["Diff(Esc)", "Files(Esc)", "Log(Esc)"][page]));
            assert!(!text.contains("↑↓ select"));
            assert!(pane.controls(true).iter().all(|c| !c.enabled));
            click_footer(&mut pane, false, 0);
            assert!(pane.modal.is_none());
        }
    }
    #[test]
    fn menu_and_scroll_buttons_disable_at_boundaries_and_with_no_matches() {
        let mut pane = GitPane {
            modal: Some(Modal::Menu {
                kind: MenuKind::Branches,
                items: vec!["main".into(), "topic".into()],
                selected: 0,
                filter: String::new(),
            }),
            ..Default::default()
        };
        render(&mut pane, 80, 20);
        assert!(!pane.controls(true)[0].enabled);
        assert!(pane.controls(true)[1].enabled);
        click_footer(&mut pane, true, 1);
        assert!(!pane.controls(true)[1].enabled);
        pane.key(key(KeyCode::Char('z')));
        assert!(pane.controls(true).iter().all(|c| !c.enabled));
        assert!(!pane.controls(false)[0].enabled);
        click_footer(&mut pane, false, 0);
        assert!(pane.modal.is_some());
        pane.key(key(KeyCode::Enter));
        assert!(pane.modal.is_some());
        pane.modal = Some(Modal::Message("Details\n".repeat(60)));
        pane.scroll = 0;
        render(&mut pane, 80, 20);
        assert!(!pane.controls(true)[0].enabled);
        assert!(pane.controls(true)[1].enabled);
        pane.scroll = pane.limit;
        assert!(pane.controls(true)[0].enabled);
        assert!(!pane.controls(true)[1].enabled);
        pane.modal = None;
        assert!(pane.controls(true).iter().all(|c| !c.enabled));
    }
    #[test]
    fn stage_single_file_button_and_key_keep_other_files_out_of_request() {
        for in_diff in [false, true] {
            let mut pane = diff_pane();
            if !in_diff {
                pane.diff = None;
            }
            let (send, receive) = mpsc::sync_channel(2);
            pane.requests = Some(send);
            render(&mut pane, 48, 20);
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 19,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(
                matches!(receive.try_recv().unwrap(), Request::Execute(_, Action::Stage(f)) if f.path == std::path::Path::new("src/中文.rs"))
            );
            pane.busy = false;
            pane.key(key(KeyCode::Char('s')));
            assert!(
                matches!(receive.try_recv().unwrap(), Request::Execute(_, Action::Stage(f)) if f.path == std::path::Path::new("src/中文.rs"))
            );
        }
    }
    #[test]
    fn log_search_applies_after_enter_and_keeps_query_when_paging() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            history: Some(service::History {
                commits: vec![],
                page: 2,
                has_more: false,
                query: String::new(),
            }),
            ..Default::default()
        };
        let (send, receive) = mpsc::sync_channel(8);
        pane.requests = Some(send);
        let (updates, responses) = mpsc::channel();
        pane.updates = Some(responses);
        render(&mut pane, 48, 20);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15,
            row: 19,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(
            pane.modal,
            Some(Modal::Form {
                kind: FormKind::Search,
                ..
            })
        ));
        pane.input(&Event::Paste("git 中文".into()));
        assert!(receive.try_recv().is_err());
        pane.key(key(KeyCode::Enter));
        assert!(
            matches!(receive.try_recv().unwrap(), Request::History(_, 0, query) if query == "git 中文")
        );
        updates
            .send(Response::History(Ok(service::History {
                commits: vec![],
                page: 0,
                has_more: true,
                query: "git 中文".into(),
            })))
            .unwrap();
        pane.poll();
        let text = render(&mut pane, 48, 20);
        assert!(text.contains("Search: git"));
        assert!(text.contains("No matching commits"));
        pane.key(key(KeyCode::Char('.')));
        assert!(
            matches!(receive.try_recv().unwrap(), Request::History(_, 1, query) if query == "git 中文")
        );
        pane.busy = false;
        pane.key(key(KeyCode::Char('/')));
        pane.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        pane.key(key(KeyCode::Esc));
        assert_eq!(pane.history.as_ref().unwrap().query, "git 中文");
        assert!(receive.try_recv().is_err());
        pane.key(key(KeyCode::Char('/')));
        pane.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        pane.key(key(KeyCode::Enter));
        assert!(
            matches!(receive.try_recv().unwrap(), Request::History(_, 0, query) if query.is_empty())
        );
    }
    #[test]
    fn log_pages_details_mouse_and_back_keep_selection() {
        let mut pane = GitPane {
            snapshot: Some(snapshot("/project")),
            ..Default::default()
        };
        let (send, receive) = mpsc::sync_channel(8);
        pane.requests = Some(send);
        let (updates, responses) = mpsc::channel();
        pane.updates = Some(responses);
        pane.key(key(KeyCode::Char('L')));
        assert!(
            matches!(receive.try_recv().unwrap(), Request::History(root, 0, query) if query.is_empty() && root == std::path::Path::new("/project"))
        );
        let commits: Vec<_> = (0..50)
            .map(|i| service::Commit {
                id: format!("{i:040x}"),
                short: format!("{i:07x}"),
                date: "2026-10-09T12:00:00+08:00".into(),
                author: "Author".into(),
                subject: "A long commit subject 中文 with details".into(),
                refs: String::new(),
            })
            .collect();
        updates
            .send(Response::History(Ok(service::History {
                commits,
                page: 0,
                has_more: true,
                query: String::new(),
            })))
            .unwrap();
        pane.poll();
        let text = render(&mut pane, 48, 20);
        assert!(text.contains("GIT LOG"));
        assert!(!pane.controls(true)[0].enabled);
        assert!(pane.controls(true)[1].enabled);
        pane.key(key(KeyCode::End));
        render(&mut pane, 48, 20);
        assert_eq!(pane.history_selected, 49);
        let row = pane.rows.iter().find(|(_, i)| *i == 49).unwrap().0 - pane.scroll + pane.body.y;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            matches!(receive.try_recv().unwrap(), Request::Show(_, id) if id == format!("{:040x}", 49))
        );
        updates
            .send(Response::Show(Ok(
                "commit details\nFull message\n+new content".into(),
            )))
            .unwrap();
        pane.poll();
        assert!(render(&mut pane, 48, 20).contains("Full message"));
        pane.key(key(KeyCode::Esc));
        render(&mut pane, 48, 20);
        assert_eq!(pane.history_selected, 49);
        assert!(pane.scroll > 0);
        pane.key(key(KeyCode::Char('.')));
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::History(_, 1, query) if query.is_empty()
        ));
        updates
            .send(Response::History(Ok(service::History {
                commits: vec![],
                page: 1,
                has_more: false,
                query: String::new(),
            })))
            .unwrap();
        pane.poll();
        assert!(render(&mut pane, 32, 12).contains("No commits on this page"));
        assert!(!pane.controls(true)[1].enabled);
        pane.key(key(KeyCode::Esc));
        assert!(pane.history.is_none());
        assert!(render(&mut pane, 48, 20).contains("UNSTAGED"));
    }
}
