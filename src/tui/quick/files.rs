//! File sidebar. Filesystem and Herdr calls run on a single background worker.
use super::git::{safe_text, wrap_line};
use super::*;
use crate::files::{self as service, Entry, Operation};
use std::path::{Path, PathBuf};

enum Request {
    Scan(PathBuf, u64),
    Execute(PathBuf, Operation),
    Open(PathBuf, PathBuf),
}
enum Response {
    Follow(PathBuf),
    Listing(PathBuf, u64, std::result::Result<Vec<Entry>, String>),
    Completed(PathBuf, std::result::Result<String, String>),
}
#[derive(Clone, Copy)]
enum FormKind {
    File,
    Directory,
    Rename,
    Copy,
    Move,
}
enum Modal {
    Form {
        kind: FormKind,
        text: String,
        source: Option<PathBuf>,
    },
    Delete {
        path: PathBuf,
        confirm: bool,
    },
    Help,
    Menu {
        source: Option<Entry>,
        selected: usize,
    },
}
const MENU: [(&str, char, bool); 7] = [
    ("Open", 'o', true),
    ("Rename", 'R', true),
    ("Copy to", 'c', true),
    ("Move to", 'm', true),
    ("Delete", 'd', true),
    ("New file", 'n', false),
    ("New directory", 'N', false),
];

struct FilterDraft {
    query: String,
    selected: Option<PathBuf>,
    index: usize,
    offset: usize,
}

#[derive(Default)]
pub(super) struct FilePane {
    path: PathBuf,
    pub(super) icon_mode: super::file_icons::IconMode,
    home_path: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    offset: usize,
    filter: String,
    filter_edit: Option<FilterDraft>,
    filter_rect: Rect,
    filter_clear_rect: Rect,
    hidden: bool,
    pinned: bool,
    followed: Option<PathBuf>,
    modal: Option<Modal>,
    busy: bool,
    refreshing: bool,
    scan_version: u64,
    operation_label: Option<&'static str>,
    loading_since: Option<Instant>,
    reveal_path: Option<PathBuf>,
    last_click: Option<(PathBuf, Instant)>,
    menu_rows: Vec<(Rect, usize)>,
    menu_offset: usize,
    menu_scroll_manual: bool,
    menu_height: usize,
    popup_area: Rect,
    action_return: Option<(Option<Entry>, usize)>,
    body: Rect,
    toolbar: Vec<FooterButton>,
    breadcrumbs: Vec<(Rect, PathBuf)>,
    notice: Option<String>,
    notice_until: Option<Instant>,
    error: Option<String>,
    sticky_error: bool,
    requests: Option<mpsc::SyncSender<Request>>,
    updates: Option<mpsc::Receiver<Response>>,
    refreshed: Option<Instant>,
    rows: Vec<(Rect, usize)>,
    buttons: Vec<FooterButton>,
    body_height: usize,
    help_scroll: u16,
    language: HelpLanguage,
}

