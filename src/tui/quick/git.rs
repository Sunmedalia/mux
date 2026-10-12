//! Side-pane Git UI. The worker owns all subprocesses; the UI only sends requests.
use super::*;
use crate::git::{self as service, Action, Diff, Group, Snapshot};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
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
        description: Box<str>,
    },
    Message(String),
    Help(HelpPage),
    OperationResult {
        recover_commit: bool,
    },
}
#[derive(Clone)]
struct PreviewWatch {
    root: PathBuf,
    file: service::File,
    version: u64,
    revision: Vec<u8>,
    enabled: bool,
}

struct LastOperation {
    root: PathBuf,
    name: &'static str,
    result: std::result::Result<String, String>,
}

struct CommitDraft {
    root: PathBuf,
    message: String,
}

enum Request {
    Refresh,
    Diff(PathBuf, service::File, bool),
    Menu(PathBuf, MenuKind),
    Commit {
        root: PathBuf,
        draft: Option<String>,
    },
    Network(PathBuf, service::NetworkIntent),
    Execute(PathBuf, Action),
    History(PathBuf, usize, String),
    Show(PathBuf, String),
}
enum Response {
    Snapshot(std::result::Result<Option<Snapshot>, String>),
    Diff(std::result::Result<Diff, String>, bool),
    Menu(MenuKind, std::result::Result<Vec<String>, String>),
    Commit {
        root: PathBuf,
        draft: Option<String>,
        result: std::result::Result<(Snapshot, Vec<u8>), String>,
    },
    Network(PathBuf, std::result::Result<Action, String>),
    Completed {
        root: PathBuf,
        name: &'static str,
        is_commit: bool,
        result: std::result::Result<String, String>,
        snapshot: std::result::Result<Snapshot, String>,
        diff: Option<std::result::Result<Option<Diff>, String>>,
    },
    Preview(PathBuf, u64, std::result::Result<Option<Diff>, String>),
    History(std::result::Result<service::History, String>),
    Show(std::result::Result<String, String>),
}
#[derive(Default)]
pub(super) struct GitPane {
    snapshot: Option<Snapshot>,
    file_names: Vec<String>,
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
    help_language: HelpLanguage,
    busy: bool,
    notice: String,
    notice_until: Option<Instant>,
    error: Option<String>,
    last_operation: Option<LastOperation>,
    pending_commit: Option<CommitDraft>,
    refreshed: Option<Instant>,
    requests: Option<mpsc::SyncSender<Request>>,
    updates: Option<mpsc::Receiver<Response>>,
    pinned: Arc<AtomicBool>,
    bound_root: Arc<Mutex<Option<PathBuf>>>,
    manual_pin: bool,
    watch: Arc<Mutex<Option<PreviewWatch>>>,
    preview_version: u64,
    preview_refreshing: Arc<AtomicBool>,
    preview_stale: bool,
    operation: Option<(&'static str, Instant)>,
    clear_after_operation: bool,
    return_scroll: Option<u16>,
    filter: String,
    search_before: Option<String>,
    collapsed: Vec<Group>,
    group_rows: Vec<(u16, Group)>,
    repository_button: Option<Rect>,
    pin_button: Option<Rect>,
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
    fn help_available(&self) -> bool {
        !self.busy
            && self.search_before.is_none()
            && (self.modal.is_none() || matches!(self.modal, Some(Modal::Help(_))))
    }

    pub(super) fn can_switch_workspace(&self) -> bool {
        !self.busy && self.modal.is_none() && self.search_before.is_none()
    }
    pub(super) fn start(&mut self) {
        let (send, requests) = mpsc::sync_channel(8);
        let (updates, receive) = mpsc::channel();
        self.requests = Some(send);
        self.updates = Some(receive);
        let pinned = self.pinned.clone();
        let bound_root = self.bound_root.clone();
        let watch = self.watch.clone();
        let preview_refreshing = self.preview_refreshing.clone();
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
                    let effective_cwd = if pinned.load(Ordering::Relaxed) {
                        bound_root
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(|| cwd.clone())
                    } else {
                        cwd.clone()
                    };
                    let result = if context_known {
                        service::discover(&effective_cwd)
                            .and_then(|root| root.map(|root| service::snapshot(&root)).transpose())
                            .map_err(|e| e.to_string())
                    } else {
                        Ok(None)
                    };
                    if updates.send(Response::Snapshot(result)).is_err() {
                        break;
                    }
                    // Preview refreshes are background work, not user commands.
                    // The UI checks root/version before accepting their results.
                    let target = watch.lock().unwrap().clone();
                    if let Some(target) = target.filter(|target| target.enabled) {
                        let revision = service::preview_revision(&target.root, &target.file);
                        if revision
                            .as_ref()
                            .is_ok_and(|revision| *revision != target.revision)
                        {
                            preview_refreshing.store(true, Ordering::Relaxed);
                            let result = reload_diff(&target.root, &target.file);
                            if let Ok(Some(diff)) = &result {
                                let mut current = watch.lock().unwrap();
                                if let Some(current) = current
                                    .as_mut()
                                    .filter(|current| current.version == target.version)
                                {
                                    current.revision = diff.revision.clone();
                                }
                            }
                            if updates
                                .send(Response::Preview(target.root, target.version, result))
                                .is_err()
                            {
                                preview_refreshing.store(false, Ordering::Relaxed);
                                break;
                            }
                        } else if let Err(error) = revision {
                            let _ = updates.send(Response::Preview(
                                target.root,
                                target.version,
                                Err(error.to_string()),
                            ));
                        }
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
                    Request::Commit { root, draft } => {
                        let result = service::index_stamp(&root)
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
                            .map_err(|e| e.to_string());
                        Response::Commit {
                            root,
                            draft,
                            result,
                        }
                    }
                    Request::Network(root, intent) => {
                        let result =
                            service::prepare_network(&root, intent).map_err(|e| e.to_string());
                        Response::Network(root, result)
                    }
                    Request::Execute(root, action) => {
                        let previous = watch.lock().unwrap().clone();
                        let name = action_name(&action);
                        let is_commit = matches!(action, Action::Commit { .. });
                        let result = service::perform(&root, action).map_err(|e| e.to_string());
                        let snapshot = service::snapshot(&root).map_err(|e| e.to_string());
                        let diff = previous
                            .filter(|previous| previous.root == root)
                            .map(|previous| reload_diff(&root, &previous.file));
                        checked = Instant::now() - Duration::from_secs(3);
                        Response::Completed {
                            root,
                            name,
                            is_commit,
                            result,
                            snapshot,
                            diff,
                        }
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
        let pinned =
            self.manual_pin || self.busy || self.modal.is_some() || self.search_before.is_some();
        *self.bound_root.lock().unwrap() = if pinned { self.root() } else { None };
        self.pinned.store(pinned, Ordering::Relaxed);
    }
    #[expect(
        clippy::result_large_err,
        reason = "Return the unsent request intact without allocating or cloning its Diff"
    )]
    fn request(&mut self, request: Request) -> std::result::Result<(), Request> {
        if self.busy {
            return Err(request);
        }
        let name = match &request {
            Request::Diff(..) => "Loading Diff",
            Request::Menu(..) => "Loading branches / remotes",
            Request::Commit { .. } => "Reviewing staged changes",
            Request::Network(..) => "Preparing network Review",
            Request::Execute(_, action) => action_name(action),
            Request::History(..) => "Loading history",
            Request::Show(..) => "Loading commit",
            Request::Refresh => "Refreshing Git",
        };
        let Some(send) = &self.requests else {
            self.error = Some("Git worker unavailable; reopen Pulse".into());
            return Err(request);
        };
        if let Err(error) = send.try_send(request) {
            self.error = Some(
                match &error {
                    mpsc::TrySendError::Full(_) => "Git worker busy; retry confirmation",
                    mpsc::TrySendError::Disconnected(_) => "Git worker unavailable; reopen Pulse",
                }
                .into(),
            );
            return Err(match error {
                mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request) => {
                    request
                }
            });
        }
        self.busy = true;
        self.operation = Some((name, Instant::now()));
        self.notice_until = None;
        self.notice = "Working…".into();
        self.error = None;
        self.sync_watch();
        self.pin();
        Ok(())
    }
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        if self
            .notice_until
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.notice_until = None;
            self.notice.clear();
            changed = true;
        }
        while let Some(response) = self.updates.as_ref().and_then(|u| u.try_recv().ok()) {
            changed = true;
            match response {
                Response::Snapshot(Ok(snapshot)) => {
                    if !self.busy && self.modal.is_none() {
                        self.apply_snapshot(snapshot);
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
                            let preserve = self.diff.as_ref().is_some_and(|previous| {
                                previous.file.path == diff.file.path
                                    && previous.file.group == diff.file.group
                            });
                            self.install_diff(diff, preserve);
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
                Response::Commit {
                    root,
                    draft,
                    result,
                } => {
                    if self.root().as_ref() != Some(&root) {
                        continue;
                    }
                    self.busy = false;
                    match result {
                        Ok((snapshot, index)) => {
                            let head = snapshot.head.clone();
                            self.apply_snapshot(Some(snapshot));
                            self.modal = Some(Modal::Form {
                                kind: FormKind::Commit { head, index },
                                text: draft.unwrap_or_default(),
                            })
                        }
                        Err(error) => self.report_error(error),
                    }
                }
                Response::Network(root, result) => {
                    if self.root().as_ref() == Some(&root) {
                        self.busy = false;
                        match result {
                            Ok(action) => {
                                let (verb, target) = match &action {
                                    Action::Pull(target) => ("Pull", target),
                                    Action::Push(target) => ("Push", target),
                                    _ => unreachable!("Network preparation returns Pull/Push"),
                                };
                                let description = format!(
                                    "{verb} {}\nHEAD {}\nRemote {}\nTarget {}{}",
                                    target.branch_ref,
                                    &target.head[..target.head.len().min(12)],
                                    target.remote,
                                    target.target_ref,
                                    if target.set_upstream {
                                        "\nSet upstream after push"
                                    } else {
                                        ""
                                    }
                                );
                                self.confirm(action, description);
                            }
                            Err(error) => self.report_error(error),
                        }
                    }
                }
                Response::Completed {
                    root,
                    name,
                    is_commit,
                    result,
                    snapshot,
                    diff,
                } => {
                    if self.root().as_ref() == Some(&root) {
                        self.finish_operation(
                            LastOperation { root, name, result },
                            is_commit,
                            Some(snapshot),
                            diff,
                        );
                    }
                }
                Response::Preview(root, version, result) => {
                    self.preview_refreshing.store(false, Ordering::Relaxed);
                    if self.root().as_ref() == Some(&root)
                        && version == self.preview_version
                        && !self.busy
                        && self.modal.is_none()
                        && self.diff.is_some()
                    {
                        match result {
                            Ok(Some(diff)) => {
                                if self.diff.as_ref().is_some_and(|old| {
                                    old.fingerprint != diff.fingerprint || old.file != diff.file
                                }) {
                                    self.install_diff(diff, true);
                                    self.notice = "Diff updated after external changes".into();
                                    self.notice_until =
                                        Some(Instant::now() + Duration::from_secs(5));
                                } else if let Some(old) = &mut self.diff {
                                    old.revision = diff.revision;
                                    self.preview_stale = false;
                                    self.error = None;
                                }
                            }
                            Ok(None) => self.close_missing_diff(),
                            Err(error) => {
                                self.preview_stale = true;
                                self.error = Some(format!("Diff refresh failed · {error}"));
                            }
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
                            self.return_scroll = None;
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
            if !self.busy {
                self.operation = None;
            }
            self.ensure_selection();
            self.sync_watch();
            self.pin();
        }
        changed
    }
    fn apply_snapshot(&mut self, snapshot: Option<Snapshot>) {
        let root_changed =
            self.snapshot.as_ref().map(|s| &s.root) != snapshot.as_ref().map(|s| &s.root);
        if root_changed && self.manual_pin {
            return;
        }
        let branch_changed =
            self.snapshot.as_ref().map(|s| &s.branch) != snapshot.as_ref().map(|s| &s.branch);
        let selected = self
            .snapshot
            .as_ref()
            .and_then(|s| s.files.get(self.selected))
            .cloned();
        if root_changed || branch_changed {
            self.diff = None;
            self.history = None;
            self.preview_stale = false;
            self.diff_anchor = None;
            self.scroll = 0;
            self.notice.clear();
        }
        if root_changed {
            self.selected = 0;
            self.filter.clear();
            self.search_before = None;
            self.collapsed.clear();
        } else if let (Some(file), Some(snapshot)) = (&selected, &snapshot) {
            self.selected = snapshot
                .files
                .iter()
                .position(|f| f.path == file.path && f.group == file.group)
                .or_else(|| snapshot.files.iter().position(|f| f.path == file.path))
                .unwrap_or(self.selected);
        }
        let paths_changed = match (&self.snapshot, &snapshot) {
            (Some(old), Some(new)) => !old
                .files
                .iter()
                .map(|file| &file.path)
                .eq(new.files.iter().map(|file| &file.path)),
            (None, None) => false,
            _ => true,
        };
        if paths_changed {
            self.file_names = snapshot
                .as_ref()
                .map_or_else(Vec::new, |snapshot| file_display_names(&snapshot.files));
        }
        self.snapshot = snapshot;
        self.ensure_selection();
        if !self.preview_stale {
            self.error = None;
        }
        self.refreshed = Some(Instant::now());
    }
    fn sync_watch(&mut self) {
        let target = self.root().zip(self.diff.as_ref());
        let mut watch = self.watch.lock().unwrap();
        if let Some((root, diff)) = target {
            let same = watch.as_ref().is_some_and(|watch| {
                watch.root == root
                    && watch.file.path == diff.file.path
                    && watch.file.group == diff.file.group
            });
            if !same {
                self.preview_version = self.preview_version.wrapping_add(1);
            }
            *watch = Some(PreviewWatch {
                root,
                file: diff.file.clone(),
                version: self.preview_version,
                revision: diff.revision.clone(),
                enabled: !self.busy && self.modal.is_none(),
            });
        } else if watch.take().is_some() {
            self.preview_version = self.preview_version.wrapping_add(1);
        }
    }
    fn install_diff(&mut self, diff: Diff, preserve: bool) {
        if preserve && let Some(previous) = &self.diff {
            let line = self
                .diff_rows
                .get(usize::from(self.scroll))
                .copied()
                .unwrap_or(0);
            self.diff_anchor = Some(remap_line(&previous.lines, &diff.lines, line));
            self.hunk = remap_hunk(previous, &diff, self.hunk);
        } else {
            self.diff_anchor = None;
            self.hunk = 0;
            self.scroll = 0;
            self.horizontal = 0;
        }
        self.preview_version = self.preview_version.wrapping_add(1);
        self.diff = Some(diff);
        self.preview_stale = false;
        self.error = None;
    }
    fn close_missing_diff(&mut self) {
        self.diff = None;
        self.diff_anchor = None;
        self.preview_stale = false;
        self.scroll = 0;
        self.horizontal = 0;
        self.notice = "File no longer has changes in this group · returned to Files".into();
    }
    fn finish_operation(
        &mut self,
        operation: LastOperation,
        is_commit: bool,
        snapshot: Option<std::result::Result<Snapshot, String>>,
        diff: Option<std::result::Result<Option<Diff>, String>>,
    ) {
        self.busy = false;
        let success = operation.result.is_ok();
        let summary = clipped(
            match &operation.result {
                Ok(message) => message,
                Err(error) => error,
            }
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Git operation completed"),
            160,
        );
        self.last_operation = Some(operation);
        if success {
            if is_commit {
                self.pending_commit = None;
            }
            let mut refresh_error = None;
            if let Some(snapshot) = snapshot {
                match snapshot {
                    Ok(snapshot) => self.apply_snapshot(Some(snapshot)),
                    Err(error) => {
                        refresh_error =
                            Some(format!("Operation finished; refresh failed · {error}"));
                    }
                }
            }
            if self.clear_after_operation {
                self.diff = None;
                self.diff_anchor = None;
                self.scroll = 0;
            } else if self.diff.is_some()
                && let Some(diff) = diff
            {
                match diff {
                    Ok(Some(diff)) => {
                        self.install_diff(diff, true);
                        // The original block may have disappeared after staging.
                        self.diff_anchor = self
                            .diff
                            .as_ref()
                            .and_then(|d| d.hunks.get(self.hunk))
                            .map(|h| h.start);
                    }
                    Ok(None) => self.close_missing_diff(),
                    Err(error) => {
                        self.preview_stale = true;
                        self.error = Some(format!("Diff refresh failed · {error}"));
                    }
                }
            }
            if let Some(error) = refresh_error {
                self.preview_stale = self.diff.is_some();
                self.error = Some(error);
            }
            self.notice = summary;
        } else {
            self.return_scroll = Some(self.scroll);
            self.notice.clear();
            self.modal = Some(Modal::OperationResult {
                recover_commit: is_commit,
            });
            self.scroll = 0;
        }
        self.clear_after_operation = false;
    }
    fn diff_pending(&self) -> bool {
        self.preview_stale || self.preview_refreshing.load(Ordering::Relaxed)
    }
    fn visible_files(&self) -> Vec<usize> {
        let query = self.filter.to_lowercase();
        self.snapshot.as_ref().map_or(Vec::new(), |snapshot| {
            snapshot
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| {
                    (query.is_empty() && !self.collapsed.contains(&file.group))
                        || (!query.is_empty() && file.label().to_lowercase().contains(&query))
                })
                .map(|(index, _)| index)
                .collect()
        })
    }
    fn ensure_selection(&mut self) {
        if self.history.is_some() || self.diff.is_some() {
            return;
        }
        let visible = self.visible_files();
        if !visible.contains(&self.selected) {
            self.selected = visible
                .iter()
                .copied()
                .find(|index| *index >= self.selected)
                .or_else(|| visible.last().copied())
                .unwrap_or(0);
        }
    }
    fn move_selection(&mut self, next: bool) {
        let visible = self.visible_files();
        if let Some(position) = visible.iter().position(|index| *index == self.selected) {
            let target = if next {
                (position + 1).min(visible.len() - 1)
            } else {
                position.saturating_sub(1)
            };
            self.selected = visible[target];
        }
    }
    fn repository_details(&mut self) {
        self.remember_scroll();
        if let Some(snapshot) = &self.snapshot {
            let fetched = snapshot.fetched.map_or("unknown".into(), fetch_age);
            self.modal = Some(Modal::Message(format!(
                "REPOSITORY\n{}\nBranch: {}\nUpstream: {}\nLocal refs: ↑{} ↓{}\nLocal fetch record: {}\nAhead/behind use local remote-tracking refs.\nMode: {}\np toggles Follow / Pinned from Files.",
                service::display_path(&snapshot.root),
                safe_text(&snapshot.branch),
                snapshot.upstream.as_deref().unwrap_or("none"),
                snapshot.ahead,
                snapshot.behind,
                fetched,
                if self.manual_pin {
                    "Pinned"
                } else {
                    "Follow terminal"
                }
            )));
            self.scroll = 0;
        }
    }
    fn remember_scroll(&mut self) {
        if self.return_scroll.is_none()
            && self.modal.is_none()
            && (self.diff.is_some() || self.history.is_some())
        {
            self.return_scroll = Some(self.scroll);
        }
    }
    fn report_error(&mut self, error: String) {
        if self.return_scroll.is_none() && self.diff.is_some() && self.modal.is_none() {
            self.return_scroll = Some(self.scroll);
        }
        self.error = Some(error.clone());
        self.modal = Some(Modal::Message(error));
        self.scroll = 0;
    }
    fn last_failure(&self) -> Option<&str> {
        self.last_operation
            .as_ref()
            .filter(|operation| {
                self.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.root == operation.root)
            })
            .and_then(|operation| operation.result.as_ref().err())
            .and_then(|error| error.lines().find(|line| !line.trim().is_empty()))
    }
    fn open_operation_result(&mut self) {
        self.remember_scroll();
        self.scroll = 0;
        self.modal = Some(if self.last_operation.is_some() {
            Modal::OperationResult {
                recover_commit: false,
            }
        } else {
            Modal::Message(
                self.error
                    .clone()
                    .unwrap_or_else(|| "No operation result yet".into()),
            )
        });
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
        if !self.visible_files().contains(&self.selected) {
            return None;
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
            let _ = self.request(Request::Diff(root, file, false));
        }
    }
    #[expect(
        clippy::result_large_err,
        reason = "Preserve action ownership for failed enqueue without allocating or cloning"
    )]
    fn execute(&mut self, action: Action) -> std::result::Result<(), Action> {
        if self.diff_pending() && matches!(action, Action::Hunk { .. } | Action::Discard(_)) {
            self.error =
                Some("Diff is updating; refresh before changing a block or discarding".into());
            return Err(action);
        }
        let Some(root) = self.root() else {
            return Err(action);
        };
        let draft = if let Action::Commit { message, .. } = &action {
            Some(CommitDraft {
                root: root.clone(),
                message: message.clone(),
            })
        } else {
            None
        };
        let clear = matches!(
            action,
            Action::StageAll
                | Action::UnstageAll
                | Action::Switch(_)
                | Action::Create(_)
                | Action::Track { .. }
                | Action::Commit { .. }
        );
        if let Err(Request::Execute(_, action)) = self.request(Request::Execute(root, action)) {
            return Err(action);
        }
        if let Some(draft) = draft {
            self.pending_commit = Some(draft);
        }
        if let Some(scroll) = self.return_scroll.take() {
            self.scroll = scroll;
        }
        self.clear_after_operation = clear;
        self.modal = None;
        self.sync_watch();
        Ok(())
    }
    fn open_commit(&mut self) {
        if let Some(root) = self.root() {
            let draft = self
                .pending_commit
                .as_ref()
                .filter(|draft| draft.root == root)
                .map(|draft| draft.message.clone());
            let _ = self.request(Request::Commit { root, draft });
        }
    }
    fn confirm(&mut self, action: Action, description: String) {
        self.remember_scroll();
        let description = if let Some(snapshot) = &self.snapshot {
            let branch = match &action {
                Action::Pull(target) | Action::Push(target) => target.branch_ref.as_str(),
                _ => snapshot.branch.as_str(),
            };
            format!(
                "{}\nBranch: {branch}\n\n{description}",
                service::display_path(&snapshot.root)
            )
        } else {
            description
        };
        self.modal = Some(Modal::Confirm {
            action,
            description: description.into_boxed_str(),
        });
        self.scroll = 0;
        self.pin();
    }
    fn confirm_discard(&mut self, diff: Diff, hunk: bool) {
        if self.diff_pending() {
            self.error = Some("Diff is updating; refresh before discarding".into());
            return;
        }
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
        self.remember_scroll();
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
                    let _ = self.execute(Action::Stage(file));
                }
            }
            1 => {
                if let Some(file) = self.file() {
                    let _ = self.execute(Action::Unstage(file));
                }
            }
            2 => {
                if let (Some(root), Some(file)) = (self.root(), self.file()) {
                    let _ = self.request(Request::Diff(root, file, true));
                }
            }
            3 => {
                let _ = self.execute(Action::StageAll);
            }
            4 => {
                let _ = self.execute(Action::UnstageAll);
            }
            5 => self.open_commit(),
            6 => {
                if let Some(root) = self.root() {
                    let _ = self.request(Request::Menu(root, MenuKind::Branches));
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
                    let _ = self.request(Request::Menu(root, MenuKind::Fetch));
                }
            }
            9 => {
                if let Some(root) = self.root() {
                    let _ = self.request(Request::Network(root, service::NetworkIntent::Pull));
                }
            }
            10 => {
                if self.snapshot.as_ref().is_some_and(|s| s.upstream.is_some()) {
                    if let Some(root) = self.root() {
                        let _ = self.request(Request::Network(
                            root,
                            service::NetworkIntent::Push { remote: None },
                        ));
                    }
                } else if let Some(root) = self.root() {
                    let _ = self.request(Request::Menu(root, MenuKind::Push));
                }
            }
            11 => self.open_history(0),
            12 => {
                if let Some(diff) = self.diff.clone()
                    && diff.file.group == Group::Worktree
                    && diff.reason.is_none()
                    && !diff.hunks.is_empty()
                {
                    let _ = self.execute(Action::Hunk {
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
            MenuKind::Fetch => {
                let _ = self.execute(Action::Fetch(item));
            }
            MenuKind::Push => {
                if let Some(root) = self.root() {
                    let _ = self.request(Request::Network(
                        root,
                        service::NetworkIntent::Push { remote: Some(item) },
                    ));
                }
            }
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
            let _ = self.request(Request::History(root, page, query));
        }
    }
    fn search_history(&mut self) {
        self.remember_scroll();
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
            KeyCode::Char('g') if key.modifiers == KeyModifiers::NONE => return false,
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
                    let _ = self.request(Request::Show(root, commit.id.clone()));
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
            KeyCode::Char('l') => self.open_operation_result(),
            KeyCode::Char('b') => self.operation(6),
            KeyCode::Char('?') => {
                self.remember_scroll();
                self.scroll = 0;
                self.modal = Some(Modal::Help(HelpPage::Log));
            }
            _ => {}
        }
        self.pin();
        true
    }
    fn scroll_to(&mut self, target: u16) {
        self.scroll = target.min(self.limit);
        let visible_end = self.scroll.saturating_add(self.body.height.max(1));
        if !self.busy && self.modal.is_none() && self.diff.is_none() {
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
    fn key(&mut self, mut key: KeyEvent) -> bool {
        let typing = self.search_before.is_some()
            || matches!(
                self.modal,
                Some(Modal::Form { .. }) | Some(Modal::Menu { .. })
            );
        if key.code == KeyCode::Char('q') && !typing && key.modifiers == KeyModifiers::NONE {
            key.code = KeyCode::Esc;
        }

        if self.busy {
            self.reading_key(key.code);
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
                Modal::OperationResult { recover_commit } => match key.code {
                    KeyCode::Esc | KeyCode::Enter => {
                        if let Some(scroll) = self.return_scroll.take() {
                            self.scroll = scroll;
                        }
                        if recover_commit {
                            self.open_commit();
                        }
                    }
                    KeyCode::Down => {
                        self.scroll = (self.scroll + 1).min(self.limit);
                        self.modal = Some(Modal::OperationResult { recover_commit });
                    }
                    KeyCode::Up => {
                        self.scroll = self.scroll.saturating_sub(1);
                        self.modal = Some(Modal::OperationResult { recover_commit });
                    }
                    _ => self.modal = Some(Modal::OperationResult { recover_commit }),
                },
                Modal::Help(page) => match key.code {
                    KeyCode::Char('l') => {
                        self.help_language.toggle();
                        self.scroll = 0;
                        self.modal = Some(Modal::Help(page));
                    }
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => {
                        if let Some(scroll) = self.return_scroll.take() {
                            self.scroll = scroll;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.scroll = self.scroll.saturating_add(1).min(self.limit);
                        self.modal = Some(Modal::Help(page));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.scroll = self.scroll.saturating_sub(1);
                        self.modal = Some(Modal::Help(page));
                    }
                    KeyCode::Home => {
                        self.scroll = 0;
                        self.modal = Some(Modal::Help(page));
                    }
                    KeyCode::End => {
                        self.scroll = self.limit;
                        self.modal = Some(Modal::Help(page));
                    }
                    _ => self.modal = Some(Modal::Help(page)),
                },
                Modal::Message(message) => match key.code {
                    KeyCode::Esc | KeyCode::Enter => {
                        if let Some(scroll) = self.return_scroll.take() {
                            self.scroll = scroll;
                        }
                    }
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
                    KeyCode::Enter | KeyCode::Char('y') => {
                        if let Err(action) = self.execute(action) {
                            self.modal = Some(Modal::Confirm {
                                action,
                                description,
                            });
                        }
                    }
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
                        if matches!(kind, FormKind::Commit { .. })
                            && self.pending_commit.as_ref().is_some_and(|draft| {
                                Some(&draft.root)
                                    == self.snapshot.as_ref().map(|snapshot| &snapshot.root)
                            })
                        {
                            self.pending_commit = None;
                        }
                    } else if submit && matches!(kind, FormKind::Search) {
                        if let Some(root) = self.root() {
                            let _ = self.request(Request::History(root, 0, text.trim().into()));
                        }
                    } else if submit {
                        if text.lines().next().unwrap_or("").trim().is_empty() {
                            self.error = Some("A title/name is required".into());
                            self.modal = Some(Modal::Form { kind, text });
                        } else {
                            let description = format!(
                                "Confirm {}?\n{text}",
                                if matches!(kind, FormKind::Commit { .. }) {
                                    "commit"
                                } else {
                                    "operation"
                                }
                            );
                            let action = match kind {
                                FormKind::Commit { head, index } => Action::Commit {
                                    message: text,
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
                            self.confirm(action, description);
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
            if self.modal.is_none()
                && !self.busy
                && let Some(scroll) = self.return_scroll.take()
            {
                self.scroll = scroll;
            }
            self.sync_watch();
            self.pin();
            return true;
        }
        if self.search_before.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.search_before = None;
                }
                KeyCode::Esc => {
                    self.filter = self.search_before.take().unwrap();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.clear()
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.filter.push(c)
                }
                _ => {}
            }
            self.scroll = 0;
            self.ensure_selection();
            self.pin();
            return true;
        }
        if self.history.is_some() {
            let handled = self.history_key(key);
            self.sync_watch();
            self.pin();
            return handled;
        }
        match key.code {
            KeyCode::Char('T') => return false,
            KeyCode::Char('g') if key.modifiers == KeyModifiers::NONE => return false,
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
                self.remember_scroll();
                self.scroll = 0;
                self.modal = Some(Modal::Help(HelpPage::Git));
            }
            KeyCode::Char('l') => self.open_operation_result(),
            KeyCode::Char('r') => {
                if let Some(diff) = &self.diff {
                    if let Some(root) = self.root() {
                        let _ = self.request(Request::Diff(root, diff.file.clone(), false));
                    }
                } else if let Some(send) = &self.requests {
                    let _ = send.try_send(Request::Refresh);
                }
            }
            KeyCode::Char('/') if self.diff.is_none() && self.snapshot.is_some() => {
                self.search_before = Some(self.filter.clone());
            }
            KeyCode::Char('p') if self.snapshot.is_some() => {
                self.manual_pin = !self.manual_pin;
                self.notice = if self.manual_pin {
                    "Pinned to current repository"
                } else {
                    "Following project terminal"
                }
                .into();
                if !self.manual_pin
                    && let Some(send) = &self.requests
                {
                    let _ = send.try_send(Request::Refresh);
                }
            }
            KeyCode::Char('i') => self.repository_details(),
            KeyCode::Char('h') => self.operation(12),
            KeyCode::Char('n') => self.operation(5),
            KeyCode::Char('b') => self.operation(6),
            KeyCode::Char('A' | 'a') => {
                let _ = self.execute(Action::StageAll);
            }
            KeyCode::Char('s') => {
                if self.file().is_some_and(|f| f.group != Group::Index) {
                    self.operation(0);
                } else {
                    self.error = Some("Select an unstaged file".into());
                }
            }
            KeyCode::Char('L') => self.open_history(0),
            KeyCode::Char('U') => {
                let _ = self.execute(Action::UnstageAll);
            }
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
                            let _ = self.execute(Action::Hunk {
                                diff,
                                hunk: self.hunk,
                                discard: false,
                            });
                        } else {
                            let _ = self.execute(if index {
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
                    let _ = self.request(Request::Diff(root, file, false));
                }
            }
            KeyCode::Esc => {
                if self.diff.take().is_some() {
                    self.preview_stale = false;
                    self.diff_anchor = None;
                    self.scroll = 0;
                    self.horizontal = 0;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.ensure_selection();
                    self.scroll = 0;
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
                    self.move_selection(false);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.diff.is_some() {
                    self.scroll = (self.scroll + 1).min(self.limit);
                } else {
                    self.move_selection(true);
                }
            }
            KeyCode::PageDown => {
                self.scroll_to(self.scroll.saturating_add(self.body.height.max(1)))
            }
            KeyCode::PageUp => self.scroll_to(self.scroll.saturating_sub(self.body.height.max(1))),
            KeyCode::Home => {
                self.scroll = 0;
                if self.diff.is_none() {
                    self.selected = self.visible_files().first().copied().unwrap_or(0);
                }
            }
            KeyCode::End => {
                self.scroll = self.limit;
                if self.diff.is_none() {
                    self.selected = self.visible_files().last().copied().unwrap_or(0);
                }
            }
            _ => {}
        }
        self.ensure_selection();
        self.sync_watch();
        self.pin();
        true
    }
    fn reading_key(&mut self, code: KeyCode) {
        let page = self.body.height.max(1);
        self.scroll = match code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll.saturating_add(1).min(self.limit),
            KeyCode::PageUp => self.scroll.saturating_sub(page),
            KeyCode::PageDown => self.scroll.saturating_add(page).min(self.limit),
            KeyCode::Home => 0,
            KeyCode::End => self.limit,
            _ => self.scroll,
        };
        if self.nowrap && self.diff.is_some() {
            if code == KeyCode::Left {
                self.horizontal = self.horizontal.saturating_sub(8);
            }
            if code == KeyCode::Right {
                self.horizontal = self.horizontal.saturating_add(8);
            }
        }
    }
    pub(super) fn input(&mut self, event: &Event) -> bool {
        match event {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => self.key(*key),
            Event::Paste(_) if self.busy => true,
            Event::Paste(text) => {
                if self.search_before.is_some() {
                    self.filter.push_str(&text.replace(['\r', '\n'], ""));
                    self.ensure_selection();
                    self.scroll = 0;
                } else if let Some(Modal::Form { text: field, .. }) = &mut self.modal {
                    field.push_str(text);
                    self.scroll = u16::MAX;
                }
                true
            }
            Event::Mouse(mouse) => {
                if self.busy {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => {
                            self.scroll = self.scroll.saturating_add(3).min(self.limit)
                        }
                        MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                        MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Drag(MouseButton::Left) => {
                            if let Some(target) = scrollbar_target(
                                self.body,
                                self.body.right(),
                                mouse.column,
                                mouse.row,
                                self.limit,
                            ) {
                                self.scroll = target.min(self.limit);
                            } else {
                                let controls = self.controls(true);
                                if mouse.row == self.screen.bottom().saturating_sub(2)
                                    && let Some(index) =
                                        control_rects(self.screen, mouse.row, &controls)
                                            .iter()
                                            .position(|rect| {
                                                contains(*rect, mouse.column, mouse.row)
                                            })
                                    && let Some(control) =
                                        controls.get(index).filter(|control| control.enabled)
                                {
                                    self.reading_key(control.key.code);
                                }
                            }
                        }
                        _ => {}
                    }
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
                    if self.help_available()
                        && area.width >= 10
                        && contains(pulse_help_rect(area), mouse.column, mouse.row)
                    {
                        return self.key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
                    }
                    let button = page_header_rects(area, reserve, true)[1];
                    if self.modal.is_none()
                        && self.search_before.is_none()
                        && contains(button, mouse.column, mouse.row)
                    {
                        return false;
                    }
                    return true;
                }
                if self.modal.is_none() && self.search_before.is_none() {
                    if self
                        .repository_button
                        .is_some_and(|rect| contains(rect, mouse.column, mouse.row))
                    {
                        return self.key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
                    }
                    if self
                        .pin_button
                        .is_some_and(|rect| contains(rect, mouse.column, mouse.row))
                    {
                        return self.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
                    }
                }
                if self.modal.is_none()
                    && self.search_before.is_none()
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
                    if let Some(index) = control_rects(self.screen, row, &controls)
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
                if self.modal.is_none()
                    && self.diff.is_none()
                    && self.history.is_none()
                    && self.filter.is_empty()
                    && self.search_before.is_none()
                    && let Some((_, group)) =
                        self.group_rows.iter().find(|(heading, _)| *heading == row)
                {
                    let group = *group;
                    if self.collapsed.contains(&group) {
                        self.collapsed.retain(|current| *current != group);
                    } else {
                        self.collapsed.push(group);
                    }
                    self.ensure_selection();
                    return true;
                }
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
                } else if self.search_before.is_none()
                    && let Some((_, index)) = self.rows.iter().find(|(r, _)| *r == row)
                {
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
        self.group_rows.clear();
        if let Some(modal) = &self.modal {
            return match modal {
                Modal::OperationResult { .. } => {
                    let Some(operation) = &self.last_operation else {
                        return vec![line("No operation result yet", SOFT)];
                    };
                    let (body, color) = match &operation.result {
                        Ok(body) => (body, GREEN),
                        Err(body) => (body, RED),
                    };
                    let mut lines = vec![
                        line(
                            format!(
                                "GIT · {} · {}",
                                operation.name,
                                if operation.result.is_ok() {
                                    "SUCCESS"
                                } else {
                                    "FAILED"
                                }
                            ),
                            color,
                        ),
                        line(service::display_path(&operation.root), SOFT),
                        Line::default(),
                    ];
                    lines.extend(body.lines().map(|text| line(safe_text(text), INK)));
                    lines
                }
                Modal::Help(page) => help_content(*page, self.help_language, self.body.width),
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
        if (self.screen.width < 32 || self.screen.height < 16)
            && (!self.filter.is_empty() || self.search_before.is_some())
        {
            out.push(line(
                clipped(
                    &format!(
                        "/ {}{}",
                        safe_text(&self.filter),
                        if self.search_before.is_some() {
                            "▏"
                        } else {
                            ""
                        }
                    ),
                    usize::from(self.body.width),
                ),
                GOLD,
            ));
        }
        let visible = self.visible_files();
        if !self.filter.is_empty() && visible.is_empty() {
            out.push(line("No matching files · /: search · Esc: clear", SOFT));
        }
        for group in [Group::Conflict, Group::Worktree, Group::Index] {
            let count = snapshot
                .files
                .iter()
                .filter(|file| file.group == group)
                .count();
            let matching: Vec<_> = visible
                .iter()
                .copied()
                .filter(|index| snapshot.files[*index].group == group)
                .collect();
            if count == 0 || (!self.filter.is_empty() && matching.is_empty()) {
                continue;
            }
            if !out.is_empty() {
                out.push(Line::default());
            }
            self.group_rows
                .push((out.len().min(u16::MAX as usize) as u16, group));
            let folded = self.filter.is_empty() && self.collapsed.contains(&group);
            let label = match group {
                Group::Conflict => "CONFLICTS",
                Group::Worktree => "UNSTAGED",
                Group::Index => "STAGED",
            };
            let count_label = if self.filter.is_empty() {
                count.to_string()
            } else {
                format!("{}/{count}", matching.len())
            };
            let mut heading = section(
                &format!("{} {label}  {count_label}", if folded { "▸" } else { "▾" }),
                self.body.width,
            );
            heading.spans[0].style = Style::default()
                .fg(group_color(group))
                .add_modifier(Modifier::BOLD);
            out.push(heading);
            for index in matching {
                for row in file_lines(
                    &snapshot.files[index],
                    &self.file_names[index],
                    self.selected == index,
                    self.body.width,
                ) {
                    self.rows
                        .push((out.len().min(u16::MAX as usize) as u16, index));
                    out.push(row);
                }
            }
        }
        out
    }
    fn controls(&self, navigation: bool) -> Vec<Control> {
        let control = |label, compact, code| Control::new(label, compact, code);
        if self.busy && navigation {
            let mut up = control("Page up", "PgUp", KeyCode::PageUp);
            up.enabled = self.scroll > 0;
            let mut down = control("Page down", "PgDn", KeyCode::PageDown);
            down.enabled = self.scroll < self.limit;
            return vec![up, down];
        }
        if self.search_before.is_some() {
            if navigation {
                return vec![];
            }
            let mut clear = control("Clear(Ctrl+U)", "^U Clear", KeyCode::Char('u'));
            clear.key.modifiers = KeyModifiers::CONTROL;
            clear.enabled = !self.filter.is_empty();
            return vec![
                control("Apply(Enter)", "↵ Apply", KeyCode::Enter),
                clear,
                control("Cancel(Esc)", "Esc Cancel", KeyCode::Esc),
            ];
        }
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
                let help = matches!(self.modal, Some(Modal::Help(_)));
                let mut previous = control(
                    if help {
                        self.help_language.text("上一页", "Page up")
                    } else {
                        "Page up"
                    },
                    "PgUp",
                    KeyCode::PageUp,
                );
                previous.enabled = self.scroll > 0;
                let mut next = control(
                    if help {
                        self.help_language.text("下一页", "Page down")
                    } else {
                        "Page down"
                    },
                    "PgDn",
                    KeyCode::PageDown,
                );
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
                Some(Modal::Help(_)) => vec![
                    control(
                        self.help_language.switch_label(),
                        self.help_language.text("English", "中文"),
                        KeyCode::Char('l'),
                    ),
                    control(
                        self.help_language.text("返回(Esc)", "Back(Esc)"),
                        self.help_language.text("Esc 返回", "Esc Back"),
                        KeyCode::Esc,
                    ),
                ],
                Some(Modal::Message(_) | Modal::OperationResult { .. }) => vec![control(
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
                        if index { "Unstage" } else { "Stage(s)" },
                        KeyCode::Char(if index { 'u' } else { 's' }),
                    );
                    single.enabled = file.is_some() && !(index && block && self.diff_pending());
                    single.minimum_width = if index { 7 } else { 0 };
                    let mut stage = if self.diff.is_some() {
                        if block
                            && !index
                            && file.as_ref().is_some_and(|f| f.group == Group::Worktree)
                        {
                            let mut stage =
                                control("Stage block(h)", "Block(h)", KeyCode::Char('h'));
                            stage.enabled = !self.diff_pending();
                            stage
                        } else if file
                            .as_ref()
                            .is_some_and(|file| file.group == Group::Worktree)
                        {
                            control("Refresh(r)", "r Refresh", KeyCode::Char('r'))
                        } else {
                            control("Repository(i)", "i Repo", KeyCode::Char('i'))
                        }
                    } else {
                        let mut search = control("Search(/)", "/ Search", KeyCode::Char('/'));
                        search.enabled = self.snapshot.is_some();
                        search
                    };
                    stage.minimum_width = 0;
                    let second = if file.as_ref().is_some_and(|f| f.group == Group::Worktree) {
                        let mut discard = control(
                            if block {
                                "Discard block(d)"
                            } else {
                                "Discard file(d)"
                            },
                            "Discard",
                            KeyCode::Char('d'),
                        );
                        discard.minimum_width = 7;
                        discard.enabled = !self.diff_pending();
                        discard
                    } else {
                        control("Refresh(r)", "r Refresh", KeyCode::Char('r'))
                    };
                    let mut menu = control("Ops(o)", "o Ops", KeyCode::Char('o'));
                    menu.enabled = self.snapshot.is_some();
                    vec![single, stage, second, menu]
                }
            }
        };
        if !navigation && self.modal.is_none() && self.history.is_none() && self.diff.is_none() {
            controls.push(if self.filter.is_empty() {
                control("Token Home(q)", "q Home", KeyCode::Char('q'))
            } else {
                control("Clear filter(q)", "q Clear", KeyCode::Char('q'))
            });
        }
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
                summary.push(Span::styled(
                    if compact { " " } else { " · " },
                    Style::default().fg(SOFT),
                ));
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
                if compact {
                    format!("{label}{count}")
                } else {
                    format!("{label} {count}")
                },
                Style::default().fg(group_color(group)),
            ));
        }
        let branch = self.repository_branch_label(width);
        let name = snapshot.root.file_name().map_or_else(
            || service::display_path(&snapshot.root),
            |name| safe_text(&name.to_string_lossy()),
        );
        let path = clipped(&name, usize::from(width).saturating_sub(branch.width() + 1));
        let mut identity = pair(&path, branch, width, SOFT);
        identity.spans[0].style = Style::default().fg(INK).add_modifier(Modifier::UNDERLINED);
        identity.spans[2].style = Style::default().fg(SOFT);
        let mode = self.pin_label(width);
        let mut context = vec![Span::styled(
            mode,
            Style::default()
                .fg(if self.manual_pin { GOLD } else { SOFT })
                .add_modifier(Modifier::UNDERLINED),
        )];
        if snapshot.ahead > 0 || snapshot.behind > 0 {
            context.push(Span::styled(
                format!(" ↑{}↓{}", snapshot.ahead, snapshot.behind),
                Style::default().fg(GOLD),
            ));
        }
        if compact {
            context.push(Span::raw(" "));
            context.extend(summary);
            return vec![identity, Line::from(context)];
        }
        let upstream = snapshot
            .upstream
            .as_ref()
            .map_or("No upstream · o: remotes".into(), |upstream| {
                upstream.clone()
            });
        context.push(Span::styled(
            format!(" · {upstream}"),
            Style::default().fg(SOFT),
        ));
        let mut headers = vec![identity, Line::from(context), Line::from(summary)];
        if !self.filter.is_empty() || self.search_before.is_some() {
            headers.push(line(
                clipped(
                    &format!(
                        "Search: {}{}",
                        safe_text(&self.filter),
                        if self.search_before.is_some() {
                            "▏"
                        } else {
                            ""
                        }
                    ),
                    usize::from(width),
                ),
                GOLD,
            ));
        }
        headers
    }
    fn pin_label(&self, width: u16) -> &'static str {
        if width < 30 {
            if self.manual_pin { "P(p)" } else { "F(p)" }
        } else if self.manual_pin {
            "Pinned(p)"
        } else {
            "Follow(p)"
        }
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
        self.repository_button = None;
        self.pin_button = None;
        self.ensure_selection();
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
        draw_page_header(
            frame,
            header_area,
            reserve,
            true,
            self.modal.is_none() && !self.busy && self.search_before.is_none(),
        );
        if header_area.width >= 10 {
            frame.render_widget(
                Paragraph::new("Help").style(Style::default().fg(SOFT).add_modifier(
                    if self.help_available() {
                        Modifier::empty()
                    } else {
                        Modifier::DIM
                    },
                )),
                pulse_help_rect(header_area),
            );
        }
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
                if let Some(snapshot) = &self.snapshot {
                    let name = snapshot.root.file_name().map_or_else(
                        || service::display_path(&snapshot.root),
                        |name| safe_text(&name.to_string_lossy()),
                    );
                    let available = width.saturating_sub(button_width + 1);
                    let name_width = clipped(&name, usize::from(available)).width() as u16;
                    if name_width > 0 {
                        self.repository_button = Some(Rect::new(area.x, area.y + 1, name_width, 1));
                    }
                    if header_height > 1 {
                        self.pin_button = Some(Rect::new(
                            area.x,
                            area.y + 2,
                            (self.pin_label(width).width() as u16).min(width),
                            1,
                        ));
                    }
                }
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
            .is_some_and(|m| !matches!(m, Modal::Menu { .. } | Modal::Help(_)))
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
        if !self.busy && self.modal.is_none() && self.diff.is_none() {
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
        } else if !self.busy
            && let Some(Modal::Menu { selected, .. }) = &self.modal
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
                self.operation.map_or("Working…".into(), |(name, start)| {
                    let elapsed = format!(" · {}s", start.elapsed().as_secs());
                    format!(
                        "{}{}",
                        clipped(
                            &format!("{name}…"),
                            usize::from(area.width).saturating_sub(elapsed.width())
                        ),
                        elapsed
                    )
                })
            } else if self.preview_refreshing.load(Ordering::Relaxed) {
                "Updating Diff… · scrolling available".into()
            } else if let Some(error) = &self.error {
                if self.modal.is_some() {
                    format!("! {error}")
                } else {
                    format!("! {error} · l: details")
                }
            } else if let Some(error) = self.last_failure() {
                format!("! {error} · l: details")
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
                    Modal::Help(_) => self.help_language.text(
                        "l 中/英 · ↑↓ 滚动 · ? / Esc 返回",
                        "l 中文/EN · ↑↓ scroll · ? / Esc back",
                    ),
                    Modal::Message(_) | Modal::OperationResult { .. } => {
                        "↑↓ / PgUp / PgDn scroll · Esc back"
                    }
                }
                .into()
            } else if self.search_before.is_some() {
                "Type file path · Enter apply · Esc cancel · Ctrl+U clear".into()
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
                        "h: stage block · d: discard block · r refresh".into()
                    }
                } else {
                    "↑↓ scroll · whole-file actions · r refresh".into()
                }
            } else {
                self.refreshed.map_or("Reading Git…".into(), |time| {
                    format!(
                        "Updated {}s ago · / search · i details",
                        time.elapsed().as_secs()
                    )
                })
            };
            frame.render_widget(
                Paragraph::new(clipped(&status, usize::from(area.width))).style(
                    Style::default().fg(if self.error.is_some() || self.last_failure().is_some() {
                        RED
                    } else {
                        SOFT
                    }),
                ),
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
            for (control, rect) in controls.iter().zip(control_rects(screen, row, &controls)) {
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
fn reload_diff(
    root: &std::path::Path,
    previous: &service::File,
) -> std::result::Result<Option<Diff>, String> {
    service::snapshot(root)
        .and_then(|snapshot| {
            snapshot
                .files
                .iter()
                .find(|file| file.path == previous.path && file.group == previous.group)
                .map(|file| service::diff(root, file))
                .transpose()
        })
        .map_err(|error| error.to_string())
}
fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Stage(_) => "Staging file",
        Action::Unstage(_) => "Unstaging file",
        Action::StageAll => "Staging all",
        Action::UnstageAll => "Unstaging all",
        Action::Discard(_) => "Discarding file",
        Action::Hunk { discard: true, .. } => "Discarding block",
        Action::Hunk { diff, .. } if diff.file.group == Group::Index => "Unstaging block",
        Action::Hunk { .. } => "Staging block",
        Action::Commit { .. } => "Committing",
        Action::Switch(_) => "Switching branch",
        Action::Create(_) | Action::Track { .. } => "Creating branch",
        Action::Fetch(_) => "Fetching",
        Action::Pull(_) => "Pulling",
        Action::Push(_) => "Pushing",
    }
}
fn fetch_age(time: std::time::SystemTime) -> String {
    let seconds = time.elapsed().unwrap_or_default().as_secs();
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86400)
    }
}
fn remap_hunk(old: &Diff, new: &Diff, selected: usize) -> usize {
    let Some(hunk) = old.hunks.get(selected) else {
        return 0;
    };
    let coordinate = |diff: &Diff, hunk: &service::Hunk| {
        diff.lines
            .get(hunk.start)
            .and_then(|line| line.split_whitespace().nth(2))
            .map_or(0, range_start)
    };
    let target = coordinate(old, hunk);
    // Preserve the block when its changed content survives, including renumbering.
    let changes: Vec<_> = old.lines[hunk.start..hunk.end]
        .iter()
        .filter(|line| line.starts_with('+') || line.starts_with('-'))
        .take(8)
        .collect();
    if let Some(index) = new
        .hunks
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            changes
                .iter()
                .any(|line| new.lines[candidate.start..candidate.end].contains(line))
        })
        .min_by_key(|(_, candidate)| coordinate(new, candidate).abs_diff(target))
        .map(|(index, _)| index)
    {
        return index;
    }
    // A staged/discarded block disappeared: choose the next block in source order.
    new.hunks
        .iter()
        .position(|candidate| coordinate(new, candidate) >= target)
        .unwrap_or(new.hunks.len().saturating_sub(1))
}
// Prefer unchanged nearby content, then the nearest source line coordinate.
fn remap_line(old: &[String], new: &[String], line: usize) -> usize {
    for distance in 0..8 {
        for candidate in [line.saturating_add(distance), line.saturating_sub(distance)] {
            if let Some(text) = old
                .get(candidate)
                .filter(|text| !text.trim().is_empty() && !text.starts_with("@@"))
                && let Some(index) = new
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| *value == text)
                    .min_by_key(|(index, _)| index.abs_diff(candidate))
                    .map(|(index, _)| index)
            {
                return index;
            }
        }
    }
    let coordinates = |lines: &[String]| {
        let mut source = 0;
        lines
            .iter()
            .map(|text| {
                if text.starts_with("@@ ") {
                    source = text.split_whitespace().nth(2).map_or(0, range_start);
                } else if text.starts_with(' ')
                    || (text.starts_with('+') && !text.starts_with("+++"))
                {
                    source += 1;
                }
                source
            })
            .collect::<Vec<usize>>()
    };
    let old_coordinates = coordinates(old);
    let target = old_coordinates.get(line).copied().unwrap_or(0);
    coordinates(new)
        .iter()
        .enumerate()
        .min_by_key(|(index, value)| (value.abs_diff(target), index.abs_diff(line)))
        .map_or(0, |(index, _)| index)
}

