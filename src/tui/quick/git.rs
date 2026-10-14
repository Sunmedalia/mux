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
const ACTION_LABELS: [&str; 28] = [
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
    "Log",
    "Stage selected block",
    "Unstage selected block",
    "Discard selected block",
    "Refresh",
    "Repository details",
    "Last operation result",
    "Toggle Diff wrapping",
    "Previous file",
    "Next file",
    "Previous block",
    "Next block",
    "Return to files",
    "Commit details",
    "Newer commits",
    "Older commits",
    "Search commits",
];
enum FormKind {
    Commit {
        head: Option<String>,
        index: Vec<u8>,
    },
    Create,
    Track(String),
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
    CommitDetails(String),
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
    last_commit_click: Option<(String, Instant)>,
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
    search_position: Option<(usize, u16)>,
    search_rect: Rect,
    search_clear_rect: Rect,
    popup_area: Rect,
    view_width: u16,
    action_return: Option<(usize, String)>,
    action_ids: Vec<usize>,
    action_view: Option<(Rect, u16)>,
    collapsed: Vec<Group>,
    group_rows: Vec<(u16, Group)>,
    repository_button: Option<Rect>,
    repository_anchor: Option<Rect>,
    pin_button: Option<Rect>,
    body: Rect,
    screen: Rect,
    branch_button: Option<Rect>,
    branch_anchor: Option<Rect>,
    rows: Vec<(u16, usize)>,
    menu_rows: Vec<(u16, usize)>,
    menu_scroll_manual: bool,
    limit: u16,
}