impl FilePane {
    pub(super) fn refresh_icon_mode(&mut self, paths: &AppPaths) -> bool {
        let path = paths.state_dir.join("pulse-file-icons.json");
        let mode = super::file_icons::IconMode::load(&path);
        if self.icon_mode != mode {
            self.icon_mode = mode;
            self.last_click = None;
            true
        } else {
            false
        }
    }
    pub(super) fn help_available(&self) -> bool {
        !self.busy
            && self.filter_edit.is_none()
            && (self.modal.is_none() || matches!(self.modal, Some(Modal::Help)))
    }
    pub(super) fn can_switch_workspace(&self) -> bool {
        !self.busy && self.modal.is_none() && self.filter_edit.is_none()
    }
    pub(super) fn start(&mut self) {
        self.path = std::env::current_dir().unwrap_or_default();
        self.home_path = self.path.clone();
        self.loading_since = Some(Instant::now());
        let (send, requests) = mpsc::sync_channel(8);
        let (updates, receive) = mpsc::channel();
        self.requests = Some(send);
        self.updates = Some(receive);
        let workspace = std::env::var("MUX_MONITOR_WORKSPACE")
            .or_else(|_| std::env::var("HERDR_WORKSPACE_ID"))
            .ok();
        let tab = std::env::var("MUX_MONITOR_TAB")
            .or_else(|_| std::env::var("HERDR_TAB_ID"))
            .ok();
        let source = std::env::var("MUX_MONITOR_SOURCE_PANE").unwrap_or_default();
        let own = std::env::var("HERDR_PANE_ID").unwrap_or_default();
        std::thread::spawn(move || {
            let mut checked = Instant::now() - Duration::from_secs(3);
            let mut last = None;
            let mut first = true;
            loop {
                if checked.elapsed() >= Duration::from_secs(2) {
                    if std::env::var("HERDR_ENV").as_deref() == Ok("1")
                        && let (Some(workspace), Some(tab)) = (&workspace, &tab)
                        && let Ok(panes) = herdr(&["pane", "list", "--workspace", workspace])
                        && let Some(path) = git::project_cwd(&panes, tab, &source, &own, first)
                    {
                        first = false;
                        if last.as_ref() != Some(&path) {
                            last = Some(path.clone());
                            if updates.send(Response::Follow(path)).is_err() {
                                break;
                            }
                        }
                    }
                    checked = Instant::now();
                }
                let request = match requests.recv_timeout(Duration::from_millis(250)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                };
                let response = match request {
                    Request::Scan(path, version) => {
                        let result = service::list(&path).map_err(|e| format!("{e:#}"));
                        Response::Listing(path, version, result)
                    }
                    Request::Execute(path, operation) => {
                        let result = service::execute(&operation)
                            .map(|_| "Operation completed".into())
                            .map_err(|e| format!("{e:#}"));
                        Response::Completed(path, result)
                    }
                    Request::Open(path, file) => Response::Completed(
                        path,
                        open_file(&file)
                            .map(|_| "Opened in a new Herdr tab".into())
                            .map_err(|e| format!("{e:#}")),
                    ),
                };
                if updates.send(response).is_err() {
                    break;
                }
            }
        });
        self.refresh();
    }
    fn submit(&mut self, request: Request) -> bool {
        let scan = matches!(request, Request::Scan(..));
        let label = match &request {
            Request::Scan(..) => None,
            Request::Open(..) => Some("Opening file…"),
            Request::Execute(_, operation) => Some(match operation {
                Operation::File(_) => "Creating file…",
                Operation::Directory(_) => "Creating directory…",
                Operation::Rename(..) => "Renaming…",
                Operation::Copy(..) => "Copying…",
                Operation::Move(..) => "Moving…",
                Operation::Delete(_) => "Deleting…",
            }),
        };
        let result = self
            .requests
            .as_ref()
            .context("File worker unavailable")
            .and_then(|send| {
                send.try_send(request)
                    .context("File worker busy or unavailable")
            });
        match result {
            Ok(()) if scan => {
                self.refreshing = true;
                true
            }
            Ok(()) => {
                self.busy = true;
                self.action_return = None;
                self.notice = None;
                self.notice_until = None;
                self.operation_label = label;
                self.scan_version = self.scan_version.wrapping_add(1);
                self.error = None;
                self.sticky_error = false;
                self.last_click = None;
                true
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.sticky_error = true;
                false
            }
        }
    }
    fn refresh(&mut self) {
        if !self.busy && !self.refreshing {
            self.submit(Request::Scan(self.path.clone(), self.scan_version));
        }
    }
    fn navigate(&mut self, path: PathBuf, pin: bool) {
        self.path = path;
        self.scan_version = self.scan_version.wrapping_add(1);
        self.refreshed = None;
        self.loading_since = Some(Instant::now());
        self.last_click = None;
        self.reveal_path = None;
        self.pinned = pin;
        self.entries.clear();
        self.selected = 0;
        self.offset = 0;
        self.filter.clear();
        self.filter_edit = None;
        self.notice = None;
        self.notice_until = None;
        self.error = None;
        self.sticky_error = false;
        self.refresh();
    }
    pub(super) fn return_home(&mut self) {
        if self.busy {
            return;
        }
        self.modal = None;
        self.action_return = None;
        self.help_scroll = 0;
        let root = self.followed.clone().unwrap_or_else(|| {
            if self.home_path.as_os_str().is_empty() {
                self.path.clone()
            } else {
                self.home_path.clone()
            }
        });
        self.navigate(root, false);
    }
    pub(super) fn poll(&mut self, active: bool) -> bool {
        let mut redraw = false;
        loop {
            let update = match self.updates.as_ref().map(|receive| receive.try_recv()) {
                Some(Ok(update)) => update,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.updates = None;
                    self.requests = None;
                    self.busy = false;
                    self.refreshing = false;
                    self.operation_label = None;
                    self.error = Some("File worker stopped; reopen Pulse".into());
                    self.sticky_error = true;
                    redraw = true;
                    break;
                }
                _ => break,
            };
            match update {
                Response::Follow(path) => self.followed = Some(path),
                Response::Listing(path, version, result) => {
                    self.refreshing = false;
                    if path != self.path || version != self.scan_version {
                        continue;
                    }
                    let first = self.refreshed.is_none();
                    self.refreshed = Some(Instant::now());
                    self.loading_since = None;
                    redraw |= first;
                    match result {
                        Ok(entries) => {
                            let selected = self
                                .reveal_path
                                .take()
                                .or_else(|| self.selected_entry().map(|e| e.path.clone()));
                            if self.entries != entries {
                                self.entries = entries;
                                redraw = true;
                            }
                            if let Some(path) = selected {
                                let next = self
                                    .visible()
                                    .iter()
                                    .position(|e| e.path == path)
                                    .unwrap_or(self.selected);
                                redraw |= next != self.selected;
                                self.selected = next;
                            }
                            self.clamp();
                            if !self.sticky_error {
                                redraw |= self.error.take().is_some();
                            }
                        }
                        Err(error) => {
                            redraw |=
                                !self.entries.is_empty() || self.error.as_ref() != Some(&error);
                            self.entries.clear();
                            self.error = Some(error);
                        }
                    }
                }
                Response::Completed(path, result) => {
                    redraw = true;
                    self.busy = false;
                    self.operation_label = None;
                    self.refreshed = None;
                    if path != self.path {
                        continue;
                    }
                    self.notice = None;
                    match result {
                        Ok(notice) => {
                            self.notice = Some(notice);
                            self.notice_until = Some(Instant::now() + Duration::from_secs(4));
                            self.error = None;
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.sticky_error = true;
                        }
                    }
                    self.refresh();
                }
            }
        }
        if active && !self.busy && self.modal.is_none() && self.requests.is_some() {
            if self.filter_edit.is_none()
                && !self.pinned
                && let Some(path) = self.followed.clone().filter(|p| p != &self.path)
            {
                self.navigate(path, false);
                redraw = true;
            } else if !self.refreshing
                && self
                    .refreshed
                    .is_none_or(|time| time.elapsed() >= Duration::from_secs(2))
            {
                self.refresh();
            }
        }
        if self.notice_until.is_some_and(|time| Instant::now() >= time) {
            self.notice = None;
            self.notice_until = None;
            redraw = true;
        }
        redraw
    }
    fn visible(&self) -> Vec<&Entry> {
        let query = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                let name = e.path.file_name().unwrap_or_default().to_string_lossy();
                (self.hidden || !name.starts_with('.')) && name.to_lowercase().contains(&query)
            })
            .collect()
    }
    fn selected_entry(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }
    fn clamp(&mut self) {
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
        let height = self.body_height.max(1);
        self.offset = self
            .offset
            .min(self.selected)
            .min(self.visible().len().saturating_sub(height));
        if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
    }
    fn begin_filter(&mut self) {
        if self.busy || self.modal.is_some() || self.filter_edit.is_some() {
            return;
        }
        self.last_click = None;
        self.filter_edit = Some(FilterDraft {
            query: self.filter.clone(),
            selected: self.selected_entry().map(|entry| entry.path.clone()),
            index: self.selected,
            offset: self.offset,
        });
    }
    fn finish_filter(&mut self, cancel: bool) {
        if let Some(draft) = self.filter_edit.take()
            && cancel
        {
            self.filter = draft.query;
            self.selected = draft
                .selected
                .and_then(|path| self.visible().iter().position(|entry| entry.path == path))
                .unwrap_or(draft.index);
            self.offset = draft.offset;
            self.clamp();
        }
    }
    fn filter_input(&mut self, key: KeyEvent) -> bool {
        if self.filter_edit.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.finish_filter(true);
                return true;
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.finish_filter(false);
                return true;
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                self.finish_filter(false);
                return false;
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear()
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(ch)
            }
            _ => return true,
        }
        self.selected = 0;
        self.offset = 0;
        self.clamp();
        true
    }
    fn draw_filter(&mut self, frame: &mut ratatui::Frame, rect: Rect) {
        self.filter_rect = rect;
        self.filter_clear_rect = draw_search_field(
            frame,
            rect,
            &self.filter,
            self.filter_edit.is_some(),
            !self.busy && self.modal.is_none(),
            "Find files (/)",
        );
    }
    fn form(&mut self, kind: FormKind) {
        let source = self.selected_entry().map(|e| e.path.clone());
        self.form_with_source(kind, source);
    }
    fn form_with_source(&mut self, kind: FormKind, source: Option<PathBuf>) {
        if matches!(kind, FormKind::Rename | FormKind::Copy | FormKind::Move) && source.is_none() {
            return;
        }
        let text = if matches!(kind, FormKind::Rename) {
            source
                .as_ref()
                .and_then(|p| p.file_name())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        self.modal = Some(Modal::Form { kind, text, source });
    }
    fn finish_form(&mut self, kind: FormKind, text: String, source: Option<PathBuf>) {
        let result = (|| -> Result<Operation> {
            anyhow::ensure!(!text.is_empty(), "Enter a name or destination path");
            let target = if matches!(
                kind,
                FormKind::File | FormKind::Directory | FormKind::Rename
            ) {
                let name = Path::new(&text);
                anyhow::ensure!(
                    name.components().count() == 1
                        && matches!(
                            name.components().next(),
                            Some(std::path::Component::Normal(_))
                        ),
                    "Enter a single name, without a directory path"
                );
                self.path.join(name)
            } else {
                self.path.join(&text)
            };
            Ok(match kind {
                FormKind::File => Operation::File(target),
                FormKind::Directory => Operation::Directory(target),
                FormKind::Rename => {
                    Operation::Rename(source.clone().context("No file selected")?, target)
                }
                FormKind::Copy => {
                    Operation::Copy(source.clone().context("No file selected")?, target)
                }
                FormKind::Move => {
                    Operation::Move(source.clone().context("No file selected")?, target)
                }
            })
        })();
        match result {
            Ok(operation) => {
                if !self.submit(Request::Execute(self.path.clone(), operation)) {
                    self.modal = Some(Modal::Form { kind, text, source });
                }
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.modal = Some(Modal::Form { kind, text, source });
            }
        }
    }
    fn open_entry(&mut self, entry: Entry) {
        if entry.directory {
            self.navigate(entry.path, true);
        } else {
            self.submit(Request::Open(self.path.clone(), entry.path));
        }
    }
    fn open_actions(&mut self, source: Option<Entry>) {
        self.last_click = None;
        self.menu_offset = 0;
        self.menu_scroll_manual = false;
        self.action_return = None;
        self.modal = Some(Modal::Menu {
            selected: if source.is_some() { 0 } else { 5 },
            source,
        });
    }
    fn cancel_dialog(&mut self) {
        if let Some((source, selected)) = self.action_return.take() {
            self.modal = Some(Modal::Menu { source, selected });
        }
        if !self.sticky_error {
            self.error = None;
        }
    }
    fn context_action(&mut self, index: usize, source: Option<Entry>) {
        let (_, key, requires_file) = MENU[index];
        if requires_file && source.is_none() {
            self.modal = Some(Modal::Menu {
                source,
                selected: index,
            });
            return;
        }
        if matches!(key, 'R' | 'c' | 'm' | 'd' | 'n' | 'N') {
            self.action_return = Some((source.clone(), index));
        }
        let path = source.as_ref().map(|entry| entry.path.clone());
        match key {
            'o' => {
                if let Some(entry) = source {
                    self.open_entry(entry);
                }
            }
            'R' => self.form_with_source(FormKind::Rename, path),
            'c' => self.form_with_source(FormKind::Copy, path),
            'm' => self.form_with_source(FormKind::Move, path),
            'd' => {
                if let Some(path) = path {
                    self.help_scroll = 0;
                    self.modal = Some(Modal::Delete {
                        path,
                        confirm: false,
                    });
                }
            }
            other => {
                self.input(&Event::Key(KeyEvent::new(
                    KeyCode::Char(other),
                    KeyModifiers::NONE,
                )));
            }
        }
    }
    fn scroll_list(&mut self, offset: usize) {
        let count = self.visible().len();
        self.offset = offset.min(count.saturating_sub(self.body_height));
        self.selected = self
            .selected
            .max(self.offset)
            .min((self.offset + self.body_height).saturating_sub(1))
            .min(count.saturating_sub(1));
        self.last_click = None;
    }
    fn mouse_key(&mut self, mouse: &MouseEvent) -> Option<KeyCode> {
        if self.modal.is_none()
            && !self.busy
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        {
            if contains(self.filter_rect, mouse.column, mouse.row) {
                self.begin_filter();
                if contains(self.filter_clear_rect, mouse.column, mouse.row) {
                    self.filter.clear();
                    self.selected = 0;
                    self.offset = 0;
                    self.clamp();
                }
                return None;
            }
            self.finish_filter(false);
        }
        if self.modal.is_none() && contains(self.body, mouse.column, mouse.row) {
            let limit = self.visible().len().saturating_sub(self.body_height);
            if matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
            ) && let Some(target) = scrollbar_target(
                self.body,
                self.body.right().saturating_sub(1),
                mouse.column,
                mouse.row,
                limit.min(u16::MAX as usize) as u16,
            ) {
                self.scroll_list(usize::from(target));
                return None;
            }
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                let offset = if mouse.kind == MouseEventKind::ScrollUp {
                    self.offset.saturating_sub(3)
                } else {
                    self.offset.saturating_add(3)
                };
                self.scroll_list(offset);
                return None;
            }
        }
        if self.busy {
            return None;
        }
        if matches!(self.modal, Some(Modal::Menu { .. })) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && !contains(self.popup_area, mouse.column, mouse.row)
                && !self
                    .buttons
                    .iter()
                    .any(|button| contains(button.rect, mouse.column, mouse.row))
            {
                self.modal = None;
                self.last_click = None;
                return None;
            }
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                if contains(self.popup_area, mouse.column, mouse.row) {
                    let height = self.menu_height.max(1);
                    let limit = MENU.len().saturating_sub(height);
                    self.menu_offset = if mouse.kind == MouseEventKind::ScrollUp {
                        self.menu_offset.saturating_sub(3)
                    } else {
                        self.menu_offset.saturating_add(3).min(limit)
                    };
                    self.menu_scroll_manual = true;
                    if let Some(Modal::Menu { selected, .. }) = &mut self.modal {
                        *selected = (*selected).clamp(
                            self.menu_offset,
                            (self.menu_offset + height - 1).min(MENU.len() - 1),
                        );
                    }
                }
                return None;
            }
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => Some(KeyCode::Up),
            MouseEventKind::ScrollDown => Some(KeyCode::Down),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(button) = self
                    .buttons
                    .iter()
                    .chain(self.toolbar.iter())
                    .find(|button| contains(button.rect, mouse.column, mouse.row))
                {
                    self.last_click = None;
                    return button.enabled.then_some(button.key);
                }
                if self.modal.is_none() {
                    if let Some((_, path)) = self
                        .breadcrumbs
                        .iter()
                        .find(|(rect, _)| contains(*rect, mouse.column, mouse.row))
                    {
                        let path = path.clone();
                        if path != self.path {
                            self.navigate(path, true);
                        }
                        return None;
                    }
                    if let Some((_, selected)) = self
                        .rows
                        .iter()
                        .find(|(rect, _)| contains(*rect, mouse.column, mouse.row))
                    {
                        self.selected = *selected;
                        if let Some(entry) = self.selected_entry().cloned() {
                            let double = self.last_click.as_ref().is_some_and(|(path, time)| {
                                path == &entry.path && time.elapsed() <= Duration::from_millis(450)
                            });
                            if double {
                                self.last_click = None;
                                self.open_entry(entry);
                            } else {
                                self.last_click = Some((entry.path, Instant::now()));
                            }
                        }
                    } else {
                        self.last_click = None;
                    }
                } else if matches!(self.modal, Some(Modal::Menu { .. }))
                    && let Some((_, index)) = self
                        .menu_rows
                        .iter()
                        .find(|(rect, _)| contains(*rect, mouse.column, mouse.row))
                {
                    let index = *index;
                    if MENU[index].2 && matches!(self.modal, Some(Modal::Menu { source: None, .. }))
                    {
                        return None;
                    }
                    if let Some(Modal::Menu { source, .. }) = self.modal.take() {
                        self.context_action(index, source);
                    }
                }
                None
            }
            MouseEventKind::Down(MouseButton::Right) if self.modal.is_none() => {
                if !contains(self.body, mouse.column, mouse.row) {
                    return None;
                }
                self.last_click = None;
                let source = self
                    .rows
                    .iter()
                    .find(|(rect, _)| contains(*rect, mouse.column, mouse.row))
                    .map(|(_, index)| *index)
                    .and_then(|index| {
                        self.selected = index;
                        self.selected_entry().cloned()
                    });
                self.open_actions(source);
                None
            }
            _ => None,
        }
    }
    pub(super) fn input(&mut self, event: &Event) -> bool {
        let key = match event {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                self.last_click = None;
                *key
            }
            Event::Mouse(mouse) => {
                let Some(code) = self.mouse_key(mouse) else {
                    return true;
                };
                KeyEvent::new(code, KeyModifiers::NONE)
            }
            _ => return true,
        };
        if self.busy {
            return true;
        }
        self.menu_scroll_manual = false;
        if self.filter_input(key) {
            return true;
        }
        if key.code == KeyCode::Char('H')
            && (self.modal.is_none() || matches!(self.modal, Some(Modal::Help)))
        {
            self.return_home();
            return true;
        }
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Menu {
                    source,
                    mut selected,
                } => {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => return true,
                        KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            selected = (selected + 1).min(MENU.len() - 1);
                        }
                        KeyCode::PageDown => {
                            selected = selected
                                .saturating_add(self.menu_height.max(1))
                                .min(MENU.len() - 1)
                        }
                        KeyCode::PageUp => {
                            selected = selected.saturating_sub(self.menu_height.max(1))
                        }
                        KeyCode::Home => selected = 0,
                        KeyCode::End => selected = MENU.len() - 1,
                        KeyCode::Enter => {
                            self.context_action(selected, source);
                            return true;
                        }
                        KeyCode::Char(ch) => {
                            if let Some(index) =
                                MENU.iter().position(|(_, shortcut, _)| *shortcut == ch)
                            {
                                self.context_action(index, source);
                                return true;
                            }
                        }
                        _ => {}
                    }
                    if source.is_none() {
                        selected = selected.max(5);
                    }
                    let height = self.menu_height.max(1);
                    self.menu_offset = self.menu_offset.min(selected);
                    if selected >= self.menu_offset + height {
                        self.menu_offset = selected + 1 - height;
                    }
                    self.modal = Some(Modal::Menu { source, selected });
                }
                Modal::Form {
                    kind,
                    mut text,
                    source,
                } => {
                    match key.code {
                        KeyCode::Esc => {
                            self.cancel_dialog();
                            return true;
                        }
                        KeyCode::Enter => {
                            self.finish_form(kind, text, source);
                            return true;
                        }
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            text.clear()
                        }
                        KeyCode::Char(ch)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            text.push(ch)
                        }
                        _ => {}
                    }
                    self.modal = Some(Modal::Form { kind, text, source });
                }
                Modal::Delete { path, mut confirm } => {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q' | 'n') => {
                            self.cancel_dialog();
                            return true;
                        }
                        KeyCode::Left | KeyCode::Right | KeyCode::Tab => confirm = !confirm,
                        KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                        KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                        KeyCode::PageDown => {
                            self.help_scroll =
                                self.help_scroll.saturating_add(self.body_height as u16)
                        }
                        KeyCode::PageUp => {
                            self.help_scroll =
                                self.help_scroll.saturating_sub(self.body_height as u16)
                        }
                        KeyCode::Char('y') => {
                            if !self.submit(Request::Execute(
                                self.path.clone(),
                                Operation::Delete(path.clone()),
                            )) {
                                self.modal = Some(Modal::Delete { path, confirm });
                            }
                            return true;
                        }
                        KeyCode::Enter if confirm => {
                            if !self.submit(Request::Execute(
                                self.path.clone(),
                                Operation::Delete(path.clone()),
                            )) {
                                self.modal = Some(Modal::Delete { path, confirm });
                            }
                            return true;
                        }
                        KeyCode::Enter => {
                            self.cancel_dialog();
                            return true;
                        }
                        _ => {}
                    }
                    self.modal = Some(Modal::Delete { path, confirm });
                }
                Modal::Help => {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q' | '?') => return true,
                        KeyCode::Char('l') => {
                            self.language.toggle();
                            self.help_scroll = 0;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.help_scroll = self.help_scroll.saturating_add(1)
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.help_scroll = self.help_scroll.saturating_sub(1)
                        }
                        KeyCode::PageDown => {
                            self.help_scroll =
                                self.help_scroll.saturating_add(self.body_height as u16)
                        }
                        KeyCode::PageUp => {
                            self.help_scroll =
                                self.help_scroll.saturating_sub(self.body_height as u16)
                        }
                        _ => {}
                    }
                    self.modal = Some(Modal::Help);
                }
            }
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return true;
        }
        match key.code {
            KeyCode::Char('q') => self.return_home(),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.clamp();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.clamp();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = self.selected.saturating_add(1);
                self.clamp();
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(self.body_height.max(1));
                self.clamp();
            }
            KeyCode::PageDown => {
                self.selected = self.selected.saturating_add(self.body_height.max(1));
                self.clamp();
            }
            KeyCode::Home => {
                self.selected = 0;
                self.clamp();
            }
            KeyCode::End => {
                self.selected = self.visible().len().saturating_sub(1);
                self.clamp();
            }
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry().cloned() {
                    self.open_entry(entry);
                }
            }
            KeyCode::Right => {
                if let Some(entry) = self.selected_entry().filter(|entry| entry.directory) {
                    self.navigate(entry.path.clone(), true);
                }
            }
            KeyCode::Left | KeyCode::Backspace => {
                if let Some(parent) = self.path.parent() {
                    let parent = parent.to_path_buf();
                    let child = self.path.clone();
                    self.navigate(parent, true);
                    self.reveal_path = Some(child);
                }
            }
            KeyCode::Char('p') => {
                self.pinned = !self.pinned;
            }
            KeyCode::Char('.') => {
                self.hidden = !self.hidden;
                self.selected = 0;
                self.clamp();
            }
            KeyCode::Char('/') => self.begin_filter(),
            KeyCode::Char('o') => self.open_actions(self.selected_entry().cloned()),
            KeyCode::Char('n') => self.form(FormKind::File),
            KeyCode::Char('N') => self.form(FormKind::Directory),
            KeyCode::Char('R') => self.form(FormKind::Rename),
            KeyCode::Char('c') => self.form(FormKind::Copy),
            KeyCode::Char('m') => self.form(FormKind::Move),
            KeyCode::Char('d') => {
                self.help_scroll = 0;
                if let Some(entry) = self.selected_entry() {
                    self.modal = Some(Modal::Delete {
                        path: entry.path.clone(),
                        confirm: false,
                    });
                }
            }
            KeyCode::Char('r') => {
                self.sticky_error = false;
                self.error = None;
                self.refresh();
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                self.help_scroll = 0;
            }
            _ => {}
        }
        true
    }
    fn draw_breadcrumbs(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let mut paths: Vec<_> = self.path.ancestors().map(Path::to_path_buf).collect();
        paths.reverse();
        let label = |path: &Path| {
            safe_text(
                &path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy(),
            )
        };
        let mut first = 0;
        while first + 1 < paths.len()
            && paths[first..]
                .iter()
                .map(|p| label(p).width() + 3)
                .sum::<usize>()
                > usize::from(area.width)
        {
            first += 1;
        }
        let mut x = area.x;
        if first > 0 && area.width >= 3 {
            let rect = Rect::new(x, area.y, 4.min(area.width), 1);
            frame.render_widget(
                Paragraph::new("… / ").style(Style::default().fg(BLUE)),
                rect,
            );
            self.breadcrumbs.push((rect, paths[first - 1].clone()));
            x = x.saturating_add(4).min(area.right());
        }
        for path in &paths[first..] {
            let text = label(path);
            let width = (text.width() as u16).min(area.right().saturating_sub(x));
            if width == 0 {
                break;
            }
            let rect = Rect::new(x, area.y, width, 1);
            let style = Style::default()
                .fg(if path == &self.path { INK } else { BLUE })
                .add_modifier(if path == &self.path {
                    Modifier::BOLD
                } else {
                    Modifier::UNDERLINED
                });
            frame.render_widget(
                Paragraph::new(clipped(&text, width.into())).style(style),
                rect,
            );
            self.breadcrumbs.push((rect, path.clone()));
            x += width;
            if path != &self.path && x < area.right() {
                frame.render_widget(
                    Paragraph::new(if path.parent().is_none() { " " } else { " / " })
                        .style(Style::default().fg(SOFT)),
                    Rect::new(x, area.y, 3.min(area.right() - x), 1),
                );
                x = x
                    .saturating_add(if path.parent().is_none() { 1 } else { 3 })
                    .min(area.right());
            }
        }
    }
    fn list_content(&self, width: u16) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (index, entry) in self
            .visible()
            .into_iter()
            .enumerate()
            .skip(self.offset)
            .take(self.body_height)
        {
            let name = safe_text(&entry.path.file_name().unwrap_or_default().to_string_lossy());
            let selected = index == self.selected;
            out.push(super::file_icons::name_row(
                entry,
                &name,
                selected,
                width,
                self.icon_mode,
            ));
            let date = entry
                .modified
                .map(|time| {
                    chrono::DateTime::<chrono::Local>::from(time)
                        .format("%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "unknown date".into());
            out.push(painted_row(
                &format!(
                    "     {} · {date}",
                    if entry.link {
                        "link".into()
                    } else if entry.directory {
                        "folder".into()
                    } else {
                        human_size(entry.size)
                    }
                ),
                width,
                Style::default()
                    .fg(SOFT)
                    .bg(if selected { RAIL } else { BG }),
            ));
        }
        if out.is_empty() {
            out.push(line(
                if self.refreshed.is_none() && self.refreshing {
                    ""
                } else if !self.filter.is_empty() {
                    "No matches · Esc: clear filter"
                } else {
                    "Empty directory · New: create a file"
                },
                SOFT,
            ));
        }
        out
    }
    fn draw_scrollbar(&self, frame: &mut ratatui::Frame, body: Rect) {
        if body.width > 0 && body.height > 1 && self.visible().len() > self.body_height {
            let mut state = scroll_state(self.visible().len(), self.offset, self.body_height);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(Style::default().fg(BLUE))
                    .track_style(Style::default().fg(RAIL)),
                Rect::new(body.right().saturating_sub(1), body.y, 1, body.height),
                &mut state,
            );
        }
    }
    fn popup_layout(&self, body: Rect) -> Option<(Rect, Rect, &'static str)> {
        let (height, title) = match &self.modal {
            Some(Modal::Menu { .. }) => (MENU.len() as u16 + 4, " Actions "),
            Some(Modal::Form { kind, .. }) => (
                if matches!(kind, FormKind::Copy | FormKind::Move) {
                    8
                } else {
                    6
                },
                match kind {
                    FormKind::File => " New file ",
                    FormKind::Directory => " New directory ",
                    FormKind::Rename => " Rename ",
                    FormKind::Copy => " Copy ",
                    FormKind::Move => " Move ",
                },
            ),
            Some(Modal::Delete { .. }) => (10, " Delete permanently "),
            _ => return None,
        };
        let width = body.width.min(56);
        let height = height.min(body.height);
        let rect = Rect::new(
            body.x + body.width.saturating_sub(width) / 2,
            body.bottom().saturating_sub(height),
            width,
            height,
        );
        let inner = if width >= 4 && height >= 3 {
            rect.inner(Margin::new(1, 1))
        } else {
            rect
        };
        Some((rect, inner, title))
    }
    pub(super) fn draw(&mut self, frame: &mut ratatui::Frame) {
        let screen = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(INK)),
            screen,
        );
        let (area, _) = pulse_header_area(screen);
        self.rows.clear();
        self.buttons.clear();
        self.toolbar.clear();
        self.breadcrumbs.clear();
        self.menu_rows.clear();
        self.filter_rect = Rect::default();
        self.filter_clear_rect = Rect::default();
        let show_status = self.busy
            || self.error.is_some()
            || self.notice.is_some()
            || (self.refreshed.is_none()
                && self.refreshing
                && self
                    .loading_since
                    .is_some_and(|time| time.elapsed() >= Duration::from_millis(200)));
        let filter_height = if area.height >= 8 { 3 } else { 1 };
        let header_rows = 3 + filter_height + u16::from(show_status);
        let footer_rows = u16::from(area.height > header_rows);
        let body = Rect::new(
            area.x,
            area.y.saturating_add(header_rows).min(area.bottom()),
            area.width,
            area.height.saturating_sub(header_rows + footer_rows),
        );
        self.body = body;
        let content_body = Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height);
        self.body_height = usize::from(body.height / 2).max(1);
        if self.modal.is_none() {
            self.clamp();
        }
        let status = if self.busy {
            self.operation_label
                .unwrap_or("File operation in progress…")
                .into()
        } else if self.refreshed.is_none()
            && self.refreshing
            && self
                .loading_since
                .is_some_and(|time| time.elapsed() >= Duration::from_millis(200))
        {
            "Reading directory…".into()
        } else if let Some(error) = &self.error {
            safe_text(error)
        } else if let Some(notice) = &self.notice {
            safe_text(notice)
        } else {
            format!(
                "{} / {} items · {}",
                if self.visible().is_empty() {
                    0
                } else {
                    self.selected + 1
                },
                self.visible().len(),
                "Double-click: open"
            )
        };
        if area.height > 1 {
            self.draw_breadcrumbs(frame, Rect::new(area.x, area.y + 1, area.width, 1));
        }
        if area.height > 2 {
            let toolbar_area = Rect::new(area.x, area.y + 2, area.width, 1);
            let enabled = !self.busy && self.modal.is_none();
            self.toolbar = layout_footer(
                toolbar_area,
                vec![
                    footer_control(
                        if self.pinned {
                            ["Pinned(p)", "Pinned", "P"]
                        } else {
                            ["Following(p)", "Follow", "F"]
                        },
                        KeyCode::Char('p'),
                        enabled,
                    ),
                    footer_control(
                        if self.hidden {
                            ["Hidden: on(.)", "Hidden on", ".on"]
                        } else {
                            ["Hidden: off(.)", "Hidden off", ".off"]
                        },
                        KeyCode::Char('.'),
                        enabled,
                    ),
                    footer_control(["Refresh(r)", "Refresh", "r"], KeyCode::Char('r'), enabled),
                ],
            );
        }
        if area.height > 3 {
            self.draw_filter(
                frame,
                Rect::new(area.x, area.y + 3, area.width, filter_height),
            );
        }
        if show_status && area.height > 3 + filter_height {
            frame.render_widget(
                Paragraph::new(line(
                    clipped(&status, area.width.into()),
                    if self.error.is_some() {
                        RED
                    } else if self.busy {
                        GOLD
                    } else {
                        SOFT
                    },
                )),
                Rect::new(area.x, area.y + 3 + filter_height, area.width, 1),
            );
        }
        let popup = self.popup_layout(body);
        self.popup_area = popup.map_or(Rect::default(), |(rect, _, _)| rect);
        let dialog_body = popup.map_or(content_body, |(_, inner, _)| inner);
        if let Some((rect, _, title)) = popup {
            frame.render_widget(
                Paragraph::new(self.list_content(content_body.width)),
                content_body,
            );
            self.draw_scrollbar(frame, body);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                popup_block(
                    rect,
                    title,
                    matches!(self.modal, Some(Modal::Delete { .. })),
                ),
                rect,
            );
        }
        let mut content = Vec::new();
        match &self.modal {
            Some(Modal::Form {
                kind, text, source, ..
            }) => {
                let title = match kind {
                    FormKind::File => "New file name",
                    FormKind::Directory => "New directory name",
                    FormKind::Rename => "Rename to",
                    FormKind::Copy => "Copy to path",
                    FormKind::Move => "Move to path",
                };
                if dialog_body.height >= 3
                    && matches!(kind, FormKind::Rename | FormKind::Copy | FormKind::Move)
                    && let Some(source) = source
                {
                    content.push(line(
                        clipped(
                            &format!(
                                "File: {}",
                                safe_text(
                                    &source.file_name().unwrap_or_default().to_string_lossy()
                                )
                            ),
                            dialog_body.width.into(),
                        ),
                        BLUE,
                    ));
                }
                if dialog_body.height >= 2 {
                    content.push(line(title, GOLD));
                }
                // Keep the end of long input visible, including its cursor.
                let value = safe_text(text);
                let mut suffix = value.as_str();
                while suffix.width() + 1 > usize::from(dialog_body.width) && !suffix.is_empty() {
                    suffix = &suffix[suffix.chars().next().unwrap().len_utf8()..];
                }
                content.push(painted_row(
                    &format!("{suffix}▏"),
                    dialog_body.width,
                    Style::default().fg(INK).bg(RAIL),
                ));
                content.push(line("Enter: save · Ctrl+U: clear · Esc: cancel", SOFT));
                if matches!(kind, FormKind::Copy | FormKind::Move) {
                    content.extend(wrap_line(line("Destination is a new path, relative to the current directory or absolute. Existing targets are never overwritten.", SOFT), usize::from(dialog_body.width).max(1)));
                }
            }
            Some(Modal::Delete { path, confirm }) => {
                content.extend(wrap_line(
                    line(
                        format!("Permanently delete {}?", safe_text(&path.to_string_lossy())),
                        RED,
                    ),
                    usize::from(dialog_body.width).max(1),
                ));
                content.extend(wrap_line(
                    line(
                        "Directories and all contents will be removed. This cannot be undone.",
                        GOLD,
                    ),
                    usize::from(dialog_body.width).max(1),
                ));
                content.push(line(
                    if *confirm {
                        " Cancel    [Delete]"
                    } else {
                        "[Cancel]    Delete"
                    },
                    INK,
                ));
                content.push(line(
                    "←/→: select · ↑/↓: scroll · Enter: confirm · Esc: cancel",
                    SOFT,
                ));
            }
            Some(Modal::Menu { source, selected }) => {
                content.push(line(
                    clipped(
                        &source.as_ref().map_or_else(
                            || "Directory actions".into(),
                            |entry| {
                                safe_text(
                                    &entry.path.file_name().unwrap_or_default().to_string_lossy(),
                                )
                            },
                        ),
                        dialog_body.width.into(),
                    ),
                    BLUE,
                ));
                let height = usize::from(if dialog_body.height >= 3 {
                    dialog_body.height - 2
                } else {
                    dialog_body.height
                });
                self.menu_height = height;
                self.menu_offset = self.menu_offset.min(MENU.len().saturating_sub(height));
                if !self.menu_scroll_manual {
                    self.menu_offset = self.menu_offset.min(*selected);
                    if height > 0 && *selected >= self.menu_offset + height {
                        self.menu_offset = selected + 1 - height;
                    }
                }
                if dialog_body.height < 3 {
                    content.clear();
                }
                for (index, (base_label, shortcut, requires_file)) in
                    MENU.iter().enumerate().skip(self.menu_offset).take(height)
                {
                    let label = match *shortcut {
                        'o' if source.as_ref().is_some_and(|entry| entry.directory) => {
                            "Enter directory"
                        }
                        'o' => "Open in new tab",
                        _ => base_label,
                    };
                    let enabled = !requires_file || source.is_some();
                    let style = Style::default()
                        .fg(if !enabled {
                            SOFT
                        } else if *shortcut == 'd' {
                            RED
                        } else if index == *selected {
                            GOLD
                        } else {
                            INK
                        })
                        .bg(if index == *selected { RAIL } else { BG })
                        .add_modifier(if !enabled {
                            Modifier::DIM
                        } else if index == *selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        });
                    content.push(painted_row(
                        &format!(
                            "{} {label} ({shortcut})",
                            if index == *selected { "›" } else { " " }
                        ),
                        dialog_body.width,
                        style,
                    ));
                    self.menu_rows.push((
                        Rect::new(
                            dialog_body.x,
                            dialog_body.y + content.len() as u16 - 1,
                            dialog_body.width,
                            1,
                        ),
                        index,
                    ));
                }
                if dialog_body.height >= 3 {
                    content.push(line(
                        clipped(
                            &format!("{} / {} · ↑↓ select · Esc close", selected + 1, MENU.len()),
                            dialog_body.width.into(),
                        ),
                        SOFT,
                    ));
                }
            }
            Some(Modal::Help) => {
                content = help_content(HelpPage::File, self.language, dialog_body.width)
            }
            None => content = self.list_content(dialog_body.width),
        }
        if matches!(self.modal, Some(Modal::Help | Modal::Delete { .. })) {
            self.help_scroll = self
                .help_scroll
                .min((content.len() as u16).saturating_sub(dialog_body.height));
        }
        frame.render_widget(
            Paragraph::new(content).scroll((
                if matches!(self.modal, Some(Modal::Help | Modal::Delete { .. })) {
                    self.help_scroll
                } else {
                    0
                },
                0,
            )),
            dialog_body,
        );
        if self.modal.is_none() {
            let rows: Vec<_> = self
                .visible()
                .into_iter()
                .enumerate()
                .skip(self.offset)
                .take(self.body_height)
                .enumerate()
                .map(|(row, (index, _))| {
                    (
                        Rect::new(
                            body.x,
                            body.y + row as u16 * 2,
                            content_body.width,
                            2.min(body.height.saturating_sub(row as u16 * 2)),
                        ),
                        index,
                    )
                })
                .collect();
            self.rows = rows;
        }
        if footer_rows > 0 {
            let enabled = !self.busy;
            let selected = self.selected_entry().is_some();
            let controls = match self.modal {
                Some(Modal::Menu {
                    source: ref entry,
                    selected: index,
                }) => vec![
                    footer_control(
                        ["Choose(Enter)", "Choose", "↵"],
                        KeyCode::Enter,
                        !MENU[index].2 || entry.is_some(),
                    ),
                    footer_control(["Cancel(Esc)", "Cancel", "Esc"], KeyCode::Esc, true),
                ],
                Some(Modal::Form { .. }) => vec![
                    footer_control(["Save(Enter)", "Save", "↵"], KeyCode::Enter, enabled),
                    footer_control(
                        if self.action_return.is_some() {
                            ["Back(Esc)", "Back", "Esc"]
                        } else {
                            ["Cancel(Esc)", "Cancel", "Esc"]
                        },
                        KeyCode::Esc,
                        enabled,
                    ),
                ],
                Some(Modal::Delete { .. }) => vec![
                    footer_control(
                        if self.action_return.is_some() {
                            ["Back(Esc)", "Back", "Esc"]
                        } else {
                            ["Cancel(Esc)", "Cancel", "Esc"]
                        },
                        KeyCode::Esc,
                        enabled,
                    ),
                    footer_control(["Delete(y)", "Delete", "y"], KeyCode::Char('y'), enabled),
                ],
                Some(Modal::Help) => vec![
                    footer_control(
                        [self.language.switch_label(), "Lang", "l"],
                        KeyCode::Char('l'),
                        true,
                    ),
                    footer_control(["Back(q)", "Back", "q"], KeyCode::Char('q'), true),
                    footer_control(["Home(H)", "Home", "H"], KeyCode::Char('H'), true),
                ],
                None => vec![
                    footer_control(
                        ["Open(Enter)", "Open", "↵"],
                        KeyCode::Enter,
                        enabled && selected,
                    ),
                    footer_control(
                        ["Parent(←)", "← Up", "←"],
                        KeyCode::Backspace,
                        enabled && self.path.parent().is_some(),
                    ),
                    footer_control(["Actions(o)", "Actions", "o"], KeyCode::Char('o'), enabled),
                    footer_control(["Home(H)", "Home", "H"], KeyCode::Char('H'), enabled),
                ],
            };
            self.buttons.extend(layout_footer(area, controls));
            for button in self.buttons.iter().chain(self.toolbar.iter()) {
                frame.render_widget(
                    Paragraph::new(button.label.clone())
                        .alignment(Alignment::Center)
                        .style(footer_style(button.key, button.enabled)),
                    button.rect,
                );
            }
        }
        if self.modal.is_none() {
            self.draw_scrollbar(frame, body);
        }
    }
}