fn group_color(group: Group) -> Color {
    match group {
        Group::Worktree => GOLD,
        Group::Index => GREEN,
        Group::Conflict => RED,
    }
}
fn file_display_names(files: &[service::File]) -> Vec<String> {
    use std::{collections::BTreeSet, ffi::OsStr, path::Path};
    let mut groups: BTreeMap<&OsStr, BTreeSet<&Path>> = BTreeMap::new();
    for file in files {
        groups
            .entry(file.path.file_name().unwrap_or(file.path.as_os_str()))
            .or_default()
            .insert(&file.path);
    }
    files
        .iter()
        .map(|file| {
            let group = &groups[file.path.file_name().unwrap_or(file.path.as_os_str())];
            let count = file.path.components().count();
            let depth = (1..=count)
                .find(|&depth| {
                    group.iter().all(|other| {
                        *other == file.path
                            || !file
                                .path
                                .components()
                                .rev()
                                .take(depth)
                                .eq(other.components().rev().take(depth))
                    })
                })
                .unwrap_or(count);
            let suffix: PathBuf = file.path.components().skip(count - depth).collect();
            service::display_path(&suffix)
        })
        .collect()
}

fn file_lines(
    file: &service::File,
    display_name: &str,
    selected: bool,
    width: u16,
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
    let available = usize::from(width).saturating_sub(5 + file.stat.width());
    let leaf = clipped(display_name, available);
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
    if !selected {
        return out;
    }
    let context = file.label();
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
    minimum_width: u16,
}
impl Control {
    fn new(label: &'static str, compact: &'static str, code: KeyCode) -> Self {
        Self {
            label,
            compact,
            key: KeyEvent::new(code, KeyModifiers::NONE),
            enabled: true,
            minimum_width: 0,
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
fn control_rects(screen: Rect, row: u16, controls: &[Control]) -> Vec<Rect> {
    let count = controls.len();
    if count == 0 {
        return vec![];
    }
    let constraints = controls.iter().map(|control| {
        if control.minimum_width > screen.width / count as u16
            && screen.width >= control.minimum_width + count as u16 - 1
        {
            Constraint::Min(control.minimum_width)
        } else {
            Constraint::Ratio(1, count as u32)
        }
    });
    Layout::horizontal(constraints)
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
            fetched: None,
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
    fn q_returns_one_level_but_is_text_in_forms_and_busy_blocks_back() {
        let mut pane = diff_pane();
        pane.modal = Some(Modal::Help(HelpPage::Git));
        assert!(pane.key(key(KeyCode::Char('q'))));
        assert!(pane.modal.is_none());
        assert!(pane.diff.is_some());
        assert!(pane.key(key(KeyCode::Char('q'))));
        assert!(pane.diff.is_none());
        assert!(!pane.key(key(KeyCode::Char('q'))));
        assert!(
            pane.controls(false)
                .iter()
                .any(|c| c.key.code == KeyCode::Char('q') && c.label.contains("Home"))
        );
        pane.modal = Some(Modal::Form {
            kind: FormKind::Create,
            text: String::new(),
        });
        assert!(pane.key(key(KeyCode::Char('q'))));
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "q"));
        pane.key(key(KeyCode::Esc));
        pane.busy = true;
        assert!(pane.key(key(KeyCode::Char('q'))));
        assert!(pane.busy);
        assert!(pane.controls(false).iter().all(|c| !c.enabled));
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
            let mut pane = {
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/project")));
                pane
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
            assert!(!pane.key(key(KeyCode::Char('g'))));
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(data));
            pane
        };
        let mut terminal = Terminal::new(TestBackend::new(48, 30)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let first: String = (0..48)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect();
        assert!(first.contains("project") && first.contains("main"));
        assert!(!first.contains("/workspace/"));
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
            let mut pane = {
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/project")));
                pane
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
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
        pane.apply_snapshot(None);
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(data));
            pane
        };
        let (send, receive) = mpsc::sync_channel(2);
        pane.requests = Some(send);
        render(&mut pane, 48, 30);
        let mapped: Vec<_> = pane.rows.iter().filter(|(_, i)| *i == 1).collect();
        assert_eq!(mapped.len(), 1, "unselected files stay on one row");
        let row = mapped[0].0 + pane.body.y;
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x + 4,
            row,
            modifiers: KeyModifiers::NONE,
        });
        pane.input(&click);
        assert_eq!(pane.selected, 1);
        render(&mut pane, 48, 30);
        let mapped: Vec<_> = pane.rows.iter().filter(|(_, i)| *i == 1).collect();
        assert!(mapped.len() > 1, "selected file expands its full path");
        let click = Event::Mouse(MouseEvent {
            row: mapped[1].0 + pane.body.y,
            ..match click {
                Event::Mouse(mouse) => mouse,
                _ => unreachable!(),
            }
        });
        pane.input(&click);
        assert!(
            matches!(receive.try_recv().unwrap(), Request::Diff(_, file, false) if file.path == path && file.group == Group::Index)
        );
        let file = &pane.snapshot.as_ref().unwrap().files[1];
        let lines = file_lines(file, &pane.file_names[1], true, 45);
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(data));
            pane
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
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
            modal: Some(Modal::Form {
                kind: FormKind::Commit {
                    head: None,
                    index: vec![],
                },
                text: String::new(),
            }),
            ..{
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/project")));
                pane
            }
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
            updates: Some(receive),
            modal: Some(Modal::Form {
                kind: FormKind::Create,
                text: "new".into(),
            }),
            ..{
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/old")));
                pane
            }
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
            revision: vec![],
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
        pane.apply_snapshot(Some(snapshot("/project")));
        pane.diff = Some(Diff {
            file: pane.file().unwrap(),
            raw: vec![],
            lines: vec!["+long diff line".into()],
            hunks: vec![],
            fingerprint: vec![],
            revision: vec![],
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(data));
            pane
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
            revision: vec![],
            reason: None,
        };
        {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot));
            pane.diff = Some(diff);
            pane
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
        assert!(text.contains("Unstage"));
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
                for (control, rect) in controls
                    .iter()
                    .zip(control_rects(pane.screen, 0, &controls))
                {
                    assert!(control.label(rect.width).width() <= usize::from(rect.width));
                }
            }
        }
        for width in [20, 28, 32, 48] {
            for block in [false, true] {
                let mut unstaged = diff_pane();
                if !block {
                    unstaged.diff.as_mut().unwrap().hunks.clear();
                }
                let (send, receive) = mpsc::sync_channel(1);
                unstaged.requests = Some(send);
                let text = render(&mut unstaged, width, 20);
                let controls = unstaged.controls(false);
                let rect = control_rects(unstaged.screen, 19, &controls)[2];
                assert_eq!(controls[2].label(rect.width), "Discard", "{width} columns");
                assert!(text.contains("Discard"));
                unstaged.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.right() - 1,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }));
                let Some(Modal::Confirm { action, .. }) = unstaged.modal else {
                    panic!("Discard must require confirmation");
                };
                if block {
                    assert!(matches!(action, Action::Hunk { discard: true, .. }));
                } else {
                    assert!(matches!(action, Action::Discard(_)));
                }
                assert!(receive.try_recv().is_err());
            }
            let mut staged = diff_pane();
            staged.selected = 1;
            staged.diff = None;
            let (send, receive) = mpsc::sync_channel(1);
            staged.requests = Some(send);
            let text = render(&mut staged, width, 20);
            let controls = staged.controls(false);
            let rect = control_rects(staged.screen, 19, &controls)[0];
            assert_eq!(controls[0].label(rect.width), "Unstage", "{width} columns");
            assert!(text.contains("Unstage"));
            staged.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.right() - 1,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(
                matches!(receive.try_recv().unwrap(), Request::Execute(_, Action::Unstage(file)) if file.path.as_path() == std::path::Path::new("staged.rs"))
            );
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
        let (header, _) = pulse_header_area(pane.screen);
        let help = pulse_help_rect(header);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: help.x,
            row: help.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(pane.modal, Some(Modal::Help(_))));
    }
    #[test]
    fn stage_block_button_and_shortcut_keep_stage_all_on_its_existing_key() {
        let mut pane = diff_pane();
        let (send, receive) = mpsc::sync_channel(2);
        pane.requests = Some(send);
        render(&mut pane, 48, 20);
        click_footer(&mut pane, false, 1);
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(
                _,
                Action::Hunk {
                    hunk: 0,
                    discard: false,
                    ..
                }
            )
        ));
        pane.busy = false;
        pane.key(key(KeyCode::Char('h')));
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(_, Action::Hunk { .. })
        ));
        pane.busy = false;
        pane.key(key(KeyCode::Char('a')));
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(_, Action::StageAll)
        ));
        pane.busy = false;
        pane.diff = None;
        pane.key(key(KeyCode::Char('a')));
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(_, Action::StageAll)
        ));
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
        assert!(row(19).contains("Block(h)"));
    }
    #[test]
    fn header_switch_returns_to_token_without_losing_diff_and_title_keeps_context() {
        for (width, height) in [(20, 10), (32, 12), (48, 20), (100, 40)] {
            let mut pane = diff_pane();
            let text = render(&mut pane, width, height);
            let (area, reserve) = pulse_header_area(pane.screen);
            let parts = page_header_rects(area, reserve, true);
            assert_eq!(
                Monitor {
                    page: config::PulseStartPage::Git,
                    ..Default::default()
                }
                .header_switch_rect(area, reserve == 4),
                parts[1]
            );
            assert!(text.contains("GIT") && text.contains("[Token(g)]"));
            let click = |column| {
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                })
            };
            assert!(pane.input(&click(parts[0].x)));
            assert!(pane.diff.is_some());
            assert!(!pane.input(&click(parts[1].x)));
            assert!(!pane.input(&click(parts[1].right() - 1)));
            assert!(pane.diff.is_some());
            assert!(
                !pane
                    .controls(true)
                    .iter()
                    .any(|c| c.key.code == KeyCode::Char('g'))
            );
            pane.modal = Some(Modal::Message("Details".into()));
            assert!(pane.input(&click(parts[1].x)));
            assert!(pane.modal.is_some());
            assert!(pane.key(key(KeyCode::Char('g'))));
            assert!(pane.modal.is_some());
            let mut monitor = Monitor {
                page: config::PulseStartPage::Git,
                git: pane,
                ..Default::default()
            };
            assert!(!monitor.workspace_shortcut(key(KeyCode::Char('g'))));
            assert_eq!(monitor.page, config::PulseStartPage::Git);
            monitor.git.modal = None;
            monitor.git.busy = true;
            assert!(!monitor.workspace_shortcut(key(KeyCode::Char('g'))));
            assert_eq!(monitor.page, config::PulseStartPage::Git);
        }
    }
    fn click_footer(pane: &mut GitPane, navigation: bool, index: usize) {
        let row = pane.screen.bottom() - if navigation { 2 } else { 1 };
        let rect = control_rects(pane.screen, row, &pane.controls(navigation))[index];
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }
    #[test]
    fn form_buttons_review_with_the_correct_key_and_commit_back_keeps_draft() {
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
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
            modal: Some(Modal::Form {
                kind: FormKind::Search,
                text: "topic".into(),
            }),
            ..{
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/project")));
                pane
            }
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
    fn details_return_to_their_page_and_home_appears_only_on_git_root() {
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
                let controls = pane.controls(navigation);
                assert!(!controls.iter().any(|c| c.key.code == KeyCode::Char('g')));
                assert_eq!(
                    controls.iter().any(|c| c.label.contains("Home")),
                    page == 1 && !navigation
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
    fn chinese_help_scrolls_and_returns_to_diff_or_history() {
        for history in [false, true] {
            let mut pane = diff_pane();
            if history {
                pane.diff = None;
                pane.history = Some(service::History {
                    commits: vec![],
                    page: 0,
                    has_more: false,
                    query: String::new(),
                });
            }
            pane.scroll = 3;
            pane.key(key(KeyCode::Char('?')));
            assert_eq!(pane.scroll, 0);
            assert!(matches!(pane.modal, Some(Modal::Help(_))));
            let text = render(&mut pane, 32, 12);
            assert!(text.contains("Git") && text.contains('帮'));
            assert!(pane.lines()[0].to_string().contains(if history {
                "Git 历史帮助"
            } else {
                "Git 帮助"
            }));
            pane.key(key(KeyCode::Char('l')));
            assert_eq!(pane.help_language, HelpLanguage::English);
            assert_eq!(pane.scroll, 0);
            let text = render(&mut pane, 32, 12);
            assert!(text.contains("Git help"));
            click_footer(&mut pane, false, 0);
            assert_eq!(pane.help_language, HelpLanguage::Chinese);
            assert_eq!(pane.scroll, 0);
            pane.key(key(KeyCode::End));
            assert!(pane.limit > 0 && pane.scroll == pane.limit);
            render(&mut pane, 32, 12);
            pane.key(key(KeyCode::Char('?')));
            assert!(pane.modal.is_none());
            assert_eq!(pane.scroll, 3);
            assert_eq!(pane.history.is_some(), history);
            assert_eq!(pane.diff.is_some(), !history);
        }
    }

    #[test]
    fn help_moves_to_header_and_keeps_git_footer_actions() {
        for width in [20, 24, 32, 48, 100] {
            for page in 0..3 {
                let mut pane = diff_pane();
                if page != 1 {
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
                let text = render(&mut pane, width, 20);
                assert!(text.contains("Help"));
                for navigation in [true, false] {
                    assert!(
                        !pane
                            .controls(navigation)
                            .iter()
                            .any(|control| control.key.code == KeyCode::Char('?'))
                    );
                }
                if page == 0 {
                    assert_eq!(
                        pane.controls(true)
                            .iter()
                            .map(|control| control.key.code)
                            .collect::<Vec<_>>(),
                        vec![
                            KeyCode::Enter,
                            KeyCode::Char('n'),
                            KeyCode::Char('L'),
                            KeyCode::Char('b')
                        ]
                    );
                }
                let (header, _) = pulse_header_area(pane.screen);
                let help = pulse_help_rect(header);
                assert!(pane.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: help.right() - 1,
                    row: help.y,
                    modifiers: KeyModifiers::NONE
                })));
                assert!(matches!(pane.modal, Some(Modal::Help(_))));
                pane.key(key(KeyCode::Esc));
                assert!(pane.modal.is_none());
                assert_eq!(pane.diff.is_some(), page == 1);
                assert_eq!(pane.history.is_some(), page == 2);
            }
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
            history: Some(service::History {
                commits: vec![],
                page: 2,
                has_more: false,
                query: String::new(),
            }),
            ..{
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/project")));
                pane
            }
        };
        let (send, receive) = mpsc::sync_channel(8);
        pane.requests = Some(send);
        let (updates, responses) = mpsc::channel();
        pane.updates = Some(responses);
        render(&mut pane, 48, 20);
        click_footer(&mut pane, false, 1);
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
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
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
    #[test]
    fn file_search_is_live_literal_and_restores_collapsed_groups() {
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
        };
        pane.snapshot.as_mut().unwrap().files[1].old_path = Some("SRC/Old[Name].rs".into());
        pane.collapsed.push(Group::Index);
        pane.key(key(KeyCode::Char('/')));
        assert!(pane.pinned.load(Ordering::Relaxed));
        assert!(!pane.can_switch_workspace());
        render(&mut pane, 48, 30);
        let (area, reserve) = pulse_header_area(pane.screen);
        let tab = page_header_rects(area, reserve, true)[1];
        assert!(pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tab.x,
            row: tab.y,
            modifiers: KeyModifiers::NONE
        })));
        assert!(pane.search_before.is_some());
        pane.input(&Event::Paste("old[name]".into()));
        assert_eq!(pane.visible_files(), vec![1]);
        assert_eq!(pane.selected, 1);
        assert!(render(&mut pane, 48, 30).contains("1/1"));
        pane.key(key(KeyCode::Enter));
        assert!(!pane.pinned.load(Ordering::Relaxed));
        assert_eq!(pane.file().unwrap().group, Group::Index);
        pane.key(key(KeyCode::Down));
        assert_eq!(pane.selected, 1);
        pane.key(key(KeyCode::Char('/')));
        pane.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(pane.visible_files(), vec![0]);
        pane.key(key(KeyCode::Esc));
        assert_eq!(pane.filter, "old[name]");
        pane.key(key(KeyCode::Esc));
        assert!(pane.filter.is_empty());
        assert_eq!(pane.visible_files(), vec![0]);
        assert!(pane.collapsed.contains(&Group::Index));
        pane.key(key(KeyCode::Char('/')));
        pane.input(&Event::Paste("no-match".into()));
        let text = render(&mut pane, 48, 30);
        assert!(text.contains("No matching files"));
        assert!(pane.file().is_none());
        assert!(pane.controls(true).is_empty());
        pane.key(key(KeyCode::Enter));
        assert!(!pane.controls(true)[0].enabled);
        let (send, receive) = mpsc::sync_channel(1);
        pane.requests = Some(send);
        pane.key(key(KeyCode::Char('a')));
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(_, Action::StageAll)
        ));
    }
    #[test]
    fn clicking_group_headers_never_selects_hidden_files() {
        let mut pane = {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane
        };
        render(&mut pane, 48, 30);
        let heading = pane
            .group_rows
            .iter()
            .find(|(_, group)| *group == Group::Worktree)
            .unwrap()
            .0;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x,
            row: pane.body.y + heading,
            modifiers: KeyModifiers::NONE,
        }));
        render(&mut pane, 48, 30);
        assert_eq!(pane.visible_files(), vec![1]);
        assert_eq!(pane.selected, 1);
        assert!(pane.rows.iter().all(|(_, index)| *index == 1));
        pane.collapsed.push(Group::Index);
        render(&mut pane, 48, 30);
        assert!(pane.file().is_none());
        assert!(!pane.controls(true)[0].enabled);
        assert_eq!(pane.group_rows.len(), 2);
    }
    #[test]
    fn pin_and_repository_details_keep_manual_and_temporary_pins_separate() {
        let (send, receive) = mpsc::channel();
        let mut pane = GitPane {
            updates: Some(receive),
            ..{
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/workspace/project")));
                pane
            }
        };
        let text = render(&mut pane, 20, 8);
        assert!(text.contains("↑2↓1"));
        let button = pane.pin_button.unwrap();
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(pane.manual_pin);
        assert!(pane.pinned.load(Ordering::Relaxed));
        assert_eq!(
            *pane.bound_root.lock().unwrap(),
            Some(PathBuf::from("/workspace/project"))
        );
        render(&mut pane, 48, 30);
        let button = pane.repository_button.unwrap();
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            matches!(&pane.modal, Some(Modal::Message(message)) if message.contains("/workspace/project") && message.contains("Local fetch record: unknown") && message.contains("Mode: Pinned"))
        );
        pane.key(key(KeyCode::Esc));
        assert!(pane.pinned.load(Ordering::Relaxed));
        send.send(Response::Snapshot(Ok(Some(snapshot("/other")))))
            .unwrap();
        pane.poll();
        assert_eq!(pane.root(), Some("/workspace/project".into()));
        pane.key(key(KeyCode::Char('p')));
        assert!(!pane.manual_pin);
        assert!(!pane.pinned.load(Ordering::Relaxed));
        assert!(pane.bound_root.lock().unwrap().is_none());
        send.send(Response::Snapshot(Ok(Some(snapshot("/other")))))
            .unwrap();
        pane.poll();
        assert_eq!(pane.root(), Some("/other".into()));
        pane.apply_snapshot(None);
        render(&mut pane, 48, 30);
        pane.key(key(KeyCode::Char('p')));
        assert!(!pane.manual_pin);
        assert!(pane.pin_button.is_none());
    }
    #[test]
    fn successful_block_operation_keeps_diff_and_advances_to_remaining_block() {
        let mut pane = diff_pane();
        render(&mut pane, 48, 20);
        pane.sync_watch();
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        let mut next = pane.diff.clone().unwrap();
        next.lines.drain(4..8);
        next.hunks = vec![service::Hunk { start: 4, end: 7 }];
        next.fingerprint = vec![2];
        pane.busy = true;
        send.send(Response::Completed {
            root: "/project".into(),
            name: "Staging block",
            is_commit: false,
            result: Ok("Staged block".into()),
            snapshot: Ok(snapshot("/project")),
            diff: Some(Ok(Some(next))),
        })
        .unwrap();
        pane.poll();
        assert!(pane.diff.is_some());
        assert_eq!(pane.hunk, 0);
        assert_eq!(pane.diff.as_ref().unwrap().hunks.len(), 1);
        assert_eq!(pane.diff_anchor, Some(4));
        assert_eq!(pane.notice, "Staged block");
        let mut data = snapshot("/project");
        data.files[0].group = Group::Index;
        pane.busy = true;
        send.send(Response::Completed {
            root: "/project".into(),
            name: "Staging file",
            is_commit: false,
            result: Ok("Staged file".into()),
            snapshot: Ok(data),
            diff: Some(Ok(None)),
        })
        .unwrap();
        pane.poll();
        assert!(pane.diff.is_none());
        assert_eq!(pane.file().unwrap().path, PathBuf::from("src/中文.rs"));
        assert_eq!(pane.file().unwrap().group, Group::Index);
    }
    #[test]
    fn failed_operation_preserves_diff_selection_and_scroll_after_details_close() {
        let mut pane = diff_pane();
        render(&mut pane, 32, 12);
        pane.hunk = 1;
        pane.scroll = pane.limit;
        let original_scroll = pane.scroll;
        let original_raw = pane.diff.as_ref().unwrap().raw.clone();
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        pane.busy = true;
        send.send(Response::Completed {
            root: "/project".into(),
            name: "Staging block",
            is_commit: false,
            result: Err("hook or Git failure".into()),
            snapshot: Ok(snapshot("/project")),
            diff: Some(Ok(None)),
        })
        .unwrap();
        pane.poll();
        assert!(pane.modal.is_some());
        assert!(!pane.busy);
        render(&mut pane, 32, 12);
        pane.key(key(KeyCode::Esc));
        render(&mut pane, 32, 12);
        assert_eq!(pane.scroll, original_scroll);
        assert_eq!(pane.hunk, 1);
        assert_eq!(pane.diff.as_ref().unwrap().raw, original_raw);
    }
    #[test]
    fn background_preview_preserves_anchor_and_rejects_obsolete_responses() {
        let mut pane = diff_pane();
        render(&mut pane, 32, 12);
        pane.scroll = 2;
        let old_line = pane.diff_rows[2];
        pane.sync_watch();
        let version = pane.preview_version;
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        let mut next = pane.diff.clone().unwrap();
        next.lines.insert(5, " new context".into());
        next.hunks[0].end += 1;
        next.hunks[1].start += 1;
        next.hunks[1].end += 1;
        next.fingerprint = vec![2];
        send.send(Response::Preview(
            "/project".into(),
            version,
            Ok(Some(next)),
        ))
        .unwrap();
        pane.poll();
        assert_eq!(pane.diff_anchor, Some(old_line + 1));
        assert!(!pane.busy);
        assert!(pane.notice_until.is_some());
        let accepted = pane.diff.as_ref().unwrap().fingerprint.clone();
        send.send(Response::Preview("/project".into(), version, Ok(None)))
            .unwrap();
        send.send(Response::Preview(
            "/other".into(),
            pane.preview_version,
            Ok(None),
        ))
        .unwrap();
        pane.poll();
        assert_eq!(pane.diff.as_ref().unwrap().fingerprint, accepted);
        let current = pane.preview_version;
        send.send(Response::Preview("/project".into(), current, Ok(None)))
            .unwrap();
        pane.poll();
        assert!(pane.diff.is_none());
        assert!(pane.notice.contains("no longer"));
    }
    #[test]
    fn preview_refresh_disables_stale_block_and_discard_actions_until_recovered() {
        let mut pane = diff_pane();
        let (requests, receive) = mpsc::sync_channel(3);
        pane.requests = Some(requests);
        pane.preview_refreshing.store(true, Ordering::Relaxed);
        assert!(!pane.controls(false)[1].enabled);
        assert!(!pane.controls(false)[2].enabled);
        pane.key(key(KeyCode::Char('h')));
        pane.key(key(KeyCode::Char('d')));
        assert!(receive.try_recv().is_err());
        assert!(pane.modal.is_none());
        pane.preview_refreshing.store(false, Ordering::Relaxed);
        pane.preview_stale = true;
        pane.key(key(KeyCode::Char('h')));
        assert!(receive.try_recv().is_err());
        let mut refreshed = pane.diff.clone().unwrap();
        refreshed.fingerprint = vec![3];
        pane.install_diff(refreshed, true);
        pane.key(key(KeyCode::Char('h')));
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(_, Action::Hunk { .. })
        ));
    }
    #[test]
    fn busy_pane_allows_reading_without_changing_target_or_submitting_commands() {
        let mut pane = diff_pane();
        render(&mut pane, 32, 12);
        pane.busy = true;
        pane.operation = Some(("Pushing", Instant::now() - Duration::from_secs(8)));
        let (send, receive) = mpsc::sync_channel(3);
        pane.requests = Some(send);
        let text = render(&mut pane, 20, 10);
        assert!(text.contains("Pushing"));
        assert!(text.contains("8s"));
        pane.key(key(KeyCode::End));
        assert_eq!(pane.scroll, pane.limit);
        let selected = pane.selected;
        let hunk = pane.hunk;
        pane.key(key(KeyCode::Char('a')));
        pane.key(key(KeyCode::Char('h')));
        pane.key(key(KeyCode::Char('p')));
        pane.input(&Event::Paste("accidental command".into()));
        assert!(receive.try_recv().is_err());
        assert_eq!(pane.selected, selected);
        assert_eq!(pane.hunk, hunk);
        assert!(!pane.manual_pin);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: pane.body.x,
            row: pane.body.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(pane.scroll < pane.limit);
        render(&mut pane, 20, 10);
        let controls = pane.controls(true);
        assert!(controls.iter().any(|control| control.enabled));
        assert!(pane.controls(false).iter().all(|control| !control.enabled));
    }
    #[test]
    fn hunk_remapping_selects_next_block_when_middle_block_disappears() {
        let mut pane = diff_pane();
        let mut old = pane.diff.take().unwrap();
        old.lines.extend([
            "@@ -200,1 +200,1 @@".into(),
            "-last old".into(),
            "+last new".into(),
        ]);
        old.hunks.push(service::Hunk { start: 11, end: 14 });
        let mut next = old.clone();
        next.lines.drain(8..11);
        next.hunks = vec![
            service::Hunk { start: 4, end: 8 },
            service::Hunk { start: 8, end: 11 },
        ];
        assert_eq!(remap_hunk(&old, &next, 1), 1);
        assert_eq!(remap_hunk(&old, &next, 0), 0);
    }
    #[test]
    fn special_file_diff_uses_whole_file_controls_and_has_no_stage_block_button() {
        let mut pane = diff_pane();
        pane.diff.as_mut().unwrap().reason = Some("Binary file: whole-file operations only".into());
        assert!(
            pane.controls(false)
                .iter()
                .all(|control| control.key.code != KeyCode::Char('h'))
        );
        assert_eq!(pane.controls(false)[0].key.code, KeyCode::Char('s'));
        assert_eq!(pane.controls(false)[2].label, "Discard file(d)");
        let mut index = pane.diff.clone().unwrap();
        index.reason = None;
        index.file.group = Group::Index;
        pane.diff = Some(index);
        assert!(
            pane.controls(false)
                .iter()
                .all(|control| control.key.code != KeyCode::Char('h'))
        );
        assert_eq!(pane.controls(false)[0].label, "Unstage block(u)");
        assert_eq!(pane.controls(false)[1].key.code, KeyCode::Char('i'));
        assert_eq!(pane.controls(false)[2].key.code, KeyCode::Char('r'));
    }
    #[test]
    fn operations_menu_preserves_diff_scroll_on_cancel_and_block_submission() {
        let mut pane = diff_pane();
        render(&mut pane, 32, 12);
        pane.scroll = pane.limit;
        pane.hunk = 1;
        let original = pane.scroll;
        pane.key(key(KeyCode::Char('o')));
        render(&mut pane, 32, 12);
        pane.key(key(KeyCode::Esc));
        render(&mut pane, 32, 12);
        assert_eq!(pane.scroll, original);
        let (send, receive) = mpsc::sync_channel(1);
        pane.requests = Some(send);
        pane.key(key(KeyCode::Char('o')));
        for _ in 0..12 {
            pane.key(key(KeyCode::Down));
        }
        pane.key(key(KeyCode::Enter));
        assert_eq!(pane.scroll, original);
        assert!(pane.busy);
        assert!(matches!(
            receive.try_recv().unwrap(),
            Request::Execute(_, Action::Hunk { hunk: 1, .. })
        ));
    }
    #[test]
    fn operation_result_survives_reads_and_separates_refresh_failure() {
        let mut pane = diff_pane();
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        send.send(Response::Completed {
            root: "/project".into(),
            name: "Staging block",
            is_commit: false,
            result: Err("original failure\nfull diagnostic".into()),
            snapshot: Ok(snapshot("/project")),
            diff: None,
        })
        .unwrap();
        pane.busy = true;
        pane.poll();
        pane.key(key(KeyCode::Esc));
        send.send(Response::Snapshot(Ok(Some(snapshot("/project")))))
            .unwrap();
        pane.poll();
        pane.notice_until = Some(Instant::now());
        pane.poll();
        pane.open_operation_result();
        assert_eq!(
            pane.last_operation
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap_err(),
            "original failure\nfull diagnostic"
        );
        assert!(!pane.busy);
        pane.key(key(KeyCode::Esc));
        let diff = pane.diff.clone().unwrap();
        send.send(Response::Completed {
            root: "/project".into(),
            name: "Staging block",
            is_commit: false,
            result: Ok("success\nstderr detail".into()),
            snapshot: Err("read failed".into()),
            diff: Some(Ok(Some(diff))),
        })
        .unwrap();
        pane.poll();
        assert!(pane.last_operation.as_ref().unwrap().result.is_ok());
        assert!(pane.error.as_ref().unwrap().contains("read failed"));
        assert!(pane.preview_stale);
        pane.apply_snapshot(Some(snapshot("/other")));
        assert!(pane.last_failure().is_none());
        assert_eq!(
            pane.last_operation.as_ref().unwrap().root,
            PathBuf::from("/project")
        );
    }
    #[test]
    fn failed_commit_restores_draft_only_after_fresh_review() {
        let mut pane = GitPane::default();
        pane.apply_snapshot(Some(snapshot("/project")));
        let (request_send, requests) = mpsc::sync_channel(1);
        let (updates, receive) = mpsc::channel();
        pane.requests = Some(request_send);
        pane.updates = Some(receive);
        let message = "Title\nBody α\nlast line";
        pane.modal = Some(Modal::Form {
            kind: FormKind::Commit {
                head: Some("old".into()),
                index: vec![1],
            },
            text: message.into(),
        });
        pane.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        pane.key(key(KeyCode::Enter));
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Execute(_, Action::Commit { message: sent, .. }) if sent == message)
        );
        updates
            .send(Response::Completed {
                root: "/project".into(),
                name: "Committing",
                is_commit: true,
                result: Err("HEAD changed".into()),
                snapshot: Ok(snapshot("/project")),
                diff: None,
            })
            .unwrap();
        pane.poll();
        pane.key(key(KeyCode::Esc));
        let Request::Commit { root, draft } = requests.try_recv().unwrap() else {
            panic!("fresh review required")
        };
        assert_eq!(draft.as_deref(), Some(message));
        let mut fresh = snapshot("/project");
        fresh.head = Some("new".into());
        updates
            .send(Response::Commit {
                root,
                draft,
                result: Ok((fresh, vec![2])),
            })
            .unwrap();
        pane.poll();
        assert!(
            matches!(&pane.modal, Some(Modal::Form { kind: FormKind::Commit { head, index }, text }) if head.as_deref() == Some("new") && index == &[2] && text == message)
        );
        assert!(requests.try_recv().is_err());
        pane.key(key(KeyCode::Esc));
        assert!(pane.pending_commit.is_none());
    }

    #[test]
    fn rejected_enqueue_keeps_confirmation_and_failed_review_keeps_draft() {
        let mut pane = GitPane::default();
        pane.apply_snapshot(Some(snapshot("/project")));
        let (send, requests) = mpsc::sync_channel(1);
        send.try_send(Request::Refresh).unwrap();
        pane.requests = Some(send);
        pane.confirm(
            Action::Commit {
                message: "Title\nbody".into(),
                head: None,
                index: vec![1],
            },
            "review".into(),
        );
        pane.key(key(KeyCode::Enter));
        assert!(
            matches!(&pane.modal, Some(Modal::Confirm { action: Action::Commit { message, .. }, .. }) if message == "Title\nbody")
        );
        assert!(!pane.busy && pane.pending_commit.is_none());
        requests.try_recv().unwrap();
        drop(requests);
        pane.key(key(KeyCode::Enter));
        assert!(matches!(pane.modal, Some(Modal::Confirm { .. })));
        let (send, requests) = mpsc::sync_channel(1);
        let (updates, receive) = mpsc::channel();
        pane.requests = Some(send);
        pane.updates = Some(receive);
        pane.modal = None;
        pane.pending_commit = Some(CommitDraft {
            root: "/project".into(),
            message: "keep".into(),
        });
        pane.open_commit();
        let Request::Commit { root, draft } = requests.try_recv().unwrap() else {
            panic!()
        };
        updates
            .send(Response::Commit {
                root,
                draft,
                result: Err("No staged changes".into()),
            })
            .unwrap();
        pane.poll();
        pane.key(key(KeyCode::Esc));
        assert!(pane.modal.is_none());
        assert!(requests.try_recv().is_err());
        assert_eq!(pane.pending_commit.as_ref().unwrap().message, "keep");
        pane.apply_snapshot(Some(snapshot("/other")));
        pane.open_commit();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Request::Commit { draft: None, .. }
        ));
    }
}