// Unlike agent usage tracking, Git follows regular shell panes too. Only panes
// in the originating tab are candidates; focusing Pulse retains the last cwd.
pub(super) fn project_cwd(
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
    fn actions_menu(&self) -> bool {
        matches!(
            self.modal,
            Some(Modal::Menu {
                kind: MenuKind::Operations,
                ..
            })
        )
    }
    fn footer_height(&self) -> u16 {
        if self.screen.height < 7 || self.actions_menu() {
            1
        } else if self.modal.is_none() && self.diff.is_none() && self.history.is_none() {
            2
        } else if self.modal.is_none() || matches!(self.modal, Some(Modal::CommitDetails(_))) {
            1 + u16::from(self.view_status_visible())
        } else {
            3
        }
    }
    fn view_status_visible(&self) -> bool {
        self.busy
            || self.preview_refreshing.load(Ordering::Relaxed)
            || self.error.is_some()
            || self.last_failure().is_some()
            || self.search_before.is_some()
            || !self.notice.is_empty()
    }
    fn footer_row(&self, navigation: bool) -> Option<u16> {
        if navigation && self.footer_height() < 3 {
            return None;
        }
        if navigation && self.modal.is_none() && (self.diff.is_some() || self.history.is_some()) {
            return None;
        }
        Some(
            self.screen
                .bottom()
                .saturating_sub(if navigation { 2 } else { 1 }),
        )
    }
    fn help_available(&self) -> bool {
        !self.busy
            && self.search_before.is_none()
            && (self.modal.is_none() || matches!(self.modal, Some(Modal::Help(_))))
    }

    pub(super) fn return_home(&mut self) {
        if self.busy {
            return;
        }
        self.diff = None;
        self.history = None;
        self.modal = None;
        self.search_before = None;
        self.search_position = None;
        self.action_return = None;
        self.action_view = None;
        self.filter.clear();
        self.collapsed.clear();
        self.selected = 0;
        self.history_selected = 0;
        self.last_commit_click = None;
        self.branch_anchor = None;
        self.hunk = 0;
        self.scroll = 0;
        self.horizontal = 0;
        self.return_scroll = None;
        self.sync_watch();
        self.pin();
    }

    pub(super) fn can_switch_workspace(&self) -> bool {
        !self.busy
            && (self.modal.is_none() || matches!(self.modal, Some(Modal::CommitDetails(_))))
            && self.search_before.is_none()
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
                    self.menu_scroll_manual = false;
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
                            self.last_commit_click = None;
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
                        Ok(message) => self.modal = Some(Modal::CommitDetails(message)),
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
        self.repository_anchor = None;
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
        if self.return_scroll.is_none() && self.modal.is_none() {
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
        self.action_return = None;
        self.restore_action_view();
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
        self.menu_scroll_manual = false;
        if self.snapshot.is_none() {
            return;
        }
        self.remember_scroll();
        self.action_view.get_or_insert((self.body, self.limit));
        self.scroll = 0;
        self.action_ids = if self.history.is_some() {
            vec![24, 25, 26, 27, 15, 23]
        } else if self.diff.is_some() {
            vec![0, 1, 2, 12, 13, 14, 18, 19, 20, 21, 22, 15, 17, 23]
        } else {
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 15, 16, 17]
        };
        self.modal = Some(Modal::Menu {
            kind: MenuKind::Operations,
            items: self
                .action_ids
                .iter()
                .map(|index| {
                    if *index == 18 {
                        format!("Wrap {}(w)", if self.nowrap { "OFF" } else { "ON" })
                    } else {
                        ACTION_LABELS[*index].into()
                    }
                })
                .collect(),
            selected: 0,
            filter: String::new(),
        });
        self.normalize_actions(false);
        self.pin();
    }
    fn menu_action(&self, index: usize) -> usize {
        self.action_ids.get(index).copied().unwrap_or(index)
    }
    fn leave_actions(&mut self) {
        self.action_return = None;
        self.restore_action_view();
        if let Some(scroll) = self.return_scroll.take() {
            self.scroll = scroll;
        }
    }
    fn restore_action_view(&mut self) {
        if let Some((body, limit)) = self.action_view.take() {
            self.body = body;
            self.limit = limit;
        }
    }
    fn operation_enabled(&self, index: usize) -> bool {
        let file = self.file();
        let block = self
            .diff
            .as_ref()
            .filter(|diff| diff.reason.is_none() && !diff.hunks.is_empty() && !self.diff_pending());
        match index {
            0 => file.as_ref().is_some_and(|file| file.group != Group::Index),
            1 => file.as_ref().is_some_and(|file| file.group == Group::Index),
            2 => {
                file.as_ref()
                    .is_some_and(|file| file.group == Group::Worktree)
                    && !self.diff_pending()
            }
            3 => self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.files.iter().any(|file| file.group != Group::Index)
            }),
            4 => self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.files.iter().any(|file| file.group == Group::Index)
            }),
            5 => self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.files.iter().any(|file| file.group == Group::Index)
                    && !snapshot
                        .files
                        .iter()
                        .any(|file| file.group == Group::Conflict)
            }),
            12 | 14 => block.is_some_and(|diff| diff.file.group == Group::Worktree),
            13 => block.is_some_and(|diff| diff.file.group == Group::Index),
            18 => self.diff.is_some(),
            19 => self.diff_position().is_some_and(|(index, _)| index > 0),
            20 => self
                .diff_position()
                .is_some_and(|(index, count)| index + 1 < count),
            21 => self.diff.as_ref().is_some_and(|diff| {
                diff.reason.is_none() && self.hunk > 0 && self.hunk < diff.hunks.len()
            }),
            22 => self
                .diff
                .as_ref()
                .is_some_and(|diff| diff.reason.is_none() && self.hunk + 1 < diff.hunks.len()),
            23 => self.history.is_some() || self.diff.is_some(),
            24 => self
                .history
                .as_ref()
                .is_some_and(|history| !history.commits.is_empty()),
            25 => self
                .history
                .as_ref()
                .is_some_and(|history| history.page > 0),
            26 => self
                .history
                .as_ref()
                .is_some_and(|history| history.has_more),
            27 => self.history.is_some(),
            _ => self.snapshot.is_some(),
        }
    }
    fn normalize_actions(&mut self, backward: bool) {
        let Some(Modal::Menu {
            kind: MenuKind::Operations,
            items,
            selected,
            filter,
        }) = &self.modal
        else {
            return;
        };
        let enabled: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.to_lowercase().contains(&filter.to_lowercase()))
            .map(|(index, _)| self.operation_enabled(self.menu_action(index)))
            .collect();
        let current = (*selected).min(enabled.len().saturating_sub(1));
        let next = if enabled.get(current) == Some(&true) {
            current
        } else if backward {
            (0..=current)
                .rev()
                .find(|index| enabled.get(*index) == Some(&true))
                .or_else(|| enabled.iter().position(|value| *value))
                .unwrap_or(current)
        } else {
            (current..enabled.len())
                .find(|index| enabled[*index])
                .or_else(|| enabled.iter().rposition(|value| *value))
                .unwrap_or(current)
        };
        if let Some(Modal::Menu { selected, .. }) = &mut self.modal {
            *selected = next;
        }
    }
    fn operation(&mut self, index: usize) {
        self.remember_scroll();
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
                if let Some(diff) = self.diff.clone() {
                    self.confirm_discard(diff, false);
                } else if let (Some(root), Some(file)) = (self.root(), self.file()) {
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
                self.branch_anchor = None;
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
            11 => {
                self.leave_actions();
                self.open_history(0);
            }
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
            13 => {
                self.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
            }
            14 => {
                self.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
            }
            15 => {
                self.leave_actions();
                self.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
            }
            16 => self.repository_details(),
            17 => self.open_operation_result(),
            18 => {
                self.leave_actions();
                self.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
            }
            19..=27 => {
                self.leave_actions();
                let code = match index {
                    19 | 25 => KeyCode::Char(','),
                    20 | 26 => KeyCode::Char('.'),
                    21 => KeyCode::Char('['),
                    22 => KeyCode::Char(']'),
                    23 => KeyCode::Esc,
                    24 => KeyCode::Enter,
                    _ => KeyCode::Char('/'),
                };
                self.key(KeyEvent::new(code, KeyModifiers::NONE));
            }
            _ => {}
        }
        self.pin();
    }
    fn menu_select(&mut self, kind: MenuKind, item: String, index: usize) {
        self.modal = None;
        self.scroll = 0;
        match kind {
            MenuKind::Operations => self.operation(self.menu_action(index)),
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
            if self.request(Request::History(root, page, query)).is_ok() {
                self.action_return = None;
            }
        }
    }
    fn search_history(&mut self) {
        self.begin_search();
    }
    fn search_query(&self) -> &str {
        self.history
            .as_ref()
            .map_or(self.filter.as_str(), |history| history.query.as_str())
    }
    fn search_query_mut(&mut self) -> &mut String {
        self.history
            .as_mut()
            .map_or(&mut self.filter, |history| &mut history.query)
    }
    fn begin_search(&mut self) {
        if self.busy || self.modal.is_some() || self.diff.is_some() || self.search_before.is_some()
        {
            return;
        }
        self.search_before = Some(self.search_query().into());
        self.search_position = Some((
            if self.history.is_some() {
                self.history_selected
            } else {
                self.selected
            },
            self.scroll,
        ));
        self.pin();
    }
    fn finish_search(&mut self, cancel: bool) {
        let Some(previous) = self.search_before.take() else {
            return;
        };
        if cancel {
            *self.search_query_mut() = previous;
            if let Some((index, scroll)) = self.search_position.take() {
                if self.history.is_some() {
                    self.history_selected = index;
                } else {
                    self.selected = index;
                }
                self.scroll = scroll;
            }
            self.ensure_selection();
        } else {
            self.search_position = None;
            if self.history.is_some() && self.search_query() != previous {
                self.open_history(0);
            }
        }
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
                    && self.request(Request::Show(root, commit.id.clone())).is_ok()
                {
                    self.remember_scroll();
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
        self.menu_scroll_manual = matches!(self.modal, Some(Modal::Menu { .. }));
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
        self.last_commit_click = None;
        self.menu_scroll_manual = false;
        let typing = self.search_before.is_some()
            || matches!(
                self.modal,
                Some(Modal::Form { .. })
                    | Some(Modal::Menu {
                        kind: MenuKind::Branches | MenuKind::Fetch | MenuKind::Push,
                        ..
                    })
            );
        if key.code == KeyCode::Char('q') && !typing && key.modifiers == KeyModifiers::NONE {
            key.code = KeyCode::Esc;
        }

        if self.busy {
            self.reading_key(key.code);
            return true;
        }
        if key.code == KeyCode::Char('H')
            && !typing
            && !matches!(self.modal, Some(Modal::Confirm { .. }))
        {
            self.return_home();
            return true;
        }
        if let Some(modal) = self.modal.take() {
            if key.code == KeyCode::Esc
                && matches!(
                    modal,
                    Modal::Menu {
                        kind: MenuKind::Operations,
                        ..
                    }
                )
            {
                self.action_return = None;
            }
            if !matches!(modal, Modal::Menu { .. } | Modal::CommitDetails(_)) {
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
                Modal::CommitDetails(message) => {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Left) {
                        if let Some(scroll) = self.return_scroll.take() {
                            self.scroll = scroll;
                        }
                    } else {
                        self.reading_key(key.code);
                        self.modal = Some(Modal::CommitDetails(message));
                    }
                }
                Modal::OperationResult { recover_commit } => match key.code {
                    KeyCode::Esc | KeyCode::Enter => {
                        if self.action_return.is_none()
                            && let Some(scroll) = self.return_scroll.take()
                        {
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
                        if self.action_return.is_none()
                            && let Some(scroll) = self.return_scroll.take()
                        {
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
                            };
                            self.confirm(action, description);
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                            if let Some((i, item)) = visible.get(selected).filter(|(index, _)| {
                                !matches!(kind, MenuKind::Operations)
                                    || self.operation_enabled(self.menu_action(*index))
                            }) {
                                if matches!(kind, MenuKind::Operations) {
                                    self.action_return = Some((selected, filter.clone()));
                                }
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
                                KeyCode::PageDown => {
                                    selected = selected
                                        .saturating_add(usize::from(self.body.height.max(1)))
                                        .min(visible.len().saturating_sub(1))
                                }
                                KeyCode::PageUp => {
                                    selected = selected
                                        .saturating_sub(usize::from(self.body.height.max(1)))
                                }
                                KeyCode::Home => selected = 0,
                                KeyCode::End => selected = visible.len().saturating_sub(1),
                                KeyCode::Backspace => {
                                    filter.pop();
                                    selected = 0;
                                }
                                KeyCode::Char('u')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    filter.clear();
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
            self.normalize_actions(matches!(
                key.code,
                KeyCode::Up | KeyCode::PageUp | KeyCode::End
            ));
            if self.modal.is_none()
                && !self.busy
                && let Some((selected, filter)) = self.action_return.take()
            {
                self.operations();
                if let Some(Modal::Menu {
                    selected: current,
                    filter: current_filter,
                    ..
                }) = &mut self.modal
                {
                    *current = selected;
                    *current_filter = filter;
                }
            }
            if self.modal.is_none()
                && !self.busy
                && let Some(scroll) = self.return_scroll.take()
            {
                self.scroll = scroll;
            }
            if self.modal.is_none() && !self.busy {
                self.restore_action_view();
            }
            self.sync_watch();
            self.pin();
            return true;
        }
        if self.search_before.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Tab => {
                    self.finish_search(false);
                    return true;
                }
                KeyCode::Esc => {
                    self.finish_search(true);
                    return true;
                }
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                    self.finish_search(false);
                    if !self.busy {
                        return self.key(key);
                    }
                    return true;
                }
                KeyCode::Backspace => {
                    self.search_query_mut().pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search_query_mut().clear()
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.search_query_mut().push(c)
                }
                _ => return true,
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
                self.begin_search();
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
                    self.search_query_mut()
                        .push_str(&text.replace(['\r', '\n'], ""));
                    self.ensure_selection();
                    self.scroll = 0;
                } else if let Some(Modal::Form { text: field, .. }) = &mut self.modal {
                    field.push_str(text);
                    self.scroll = u16::MAX;
                }
                true
            }
            Event::Mouse(mouse) => {
                let previous_commit_click = if matches!(
                    mouse.kind,
                    MouseEventKind::Down(_)
                        | MouseEventKind::Drag(_)
                        | MouseEventKind::ScrollDown
                        | MouseEventKind::ScrollUp
                ) {
                    self.last_commit_click.take()
                } else {
                    None
                };
                if !self.busy && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    if self.modal.is_none() && contains(self.search_rect, mouse.column, mouse.row) {
                        self.begin_search();
                        if contains(self.search_clear_rect, mouse.column, mouse.row) {
                            self.search_query_mut().clear();
                            self.scroll = 0;
                            self.ensure_selection();
                        }
                        return true;
                    }
                    if self.popup_area.height > 0
                        && !contains(self.popup_area, mouse.column, mouse.row)
                        && mouse.row < self.screen.bottom().saturating_sub(self.footer_height())
                    {
                        if matches!(self.modal, Some(Modal::Menu { .. } | Modal::Message(_))) {
                            self.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                        }
                        return true;
                    }
                    if self.modal.is_none() && mouse.row > self.screen.y {
                        self.finish_search(false);
                        if self.busy {
                            return true;
                        }
                    }
                }
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
                                if (self.diff.is_some()
                                    || self.history.is_some()
                                    || self.modal.is_some())
                                    && mouse.row == self.screen.bottom().saturating_sub(2)
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
                    if self.can_switch_workspace() && contains(button, mouse.column, mouse.row) {
                        return false;
                    }
                    return true;
                }
                if self.modal.is_none() && self.search_before.is_none() {
                    if let Some(anchor) = self
                        .repository_button
                        .filter(|rect| contains(*rect, mouse.column, mouse.row))
                    {
                        self.repository_details();
                        self.repository_anchor = Some(anchor);
                        self.pin();
                        return true;
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
                    && let Some(anchor) = self
                        .branch_button
                        .filter(|rect| contains(*rect, mouse.column, mouse.row))
                {
                    let handled = self.key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
                    if self.busy {
                        self.branch_anchor = Some(anchor);
                    }
                    return handled;
                }
                for navigation in [false, true] {
                    let Some(row) = self.footer_row(navigation) else {
                        continue;
                    };
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
                        return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                } else if self.modal.is_some() {
                    return true;
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
                    let index = *index;
                    if let Some(history) = &self.history {
                        let Some(commit) = history.commits.get(index) else {
                            return true;
                        };
                        self.history_selected = index;
                        if previous_commit_click.as_ref().is_some_and(|(id, time)| {
                            id == &commit.id && time.elapsed() <= Duration::from_millis(450)
                        }) {
                            return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        }
                        self.last_commit_click = Some((commit.id.clone(), Instant::now()));
                    } else {
                        if self.selected == index {
                            return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        }
                        self.selected = index;
                    }
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
                Modal::Message(message) | Modal::CommitDetails(message) => std::iter::once(line(
                    if matches!(self.modal, Some(Modal::CommitDetails(_))) {
                        "Log › Commit"
                    } else {
                        "GIT · DETAILS"
                    },
                    BLUE,
                ))
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
                    if matches!(kind, FormKind::Commit { .. }) {
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
                                MenuKind::Operations => {
                                    if self.history.is_some() {
                                        "Log actions".into()
                                    } else {
                                        self.file().map_or("Repository actions".into(), |file| {
                                            format!(
                                                "{}: {}",
                                                if self.diff.is_some() {
                                                    "Diff"
                                                } else {
                                                    "Target"
                                                },
                                                safe_text(&file.label())
                                            )
                                        })
                                    }
                                }
                                MenuKind::Branches => "BRANCHES".into(),
                                MenuKind::Fetch => "FETCH FROM".into(),
                                MenuKind::Push => "PUSH TO".into(),
                            },
                            BLUE,
                        ),
                        line(format!("Search: {filter}▏"), SOFT),
                    ];
                    if matches!(kind, MenuKind::Operations)
                        && let Some(error) = &self.error
                    {
                        out.push(line(safe_text(error), RED));
                    }
                    for (i, (operation, item)) in items
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.to_lowercase().contains(&filter.to_lowercase()))
                        .enumerate()
                    {
                        self.menu_rows.push((out.len() as u16, i));
                        let enabled = !matches!(kind, MenuKind::Operations)
                            || self.operation_enabled(self.menu_action(operation));
                        out.push(Line::styled(
                            format!("{} {item}", if *selected == i { "›" } else { " " }),
                            Style::default()
                                .fg(if !enabled {
                                    SOFT
                                } else if *selected == i {
                                    GOLD
                                } else {
                                    INK
                                })
                                .add_modifier(if enabled {
                                    Modifier::empty()
                                } else {
                                    Modifier::DIM
                                }),
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
    fn view_navigation_controls(&self) -> Vec<Control> {
        let mut prev = Control::new("Prev(,)", ", Prev", KeyCode::Char(','));
        let mut next = Control::new("Next(.)", ". Next", KeyCode::Char('.'));
        if let Some(history) = &self.history {
            prev.enabled = history.page > 0;
            next.enabled = history.has_more;
        } else {
            let position = self.diff_position();
            prev.enabled = position.is_some_and(|(i, _)| i > 0);
            next.enabled = position.is_some_and(|(i, n)| i + 1 < n);
        }
        vec![prev, next]
    }
    fn controls(&self, navigation: bool) -> Vec<Control> {
        let control = |label, compact, code| Control::new(label, compact, code);
        if navigation
            && (self.actions_menu()
                || matches!(self.modal, Some(Modal::CommitDetails(_)))
                || (self.modal.is_none() && (self.diff.is_some() || self.history.is_some())))
        {
            return vec![];
        }
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
            clear.enabled = !self.search_query().is_empty();
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
            } else {
                vec![]
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
                Some(Modal::CommitDetails(_)) => vec![control("Back(Esc)", "Back", KeyCode::Esc)],
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
                    kind,
                    items,
                    selected,
                    filter,
                    ..
                }) => {
                    let actions = matches!(kind, MenuKind::Operations);
                    let mut select = control(
                        if actions { "Choose" } else { "Select(Enter)" },
                        if actions { "Choose" } else { "↵ Select" },
                        KeyCode::Enter,
                    );
                    select.enabled = items
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.to_lowercase().contains(&filter.to_lowercase()))
                        .nth(*selected)
                        .is_some_and(|(index, _)| {
                            !matches!(kind, MenuKind::Operations)
                                || self.operation_enabled(self.menu_action(index))
                        });
                    vec![
                        select,
                        control(
                            if actions { "Back" } else { "Back(Esc)" },
                            if actions { "Back" } else { "Esc Back" },
                            KeyCode::Esc,
                        ),
                    ]
                }
                None => {
                    let mut open = control("Diff(Enter)", "Diff", KeyCode::Enter);
                    open.enabled = self.file().is_some();
                    let mut actions = control("Actions(o)", "Actions", KeyCode::Char('o'));
                    actions.enabled = self.snapshot.is_some();
                    if self.history.is_some() || self.diff.is_some() {
                        let mut controls = self.view_navigation_controls();
                        controls.extend([
                            control("Refresh(r)", "Refresh", KeyCode::Char('r')),
                            actions,
                        ]);
                        controls
                    } else {
                        vec![
                            open,
                            control("Refresh(r)", "Refresh", KeyCode::Char('r')),
                            actions,
                        ]
                    }
                }
            }
        };
        if !navigation
            && (self.modal.is_none()
                || matches!(
                    self.modal,
                    Some(
                        Modal::Help(_)
                            | Modal::Message(_)
                            | Modal::CommitDetails(_)
                            | Modal::OperationResult { .. }
                    )
                ))
        {
            controls.push(control("Home(H)", "Home", KeyCode::Char('H')));
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
        let mut header: Vec<_> = vec![line("DIFF", BLUE)];
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
        self.snapshot.as_ref().map_or(String::new(), |snapshot| {
            clipped(&format!("⑂ {} ▾", snapshot.branch), usize::from(width / 2))
        })
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
        identity.spans[2].style = Style::default().fg(SOFT).add_modifier(Modifier::UNDERLINED);
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
        let headers = vec![identity, Line::from(context), Line::from(summary)];
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
    fn popup_layout(&self, area: Rect) -> Option<(Rect, Rect, &'static str)> {
        let (height, title) = match &self.modal {
            Some(Modal::Menu {
                kind,
                items,
                filter,
                ..
            }) => (
                (items
                    .iter()
                    .filter(|item| item.to_lowercase().contains(&filter.to_lowercase()))
                    .count() as u16
                    + 4)
                .min(16),
                match kind {
                    MenuKind::Operations => " Actions ",
                    MenuKind::Branches => " Branches ",
                    MenuKind::Fetch => " Fetch ",
                    MenuKind::Push => " Push ",
                },
            ),
            Some(Modal::Message(message)) => (
                16,
                if message.starts_with("REPOSITORY\n") {
                    " Repository "
                } else {
                    " Details "
                },
            ),
            Some(Modal::Confirm { .. }) => (12, " Confirm "),
            Some(Modal::Form { kind, .. }) => (
                if matches!(kind, FormKind::Commit { .. }) {
                    16
                } else {
                    8
                },
                " Input ",
            ),
            _ => return None,
        };
        let width = area.width.min(56);
        let anchor = if matches!(&self.modal, Some(Modal::Message(message)) if message.starts_with("REPOSITORY\n"))
        {
            self.repository_anchor
        } else if matches!(
            self.modal,
            Some(Modal::Menu {
                kind: MenuKind::Branches,
                ..
            })
        ) {
            self.branch_anchor
        } else {
            None
        };
        let y = anchor.map(|rect| rect.bottom().clamp(area.y, area.bottom()));
        let height = height.min(area.bottom().saturating_sub(y.unwrap_or(area.y)));
        let rect = Rect::new(
            anchor.map_or(area.x + area.width.saturating_sub(width) / 2, |rect| {
                rect.x.clamp(area.x, area.right().saturating_sub(width))
            }),
            y.unwrap_or_else(|| area.bottom().saturating_sub(height)),
            width,
            height,
        );
        let mut inner = if width >= 4 && height >= 3 {
            rect.inner(Margin::new(1, 1))
        } else {
            rect
        };
        inner.width = inner.width.saturating_sub(1);
        Some((rect, inner, title))
    }
    pub(super) fn draw(&mut self, frame: &mut ratatui::Frame) {
        let screen = frame.area();
        self.screen = screen;
        self.branch_button = None;
        self.repository_button = None;
        self.pin_button = None;
        self.search_rect = Rect::default();
        self.search_clear_rect = Rect::default();
        self.popup_area = Rect::default();
        self.ensure_selection();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(INK)),
            screen,
        );
        if screen.width == 0 || screen.height == 0 {
            return;
        }
        let area = screen;
        let mini = screen.height < 7;
        let footer_height = self.footer_height();
        let popup = self.popup_layout(Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1 + footer_height),
        ));
        let showing_background = self.modal.is_none() || popup.is_some();
        let mut showing_diff = self.diff.is_some() && showing_background;
        let showing_history = self.history.is_some() && showing_background;
        let search_height = if !showing_diff && showing_background {
            if area.height >= 10 { 3 } else { 1 }
        } else {
            0
        };
        let mut headers = if showing_diff {
            self.diff_header(area.width.saturating_sub(1))
        } else if showing_history {
            let history = self.history.as_ref().unwrap();
            let mut header = vec![line("LOG", BLUE)];
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
            header.extend(wrap_line(
                line("Enter: details · ,/.: pages · Esc: files", GOLD),
                usize::from(area.width.saturating_sub(1).max(1)),
            ));
            header
        } else if showing_background {
            self.repository_header(area.width.saturating_sub(1))
        } else {
            vec![]
        };
        headers.truncate(usize::from(
            area.height
                .saturating_sub(footer_height + search_height + 2),
        ));
        let header_height = headers.len() as u16;
        let (header_area, reserve) = pulse_header_area(screen);
        draw_page_header(
            frame,
            header_area,
            reserve,
            true,
            self.can_switch_workspace(),
        );
        if header_area.width >= 10 {
            frame.render_widget(
                Paragraph::new("?(?)").style(Style::default().fg(SOFT).add_modifier(
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
        let previous_width = self.view_width;
        self.body = Rect::new(
            area.x,
            area.y
                .saturating_add(1 + header_height + search_height)
                .min(area.bottom()),
            area.width.saturating_sub(1),
            area.height
                .saturating_sub(1 + header_height + search_height + footer_height),
        );
        self.view_width = self.body.width;
        if search_height > 0 {
            self.search_rect = Rect::new(
                area.x,
                area.y + 1 + header_height,
                area.width,
                search_height,
            );
            self.search_clear_rect = draw_search_field(
                frame,
                self.search_rect,
                self.search_query(),
                self.search_before.is_some(),
                !self.busy && self.modal.is_none(),
                if showing_history {
                    "Search commits (/)"
                } else {
                    "Search files (/)"
                },
            );
        }
        if let Some((rect, inner, title)) = popup {
            let modal = self.modal.take();
            let background = self.lines();
            let background_scroll = self.return_scroll.unwrap_or(self.scroll).min(
                (background.len().min(u16::MAX as usize) as u16).saturating_sub(self.body.height),
            );
            frame.render_widget(
                Paragraph::new(background).scroll((
                    background_scroll,
                    if showing_diff && self.nowrap {
                        self.horizontal
                    } else {
                        0
                    },
                )),
                self.body,
            );
            self.modal = modal;
            self.popup_area = rect;
            frame.render_widget(Clear, rect);
            let block = if matches!(
                self.modal,
                Some(Modal::Menu {
                    kind: MenuKind::Operations | MenuKind::Branches,
                    ..
                })
            ) || matches!(&self.modal, Some(Modal::Message(message)) if message.starts_with("REPOSITORY\n"))
            {
                popup_block(rect, title, false)
            } else {
                Block::default()
                    .borders(if rect.width >= 4 && rect.height >= 3 {
                        Borders::ALL
                    } else {
                        Borders::NONE
                    })
                    .title(title)
                    .border_style(Style::default().fg(SOFT))
                    .style(Style::default().bg(BG))
            };
            frame.render_widget(block, rect);
            self.body = inner;
            showing_diff = false;
        }
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
            && !self.menu_scroll_manual
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
        if !mini
            && !self.actions_menu()
            && ((self.modal.is_some() && !matches!(self.modal, Some(Modal::CommitDetails(_))))
                || (self.diff.is_none() && self.history.is_none())
                || self.view_status_visible())
        {
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
                "Updating Diff…".into()
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
                    Modal::CommitDetails(_) => "",
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
                Rect::new(area.x, screen.bottom() - footer_height, area.width, 1),
            );
        }
        for navigation in [true, false] {
            let Some(row) = self.footer_row(navigation) else {
                continue;
            };
            let controls = self.controls(navigation);
            for button in control_buttons(screen, row, &controls) {
                frame.render_widget(
                    Paragraph::new(button.label)
                        .alignment(Alignment::Center)
                        .style(footer_style(button.key, button.enabled)),
                    button.rect,
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
    fn tiny_label(&self) -> &'static str {
        match self.key.code {
            KeyCode::Enter => "↵",
            KeyCode::Esc => "Esc",
            KeyCode::Up => "↑",
            KeyCode::Down => "↓",
            KeyCode::PageUp => "PgUp",
            KeyCode::PageDown => "PgDn",
            KeyCode::Char('H') => "H",
            KeyCode::Char('o') => "o",
            KeyCode::Char('r') => "r",
            KeyCode::Char('l') => "l",
            KeyCode::Char('u') => "u",
            KeyCode::Char(',') => ",",
            KeyCode::Char('.') => ".",
            KeyCode::Char('w') => "w",
            _ => self.compact,
        }
    }
}
fn control_buttons(screen: Rect, row: u16, controls: &[Control]) -> Vec<FooterButton> {
    layout_footer(
        Rect::new(screen.x, row, screen.width, 1),
        controls
            .iter()
            .map(|control| {
                footer_control(
                    [control.label, control.compact, control.tiny_label()],
                    control.key.code,
                    control.enabled,
                )
                .modified(control.key.modifiers)
            })
            .collect(),
    )
}
fn control_rects(screen: Rect, row: u16, controls: &[Control]) -> Vec<Rect> {
    control_buttons(screen, row, controls)
        .into_iter()
        .map(|button| button.rect)
        .collect()
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
pub(super) fn safe_text(text: &str) -> String {
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
pub(super) fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
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
                .any(|c| c.key.code == KeyCode::Char('H') && c.label.contains("Home"))
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
                    assert!(text.contains("Actions"));
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
    fn repository_and_branch_popups_match_actions_border_and_open_below_names() {
        let corners = |buffer: &ratatui::buffer::Buffer, rect: Rect| {
            [
                (rect.x, rect.y),
                (rect.right() - 1, rect.y),
                (rect.x, rect.bottom() - 1),
                (rect.right() - 1, rect.bottom() - 1),
            ]
            .map(|position| buffer[position].clone())
        };
        for (width, height) in [(20, 8), (32, 12), (48, 30), (100, 40)] {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            let (send, receive) = mpsc::sync_channel(4);
            let (updates, responses) = mpsc::channel();
            pane.requests = Some(send);
            pane.updates = Some(responses);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| pane.draw(f)).unwrap();
            pane.operations();
            terminal.draw(|f| pane.draw(f)).unwrap();
            let expected = corners(terminal.backend().buffer(), pane.popup_area);
            pane.key(key(KeyCode::Esc));
            terminal.draw(|f| pane.draw(f)).unwrap();
            let repository = pane.repository_button.unwrap();
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: repository.x,
                row: repository.y,
                modifiers: KeyModifiers::NONE,
            }));
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert_eq!(pane.popup_area.y, repository.bottom());
            assert_eq!(
                corners(terminal.backend().buffer(), pane.popup_area),
                expected
            );
            pane.key(key(KeyCode::Esc));
            terminal.draw(|f| pane.draw(f)).unwrap();
            let branch = pane.branch_button.unwrap();
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: branch.x,
                row: branch.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(matches!(
                receive.try_recv().unwrap(),
                Request::Menu(_, MenuKind::Branches)
            ));
            updates
                .send(Response::Menu(
                    MenuKind::Branches,
                    Ok(vec!["main".into(), "topic".into()]),
                ))
                .unwrap();
            pane.poll();
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert_eq!(pane.popup_area.y, branch.bottom());
            assert_eq!(pane.popup_area.x, branch.x.min(width - width.min(56)));
            assert!(pane.popup_area.bottom() <= height - pane.footer_height());
            assert_eq!(
                corners(terminal.backend().buffer(), pane.popup_area),
                expected
            );
            // Clicking above the menu dismisses it without reopening a popup.
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: branch.x,
                row: branch.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(pane.modal.is_none());
            assert!(receive.try_recv().is_err());
            pane.key(key(KeyCode::Char('b')));
            assert!(pane.branch_anchor.is_none());
            assert!(matches!(
                receive.try_recv().unwrap(),
                Request::Menu(_, MenuKind::Branches)
            ));
            updates
                .send(Response::Menu(
                    MenuKind::Branches,
                    Ok(vec!["main".into(), "topic".into()]),
                ))
                .unwrap();
            pane.poll();
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert_eq!(pane.popup_area.bottom(), height - pane.footer_height());
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
        assert!(text.contains('⑂'));
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
    fn actions_wrap_and_hunk_navigation_use_visual_rows() {
        let mut pane = diff_pane();
        render(&mut pane, 48, 20);
        pane.key(key(KeyCode::Char(']')));
        assert_eq!(pane.hunk, 1);
        assert_eq!(pane.scroll, pane.hunk_rows[1].min(pane.limit));
        let anchor = pane.diff_rows[usize::from(pane.scroll)];
        click_action(&mut pane, 18);
        assert!(pane.nowrap);
        render(&mut pane, 48, 20);
        assert!(pane.diff_rows[usize::from(pane.scroll)] <= anchor);
        pane.operations();
        assert!(
            matches!(&pane.modal, Some(Modal::Menu { items, .. }) if items.iter().any(|item| item == "Wrap OFF(w)"))
        );
        pane.key(key(KeyCode::Esc));
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
        assert!(text.contains("Actions"));
        assert!(pane.operation_enabled(13));
        assert!(!text.contains("Discard(d)"));
        let controls = pane.controls(false);
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
                for button in control_buttons(pane.screen, 0, &controls) {
                    assert!(button.label.width() <= usize::from(button.rect.width));
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
                render(&mut unstaged, width, 20);
                click_action(&mut unstaged, if block { 14 } else { 2 });
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
            render(&mut staged, width, 20);
            click_action(&mut staged, 1);
            assert!(
                matches!(receive.try_recv().unwrap(), Request::Execute(_, Action::Unstage(file)) if file.path.as_path() == std::path::Path::new("staged.rs"))
            );
        }
        render(&mut pane, 48, 20);
        assert!(!pane.controls(false)[0].enabled);
        let home = pane.controls(false).len() - 1;
        click_footer(&mut pane, false, home);
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
        click_action(&mut pane, 12);
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
    fn status_is_above_the_single_button_row() {
        let mut pane = diff_pane();
        pane.notice = "Staged all changes".into();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(48, 20)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let row = |y| {
            (0..48)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(row(18).contains("Staged all changes"));
        assert!(row(19).contains("Actions"));
        assert!(row(19).contains("Prev") && row(19).contains("Next"));
        assert!(!row(19).contains("Wrap"));
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
            assert!(
                text.contains("GIT")
                    && text.contains(match width {
                        20 => "[T]",
                        32 => "TOKEN",
                        _ => "[TOKEN]",
                    })
            );
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
        let row = pane.footer_row(navigation).expect("Footer row visible");
        let rect = control_rects(pane.screen, row, &pane.controls(navigation))[index];
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }
    fn click_action(pane: &mut GitPane, index: usize) {
        pane.key(key(KeyCode::Char('o')));
        let index = pane
            .action_ids
            .iter()
            .position(|action| *action == index)
            .expect("Action unavailable on this page");
        let Some(Modal::Menu { selected, .. }) = &mut pane.modal else {
            panic!("Actions missing")
        };
        *selected = index;
        render(pane, pane.screen.width, pane.screen.height);
        let row = pane
            .menu_rows
            .iter()
            .find(|(_, item)| *item == index)
            .unwrap()
            .0;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.body.x,
            row: pane.body.y + row - pane.scroll,
            modifiers: KeyModifiers::NONE,
        }));
    }
    #[test]
    fn actions_and_repository_popups_preserve_background_and_return_to_parent() {
        for (width, height) in [(20, 8), (32, 18), (48, 30), (100, 40)] {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            let (send, receive) = mpsc::sync_channel(4);
            pane.requests = Some(send);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| pane.draw(f)).unwrap();
            let before = terminal.backend().buffer().clone();
            let save = |buffer: &ratatui::buffer::Buffer, name: &str| {
                if let Some(path) = std::env::var_os("MUX_TEST_FILE_PREVIEW") {
                    let cells: Vec<_> = buffer
                        .content
                        .iter()
                        .map(|cell| {
                            (
                                cell.symbol(),
                                format!("{:?}", cell.fg),
                                format!("{:?}", cell.bg),
                                cell.modifier.contains(Modifier::BOLD),
                            )
                        })
                        .collect();
                    std::fs::write(
                        PathBuf::from(path).join(format!("git-{name}-{width}.json")),
                        serde_json::to_vec(
                            &serde_json::json!({"width": width, "height": height, "cells": cells}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
            };
            save(&before, "files");
            assert_eq!(pane.body.x, 0);
            assert_eq!(pane.search_rect.x, 0);
            assert_eq!(pane.search_rect.width, width);
            pane.key(key(KeyCode::Char('o')));
            terminal.draw(|f| pane.draw(f)).unwrap();
            save(terminal.backend().buffer(), "actions");
            assert!(pane.popup_area.height > 0 && pane.popup_area.bottom() == height - 1);
            assert!(pane.body.right() < pane.popup_area.right());
            assert!(pane.controls(true).is_empty());
            assert_eq!(
                pane.controls(false)
                    .iter()
                    .map(|control| control.label)
                    .collect::<Vec<_>>(),
                vec!["Choose", "Back"]
            );
            pane.key(key(KeyCode::Down));
            assert!(matches!(pane.modal, Some(Modal::Menu { selected: 2, .. })));
            click_footer(&mut pane, false, 1);
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert!(pane.modal.is_none());
            assert!(before.diff(terminal.backend().buffer()).is_empty());
            click_action(&mut pane, 16);
            terminal.draw(|f| pane.draw(f)).unwrap();
            save(terminal.backend().buffer(), "repository");
            assert!(
                matches!(&pane.modal, Some(Modal::Message(message)) if message.contains("REPOSITORY") && message.contains("Branch:"))
            );
            pane.key(key(KeyCode::Esc));
            assert!(
                matches!(&pane.modal, Some(Modal::Menu { kind: MenuKind::Operations, selected, .. }) if pane.menu_action(*selected) == 16)
            );
            pane.key(key(KeyCode::Esc));
            render(&mut pane, width, height);
            let project = pane.repository_button.unwrap();
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: project.x,
                row: project.y,
                modifiers: KeyModifiers::NONE,
            }));
            terminal.draw(|f| pane.draw(f)).unwrap();
            save(terminal.backend().buffer(), "repository-anchored");
            assert_eq!(pane.popup_area.y, project.bottom());
            assert_eq!(pane.popup_area.x, project.x);
            assert!(pane.popup_area.bottom() <= height - 3);
            if pane.popup_area.bottom() < height - 3 {
                pane.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: project.x,
                    row: pane.popup_area.bottom(),
                    modifiers: KeyModifiers::NONE,
                }));
            } else {
                pane.key(key(KeyCode::Esc));
            }
            assert!(pane.modal.is_none());
            render(&mut pane, width, height);
            pane.key(key(KeyCode::Char('o')));
            if let Some(Modal::Menu { selected, .. }) = &mut pane.modal {
                *selected = 7;
            }
            render(&mut pane, width, height);
            click_footer(&mut pane, false, 0);
            assert!(matches!(
                pane.modal,
                Some(Modal::Form {
                    kind: FormKind::Create,
                    ..
                })
            ));
            pane.input(&Event::Paste("new-branch".into()));
            pane.key(key(KeyCode::Enter));
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert!(matches!(
                pane.modal,
                Some(Modal::Confirm {
                    action: Action::Create(_),
                    ..
                })
            ));
            pane.key(key(KeyCode::Esc));
            assert!(matches!(pane.modal, Some(Modal::Menu { selected: 7, .. })));
            pane.key(key(KeyCode::Esc));
            assert!(receive.try_recv().is_err());
        }
    }

    #[test]
    fn footer_actions_stay_next_to_home_and_button_gaps_ignore_clicks() {
        for width in [20, 32, 48, 100] {
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
                let (send, receive) = mpsc::sync_channel(4);
                pane.requests = Some(send);
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                terminal.draw(|f| pane.draw(f)).unwrap();
                let row = pane.footer_row(false).unwrap();
                let buttons = control_buttons(pane.screen, row, &pane.controls(false));
                assert_eq!(buttons[buttons.len() - 1].key, KeyCode::Char('H'));
                assert_eq!(buttons[buttons.len() - 2].key, KeyCode::Char('o'));
                for pair in buttons.windows(2) {
                    assert_eq!(pair[1].rect.x, pair[0].rect.right() + 1);
                    let gap = pair[0].rect.right();
                    assert_eq!(terminal.backend().buffer()[(gap, row)].bg, BG);
                    pane.input(&Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: gap,
                        row,
                        modifiers: KeyModifiers::NONE,
                    }));
                    assert!(pane.modal.is_none());
                    assert!(receive.try_recv().is_err());
                }
                let actions = &buttons[buttons.len() - 2];
                pane.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: actions.rect.right() - 1,
                    row,
                    modifiers: KeyModifiers::NONE,
                }));
                assert!(matches!(
                    pane.modal,
                    Some(Modal::Menu {
                        kind: MenuKind::Operations,
                        ..
                    })
                ));
                assert!(receive.try_recv().is_err());
            }
        }
    }

    #[test]
    fn diff_and_log_navigation_uses_bottom_row_without_hint_line() {
        for width in [20, 32, 48, 100] {
            for log in [false, true] {
                let mut pane = diff_pane();
                if log {
                    pane.diff = None;
                    pane.history = Some(service::History {
                        commits: vec![],
                        page: 1,
                        has_more: true,
                        query: "topic".into(),
                    });
                }
                let (send, receive) = mpsc::sync_channel(4);
                pane.requests = Some(send);
                let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
                terminal.draw(|f| pane.draw(f)).unwrap();
                let row = |y| {
                    (0..width)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                };
                assert_eq!(pane.footer_height(), 1);
                assert_eq!(pane.footer_row(false), Some(19));
                assert_eq!(pane.footer_row(true), None);
                if width >= 48 {
                    assert!(row(19).contains("Prev") && row(19).contains("Next"));
                }
                let buttons = control_buttons(pane.screen, 19, &pane.controls(false));
                assert!(
                    buttons
                        .iter()
                        .all(|button| button.rect.y == 19 && button.rect.width > 0)
                );
                assert!(pane.controls(true).is_empty());
                let text = (0..20).map(row).collect::<Vec<_>>().join("\n");
                assert!(!text.contains("Files(Esc)"));
                assert!(!text.contains("scroll"));
                assert!(!text.contains("Wrap"));
                assert_eq!(
                    pane.view_navigation_controls()
                        .iter()
                        .map(|c| c.key.code)
                        .collect::<Vec<_>>(),
                    vec![KeyCode::Char(','), KeyCode::Char('.')]
                );
                click_footer(&mut pane, false, 0);
                if log {
                    assert!(
                        matches!(receive.try_recv().unwrap(), Request::History(_, 0, query) if query == "topic")
                    );
                } else {
                    assert!(receive.try_recv().is_err());
                    click_footer(&mut pane, false, 1);
                    assert!(
                        matches!(receive.try_recv().unwrap(), Request::Diff(_, file, false) if file.path == std::path::Path::new("staged.rs"))
                    );
                }
            }
        }
    }

    #[test]
    fn actions_follow_log_and_diff_context_and_choose_preserves_targets() {
        for width in [20, 32, 48] {
            let mut log = diff_pane();
            log.diff = None;
            log.history = Some(service::History {
                commits: vec![],
                page: 2,
                has_more: true,
                query: "topic".into(),
            });
            let (send, receive) = mpsc::sync_channel(4);
            log.requests = Some(send);
            render(&mut log, width, 30);
            log.key(key(KeyCode::Char('o')));
            let Some(Modal::Menu { items, .. }) = &log.modal else {
                panic!()
            };
            assert!(items.iter().any(|item| item == "Older commits"));
            assert!(
                !items
                    .iter()
                    .any(|item| item.contains("Stage") || item.contains("Branch"))
            );
            for ch in "older".chars() {
                log.key(key(KeyCode::Char(ch)));
            }
            let text = render(&mut log, width, 30);
            assert!(text.contains("Choose") && text.contains("Back"));
            assert!(!text.contains("Type to filter"));
            assert!(log.controls(true).is_empty());
            assert_eq!(log.controls(false).len(), 2);
            click_footer(&mut log, false, 0);
            assert!(
                matches!(receive.try_recv().unwrap(), Request::History(root, 3, query) if root == std::path::Path::new("/project") && query == "topic")
            );
            log.busy = false;
            let original = log.scroll;
            click_action(&mut log, 27);
            assert!(log.search_before.is_some() && log.modal.is_none());
            log.input(&Event::Paste("new query".into()));
            log.key(key(KeyCode::Esc));
            assert_eq!(log.search_query(), "topic");
            assert_eq!(log.scroll, original);
            assert!(receive.try_recv().is_err());

            let mut diff = diff_pane();
            let (send, receive) = mpsc::sync_channel(4);
            diff.requests = Some(send);
            render(&mut diff, width, 30);
            diff.scroll = 3.min(diff.limit);
            let anchor = diff.diff_rows[usize::from(diff.scroll)];
            diff.key(key(KeyCode::Char('o')));
            let Some(Modal::Menu { items, .. }) = &diff.modal else {
                panic!()
            };
            assert!(items.iter().any(|item| item == "Stage selected block"));
            assert!(
                !items
                    .iter()
                    .any(|item| item == "Fetch" || item == "Older commits")
            );
            assert!(items.iter().any(|item| item == "Wrap ON(w)"));
            for ch in "wrap".chars() {
                diff.key(key(KeyCode::Char(ch)));
            }
            let text = render(&mut diff, width, 30);
            assert!(text.contains("Choose") && text.contains("Back"));
            assert!(!text.contains("Type to filter"));
            let border = diff.popup_area;
            diff.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: border.x,
                row: border.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(diff.actions_menu());
            assert!(receive.try_recv().is_err());
            click_footer(&mut diff, false, 0);
            assert!(diff.nowrap && diff.modal.is_none());
            render(&mut diff, width, 30);
            assert!(
                diff.diff_rows
                    .iter()
                    .skip(usize::from(diff.scroll))
                    .take(usize::from(diff.body.height))
                    .any(|line| *line == anchor)
            );
            assert!(receive.try_recv().is_err());
            click_action(&mut diff, 20);
            assert!(
                matches!(receive.try_recv().unwrap(), Request::Diff(root, file, false) if root == std::path::Path::new("/project") && file.path == std::path::Path::new("staged.rs") && file.group == Group::Index)
            );
        }
    }

    #[test]
    fn log_double_click_requires_same_commit_within_time_limit() {
        for width in [20, 48, 100] {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            pane.history = Some(service::History {
                commits: (0..2)
                    .map(|index| service::Commit {
                        id: format!("commit{index}"),
                        short: format!("{index:07x}"),
                        date: "2026-10-14".into(),
                        author: "Author".into(),
                        subject: "Commit".into(),
                        refs: String::new(),
                    })
                    .collect(),
                page: 0,
                has_more: false,
                query: String::new(),
            });
            let (send, receive) = mpsc::sync_channel(4);
            pane.requests = Some(send);
            render(&mut pane, width, 30);
            assert!(
                !pane
                    .controls(false)
                    .iter()
                    .any(|c| c.key.code == KeyCode::Enter)
            );
            let click = |pane: &GitPane, index| {
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: pane.body.x,
                    row: pane.body.y + pane.rows.iter().find(|(_, i)| *i == index).unwrap().0
                        - pane.scroll,
                    modifiers: KeyModifiers::NONE,
                })
            };
            pane.input(&click(&pane, 0));
            assert!(receive.try_recv().is_err());
            pane.last_commit_click =
                Some(("commit0".into(), Instant::now() - Duration::from_secs(1)));
            pane.input(&click(&pane, 0));
            assert!(receive.try_recv().is_err());
            pane.input(&click(&pane, 1));
            assert_eq!(pane.history_selected, 1);
            assert!(receive.try_recv().is_err());
            // Mouse release events between clicks preserve the double-click sequence.
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: pane.body.x,
                row: pane.body.y,
                modifiers: KeyModifiers::NONE,
            }));
            pane.input(&click(&pane, 1));
            assert!(matches!(receive.try_recv().unwrap(), Request::Show(_, id) if id == "commit1"));
            pane.input(&click(&pane, 1));
            assert!(receive.try_recv().is_err());
        }
    }

    #[test]
    fn log_actions_open_commit_as_a_page_and_back_restores_history() {
        for width in [20, 32, 48, 100] {
            let mut pane = diff_pane();
            pane.diff = None;
            pane.history = Some(service::History {
                commits: vec![service::Commit {
                    id: "abc123".into(),
                    short: "abc123".into(),
                    date: "2026-10-14".into(),
                    author: "Author".into(),
                    subject: "Selected commit".into(),
                    refs: String::new(),
                }],
                page: 2,
                has_more: true,
                query: "topic".into(),
            });
            let (send, receive) = mpsc::sync_channel(4);
            let (updates, responses) = mpsc::channel();
            pane.requests = Some(send);
            pane.updates = Some(responses);
            render(&mut pane, width, 20);
            let original = pane.scroll;
            click_action(&mut pane, 24);
            assert!(matches!(receive.try_recv().unwrap(), Request::Show(_, id) if id == "abc123"));
            updates
                .send(Response::Show(Ok(
                    "Full commit message\n+patch\n".repeat(60)
                )))
                .unwrap();
            pane.poll();
            let text = render(&mut pane, width, 20);
            assert!(text.contains("Log › Commit"));
            assert_eq!(pane.popup_area, Rect::default());
            assert_eq!(pane.body.width, width - 1);
            assert!(pane.can_switch_workspace());
            assert!(pane.action_return.is_none());
            assert!(pane.controls(true).is_empty());
            pane.key(key(KeyCode::End));
            assert!(pane.scroll > 0);
            pane.key(key(KeyCode::Left));
            render(&mut pane, width, 20);
            assert!(pane.modal.is_none());
            assert_eq!(pane.scroll, original);
            assert_eq!(pane.history.as_ref().unwrap().page, 2);
            assert_eq!(pane.history.as_ref().unwrap().query, "topic");
            assert_eq!(pane.history_selected, 0);
            pane.key(key(KeyCode::Enter));
            assert!(matches!(receive.try_recv().unwrap(), Request::Show(_, _)));
            updates
                .send(Response::Show(Err("Commit no longer available".into())))
                .unwrap();
            pane.poll();
            assert!(matches!(pane.modal, Some(Modal::Message(_))));
            render(&mut pane, width, 20);
            pane.key(key(KeyCode::Esc));
            render(&mut pane, width, 20);
            assert_eq!(pane.scroll, original);
            assert_eq!(pane.history.as_ref().unwrap().query, "topic");
        }
    }

    #[test]
    fn choose_next_block_uses_diff_scroll_limits_after_leaving_actions() {
        let mut pane = diff_pane();
        let (send, receive) = mpsc::sync_channel(4);
        pane.requests = Some(send);
        render(&mut pane, 32, 12);
        let target = pane.hunk_rows[1].min(pane.limit);
        assert!(target > 0);
        pane.key(key(KeyCode::Char('o')));
        for ch in "next block".chars() {
            pane.key(key(KeyCode::Char(ch)));
        }
        render(&mut pane, 32, 12);
        click_footer(&mut pane, false, 0);
        assert!(pane.modal.is_none());
        assert_eq!(pane.hunk, 1);
        assert_eq!(pane.scroll, target);
        assert!(receive.try_recv().is_err());
    }

    #[test]
    fn inline_search_mouse_clear_cancel_and_arrows_keep_files_interactive() {
        for (width, height) in [(20, 8), (32, 12), (48, 30)] {
            let mut pane = GitPane::default();
            pane.apply_snapshot(Some(snapshot("/project")));
            let (send, receive) = mpsc::sync_channel(4);
            pane.requests = Some(send);
            render(&mut pane, width, height);
            let rect = pane.search_rect;
            assert!(rect.height > 0 && rect.bottom() <= pane.body.y);
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }));
            pane.input(&Event::Paste("staged".into()));
            assert_eq!(pane.visible_files(), vec![1]);
            assert_eq!(pane.selected, 1);
            pane.key(key(KeyCode::Esc));
            assert!(pane.filter.is_empty());
            assert_eq!(pane.selected, 0);
            pane.key(key(KeyCode::Char('/')));
            pane.input(&Event::Paste("staged".into()));
            render(&mut pane, width, height);
            let clear = pane.search_clear_rect;
            pane.input(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: clear.x,
                row: clear.y,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(pane.filter.is_empty());
            pane.key(key(KeyCode::Down));
            assert!(pane.search_before.is_none() && pane.modal.is_none());
            assert_eq!(pane.selected, 1);
            assert!(receive.try_recv().is_err());
        }
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
        let mut pane = GitPane::default();
        pane.apply_snapshot(Some(snapshot("/project")));
        pane.history = Some(service::History {
            commits: vec![],
            page: 0,
            has_more: false,
            query: "topic".into(),
        });
        let (send, receive) = mpsc::sync_channel(2);
        pane.requests = Some(send);
        render(&mut pane, 80, 20);
        let clear = pane.search_clear_rect;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: clear.x,
            row: clear.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(pane.search_query().is_empty());
        assert!(pane.search_before.is_some());
        assert!(pane.modal.is_none());
        assert!(receive.try_recv().is_err());
        assert!(!pane.controls(false)[1].enabled);
        pane.key(key(KeyCode::Enter));
        assert!(
            matches!(receive.try_recv().unwrap(), Request::History(_, 0, query) if query.is_empty())
        );
    }
    #[test]
    fn details_return_to_their_page_and_home_stays_in_git() {
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
                    !navigation
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
                assert!(text.contains("?(?)"));
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
                        pane.controls(false)
                            .iter()
                            .map(|control| control.key.code)
                            .collect::<Vec<_>>(),
                        vec![
                            KeyCode::Enter,
                            KeyCode::Char('r'),
                            KeyCode::Char('o'),
                            KeyCode::Char('H')
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
    fn popup_wheel_reverses_scroll_immediately_and_rail_reaches_both_ends() {
        for width in [20, 32, 48] {
            for kind in 0..5 {
                let mut pane = GitPane::default();
                pane.apply_snapshot(Some(snapshot("/project")));
                let (send, receive) = mpsc::sync_channel(4);
                pane.requests = Some(send);
                match kind {
                    0 => pane.operations(),
                    1 => {
                        pane.modal = Some(Modal::Menu {
                            kind: MenuKind::Branches,
                            items: (0..40).map(|i| format!("branch-{i}")).collect(),
                            selected: 0,
                            filter: String::new(),
                        })
                    }
                    2 => pane.modal = Some(Modal::Message("Detail line\n".repeat(60))),
                    3 => {
                        pane.modal = Some(Modal::Confirm {
                            action: Action::StageAll,
                            description: "Confirm line\n".repeat(60).into(),
                        })
                    }
                    _ => {
                        pane.modal = Some(Modal::Form {
                            kind: FormKind::Commit {
                                head: None,
                                index: vec![],
                            },
                            text: "Draft line\n".repeat(60),
                        })
                    }
                }
                render(&mut pane, width, 20);
                assert!(pane.limit > 0);
                if kind < 2 {
                    pane.key(key(KeyCode::End));
                } else {
                    pane.scroll_to(pane.limit);
                }
                render(&mut pane, width, 20);
                let bottom = pane.scroll;
                assert!(bottom > 0);
                pane.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: pane.body.x,
                    row: pane.body.y,
                    modifiers: KeyModifiers::NONE,
                }));
                render(&mut pane, width, 20);
                assert!(
                    pane.scroll < bottom,
                    "Up wheel did not move popup kind {kind}, width {width}"
                );
                let up = pane.scroll;
                pane.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: pane.body.x,
                    row: pane.body.y,
                    modifiers: KeyModifiers::NONE,
                }));
                render(&mut pane, width, 20);
                assert!(pane.scroll > up);
                for down in [false, true, false] {
                    pane.input(&Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Drag(MouseButton::Left),
                        column: pane.body.right(),
                        row: if down {
                            pane.body.bottom() - 1
                        } else {
                            pane.body.y
                        },
                        modifiers: KeyModifiers::NONE,
                    }));
                    render(&mut pane, width, 20);
                    assert_eq!(pane.scroll, if down { pane.limit } else { 0 });
                }
                if kind < 2 {
                    pane.key(key(KeyCode::PageDown));
                    render(&mut pane, width, 20);
                    assert!(!pane.menu_scroll_manual);
                    let Some(Modal::Menu { selected, .. }) = &pane.modal else {
                        panic!()
                    };
                    let row = pane
                        .menu_rows
                        .iter()
                        .find(|(_, index)| index == selected)
                        .unwrap()
                        .0;
                    assert!(row >= pane.scroll && row < pane.scroll + pane.body.height);
                }
                if kind == 4 {
                    assert!(
                        matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == &"Draft line\n".repeat(60))
                    );
                }
                assert!(receive.try_recv().is_err());
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
            click_action(&mut pane, 0);
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
        pane.key(key(KeyCode::Char('/')));
        assert!(pane.search_before.is_some());
        assert!(pane.modal.is_none());
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
        assert!(text.contains("git"));
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
        assert!(text.contains("LOG"));
        assert!(!pane.controls(false)[0].enabled);
        assert!(pane.controls(false)[1].enabled);
        pane.key(key(KeyCode::End));
        render(&mut pane, 48, 20);
        assert_eq!(pane.history_selected, 49);
        let history_scroll = pane.scroll;
        let row = pane.rows.iter().find(|(_, i)| *i == 49).unwrap().0 - pane.scroll + pane.body.y;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(receive.try_recv().is_err());
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
            .send(Response::Show(Ok(format!(
                "commit details\nFull message\n{}",
                "+new content\n".repeat(80)
            ))))
            .unwrap();
        pane.poll();
        let text = render(&mut pane, 48, 20);
        assert!(text.contains("Full message") && text.contains("Log › Commit"));
        assert!(matches!(pane.modal, Some(Modal::CommitDetails(_))));
        assert_eq!(pane.popup_area, Rect::default());
        assert_eq!(pane.search_rect, Rect::default());
        assert_eq!(pane.body.width, 47);
        assert_eq!(pane.footer_height(), 1);
        pane.key(key(KeyCode::End));
        assert_eq!(pane.scroll, pane.limit);
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: pane.body.x,
            row: pane.body.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(pane.scroll < pane.limit);
        click_footer(&mut pane, false, 0);
        render(&mut pane, 48, 20);
        assert_eq!(pane.history_selected, 49);
        assert_eq!(pane.scroll, history_scroll);
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
        assert!(!pane.controls(false)[1].enabled);
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
        assert!(!pane.controls(false)[0].enabled);
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
        assert!(!pane.controls(false)[0].enabled);
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
        assert!(!pane.operation_enabled(12));
        assert!(!pane.operation_enabled(14));
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
        assert!(controls.is_empty());
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
    fn special_file_diff_actions_disable_blocks_and_keep_whole_file_operations() {
        let mut pane = diff_pane();
        pane.diff.as_mut().unwrap().reason = Some("Binary file: whole-file operations only".into());
        assert!(pane.operation_enabled(0) && pane.operation_enabled(2));
        assert!(!pane.operation_enabled(12) && !pane.operation_enabled(14));
        assert!(!pane.operation_enabled(21) && !pane.operation_enabled(22));
        let controls = pane.controls(false);
        assert_eq!(controls[controls.len() - 2].key.code, KeyCode::Char('o'));
        let mut index = pane.diff.clone().unwrap();
        index.reason = None;
        index.file.group = Group::Index;
        pane.diff = Some(index);
        assert!(pane.operation_enabled(1) && pane.operation_enabled(13));
        assert!(!pane.operation_enabled(0) && !pane.operation_enabled(12));
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
        click_action(&mut pane, 12);
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
    #[test]
    fn home_button_clears_git_subviews_without_switching_feature() {
        for history in [false, true] {
            let mut monitor = Monitor {
                page: config::PulseStartPage::Git,
                git: diff_pane(),
                ..Default::default()
            };
            if history {
                monitor.git.diff = None;
                monitor.git.history = Some(service::History {
                    commits: vec![],
                    page: 1,
                    has_more: false,
                    query: "query".into(),
                });
            }
            monitor.git.filter = "query".into();
            monitor.git.scroll = 5;
            render(&mut monitor.git, 32, 12);
            let index = monitor
                .git
                .controls(false)
                .iter()
                .position(|c| c.key.code == KeyCode::Char('H'))
                .unwrap();
            click_footer(&mut monitor.git, false, index);
            assert_eq!(monitor.page, config::PulseStartPage::Git);
            assert!(monitor.git.diff.is_none());
            assert!(monitor.git.history.is_none());
            assert!(monitor.git.filter.is_empty());
            assert_eq!(monitor.git.scroll, 0);
            monitor.return_home();
            assert_eq!(monitor.page, config::PulseStartPage::Git);
        }
        let mut pane = diff_pane();
        pane.modal = Some(Modal::Form {
            kind: FormKind::Create,
            text: String::new(),
        });
        pane.key(key(KeyCode::Char('H')));
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "H"));
        pane.busy = true;
        pane.return_home();
        assert!(pane.modal.is_some());
    }
}
