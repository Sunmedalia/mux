use super::*;
use crate::grok::{
    auth::{self, Action},
    usage,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};

enum Update {
    Progress(String),
    Done(Result<auth::Status, String>),
    Saved(String),
    Usage(u64, Result<usage::Snapshot, String>),
}
pub(super) struct AuthUi {
    accounts: BTreeMap<String, crate::grok::accounts::Account>,
    account_index: usize,
    pub busy: bool,
    pub home_selected: bool,
    pub page: Option<AccountPage>,
    cancel: Arc<AtomicBool>,
    sender: Sender<Update>,
    receiver: Receiver<Update>,
    pub(super) status: auth::Status,
    pub(super) message: String,
    progress: Vec<String>,
    pub(super) usage: Option<usage::Snapshot>,
    pub(super) usage_error: Option<String>,
    usage_refreshing: bool,
    usage_request: u64,
    usage_waking: bool,
    account_added: bool,
}
impl Default for AuthUi {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            accounts: BTreeMap::new(),
            account_index: 0,
            busy: false,
            home_selected: false,
            page: None,
            cancel: Arc::new(AtomicBool::new(false)),
            sender,
            receiver,
            status: Default::default(),
            message: "Browser or Device code signs in through Grok.".into(),
            progress: vec![],
            usage: None,
            usage_error: None,
            usage_refreshing: false,
            usage_request: 0,
            usage_waking: false,
            account_added: false,
        }
    }
}
impl Drop for AuthUi {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Clone)]
pub(super) struct AccountPage {
    pub selected: usize,
    pub model: FormField,
    pub confirm_logout: bool,
    pub(super) confirm_account: Option<(String, bool)>,
    pub scroll: u16,
}
const ACTIONS: [&str; 10] = [
    "Browser (b)",
    "Device code (d)",
    "Use OAuth (u)",
    "Refresh (r)",
    "Wake (w)",
    "Sign out (x)",
    "Back (Esc)",
    "Switch (p)",
    "Import (i)",
    "Delete (X)",
];
pub(super) fn account_actions(screen: Rect) -> Vec<Rect> {
    let natural: u16 = ACTIONS
        .iter()
        .map(|label| UnicodeWidthStr::width(*label) as u16)
        .sum();
    let roomy = natural + ACTIONS.len() as u16 * 2 + (ACTIONS.len() as u16 - 1) * 2 <= screen.width;
    let padding = if roomy { 2 } else { 0 };
    let gap = if roomy { 2 } else { 1 };
    let mut x = screen.x;
    let mut y = 0;
    let mut actions: Vec<Rect> = ACTIONS
        .iter()
        .map(|label| {
            let width = UnicodeWidthStr::width(*label) as u16 + padding;
            if x + width > screen.right() {
                x = screen.x;
                y += 1;
            }
            let rect = Rect::new(x, y, width, 1);
            x += width + gap;
            rect
        })
        .collect();
    let height = actions.last().map_or(0, |rect| rect.bottom());
    let offset = screen.bottom().saturating_sub(height);
    for rect in &mut actions {
        rect.y += offset;
    }
    actions
}
pub(super) fn account_rows(area: Rect, busy: bool, embedded: bool) -> [Rect; 4] {
    let actions = account_actions(Rect::new(0, 0, area.width, 100));
    let height = actions
        .last()
        .map_or(1, |last| last.bottom() - actions[0].y);
    if embedded {
        embedded_account_rows_with_footer(area, busy, height)
    } else {
        account_page_rows_with_footer(area, busy, height)
    }
}
pub(super) fn confirmation_area(screen: Rect) -> Rect {
    centered_rect(
        screen.width.saturating_sub(4).min(72),
        screen.height.saturating_sub(2).min(8),
        screen,
    )
}
impl App {
    pub(super) fn home_grok_oauth_selected(&self) -> bool {
        self.grok_enabled
            && self.view_mode == ViewMode::Home
            && self.home_all_selected
            && self.grok_auth.home_selected
    }
    pub(super) fn grok_oauth_provider_lines(&self, width: u16) -> Vec<Line<'static>> {
        let status = &self.grok_auth.status;
        let native_default = self
            .config
            .grok
            .preferences
            .default
            .as_deref()
            .filter(|model| {
                !model.starts_with("mux::")
                    && !self.config.grok.imports.values().any(|key| key == model)
            });
        let active = status.saved
            && self.config.grok.active_mode == Some(crate::grok::Mode::Account)
            && native_default.is_some();
        let mut lines = wrap_styled_segments(
            vec![
                (
                    (if active { " ● " } else { " ○ " }).into(),
                    Style::default().fg(if active { ENABLED } else { MUTED }),
                ),
                (
                    "Grok OAuth Account".into(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                ("  [OAuth]".into(), Style::default().fg(ROUTE)),
            ],
            width,
        );
        for text in [
            format!(
                "     Account: {}",
                status.email.as_deref().unwrap_or(if status.saved {
                    "Saved native login"
                } else {
                    "Not signed in"
                })
            ),
            format!(
                "     {}",
                if active {
                    "Native model selected"
                } else {
                    "Enter to configure"
                }
            ),
            "     Credential: Grok OAuth".into(),
        ] {
            lines.extend(wrap_styled_segments(
                vec![(text, Style::default().fg(MUTED))],
                width,
            ));
        }
        lines.push(Line::raw(""));
        lines
    }
    fn refresh_grok_accounts(&mut self) {
        if let Ok(config) = config::load(&self.paths.config) {
            self.grok_auth.accounts = config.grok.accounts;
            self.config.grok.accounts = self.grok_auth.accounts.clone();
            self.grok_auth.account_index = self
                .grok_auth
                .account_index
                .min(self.grok_auth.accounts.len().saturating_sub(1));
        }
    }
    fn selected_grok_account(&self) -> Option<String> {
        self.grok_auth
            .accounts
            .keys()
            .nth(self.grok_auth.account_index)
            .cloned()
    }
    pub(super) fn load_grok_auth_status(&mut self) {
        self.grok_auth.status = auth::status(&self.grok_home).unwrap_or_default();
        self.reconcile_grok_usage();
    }
    fn reconcile_grok_usage(&mut self) {
        let account = usage::account(&self.grok_home).ok().flatten();
        if self
            .grok_auth
            .usage
            .as_ref()
            .is_some_and(|snapshot| Some(&snapshot.account) != account.as_ref())
        {
            self.grok_auth.usage = None;
            self.grok_auth.usage_error = None;
        }
        if !self.grok_auth.status.saved {
            self.grok_auth.usage = None;
        }
    }
    fn refresh_grok_usage(&mut self) {
        self.refresh_grok_auth();
        self.reconcile_grok_usage();
        if self.grok_auth.busy || self.grok_auth.usage_refreshing {
            return;
        }
        if !self.grok_auth.status.saved {
            self.grok_auth.usage_error = Some("Sign in to view account usage".into());
            return;
        }
        self.grok_auth.usage_request = self.grok_auth.usage_request.wrapping_add(1);
        self.grok_auth.usage_refreshing = true;
        self.grok_auth.usage_error = None;
        self.spawn_grok_usage();
    }
    fn wake_grok_usage(&mut self) -> Result<()> {
        if self.grok_auth.busy || self.grok_auth.usage_refreshing {
            return Ok(());
        }
        let id = usage::account(&self.grok_home)?
            .ok_or_else(|| anyhow::anyhow!("Sign in before waking this account"))?;
        self.grok_auth.usage_request = self.grok_auth.usage_request.wrapping_add(1);
        self.grok_auth.usage_refreshing = true;
        self.grok_auth.usage_waking = true;
        self.grok_auth.usage_error = None;
        self.spawn_grok_wake(id);
        Ok(())
    }
    #[cfg(not(test))]
    fn spawn_grok_wake(&self, id: String) {
        let home = self.grok_home.clone();
        let sender = self.grok_auth.sender.clone();
        let request = self.grok_auth.usage_request;
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                usage::wake(&home, &id)?;
                usage::fetch(&home)
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Grok wake worker stopped")))
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Update::Usage(request, result));
        });
    }
    #[cfg(test)]
    fn spawn_grok_wake(&self, _: String) {}
    #[cfg(not(test))]
    fn spawn_grok_usage(&self) {
        let home = self.grok_home.clone();
        let sender = self.grok_auth.sender.clone();
        let request = self.grok_auth.usage_request;
        std::thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| usage::fetch(&home)))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Grok usage worker stopped")))
                    .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Update::Usage(request, result));
        });
    }
    // UI tests inject results through the channel; no fixture credentials go online.
    #[cfg(test)]
    fn spawn_grok_usage(&self) {}
    pub(super) fn open_grok_auth(&mut self) {
        let import_error = crate::grok::accounts::capture(&self.paths, &self.grok_home).err();
        self.refresh_grok_accounts();
        if let Ok(Some(id)) = crate::grok::accounts::current_id(&self.grok_home)
            && let Some(index) = self.grok_auth.accounts.keys().position(|key| key == &id)
        {
            self.grok_auth.account_index = index;
        }
        self.refresh_grok_auth();
        let default = self
            .config
            .grok
            .preferences
            .default
            .as_deref()
            .filter(|model| crate::grok::validate_oauth_model(&self.grok_home, model).is_ok())
            .unwrap_or("grok-build");
        self.grok_auth.page = Some(AccountPage {
            selected: 1,
            model: field("Native model", default),
            confirm_logout: false,
            confirm_account: None,
            scroll: 0,
        });
        if provider_workspace(self.screen) {
            self.return_home();
            self.select_home_index(1);
            self.focus = Focus::Details;
        }
        self.refresh_grok_usage();
        if let Some(error) = import_error {
            self.grok_auth.message = format!("Could not import current login: {error}");
        }
    }
    fn refresh_grok_auth(&mut self) {
        match auth::status(&self.grok_home) {
            Ok(status) => {
                self.grok_auth.status = status;
                self.grok_auth.message =
                    "Add accounts with Browser or Device code; Switch (p) selects the active login."
                        .into();
            }
            Err(error) => {
                self.grok_auth.status = Default::default();
                self.grok_auth.message = format!("Cannot inspect local login: {error:#}");
            }
        }
    }
    fn start_grok_auth(&mut self, action: Action) {
        if self.grok_auth.busy {
            return;
        }
        if self.grok_auth.usage_waking {
            self.grok_auth.message = "Wait for Wake to finish before changing login".into();
            return;
        }
        self.grok_auth.account_added = false;
        self.grok_auth.usage_request = self.grok_auth.usage_request.wrapping_add(1);
        self.grok_auth.usage_refreshing = false;
        self.grok_auth.usage_waking = false;
        self.grok_auth.usage = None;
        self.grok_auth.usage_error = None;
        self.grok_auth.busy = true;
        self.grok_auth.cancel = Arc::new(AtomicBool::new(false));
        self.grok_auth.progress.clear();
        self.grok_auth.message = if action == Action::Logout {
            "Signing out…"
        } else {
            "Waiting for Grok authorization… · Esc cancels"
        }
        .into();
        let cancel = self.grok_auth.cancel.clone();
        let sender = self.grok_auth.sender.clone();
        let home = self.grok_home.clone();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if action == Action::Logout {
                    crate::grok::accounts::capture(&paths, &home)?;
                    auth::run(action, &home, &cancel, |text| {
                        let _ = sender.send(Update::Progress(text));
                    })
                } else {
                    let id =
                        crate::grok::accounts::login(&paths, &home, action, &cancel, |text| {
                            let _ = sender.send(Update::Progress(text));
                        })?;
                    let _ = sender.send(Update::Saved(id));
                    auth::status(&home)
                }
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Grok authorization worker stopped")))
            .map_err(|e| format!("{e:#}"));
            let _ = sender.send(Update::Done(result));
        });
    }
    pub(super) fn cancel_grok_auth(&mut self) {
        if self.grok_auth.busy {
            self.grok_auth.cancel.store(true, Ordering::Relaxed);
            self.grok_auth.message = "Cancelling Grok authorization…".into();
        }
    }
    pub(super) fn poll_grok_auth(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.grok_auth.receiver.try_recv() {
            changed = true;
            match update {
                Update::Usage(request, result) => {
                    if request != self.grok_auth.usage_request {
                        continue;
                    }
                    let waking = std::mem::take(&mut self.grok_auth.usage_waking);
                    self.grok_auth.usage_refreshing = false;
                    self.load_grok_auth_status();
                    match result {
                        Ok(snapshot)
                            if usage::account(&self.grok_home).ok().flatten().as_ref()
                                == Some(&snapshot.account) =>
                        {
                            if waking {
                                self.grok_auth.message = "Wake complete · usage refreshed".into();
                            }
                            self.grok_auth.usage = Some(snapshot);
                            self.grok_auth.usage_error = None;
                        }
                        Ok(_) => {
                            self.grok_auth.usage_error = Some(
                                "Account changed during refresh; press r to reload usage".into(),
                            )
                        }
                        Err(error) => self.grok_auth.usage_error = Some(error),
                    }
                }
                Update::Progress(text) => {
                    if !self.grok_auth.progress.contains(&text) {
                        if self.grok_auth.progress.len() == 8 {
                            self.grok_auth.progress.remove(0);
                        }
                        if text.starts_with("Device code:") {
                            self.grok_auth.progress.insert(0, text);
                        } else {
                            self.grok_auth.progress.push(text);
                        }
                    }
                }
                Update::Saved(id) => {
                    self.grok_auth.account_added = true;
                    self.refresh_grok_accounts();
                    if let Some(index) = self.grok_auth.accounts.keys().position(|key| key == &id) {
                        self.grok_auth.account_index = index;
                    }
                    self.grok_auth.message =
                        "Account added · select it and Switch (p) to use it".into();
                }
                Update::Done(result) => {
                    self.refresh_grok_accounts();
                    let succeeded = result.is_ok();
                    self.grok_auth.busy = false;
                    self.grok_auth.progress.clear();
                    match result {
                        Ok(status) => {
                            self.grok_auth.message = if self.grok_auth.account_added {
                                "Account saved · Switch (p) to use it · current login retained"
                            } else if status.saved {
                                "Accounts saved · Switch (p) selects login · Use OAuth selects startup model"
                            } else if self.config.grok.active_mode == Some(crate::grok::Mode::Account) {
                                "Grok signed out · API providers remain available"
                            } else {
                                "Grok signed out · API provider configurations retained"
                            }
                            .into();
                            self.grok_auth.status = status;
                            self.status_error = false;
                        }
                        Err(error) => {
                            self.status_error = true;
                            self.grok_auth.message = error;
                            self.grok_auth.status =
                                auth::status(&self.grok_home).unwrap_or_default();
                        }
                    }
                    self.status = self.grok_auth.message.clone();
                    self.reconcile_grok_usage();
                    if succeeded && self.grok_auth.status.saved {
                        let message = self.grok_auth.message.clone();
                        self.refresh_grok_usage();
                        self.grok_auth.message = message;
                    }
                }
            }
        }
        changed
    }
    pub(super) fn grok_auth_key(
        &mut self,
        dialog: &mut AccountPage,
        mut key: KeyEvent,
    ) -> Result<bool> {
        if self.grok_auth.busy {
            if key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                self.cancel_grok_auth();
            } else if key.code == KeyCode::PageDown {
                dialog.scroll = dialog.scroll.saturating_add(1);
            } else if key.code == KeyCode::PageUp {
                dialog.scroll = dialog.scroll.saturating_sub(1);
            }
            return Ok(false);
        }
        if let Some((id, delete)) = dialog.confirm_account.clone() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('n') => dialog.confirm_account = None,
                KeyCode::Enter | KeyCode::Char('y') => {
                    if self.grok_auth.usage_refreshing {
                        self.grok_auth.message =
                            "Wait for usage refresh or Wake to finish before changing accounts"
                                .into();
                        return Ok(false);
                    }
                    if delete {
                        crate::grok::accounts::remove(&self.paths, &self.grok_home, &id)?;
                    } else {
                        crate::grok::accounts::activate(&self.paths, &self.grok_home, &id)?;
                    }
                    dialog.confirm_account = None;
                    self.refresh_grok_accounts();
                    self.load_grok_auth_status();
                    self.grok_auth.message = if delete {
                        "Saved account deleted"
                    } else {
                        "Grok account switched · restart Grok for new sessions"
                    }
                    .into();
                }
                _ => {}
            }
            return Ok(false);
        }
        if dialog.confirm_logout {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    dialog.confirm_logout = false;
                    self.start_grok_auth(Action::Logout);
                }
                KeyCode::Esc | KeyCode::Char('n') => dialog.confirm_logout = false,
                _ => {}
            }
            return Ok(false);
        }
        if dialog.selected == ACTIONS.len() + 1 {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.grok_auth.account_index = (self.grok_auth.account_index + 1)
                        .min(self.grok_auth.accounts.len().saturating_sub(1));
                    return Ok(false);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.grok_auth.account_index = self.grok_auth.account_index.saturating_sub(1);
                    return Ok(false);
                }
                KeyCode::Enter => {
                    if let Some(id) = self.selected_grok_account() {
                        dialog.confirm_account = Some((id, false));
                    }
                    return Ok(false);
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Char('a') && dialog.selected != 0 {
            dialog.selected = ACTIONS.len() + 1;
            return Ok(false);
        }
        if dialog.selected != 0 && key.modifiers.is_empty() {
            key.code = match key.code {
                KeyCode::Char('h') => KeyCode::Esc,
                KeyCode::Char('l') => KeyCode::Enter,
                KeyCode::Char('j') => KeyCode::Down,
                KeyCode::Char('k') => KeyCode::Up,
                other => other,
            };
        }
        match key.code {
            KeyCode::Esc => return Ok(true),
            KeyCode::Tab | KeyCode::Down => {
                dialog.selected = (dialog.selected + 1) % (ACTIONS.len() + 2);
                return Ok(false);
            }
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected = (dialog.selected + ACTIONS.len() + 1) % (ACTIONS.len() + 2);
                return Ok(false);
            }
            KeyCode::PageDown => {
                dialog.scroll = dialog.scroll.saturating_add(1);
                return Ok(false);
            }
            KeyCode::PageUp => {
                dialog.scroll = dialog.scroll.saturating_sub(1);
                return Ok(false);
            }
            _ => {}
        }
        if dialog.selected == 0 {
            if key.code == KeyCode::Enter {
                dialog.selected = 1;
            } else {
                handle_form_key(std::slice::from_mut(&mut dialog.model), &mut 0, key);
            }
            return Ok(false);
        }
        let selected = match key.code {
            KeyCode::Char('b') => 1,
            KeyCode::Char('d') => 2,
            KeyCode::Char('u') => 3,
            KeyCode::Char('r' | 's') => 4,
            KeyCode::Char('w') => 5,
            KeyCode::Char('x') => 6,
            KeyCode::Char('q') => 7,
            KeyCode::Char('p') => 8,
            KeyCode::Char('i') => 9,
            KeyCode::Char('X') => 10,
            KeyCode::Enter => dialog.selected,
            _ => return Ok(false),
        };
        dialog.scroll = 0;
        match selected {
            1 => self.start_grok_auth(Action::Browser),
            2 => self.start_grok_auth(Action::Device),
            3 => {
                if self.selected_grok_account().is_some()
                    && self.selected_grok_account()
                        != crate::grok::accounts::current_id(&self.grok_home)?
                {
                    self.grok_auth.message =
                        "Switch the selected account with p before using OAuth".into();
                    return Ok(false);
                }
                let model = dialog.model.value.trim().to_owned();
                self.select_grok_oauth(model, false)?;
            }
            4 | 5 => {
                if self.selected_grok_account().is_some()
                    && self.selected_grok_account()
                        != crate::grok::accounts::current_id(&self.grok_home)?
                {
                    self.grok_auth.message =
                        "Switch the selected saved account with p before refreshing or waking it"
                            .into();
                } else if selected == 4 {
                    self.refresh_grok_usage();
                } else {
                    self.wake_grok_usage()?;
                }
            }
            6 => dialog.confirm_logout = true,
            7 => return Ok(true),
            8 => {
                if let Some(id) = self.selected_grok_account() {
                    dialog.confirm_account = Some((id, false));
                }
            }
            9 => {
                crate::grok::accounts::capture(&self.paths, &self.grok_home)?;
                self.refresh_grok_accounts();
                self.grok_auth.message = "Current native login saved".into();
            }
            10 => {
                if let Some(id) = self.selected_grok_account() {
                    dialog.confirm_account = Some((id, true));
                }
            }
            _ => {}
        }
        Ok(false)
    }

    pub(super) fn select_grok_oauth(&mut self, model: String, reconnect: bool) -> Result<()> {
        crate::grok::validate_oauth_model(&self.grok_home, &model)?;
        if !reconnect {
            let conflicts = crate::grok::conflicts(&self.paths, &self.grok_home)?;
            if !conflicts.is_empty() {
                self.modal = Some(Modal::Grok(Box::new(grok::Dialog::Reconnect {
                    conflicts,
                    preferred: Some(model),
                    api: false,
                    scroll: 0,
                })));
                return Ok(());
            }
        }
        let previous_grok = self.config.grok.clone();
        let previous_profiles = self.config.profiles.clone();
        self.config = self.update_client_config(|c| {
            c.grok.use_account(&mut c.profiles, model.clone());
            Ok(())
        })?;
        if let Err(error) =
            crate::grok::apply(&self.paths, &self.grok_home, &self.config, None, reconnect)
        {
            let written_default = std::fs::read_to_string(self.grok_home.join("config.toml"))
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .and_then(|doc| {
                    doc.get("models")?
                        .get("default")?
                        .as_str()
                        .map(str::to_owned)
                });
            if written_default.as_deref() != Some(model.as_str()) {
                self.config = self.update_client_config(|c| {
                    c.grok = previous_grok.clone();
                    c.profiles = previous_profiles.clone();
                    Ok(())
                })?;
            }
            return Err(error);
        }
        self.grok_auth.message =
            "Grok OAuth selected · API providers remain available via /model · restart Grok".into();
        self.status = self.grok_auth.message.clone();
        self.status_error = false;
        Ok(())
    }
    pub(super) fn grok_auth_page_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(mut page) = self.grok_auth.page.take() else {
            return Ok(());
        };
        let result = self.grok_auth_key(&mut page, key);
        if let Err(error) = &result {
            self.grok_auth.message = format!("{error:#}");
            self.status_error = true;
        }
        if !matches!(&result, Ok(true)) {
            self.grok_auth.page = Some(page);
        }
        result.map(|_| ())
    }
    pub(super) fn grok_account_list_mouse(&mut self, area: Rect, mouse: MouseEvent) -> bool {
        if !contains(area, mouse.column, mouse.row) || self.grok_auth.busy {
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.grok_auth.account_index = (self.grok_auth.account_index + 1)
                    .min(self.grok_auth.accounts.len().saturating_sub(1))
            }
            MouseEventKind::ScrollUp => {
                self.grok_auth.account_index = self.grok_auth.account_index.saturating_sub(1)
            }
            MouseEventKind::Down(MouseButton::Left)
                if mouse.row > area.y && mouse.row < area.bottom().saturating_sub(1) =>
            {
                let visible = area.height.saturating_sub(2) as usize;
                let offset = self
                    .grok_auth
                    .account_index
                    .saturating_sub(visible.saturating_sub(1));
                self.grok_auth.account_index = (offset + (mouse.row - area.y - 1) as usize)
                    .min(self.grok_auth.accounts.len().saturating_sub(1));
            }
            _ => return false,
        }
        if let Some(page) = &mut self.grok_auth.page {
            page.selected = ACTIONS.len() + 1;
        }
        true
    }
    pub(super) fn grok_auth_page_mouse(&mut self, mouse: MouseEvent, screen: Rect) -> Result<()> {
        let rows = account_rows(workspace_content_area(screen), self.grok_auth.busy, false);
        if !self
            .grok_auth
            .page
            .as_ref()
            .is_some_and(|p| p.confirm_logout || p.confirm_account.is_some())
            && self.grok_account_list_mouse(rows[1], mouse)
        {
            return Ok(());
        }
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return self.grok_auth_page_key(KeyEvent::new(
                if mouse.kind == MouseEventKind::ScrollUp {
                    KeyCode::PageUp
                } else {
                    KeyCode::PageDown
                },
                KeyModifiers::NONE,
            ));
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Ok(());
        }
        let area = account_rows(workspace_content_area(screen), self.grok_auth.busy, false)[2];
        let confirm = self
            .grok_auth
            .page
            .as_ref()
            .is_some_and(|a| a.confirm_logout || a.confirm_account.is_some());
        if confirm {
            if let Some(i) =
                modal_button_rects(confirmation_area(workspace_content_area(screen)), 2)
                    .iter()
                    .position(|r| contains(*r, mouse.column, mouse.row))
            {
                self.grok_auth_page_key(KeyEvent::new(
                    KeyCode::Char(if i == 0 { 'y' } else { 'n' }),
                    KeyModifiers::NONE,
                ))?;
            }
        } else if let Some(i) = account_actions(screen)
            .iter()
            .position(|r| contains(*r, mouse.column, mouse.row))
        {
            if self.grok_auth.busy {
                if i == 6 {
                    self.cancel_grok_auth();
                }
                return Ok(());
            }
            if let Some(page) = self.grok_auth.page.as_mut() {
                page.selected = i + 1;
            }
            self.grok_auth_page_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        } else {
            let inner = panel_inner(area);
            if mouse.row == inner.y
                && contains(inner, mouse.column, mouse.row)
                && let Some(page) = self.grok_auth.page.as_mut()
            {
                page.selected = 0;
            }
        }
        Ok(())
    }
    pub(super) fn draw_grok_accounts(&self, frame: &mut ratatui::Frame, screen: Rect) {
        self.draw_grok_accounts_content(frame, workspace_content_area(screen), false);
        self.draw_client_tabs(frame, screen);
    }

    pub(super) fn draw_grok_accounts_embedded(&self, frame: &mut ratatui::Frame, area: Rect) {
        frame.render_widget(
            panel(" Grok OAuth account ", self.focus == Focus::Details),
            area,
        );
        self.draw_grok_accounts_content(frame, panel_inner(area), true);
    }

    fn draw_grok_accounts_content(
        &self,
        frame: &mut ratatui::Frame,
        content: Rect,
        embedded: bool,
    ) {
        let rows = account_rows(content, self.grok_auth.busy, embedded);
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                if embedded {
                    "OAuth login and usage"
                } else {
                    "Grok OAuth Accounts"
                },
                Style::default().fg(ROUTE).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(
                rows[0].x,
                rows[0].y + u16::from(!embedded),
                rows[0].width,
                1,
            ),
        );
        frame.render_widget(
            Paragraph::new(self.grok_auth.status.description()).wrap(Wrap { trim: false }),
            Rect::new(
                rows[0].x,
                rows[0].y + 1 + u16::from(!embedded),
                rows[0].width,
                rows[0].height.saturating_sub(1 + u16::from(!embedded)),
            ),
        );
        let current = crate::grok::accounts::current_id(&self.grok_home)
            .ok()
            .flatten();
        let items: Vec<ListItem> = if self.grok_auth.accounts.is_empty() {
            vec![ListItem::new(
                "No saved account · Browser / Device code to add",
            )]
        } else {
            self.grok_auth
                .accounts
                .iter()
                .map(|(id, account)| {
                    ListItem::new(format!(
                        "{} {} · {}",
                        if current.as_ref() == Some(id) {
                            "●"
                        } else {
                            "○"
                        },
                        account.name,
                        if current.as_ref() == Some(id) {
                            "Local login"
                        } else {
                            "Saved"
                        }
                    ))
                })
                .collect()
        };
        let mut state = ListState::default().with_selected(
            (!self.grok_auth.accounts.is_empty()).then_some(self.grok_auth.account_index),
        );
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(" Grok accounts · a focus · ↑↓ select ", true))
                .highlight_style(Style::default().bg(theme::PROVIDER_SELECTION))
                .highlight_symbol(self.theme.selection_symbol()),
            rows[1],
            &mut state,
        );
        let fallback = AccountPage {
            selected: 1,
            model: field(
                "Native model",
                self.config
                    .grok
                    .preferences
                    .default
                    .as_deref()
                    .unwrap_or("grok-build"),
            ),
            confirm_logout: false,
            confirm_account: None,
            scroll: 0,
        };
        let page = self.grok_auth.page.as_ref().unwrap_or(&fallback);
        frame.render_widget(
            panel(
                if self.grok_auth.busy {
                    " Login progress · Esc cancel "
                } else {
                    " Account configuration · active usage "
                },
                false,
            ),
            rows[2],
        );
        let inner = panel_inner(rows[2]);
        let show_model = inner.height > 1 || page.selected == 0;
        if show_model {
            draw_fields(
                frame,
                Rect::new(inner.x, inner.y, inner.width, 1),
                std::slice::from_ref(&page.model),
                0,
                page.selected == 0 && !self.grok_auth.busy,
            );
        }
        let mut lines = Vec::new();
        if self.grok_auth.busy {
            lines.extend(self.grok_auth.progress.iter().cloned().map(Line::raw));
            lines.push(Line::raw(self.grok_auth.message.clone()));
        } else {
            if self.grok_auth.usage_refreshing {
                lines.push(Line::styled(
                    if self.grok_auth.usage_waking {
                        "Waking account… · consumes a little quota"
                    } else {
                        "Refreshing usage… cached data remains visible"
                    },
                    Style::default().fg(ROUTE),
                ));
            }
            if let Some(error) = &self.grok_auth.usage_error {
                lines.push(Line::styled(
                    format!(
                        "Usage refresh failed: {error}{}",
                        if self.grok_auth.usage.is_some() {
                            " · showing cached data"
                        } else {
                            ""
                        }
                    ),
                    Style::default().fg(WARNING),
                ));
            }
            if let Some(snapshot) = &self.grok_auth.usage {
                let mut credits = credit_display_lines(snapshot, inner.width, chrono::Utc::now());
                if inner.height <= 2 {
                    credits.swap(0, 1);
                }
                lines.extend(credits);
                lines.push(Line::raw(""));
                lines.push(Line::raw(self.grok_auth.message.clone()));
            } else {
                lines.push(Line::raw(self.grok_auth.message.clone()));
                lines.push(Line::styled(
                    "Account usage not loaded · r refresh",
                    Style::default().fg(MUTED),
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((page.scroll, 0)),
            Rect::new(
                inner.x,
                inner.y + u16::from(show_model),
                inner.width,
                inner.height.saturating_sub(u16::from(show_model)),
            ),
        );
        for (i, rect) in account_actions(content).into_iter().enumerate() {
            if embedded && i == 6 && !self.grok_auth.busy {
                continue;
            }
            frame.render_widget(
                Paragraph::new(if self.grok_auth.busy && i == 6 {
                    "Cancel/Esc"
                } else {
                    ACTIONS[i]
                })
                .alignment(Alignment::Center)
                .style(button_style(
                    page.selected == i + 1,
                    (self.grok_auth.busy && i != 6)
                        || (matches!(i, 3 | 4)
                            && (self.grok_auth.usage_refreshing || !self.grok_auth.status.saved)),
                    i == 5 || i == 9,
                )),
                rect,
            );
        }
        if let Some((id, delete)) = &page.confirm_account {
            let area = confirmation_area(content);
            frame.render_widget(Clear, area);
            frame.render_widget(
                panel(
                    if *delete {
                        " Delete saved Grok account? "
                    } else {
                        " Switch Grok account? "
                    },
                    true,
                ),
                area,
            );
            let name = self
                .grok_auth
                .accounts
                .get(id)
                .map_or("selected account", |account| account.name.as_str());
            let inner = panel_inner(area);
            frame.render_widget(Paragraph::new(format!("Account: {name}\n{}", if *delete {"Deletes its saved credentials. Switch or sign out before deleting the active login."} else {"Makes this login active in Grok. Restart Grok for new sessions."})).wrap(Wrap{trim:false}),Rect::new(inner.x,inner.y,inner.width,inner.height.saturating_sub(1)));
            draw_modal_buttons(frame, area, &["Confirm", "Cancel"]);
        }
        if page.confirm_logout {
            let area = confirmation_area(content);
            frame.render_widget(Clear, area);
            frame.render_widget(panel(" Sign out of Grok? ", true), area);
            let inner = panel_inner(area);
            frame.render_widget(Paragraph::new("Grok will clear its cached login credentials.\nAPI provider configurations are retained.\n\nEnter/y confirms · n/Esc cancels").wrap(Wrap { trim: false }), Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(1)));
            draw_modal_buttons(frame, area, &["Sign out", "Cancel"]);
        }
    }
}