fn painted_row(text: &str, width: u16, style: Style) -> Line<'static> {
    let value = clipped(text, usize::from(width));
    let padding = usize::from(width).saturating_sub(value.width());
    Line::styled(format!("{value}{}", " ".repeat(padding)), style)
}
fn human_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn open_file(path: &Path) -> Result<()> {
    anyhow::ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1"),
        "Opening files in a tab requires Herdr"
    );
    anyhow::ensure!(
        path.is_absolute() && path.is_file(),
        "File is unavailable: {}",
        path.display()
    );
    let path = path
        .to_str()
        .context("Herdr cannot open a filename that is not valid UTF-8")?;
    let workspace = std::env::var("MUX_MONITOR_WORKSPACE")
        .or_else(|_| std::env::var("HERDR_WORKSPACE_ID"))
        .context("Missing Herdr workspace")?;
    let editor = crate::file_editor::editor()?;
    let editor = editor
        .to_str()
        .context("Editor configuration is not valid UTF-8")?;
    let cwd = Path::new(path)
        .parent()
        .context("Missing parent directory")?
        .to_str()
        .context("Directory is not valid UTF-8")?;
    open_file_tab(path, cwd, &workspace, editor, herdr)
}

fn open_file_tab(
    path: &str,
    cwd: &str,
    workspace: &str,
    editor: &str,
    mut execute: impl FnMut(&[&str]) -> Result<serde_json::Value>,
) -> Result<()> {
    let opened = execute(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        "mux",
        "--entrypoint",
        "file-editor",
        "--placement",
        "tab",
        "--workspace",
        workspace,
        "--cwd",
        cwd,
        "--env",
        &format!("MUX_FILE_PATH={path}"),
        "--env",
        &format!("MUX_FILE_EDITOR={editor}"),
        "--no-focus",
    ])?;
    let pane = &opened["result"]["plugin_pane"]["pane"];
    let tab = pane["tab_id"]
        .as_str()
        .context("File tab opened, but no tab ID was returned")?;
    let id = pane["pane_id"]
        .as_str()
        .context("File tab opened, but no pane ID was returned")?;
    execute(&["tab", "focus", tab])?;
    execute(&["plugin", "pane", "focus", id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn key(pane: &mut FilePane, code: KeyCode) -> bool {
        pane.input(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }
    fn pane(root: &Path) -> (FilePane, mpsc::Receiver<Request>) {
        let (send, receive) = mpsc::sync_channel(8);
        (
            FilePane {
                path: root.to_path_buf(),
                entries: service::list(root).unwrap(),
                requests: Some(send),
                ..Default::default()
            },
            receive,
        )
    }
    #[test]
    fn hidden_search_and_form_keys_do_not_switch_or_quit() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("中文qfg"), b"text").unwrap();
        std::fs::write(temp.path().join(".hidden"), b"text").unwrap();
        let (mut pane, _) = pane(temp.path());
        assert_eq!(pane.visible().len(), 1);
        key(&mut pane, KeyCode::Char('.'));
        assert_eq!(pane.visible().len(), 2);
        key(&mut pane, KeyCode::Char('/'));
        for ch in "qfg".chars() {
            assert!(key(&mut pane, KeyCode::Char(ch)));
        }
        assert_eq!(pane.filter, "qfg");
        assert_eq!(pane.visible().len(), 1);
        assert!(!pane.can_switch_workspace());
        key(&mut pane, KeyCode::Esc);
        assert!(pane.filter.is_empty());
        key(&mut pane, KeyCode::Char('n'));
        for ch in "qfg".chars() {
            key(&mut pane, KeyCode::Char(ch));
        }
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "qfg"));
        key(&mut pane, KeyCode::Esc);
        assert!(key(&mut pane, KeyCode::Char('q')));
    }
    #[test]
    fn delete_defaults_to_cancel_and_requires_explicit_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("keep");
        std::fs::write(&path, b"keep").unwrap();
        let (mut pane, receive) = pane(temp.path());
        key(&mut pane, KeyCode::Char('d'));
        assert!(matches!(
            pane.modal,
            Some(Modal::Delete { confirm: false, .. })
        ));
        key(&mut pane, KeyCode::Enter);
        assert!(receive.try_recv().is_err());
        assert!(path.exists());
        key(&mut pane, KeyCode::Char('d'));
        key(&mut pane, KeyCode::Right);
        key(&mut pane, KeyCode::Enter);
        assert!(
            matches!(receive.try_recv().unwrap(), Request::Execute(_, Operation::Delete(p)) if p == path)
        );
        assert!(pane.busy);
        assert!(!pane.can_switch_workspace());
        key(&mut pane, KeyCode::Char('n'));
        assert!(pane.modal.is_none());
    }
    #[test]
    fn navigation_pins_and_follow_waits_for_forms() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("sub");
        std::fs::create_dir(&directory).unwrap();
        let (mut pane, requests) = pane(temp.path());
        key(&mut pane, KeyCode::Enter);
        assert_eq!(pane.path, directory);
        assert!(pane.pinned);
        assert!(matches!(requests.try_recv().unwrap(), Request::Scan(p, _) if p == directory));
        pane.busy = false;
        pane.followed = Some(temp.path().to_path_buf());
        pane.poll(true);
        assert_eq!(pane.path, directory);
        key(&mut pane, KeyCode::Char('p'));
        key(&mut pane, KeyCode::Char('n'));
        pane.poll(true);
        assert_eq!(pane.path, directory);
        key(&mut pane, KeyCode::Esc);
        pane.poll(true);
        assert_eq!(pane.path, temp.path());
        assert!(!pane.pinned);
    }
    #[test]
    fn arrow_navigation_selects_enters_and_reselects_child_on_return() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let child = root.join("b-dir");
        std::fs::create_dir(root.join("a-dir")).unwrap();
        std::fs::create_dir(&child).unwrap();
        std::fs::write(root.join("c-file"), b"x").unwrap();
        let (mut pane, requests) = pane(root);
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        key(&mut pane, KeyCode::Up);
        assert_eq!(pane.selected, 0);
        for _ in 0..3 {
            key(&mut pane, KeyCode::Down);
        }
        assert_eq!(pane.selected, 2);
        key(&mut pane, KeyCode::Right);
        assert_eq!(pane.path, root);
        assert!(!pane.busy);
        assert!(requests.try_recv().is_err());
        key(&mut pane, KeyCode::Up);
        key(&mut pane, KeyCode::Right);
        assert_eq!(pane.path, child);
        assert!(pane.pinned);
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        send.send(Response::Listing(path, version, Ok(Vec::new())))
            .unwrap();
        pane.poll(false);
        key(&mut pane, KeyCode::Right);
        assert!(requests.try_recv().is_err());
        key(&mut pane, KeyCode::Left);
        assert_eq!(pane.path, root);
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        send.send(Response::Listing(
            path,
            version,
            Ok(service::list(root).unwrap()),
        ))
        .unwrap();
        pane.poll(false);
        assert_eq!(pane.selected_entry().unwrap().path, child);
        key(&mut pane, KeyCode::Char('/'));
        key(&mut pane, KeyCode::Left);
        key(&mut pane, KeyCode::Right);
        assert_eq!(pane.path, root);
        assert!(pane.filter_edit.is_some());
        key(&mut pane, KeyCode::Esc);
        pane.path = PathBuf::from("/");
        key(&mut pane, KeyCode::Left);
        assert_eq!(pane.path, Path::new("/"));
        assert!(requests.try_recv().is_err());
    }
    #[test]
    fn refresh_retains_selection_and_operation_errors() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("b"), b"keep").unwrap();
        let (mut pane, _requests) = pane(temp.path());
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        std::fs::write(temp.path().join("a"), b"new").unwrap();
        send.send(Response::Listing(
            temp.path().to_path_buf(),
            0,
            Ok(service::list(temp.path()).unwrap()),
        ))
        .unwrap();
        pane.poll(false);
        assert_eq!(
            pane.selected_entry().unwrap().path.file_name().unwrap(),
            "b"
        );
        send.send(Response::Completed(
            temp.path().to_path_buf(),
            Err("Permission denied".into()),
        ))
        .unwrap();
        pane.poll(false);
        send.send(Response::Listing(
            temp.path().to_path_buf(),
            0,
            Ok(service::list(temp.path()).unwrap()),
        ))
        .unwrap();
        pane.poll(false);
        assert_eq!(pane.error.as_deref(), Some("Permission denied"));
    }
    #[test]
    fn narrow_layout_buttons_match_clicks_and_input_does_not_overflow() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..20 {
            std::fs::write(temp.path().join(format!("file{n:02}")), b"x").unwrap();
        }
        for (width, height) in [(1, 1), (8, 6), (20, 10), (32, 12), (48, 30), (100, 40)] {
            let (mut pane, _requests) = pane(temp.path());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| pane.draw(f)).unwrap();
            for button in &pane.buttons {
                assert!(button.rect.right() <= width);
                assert!(button.rect.bottom() <= height);
            }
            if let Some(button) = pane
                .buttons
                .iter()
                .find(|b| b.key == KeyCode::Char('n') && b.rect.width > 0)
            {
                pane.input(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: button.rect.x,
                    row: button.rect.y,
                    modifiers: KeyModifiers::NONE,
                }));
                assert!(matches!(
                    pane.modal,
                    Some(Modal::Form {
                        kind: FormKind::File,
                        ..
                    })
                ));
            } else {
                key(&mut pane, KeyCode::Char('n'));
            }
            for ch in "中文很长文件名称qfg".chars() {
                key(&mut pane, KeyCode::Char(ch));
            }
            terminal.draw(|f| pane.draw(f)).unwrap();
            key(&mut pane, KeyCode::Esc);
            key(&mut pane, KeyCode::End);
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert_eq!(pane.selected, 19);
            assert!(pane.offset <= pane.selected);
        }
    }
    #[test]
    fn opening_tab_passes_literal_paths_and_focuses_returned_ids() {
        let path = "/tmp/中文 ' $(touch bad) file";
        let mut commands = Vec::new();
        open_file_tab(path, "/tmp", "w9", "vim -n", |args| {
            commands.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            Ok(serde_json::json!({"result":{"plugin_pane":{"pane":{"tab_id":"w9:t7","pane_id":"w9:p8"}}}}))
        }).unwrap();
        assert_eq!(commands.len(), 3);
        assert!(
            commands[0]
                .windows(2)
                .any(|a| a == ["--env", &format!("MUX_FILE_PATH={path}")])
        );
        assert!(
            commands[0]
                .windows(2)
                .any(|a| a == ["--env", "MUX_FILE_EDITOR=vim -n"])
        );
        assert!(commands[0].windows(2).any(|a| a == ["--workspace", "w9"]));
        assert!(commands[0].windows(2).any(|a| a == ["--cwd", "/tmp"]));
        assert_eq!(commands[1], ["tab", "focus", "w9:t7"]);
        assert_eq!(commands[2], ["plugin", "pane", "focus", "w9:p8"]);
        assert!(open_file_tab(path, "/tmp", "w9", "vi", |_| Ok(serde_json::Value::Null)).is_err());
        assert!(
            open_file_tab(path, "/tmp", "w9", "vi", |_| anyhow::bail!(
                "Herdr unavailable"
            ))
            .is_err()
        );
    }
    #[test]
    fn three_pages_keep_token_and_file_context_and_config_accepts_file() {
        let mut monitor = Monitor {
            page: config::PulseStartPage::Sessions,
            scroll: 7,
            ..Default::default()
        };
        monitor.files.path = PathBuf::from("/tmp/pinned");
        monitor.files.pinned = true;
        monitor.files.filter = "query".into();
        for (number, page) in [
            ('3', config::PulseStartPage::File),
            ('2', config::PulseStartPage::Git),
            ('3', config::PulseStartPage::File),
            ('1', config::PulseStartPage::Sessions),
        ] {
            assert!(
                monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char(number), KeyModifiers::ALT))
            );
            assert_eq!(monitor.page, page);
        }
        assert_eq!(monitor.scroll, 7);
        assert_eq!(monitor.files.path, PathBuf::from("/tmp/pinned"));
        assert_eq!(monitor.files.filter, "query");
        assert!(monitor.files.pinned);
        monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        monitor.files.form(FormKind::File);
        assert!(!monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)));
        let config: config::UiPreferences = toml::from_str("pulse_start_page = 'file'").unwrap();
        monitor.apply_preferences(config);
        monitor.apply_start_page();
        assert_eq!(monitor.page, config::PulseStartPage::File);
        assert_eq!(monitor.token_page, config::PulseStartPage::Sessions);
    }
    #[test]
    fn worker_disconnect_releases_busy_state_and_shows_error() {
        let mut pane = FilePane {
            busy: true,
            ..Default::default()
        };
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        drop(send);
        assert!(pane.poll(true));
        assert!(!pane.busy);
        assert!(pane.can_switch_workspace());
        assert!(pane.error.as_deref().unwrap().contains("worker stopped"));
        assert!(!pane.poll(true));
    }
    #[test]
    fn header_file_button_is_visible_clickable_and_respects_modal_at_narrow_sizes() {
        for (width, height) in [(20, 10), (24, 10), (32, 12), (48, 30), (100, 40)] {
            let screen = Rect::new(0, 0, width, height);
            let (area, reserve) = pulse_header_area(screen);
            let mut monitor = Monitor::default();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for page in [config::PulseStartPage::Home, config::PulseStartPage::Git] {
                monitor.page = page;
                terminal.draw(|f| monitor.draw(f)).unwrap();
                let rect = file_header_rect(area, reserve, page == config::PulseStartPage::Git);
                assert!(rect.width >= 3);
                let label = (rect.x..rect.right())
                    .map(|x| terminal.backend().buffer()[(x, rect.y)].symbol())
                    .collect::<String>();
                assert!(label.contains('F'));
                let click = Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                });
                assert!(monitor.workspace_mouse(&click, screen));
                assert_eq!(monitor.page, config::PulseStartPage::File);
                terminal.draw(|f| monitor.draw(f)).unwrap();
                let git = file_page_header_rects(area)[2];
                let click_git = Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: git.x,
                    row: git.y,
                    modifiers: KeyModifiers::NONE,
                });
                monitor.files.form(FormKind::File);
                assert!(!monitor.workspace_mouse(&click_git, screen));
                assert_eq!(monitor.page, config::PulseStartPage::File);
                key(&mut monitor.files, KeyCode::Esc);
                assert!(monitor.workspace_mouse(&click_git, screen));
                assert_eq!(monitor.page, config::PulseStartPage::Git);
            }
        }
    }
    #[test]
    fn long_delete_confirmation_can_scroll_without_selecting_delete() {
        let mut pane = FilePane {
            modal: Some(Modal::Delete {
                path: PathBuf::from(format!("/{}", "中文长路径/".repeat(30))),
                confirm: false,
            }),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert!(!pane.help_available());
        for _ in 0..30 {
            key(&mut pane, KeyCode::Down);
        }
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert!(pane.help_scroll > 0);
        assert!(matches!(
            pane.modal,
            Some(Modal::Delete { confirm: false, .. })
        ));
        key(&mut pane, KeyCode::Enter);
        assert!(pane.modal.is_none());
        assert!(!pane.busy);
    }
    #[test]
    fn parent_button_moves_up_and_home_returns_to_terminal_directory_in_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let nested = root.join("sub/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let (mut pane, requests) = pane(&nested);
        pane.home_path = root.to_path_buf();
        pane.followed = Some(root.join("sub"));
        pane.pinned = true;
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let up = pane
            .buttons
            .iter()
            .find(|b| b.key == KeyCode::Backspace)
            .unwrap()
            .rect;
        pane.input(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: up.x,
            row: up.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(pane.path, root.join("sub"));
        assert!(pane.pinned);
        assert!(matches!(requests.try_recv().unwrap(), Request::Scan(..)));
        pane.busy = false;
        pane.refreshing = false; // The simulated parent listing has completed.
        pane.path = nested;
        pane.filter = "query".into();
        let mut monitor = Monitor {
            page: config::PulseStartPage::File,
            files: pane,
            ..Default::default()
        };
        monitor.return_home();
        assert_eq!(monitor.page, config::PulseStartPage::File);
        assert_eq!(monitor.files.path, root.join("sub"));
        assert!(!monitor.files.pinned);
        assert!(monitor.files.filter.is_empty());
        assert!(matches!(requests.try_recv().unwrap(), Request::Scan(..)));
    }
    fn mouse(pane: &mut FilePane, kind: MouseEventKind, rect: Rect) {
        pane.input(&Event::Mouse(MouseEvent {
            kind,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        }));
    }
    #[test]
    fn automatic_refresh_is_silent_and_keeps_mouse_buttons_enabled() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file"), b"content").unwrap();
        let (mut pane, requests) = pane(temp.path());
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        pane.refreshed = Some(Instant::now() - Duration::from_secs(3));
        let mut terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let before = terminal.backend().buffer().clone();
        assert!(!pane.poll(true));
        assert!(pane.refreshing);
        assert!(!pane.busy);
        assert!(pane.can_switch_workspace());
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert_eq!(&before, terminal.backend().buffer());
        assert!(pane.buttons.iter().all(|b| b.enabled));
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!("Expected background scan")
        };
        send.send(Response::Listing(
            path,
            version,
            Ok(service::list(temp.path()).unwrap()),
        ))
        .unwrap();
        assert!(!pane.poll(true));
        assert!(!pane.refreshing);
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert_eq!(&before, terminal.backend().buffer());
    }
    #[test]
    fn pending_refresh_never_clears_operation_busy_and_old_generations_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file"), b"content").unwrap();
        let (mut pane, requests) = pane(temp.path());
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        pane.refresh();
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        pane.submit(Request::Execute(
            pane.path.clone(),
            Operation::Copy(pane.path.join("file"), pane.path.join("copy")),
        ));
        assert!(matches!(requests.try_recv().unwrap(), Request::Execute(..)));
        send.send(Response::Listing(path, version, Ok(Vec::new())))
            .unwrap();
        pane.poll(true);
        assert!(pane.busy);
        assert_eq!(pane.operation_label, Some("Copying…"));
        assert_eq!(pane.visible().len(), 1);
        assert!(!pane.can_switch_workspace());
        send.send(Response::Completed(pane.path.clone(), Ok("Copied".into())))
            .unwrap();
        pane.poll(true);
        assert!(!pane.busy);
        assert!(pane.refreshing);
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        pane.navigate(temp.path().join("other"), true);
        pane.navigate(temp.path().to_path_buf(), true);
        assert!(requests.try_recv().is_err()); // One scan at a time, even during rapid navigation.
        send.send(Response::Listing(
            path,
            version,
            Ok(service::list(temp.path()).unwrap()),
        ))
        .unwrap();
        pane.poll(true);
        assert!(pane.entries.is_empty()); // A→B→A rejects the earlier A generation too.
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Scan(_, v) if v == pane.scan_version)
        );
    }
    #[test]
    fn actions_wheel_reverses_immediately_and_keyboard_follows_selection() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file"), b"text").unwrap();
        for height in [12, 14, 16] {
            for with_source in [false, true] {
                let (mut pane, requests) = pane(temp.path());
                let source = with_source.then(|| pane.entries[0].clone());
                pane.open_actions(source);
                let mut terminal = Terminal::new(TestBackend::new(32, height)).unwrap();
                terminal.draw(|f| pane.draw(f)).unwrap();
                key(&mut pane, KeyCode::End);
                terminal.draw(|f| pane.draw(f)).unwrap();
                let bottom = pane.menu_offset;
                assert!(bottom > 0);
                let popup = pane.popup_area;
                mouse(&mut pane, MouseEventKind::ScrollUp, popup);
                terminal.draw(|f| pane.draw(f)).unwrap();
                assert!(pane.menu_offset < bottom);
                let up = pane.menu_offset;
                mouse(&mut pane, MouseEventKind::ScrollDown, popup);
                terminal.draw(|f| pane.draw(f)).unwrap();
                assert!(pane.menu_offset > up);
                for _ in 0..3 {
                    mouse(&mut pane, MouseEventKind::ScrollUp, popup);
                    terminal.draw(|f| pane.draw(f)).unwrap();
                }
                assert_eq!(pane.menu_offset, 0);
                if !with_source {
                    assert!(
                        !pane
                            .buttons
                            .iter()
                            .find(|b| b.key == KeyCode::Enter)
                            .unwrap()
                            .enabled
                    );
                }
                key(&mut pane, KeyCode::Down);
                terminal.draw(|f| pane.draw(f)).unwrap();
                assert!(!pane.menu_scroll_manual);
                let Some(Modal::Menu { selected, .. }) = pane.modal else {
                    panic!()
                };
                assert!(pane.menu_rows.iter().any(|(_, index)| *index == selected));
                assert!(requests.try_recv().is_err());
            }
        }
    }

    #[test]
    fn single_click_selects_double_click_opens_and_right_click_captures_target() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a"), b"a").unwrap();
        std::fs::write(temp.path().join("b"), b"b").unwrap();
        let (mut pane, requests) = pane(temp.path());
        let mut terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let second = pane.rows[1].0;
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), second);
        assert_eq!(pane.selected, 1);
        assert!(requests.try_recv().is_err());
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), second);
        assert!(
            matches!(requests.try_recv().unwrap(), Request::Open(_, p) if p == temp.path().join("b"))
        );
        pane.busy = false;
        terminal.draw(|f| pane.draw(f)).unwrap();
        let first = pane.rows[0].0;
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Right), first);
        assert!(
            matches!(&pane.modal, Some(Modal::Menu { source: Some(entry), .. }) if entry.path == temp.path().join("a"))
        );
        pane.entries.remove(0); // A refresh/removal must not retarget the menu to b.
        terminal.draw(|f| pane.draw(f)).unwrap();
        let delete = pane.menu_rows.iter().find(|(_, i)| *i == 4).unwrap().0;
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), delete);
        assert!(
            matches!(&pane.modal, Some(Modal::Delete { path, confirm: false }) if path == &temp.path().join("a"))
        );
        assert!(requests.try_recv().is_err()); // Right-click Delete still requires confirmation.
    }
    #[test]
    fn wheel_and_drag_scroll_during_refresh_and_operations() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..20 {
            std::fs::write(temp.path().join(format!("file{n:02}")), b"x").unwrap();
        }
        let (mut pane, _requests) = pane(temp.path());
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        pane.refreshing = true;
        let body = pane.body;
        mouse(&mut pane, MouseEventKind::ScrollDown, body);
        assert_eq!(pane.offset, 3);
        assert!(pane.selected >= pane.offset);
        pane.busy = true;
        let bottom = Rect::new(body.right() - 1, body.bottom() - 1, 1, 1);
        mouse(&mut pane, MouseEventKind::Drag(MouseButton::Left), bottom);
        assert_eq!(pane.offset, pane.visible().len() - pane.body_height);
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert!(pane.rows.iter().any(|(_, index)| *index == 19));
        assert!(pane.busy);
    }
    #[test]
    fn toolbar_and_breadcrumbs_are_clickable_and_parent_reselects_child() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let child = root.join("src");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(root.join(".hidden"), b"x").unwrap();
        let (mut pane, requests) = pane(&child);
        let (send, receive) = mpsc::channel();
        pane.updates = Some(receive);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let parent = pane
            .breadcrumbs
            .iter()
            .find(|(_, path)| path == &root)
            .unwrap()
            .0;
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), parent);
        assert_eq!(pane.path, root);
        assert!(pane.pinned);
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        send.send(Response::Listing(
            path,
            version,
            Ok(service::list(&root).unwrap()),
        ))
        .unwrap();
        pane.poll(false);
        terminal.draw(|f| pane.draw(f)).unwrap();
        let hidden = pane
            .toolbar
            .iter()
            .find(|b| b.key == KeyCode::Char('.'))
            .unwrap()
            .rect;
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), hidden);
        assert!(pane.hidden);
        assert_eq!(pane.visible().len(), 2);
        pane.navigate(child.clone(), true);
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        send.send(Response::Listing(path, version, Ok(Vec::new())))
            .unwrap();
        pane.poll(false);
        key(&mut pane, KeyCode::Backspace);
        let Request::Scan(path, version) = requests.try_recv().unwrap() else {
            panic!()
        };
        send.send(Response::Listing(
            path,
            version,
            Ok(service::list(&root).unwrap()),
        ))
        .unwrap();
        pane.poll(false);
        assert_eq!(pane.selected_entry().unwrap().path, child);
    }
    #[test]
    fn file_visual_hierarchy_keeps_selected_rows_and_navigation_visible() {
        let entries: Vec<_> = [
            "docs",
            "src",
            "Cargo.toml",
            "README.md",
            "herdr-plugin.toml",
            "LICENSE",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| Entry {
            path: PathBuf::from("/work/mux").join(name),
            directory: index < 2,
            link: false,
            size: 1024 * (index as u64 + 1),
            modified: Some(std::time::UNIX_EPOCH + Duration::from_secs(1791849600)),
        })
        .collect();
        for (width, height, mode) in [
            (48, 24, super::super::file_icons::IconMode::Nerd),
            (48, 24, super::super::file_icons::IconMode::Unicode),
            (32, 12, super::super::file_icons::IconMode::Nerd),
            (32, 12, super::super::file_icons::IconMode::Unicode),
        ] {
            let mut monitor = Monitor {
                page: config::PulseStartPage::File,
                files: FilePane {
                    path: PathBuf::from("/work/mux"),
                    home_path: PathBuf::from("/work/mux"),
                    entries: entries.clone(),
                    icon_mode: mode,
                    selected: 2,
                    pinned: true,
                    refreshed: Some(Instant::now()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| monitor.draw(f)).unwrap();
            let selected = monitor
                .files
                .rows
                .iter()
                .find(|(_, index)| *index == 2)
                .unwrap()
                .0;
            let buffer = terminal.backend().buffer();
            let header = (0..width)
                .map(|x| buffer[(x, 0)].symbol())
                .collect::<String>();
            assert!(header.contains("FILE"));
            assert!(!header.contains("FILEN") && !header.contains("]]"));
            for x in 0..width {
                assert_eq!(buffer[(x, 0)].bg, BG, "Header background at column {x}");
            }
            assert_eq!(buffer[(selected.x, selected.y)].bg, RAIL);
            assert_eq!(buffer[(selected.x, selected.y + 1)].bg, RAIL);
            assert!(
                buffer[(selected.x, selected.y)]
                    .modifier
                    .contains(Modifier::BOLD)
            );
            assert_eq!(monitor.files.toolbar.len(), 3);
            assert_eq!(monitor.files.buttons.len(), 4);
            assert!(
                monitor
                    .files
                    .breadcrumbs
                    .iter()
                    .any(|(_, path)| path == Path::new("/work"))
            );
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
                    PathBuf::from(&path).join(format!("file-{width}x{height}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"width": width, "height": height, "cells": cells}),
                    )
                    .unwrap(),
                )
                .unwrap();
                if mode == super::super::file_icons::IconMode::Unicode {
                    drop(cells);
                    key(&mut monitor.files, KeyCode::Char('o'));
                    terminal.draw(|f| monitor.draw(f)).unwrap();
                    let buffer = terminal.backend().buffer();
                    let cells: Vec<_> = buffer
                        .content
                        .iter()
                        .map(|cell| {
                            (
                                cell.symbol().to_string(),
                                format!("{:?}", cell.fg),
                                format!("{:?}", cell.bg),
                                cell.modifier.contains(Modifier::BOLD),
                            )
                        })
                        .collect();
                    std::fs::write(
                        PathBuf::from(&path).join(format!("file-actions-{width}.json")),
                        serde_json::to_vec(
                            &serde_json::json!({"width": width, "height": height, "cells": cells}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
            }
        }
    }
    #[test]
    fn saved_icon_settings_hot_reload_preserves_file_context() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: temp.path().join("config.toml"),
            cache: temp.path().join("cache.json"),
            state_dir: temp.path().join("state"),
        };
        let mut pane = FilePane {
            path: PathBuf::from("/project/src"),
            pinned: true,
            filter: "query".into(),
            selected: 3,
            offset: 2,
            busy: true,
            modal: Some(Modal::Form {
                kind: FormKind::File,
                text: "draft".into(),
                source: None,
            }),
            ..Default::default()
        };
        assert!(!pane.refresh_icon_mode(&paths));
        for mode in [
            super::super::file_icons::IconMode::Unicode,
            super::super::file_icons::IconMode::Nerd,
        ] {
            mode.save(&paths.state_dir.join("pulse-file-icons.json"))
                .unwrap();
            assert!(pane.refresh_icon_mode(&paths));
            assert!(!pane.refresh_icon_mode(&paths));
            assert_eq!(pane.icon_mode, mode);
            assert_eq!(pane.path, Path::new("/project/src"));
            assert!(pane.pinned && pane.busy);
            assert_eq!(pane.filter, "query");
            assert_eq!((pane.selected, pane.offset), (3, 2));
            assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "draft"));
        }
    }
    #[test]
    fn file_header_has_no_icon_switch_and_i_does_not_change_mode() {
        for (width, height) in [(1, 1), (8, 6), (20, 10), (32, 12), (48, 24), (100, 40)] {
            let screen = Rect::new(0, 0, width, height);
            let mut monitor = Monitor {
                page: config::PulseStartPage::File,
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| monitor.draw(f)).unwrap();
            let controls = workspace_header_rects(pulse_header_area(screen).0)[3];
            let label: String = (controls.x..controls.right())
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect();
            assert!(label.trim().is_empty());
            assert!(!monitor.workspace_mouse(
                &Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: controls.x,
                    row: controls.y,
                    modifiers: KeyModifiers::NONE,
                }),
                screen
            ));
            key(&mut monitor.files, KeyCode::Char('i'));
            assert_eq!(
                monitor.files.icon_mode,
                super::super::file_icons::IconMode::Nerd
            );
            key(&mut monitor.files, KeyCode::Char('n'));
            key(&mut monitor.files, KeyCode::Char('i'));
            assert!(matches!(&monitor.files.modal, Some(Modal::Form { text, .. }) if text == "i"));
        }
    }
    #[test]
    fn actions_popup_is_bottom_anchored_and_restores_background_on_dismiss() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..20 {
            std::fs::write(temp.path().join(format!("file{n:02}")), b"x").unwrap();
        }
        let (mut pane, requests) = pane(temp.path());
        pane.icon_mode = super::super::file_icons::IconMode::Unicode;
        pane.selected = 2;
        let mut terminal = Terminal::new(TestBackend::new(48, 28)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let before = terminal.backend().buffer().clone();
        let position = (pane.selected, pane.offset);
        key(&mut pane, KeyCode::Char('o'));
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert_eq!(pane.popup_area.bottom(), pane.body.bottom());
        assert!(pane.popup_area.y > pane.body.y);
        assert!(!pane.menu_rows.is_empty());
        for y in pane.body.y..pane.popup_area.y {
            for x in pane.body.x..pane.body.right() {
                assert_eq!(terminal.backend().buffer()[(x, y)], before[(x, y)]);
            }
        }
        let outside = Rect::new(pane.body.x, pane.body.y, 1, 1);
        mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), outside);
        assert!(pane.modal.is_none());
        assert_eq!((pane.selected, pane.offset), position);
        assert!(requests.try_recv().is_err());
        terminal.draw(|f| pane.draw(f)).unwrap();
        // Terminal diffs omit the continuation cells covered by wide emoji glyphs.
        assert!(before.diff(terminal.backend().buffer()).is_empty());
    }
    #[test]
    fn actions_child_dialogs_return_to_the_menu_and_failed_submission_keeps_draft() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("original"), b"x").unwrap();
        let (mut pane, requests) = pane(temp.path());
        let mut terminal = Terminal::new(TestBackend::new(48, 24)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        key(&mut pane, KeyCode::Char('o'));
        key(&mut pane, KeyCode::Char('c'));
        terminal.draw(|f| pane.draw(f)).unwrap();
        assert_eq!(pane.popup_area.bottom(), pane.body.bottom());
        assert!(matches!(
            pane.modal,
            Some(Modal::Form {
                kind: FormKind::Copy,
                ..
            })
        ));
        key(&mut pane, KeyCode::Enter); // Invalid empty input leaves the dialog open.
        assert!(matches!(pane.modal, Some(Modal::Form { .. })));
        key(&mut pane, KeyCode::Esc);
        assert!(
            matches!(&pane.modal, Some(Modal::Menu { selected: 2, source: Some(entry) }) if entry.path == temp.path().join("original"))
        );
        key(&mut pane, KeyCode::Char('d'));
        assert!(matches!(
            pane.modal,
            Some(Modal::Delete { confirm: false, .. })
        ));
        key(&mut pane, KeyCode::Enter); // Default cancel returns to Actions.
        assert!(matches!(pane.modal, Some(Modal::Menu { selected: 4, .. })));
        key(&mut pane, KeyCode::Char('R'));
        key(&mut pane, KeyCode::Char('q'));
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "originalq"));
        drop(requests); // A stopped worker must not discard the user's draft.
        key(&mut pane, KeyCode::Enter);
        assert!(matches!(&pane.modal, Some(Modal::Form { text, .. }) if text == "originalq"));
        assert!(!pane.busy);
        key(&mut pane, KeyCode::Esc);
        assert!(matches!(pane.modal, Some(Modal::Menu { selected: 1, .. })));
        key(&mut pane, KeyCode::Char('d'));
        key(&mut pane, KeyCode::Right);
        key(&mut pane, KeyCode::Enter);
        assert!(matches!(
            pane.modal,
            Some(Modal::Delete { confirm: true, .. })
        ));
        assert!(!pane.busy);
    }
    #[test]
    fn popup_bounds_scroll_and_unavailable_actions_at_small_sizes() {
        let temp = tempfile::tempdir().unwrap();
        for (width, height) in [(1, 1), (8, 6), (20, 10), (32, 12), (48, 24), (100, 40)] {
            let (mut pane, requests) = pane(temp.path());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| pane.draw(f)).unwrap();
            key(&mut pane, KeyCode::Char('o'));
            assert!(matches!(
                pane.modal,
                Some(Modal::Menu {
                    source: None,
                    selected: 5
                })
            ));
            key(&mut pane, KeyCode::Up);
            assert!(matches!(pane.modal, Some(Modal::Menu { selected: 5, .. })));
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert!(pane.popup_area.right() <= width);
            assert!(pane.popup_area.bottom() <= height);
            key(&mut pane, KeyCode::End);
            terminal.draw(|f| pane.draw(f)).unwrap();
            if pane.menu_height > 0 {
                assert!(
                    pane.menu_rows
                        .iter()
                        .any(|(_, index)| *index == MENU.len() - 1)
                );
            }
            for (rect, _) in &pane.menu_rows {
                assert!(rect.bottom() <= pane.popup_area.bottom());
            }
            key(&mut pane, KeyCode::Esc);
            key(&mut pane, KeyCode::Char('n'));
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert!(pane.popup_area.right() <= width);
            assert!(pane.popup_area.bottom() <= height);
            key(&mut pane, KeyCode::Esc);
            assert!(requests.try_recv().is_err());
        }
    }
    #[test]
    fn inline_filter_mouse_input_clear_and_click_back_to_list() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("中文qfg.txt"), b"x").unwrap();
        std::fs::write(temp.path().join("other.txt"), b"x").unwrap();
        for (width, height) in [(8, 6), (20, 10), (32, 12), (48, 24)] {
            let (mut pane, requests) = pane(temp.path());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert!(pane.filter_rect.bottom() <= pane.body.y);
            let filter = pane.filter_rect;
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(filter.x, filter.y)].fg, SOFT);
            for y in filter.y..filter.bottom() {
                for x in filter.x..filter.right() {
                    assert_eq!(buffer[(x, y)].bg, BG);
                }
            }
            if filter.height == 3 {
                assert_eq!(buffer[(filter.x, filter.y)].symbol(), "┌");
                assert_eq!(
                    buffer[(filter.right() - 1, filter.bottom() - 1)].symbol(),
                    "┘"
                );
            }
            assert!(
                !pane
                    .buttons
                    .iter()
                    .any(|button| button.key == KeyCode::Char('/'))
            );
            let rect = pane.filter_rect;
            mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), rect);
            for ch in "qfg".chars() {
                key(&mut pane, KeyCode::Char(ch));
            }
            assert_eq!(pane.filter, "qfg");
            assert_eq!(pane.visible().len(), 1);
            assert!(pane.modal.is_none());
            assert!(pane.filter_edit.is_some());
            assert!(!pane.can_switch_workspace());
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert_eq!(pane.popup_area.height, 0);
            assert_eq!(terminal.backend().buffer()[(filter.x, filter.y)].fg, SOFT);
            let clear = pane.filter_clear_rect;
            assert!(clear.width > 0);
            mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), clear);
            assert!(pane.filter.is_empty());
            assert_eq!(pane.visible().len(), 2);
            key(&mut pane, KeyCode::Enter);
            assert!(pane.filter_edit.is_none());
            assert!(requests.try_recv().is_err());
            key(&mut pane, KeyCode::Char('/'));
            key(&mut pane, KeyCode::Char('q'));
            terminal.draw(|f| pane.draw(f)).unwrap();
            let row = pane.rows[0].0;
            mouse(&mut pane, MouseEventKind::Down(MouseButton::Left), row);
            assert!(pane.filter_edit.is_none());
            assert_eq!(pane.filter, "q");
            assert!(pane.modal.is_none());
            terminal.draw(|f| pane.draw(f)).unwrap();
            assert_eq!(terminal.backend().buffer()[(filter.x, filter.y)].fg, SOFT);
        }
    }
    #[test]
    fn inline_filter_cancel_restores_position_and_follow_waits_for_editing() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..8 {
            std::fs::write(temp.path().join(format!("item{index}")), b"x").unwrap();
        }
        let (mut pane, _requests) = pane(temp.path());
        pane.selected = 5;
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        terminal.draw(|f| pane.draw(f)).unwrap();
        let position = (pane.selected, pane.offset);
        key(&mut pane, KeyCode::Char('/'));
        key(&mut pane, KeyCode::Char('7'));
        assert_eq!(pane.visible().len(), 1);
        pane.followed = Some(temp.path().join("another"));
        pane.poll(true);
        assert_eq!(pane.path, temp.path());
        key(&mut pane, KeyCode::Esc);
        assert!(pane.filter.is_empty());
        assert_eq!((pane.selected, pane.offset), position);
        assert!(pane.filter_edit.is_none());
        key(&mut pane, KeyCode::Char('/'));
        key(&mut pane, KeyCode::Char('7'));
        key(&mut pane, KeyCode::Enter);
        assert_eq!(pane.filter, "7");
        assert!(pane.filter_edit.is_none());
        assert!(pane.modal.is_none());
    }
}