/// Render the billing fields directly so the gauge does not depend on summary wording.
fn credit_display_lines(
    snapshot: &usage::Snapshot,
    width: u16,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Line<'static>> {
    let credits = &snapshot.credits;
    let mut lines = Vec::new();
    let muted = Style::default().fg(MUTED);
    let dollars = |cents: i64| format!("${:.2}", cents as f64 / 100.0);
    lines.push(Line::styled(
        format!(
            "{} credits",
            credits.period.as_deref().unwrap_or("Included")
        ),
        Style::default().fg(ROUTE).add_modifier(Modifier::BOLD),
    ));
    if let Some(percent) = credits
        .percent
        .filter(|percent| percent.is_finite() && *percent >= 0.0)
    {
        let color = if percent >= 90.0 {
            ERROR
        } else if percent >= 70.0 {
            WARNING
        } else {
            ROUTE
        };
        let label = format!(" {percent:.1}% used");
        let cells = usize::from(width)
            .saturating_sub(UnicodeWidthStr::width(label.as_str()))
            .min(36);
        let filled = (percent.clamp(0.0, 100.0) / 100.0 * cells as f64).round() as usize;
        lines.push(Line::from(vec![
            Span::styled("━".repeat(filled), Style::default().fg(color)),
            Span::styled("─".repeat(cells - filled), muted),
            Span::styled(label, Style::default().fg(color)),
        ]));
        lines.push(Line::styled(
            format!("Remaining allowance: {:.1}%", (100.0 - percent).max(0.0)),
            Style::default().fg(if percent >= 90.0 { WARNING } else { ENABLED }),
        ));
    } else {
        let label = " — used";
        let cells = usize::from(width)
            .saturating_sub(UnicodeWidthStr::width(label))
            .min(36);
        lines.push(Line::styled(format!("{}{label}", "▒".repeat(cells)), muted));
        lines.push(Line::styled("xAI did not publish usage · r refresh", muted));
    }
    if let Some(reset) = credits
        .reset_at
        .as_deref()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
    {
        lines.push(Line::styled(
            format!(
                "Resets {}",
                reset.with_timezone(&chrono::Local).format("%m/%d %H:%M %Z")
            ),
            muted,
        ));
        let minutes = (reset.with_timezone(&chrono::Utc) - now).num_minutes();
        let countdown = if reset <= now {
            "Reset time reached · r refresh".into()
        } else if minutes < 1 {
            "Reset in less than 1m".into()
        } else if minutes >= 1440 {
            format!("Reset in {}d {}h", minutes / 1440, minutes % 1440 / 60)
        } else {
            format!("Reset in {}h {}m", minutes / 60, minutes % 60)
        };
        lines.push(Line::styled(countdown, muted));
    } else {
        lines.push(Line::styled("Reset time unavailable", muted));
    }
    if let Some(error) = &credits.percent_error {
        lines.push(Line::styled(format!("Usage source: {error}"), muted));
    }
    if let Some(plan) = &credits.plan {
        lines.push(Line::raw(format!("Plan: {plan}")));
    }
    if credits.unified == Some(true) {
        lines.push(Line::styled("Shared account credit allowance", muted));
    }
    if let Some(used) = credits.used_cents {
        lines.push(Line::raw(format!(
            "Included used: {}{}",
            dollars(used),
            credits
                .limit_cents
                .map(|limit| format!(" / {}", dollars(limit)))
                .unwrap_or_default()
        )));
    } else if let Some(limit) = credits.limit_cents {
        lines.push(Line::raw(format!("Included limit: {}", dollars(limit))));
    }
    if let Some(balance) = credits.prepaid_cents {
        lines.push(Line::raw(format!("Prepaid balance: {}", dollars(balance))));
    }
    if let Some(used) = credits.on_demand_used_cents {
        lines.push(Line::raw(format!(
            "On-demand used: {}{}",
            dollars(used),
            credits
                .on_demand_cap_cents
                .map(|cap| format!(" / {}", dollars(cap)))
                .unwrap_or_default()
        )));
    } else if let Some(cap) = credits.on_demand_cap_cents {
        lines.push(Line::raw(format!("On-demand cap: {}", dollars(cap))));
    }
    if let Some(fetched) = chrono::DateTime::from_timestamp(snapshot.fetched_at, 0) {
        lines.push(Line::styled(
            format!(
                "Last refresh: {}",
                fetched
                    .with_timezone(&chrono::Local)
                    .format("%m/%d %H:%M:%S")
            ),
            muted,
        ));
    }
    lines
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn credit_gauges_handle_exceeded_and_missing_allowances() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-05T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut snapshot = usage::Snapshot {
            account: "fixture".into(),
            fetched_at: now.timestamp(),
            credits: usage::Credits {
                percent: Some(125.0),
                period: Some("Weekly".into()),
                reset_at: Some("2026-10-07T03:00:00Z".into()),
                prepaid_cents: Some(1234),
                ..Default::default()
            },
        };
        let text = |lines: &[Line<'_>]| {
            lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        for width in [16, 30, 80] {
            let lines = credit_display_lines(&snapshot, width, now);
            assert!(lines[1].width() <= width as usize);
            assert!(!lines[1].to_string().contains('─'));
            let shown = text(&lines);
            assert!(shown.contains("125.0% used") && shown.contains("Remaining allowance: 0.0%"));
            assert!(shown.contains("Reset in 2d 3h") && shown.contains("Prepaid balance: $12.34"));
        }
        snapshot.credits.percent = None;
        snapshot.credits.reset_at = None;
        let lines = credit_display_lines(&snapshot, 40, now);
        let shown = text(&lines);
        assert!(
            shown.contains("xAI did not publish usage") && shown.contains("Reset time unavailable")
        );
        assert!(!shown.contains("0.0%") && !shown.contains('━'));
        snapshot.credits.percent = Some(0.0);
        snapshot.credits.reset_at = Some("2026-10-04T00:00:00Z".into());
        let shown = text(&credit_display_lines(&snapshot, 40, now));
        assert!(
            shown.contains("Remaining allowance: 100.0%")
                && shown.contains("Reset time reached · r refresh")
        );
    }
    #[test]
    fn saved_accounts_switch_and_delete_require_confirmation() {
        let (temp, mut app) = super::super::tests::persisted_app();
        app.grok_home = temp.path().join("grok");
        std::fs::create_dir_all(&app.grok_home).unwrap();
        let write = |user: &str| {
            std::fs::write(app.grok_home.join("auth.json"), serde_json::json!({"auth_mode":"oidc","user_id":user,"email":format!("{user}@example.com"),"key":format!("SECRET_{user}")}).to_string()).unwrap();
        };
        write("one");
        let first = crate::grok::accounts::capture(&app.paths, &app.grok_home)
            .unwrap()
            .unwrap();
        write("two");
        let second = crate::grok::accounts::capture(&app.paths, &app.grok_home)
            .unwrap()
            .unwrap();
        app.select_client_tab(ClientTab::Grok);
        app.open_grok_auth();
        app.grok_auth.usage_refreshing = false;
        app.grok_auth.account_index = app
            .grok_auth
            .accounts
            .keys()
            .position(|id| id == &first)
            .unwrap();
        let press = |app: &mut App, code| {
            app.grok_auth_page_key(KeyEvent::new(code, KeyModifiers::NONE))
                .unwrap()
        };
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(
            crate::grok::accounts::current_id(&app.grok_home).unwrap(),
            Some(second.clone())
        );
        assert!(
            app.grok_auth
                .page
                .as_ref()
                .unwrap()
                .confirm_account
                .is_some()
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(
            crate::grok::accounts::current_id(&app.grok_home).unwrap(),
            Some(second.clone())
        );
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            crate::grok::accounts::current_id(&app.grok_home).unwrap(),
            Some(first)
        );
        app.grok_auth.account_index = app
            .grok_auth
            .accounts
            .keys()
            .position(|id| id == &second)
            .unwrap();
        for (width, height) in [(40, 12), (80, 24), (160, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(!text.contains("SECRET_"));
            if width >= 80 {
                assert!(text.contains("one@example.com") && text.contains("two@example.com"));
            }
        }
        press(&mut app, KeyCode::Char('X'));
        assert_eq!(app.grok_auth.accounts.len(), 2);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.grok_auth.accounts.len(), 2);
        press(&mut app, KeyCode::Char('X'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.grok_auth.accounts.len(), 1);
    }
    #[test]
    fn usage_refresh_keeps_account_list_cache_and_ignores_stale_account_results() {
        let (temp, mut app) = super::super::tests::persisted_app();
        app.grok_home = temp.path().join("grok");
        std::fs::create_dir_all(&app.grok_home).unwrap();
        let auth = app.grok_home.join("auth.json");
        std::fs::write(
            &auth,
            r#"{"auth_mode":"oidc","key":"SECRET","user_id":"one","email":"account@example.com"}"#,
        )
        .unwrap();
        app.select_client_tab(ClientTab::Grok);
        app.open_grok_auth();
        assert!(app.grok_auth.usage_refreshing);
        let snapshot = usage::Snapshot {
            account: usage::account(&app.grok_home).unwrap().unwrap(),
            credits: usage::Credits {
                percent: Some(75.0),
                period: Some("Weekly".into()),
                prepaid_cents: Some(1234),
                ..Default::default()
            },
            fetched_at: 1,
        };
        app.grok_auth
            .sender
            .send(Update::Usage(
                app.grok_auth.usage_request,
                Ok(snapshot.clone()),
            ))
            .unwrap();
        assert!(app.poll_grok_auth());
        assert!(!app.grok_auth.usage_refreshing);
        app.grok_auth.page.as_mut().unwrap().selected = 5;
        app.grok_auth_page_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.grok_auth.usage_waking && app.grok_auth.usage_refreshing);
        assert!(app.grok_auth.usage.is_some());
        let wake_request = app.grok_auth.usage_request;
        app.grok_auth_page_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.grok_auth.usage_request, wake_request);
        let mut waking_terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        waking_terminal.draw(|frame| app.draw(frame)).unwrap();
        let waking_text: String = waking_terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(waking_text.contains("Waking account") && waking_text.contains("75.0% used"));
        assert!(waking_text.contains("Wake (w)"));
        app.grok_auth
            .sender
            .send(Update::Usage(wake_request, Ok(snapshot.clone())))
            .unwrap();
        app.poll_grok_auth();
        assert!(!app.grok_auth.usage_waking && !app.grok_auth.usage_refreshing);
        assert!(app.grok_auth.message.contains("Wake complete"));
        app.refresh_grok_usage();
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("account@example.com") && text.contains("Grok accounts"));
        assert!(text.contains("Refreshing usage") && text.contains("75.0% used"));
        assert!(
            text.contains("━") && text.contains("─") && text.contains("Remaining allowance: 25.0%")
        );
        assert!(text.contains("━") && text.contains("25.0%") && text.contains("$12.34"));
        assert!(!text.contains("SECRET"));
        let mut small = Terminal::new(TestBackend::new(40, 12)).unwrap();
        app.grok_auth.usage_refreshing = false;
        small.draw(|frame| app.draw(frame)).unwrap();
        let small_text: String = small
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(small_text.contains("75.0% used"));
        app.grok_auth.usage_refreshing = true;

        app.grok_auth
            .sender
            .send(Update::Usage(
                app.grok_auth.usage_request,
                Err("Service unavailable".into()),
            ))
            .unwrap();
        app.poll_grok_auth();
        assert!(app.grok_auth.usage.is_some());
        assert_eq!(
            app.grok_auth.usage_error.as_deref(),
            Some("Service unavailable")
        );
        app.refresh_grok_usage();
        let stale_request = app.grok_auth.usage_request;
        std::fs::write(&auth, r#"{"auth_mode":"oidc","key":"NEW","user_id":"two"}"#).unwrap();
        app.grok_auth
            .sender
            .send(Update::Usage(stale_request, Ok(snapshot)))
            .unwrap();
        app.poll_grok_auth();
        assert!(app.grok_auth.usage.is_none());
        assert!(
            app.grok_auth
                .usage_error
                .as_deref()
                .unwrap()
                .contains("Account changed")
        );
        app.grok_auth.usage_request += 1;
        app.grok_auth
            .sender
            .send(Update::Usage(stale_request, Err("stale error".into())))
            .unwrap();
        app.poll_grok_auth();
        assert!(
            !app.grok_auth
                .usage_error
                .as_deref()
                .unwrap()
                .contains("stale error")
        );
    }
}
