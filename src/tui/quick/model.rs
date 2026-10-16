use super::*;

#[derive(Default)]
pub(super) struct Monitor {
    pub(super) takeover: session_takeover::Ui,
    pub(super) session_browser: sessions::Browser,
    pub(super) picker: Option<picker::Picker>,
    pub(super) accounts: accounts::Accounts,
    pub(super) pulse_theme: theme::PulseTheme,
    pub(super) snapshot: Snapshot,
    pub(super) sessions: crate::sessions::Snapshot,
    pub(super) sessions_refreshed: Option<Instant>,
    pub(super) page: config::PulseStartPage,
    pub(super) token_page: config::PulseStartPage,
    pub(super) git: git::GitPane,
    pub(super) files: files::FilePane,
    pub(super) visual_mode: bool,
    pub(super) preferences: config::UiPreferences,
    pub(super) sessions_sort_tokens: bool,
    pub(super) source_pane: Option<String>,
    pub(super) focused_pane: Option<String>,
    pub(super) focused_client: Option<usize>,
    pub(super) active_session: Option<AgentSession>,
    pub(super) client: usize,
    pub(super) scroll: u16,
    pub(super) limit: u16,
    pub(super) models: bool,
    pub(super) help: bool,
    pub(super) help_language: HelpLanguage,
    pub(super) help_return_scroll: u16,
    pub(super) refreshed: Option<Instant>,
    pub(super) error: Option<String>,
    pub(super) notice: Option<String>,
}
impl Monitor {
    pub(super) fn help_input(&mut self, input: &Event, screen: Rect) -> bool {
        let (area, body) = help_area(screen);
        let code = match input {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return false;
                }
                Some(key.code)
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => Some(KeyCode::Down),
                MouseEventKind::ScrollUp => Some(KeyCode::Up),
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(target) = scrollbar_target(
                        body,
                        area.right().saturating_sub(1),
                        mouse.column,
                        mouse.row,
                        self.limit,
                    ) {
                        self.scroll = target;
                        return true;
                    }
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                        if contains(
                            page_header_rects(area, 0, false)[1],
                            mouse.column,
                            mouse.row,
                        ) {
                            self.select_workspace(true);
                        }
                        help_footer(area, self.help_language)
                            .iter()
                            .find(|button| contains(button.rect, mouse.column, mouse.row))
                            .map(|button| button.key)
                    } else {
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        };
        match code {
            Some(KeyCode::Char('H')) => self.return_home(),
            Some(KeyCode::Char('l')) => {
                self.help_language.toggle();
                self.scroll = 0;
            }
            Some(KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?' | 'q')) => {
                self.help = false;
                self.scroll = self.help_return_scroll;
            }
            Some(KeyCode::Up | KeyCode::Char('k')) => self.scroll = self.scroll.saturating_sub(1),
            Some(KeyCode::Down | KeyCode::Char('j')) => {
                self.scroll = self.scroll.saturating_add(1).min(self.limit)
            }
            Some(KeyCode::PageUp) => self.scroll = self.scroll.saturating_sub(body.height.max(1)),
            Some(KeyCode::PageDown) => {
                self.scroll = self
                    .scroll
                    .saturating_add(body.height.max(1))
                    .min(self.limit)
            }
            Some(KeyCode::Home) => self.scroll = 0,
            Some(KeyCode::End) => self.scroll = self.limit,
            _ => {}
        }
        true
    }

    pub(super) fn return_home(&mut self) {
        match self.page {
            config::PulseStartPage::File => {
                self.files.return_home();
                return;
            }
            config::PulseStartPage::Git => {
                self.git.return_home();
                return;
            }
            _ => {}
        }
        self.page = config::PulseStartPage::Home;
        self.token_page = self.page;
        self.help = false;
        self.reset_session_browser();
        self.scroll = 0;
        if let Some(client) = self.focused_client {
            self.client = client;
        }
    }
    pub(super) fn footer_key(&self, input: &Event, screen: Rect) -> Option<Option<KeyEvent>> {
        if matches!(
            self.page,
            config::PulseStartPage::Git | config::PulseStartPage::File
        ) || self.help
        {
            return None;
        }
        let Event::Mouse(mouse) = input else {
            return None;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        let area = screen;
        self.token_footer(area)
            .iter()
            .find(|button| contains(button.rect, mouse.column, mouse.row))
            .map(|button| {
                button
                    .enabled
                    .then(|| KeyEvent::new(button.key, button.modifiers))
            })
    }

    pub(super) fn apply_preferences(&mut self, preferences: config::UiPreferences) {
        self.visual_mode = preferences.pulse_visual;
        self.models = preferences.pulse_models;
        self.sessions_sort_tokens = preferences.pulse_sort_tokens;
        self.preferences = preferences;
        self.scroll = 0;
    }

    pub(super) fn apply_start_page(&mut self) {
        self.page = self.preferences.pulse_start_page;
        if !matches!(
            self.page,
            config::PulseStartPage::Git | config::PulseStartPage::File
        ) {
            self.token_page = self.page;
        }
    }

    pub(super) fn select_workspace(&mut self, git: bool) {
        if !git
            && !matches!(
                self.page,
                config::PulseStartPage::Git | config::PulseStartPage::File
            )
        {
            return;
        }
        self.select_page(if git {
            config::PulseStartPage::Git
        } else {
            self.token_page
        });
    }

    pub(super) fn can_switch_workspace(&self) -> bool {
        self.picker.is_none()
            && (self.page != config::PulseStartPage::Git || self.git.can_switch_workspace())
            && (self.page != config::PulseStartPage::File || self.files.can_switch_workspace())
    }

    fn select_page(&mut self, page: config::PulseStartPage) {
        if self.page == page {
            return;
        }
        if !matches!(
            self.page,
            config::PulseStartPage::Git | config::PulseStartPage::File
        ) {
            self.token_page = self.page;
        }
        self.page = page;
    }

    pub(super) fn workspace_shortcut(&mut self, key: KeyEvent) -> bool {
        if key.kind != event::KeyEventKind::Press || !self.can_switch_workspace() {
            return false;
        }
        let page = match (key.code, key.modifiers) {
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                if self.page == config::PulseStartPage::Git {
                    self.token_page
                } else {
                    config::PulseStartPage::Git
                }
            }
            (KeyCode::Char('f'), KeyModifiers::NONE) => {
                if self.page == config::PulseStartPage::File {
                    self.token_page
                } else {
                    config::PulseStartPage::File
                }
            }
            (KeyCode::Char('1'), KeyModifiers::ALT) => {
                if matches!(
                    self.page,
                    config::PulseStartPage::Git | config::PulseStartPage::File
                ) {
                    self.token_page
                } else {
                    self.page
                }
            }
            (KeyCode::Char('2'), KeyModifiers::ALT) => config::PulseStartPage::Git,
            (KeyCode::Char('3'), KeyModifiers::ALT) => config::PulseStartPage::File,
            _ => return false,
        };
        self.select_page(page);
        true
    }

    pub(super) fn workspace_mouse(&mut self, input: &Event, screen: Rect) -> bool {
        let Event::Mouse(mouse) = input else {
            return false;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        let area = if self.help
            && !matches!(
                self.page,
                config::PulseStartPage::Git | config::PulseStartPage::File
            ) {
            help_area(screen).0
        } else {
            pulse_header_area(screen).0
        };
        if self.page == config::PulseStartPage::File {
            let [_, _, _, _, help] = file_page_header_rects(area);
            if self.files.help_available() && contains(help, mouse.column, mouse.row) {
                self.files.input(&Event::Key(KeyEvent::new(
                    KeyCode::Char('?'),
                    KeyModifiers::NONE,
                )));
                return true;
            }
        }
        if !self.can_switch_workspace() {
            return false;
        }
        for (index, rect) in workspace_header_rects(area).into_iter().take(3).enumerate() {
            if contains(rect, mouse.column, mouse.row) {
                let target = match index {
                    0 if matches!(
                        self.page,
                        config::PulseStartPage::Git | config::PulseStartPage::File
                    ) =>
                    {
                        self.token_page
                    }
                    0 => self.page,
                    1 => config::PulseStartPage::Git,
                    _ => config::PulseStartPage::File,
                };
                self.select_page(target);
                return true;
            }
        }
        false
    }

    pub(super) fn refresh_preferences(&mut self, preferences: config::UiPreferences) -> bool {
        let changed = self.preferences.pulse_visual != preferences.pulse_visual
            || self.preferences.pulse_models != preferences.pulse_models
            || self.preferences.pulse_sort_tokens != preferences.pulse_sort_tokens;
        if changed {
            self.apply_preferences(preferences);
        } else {
            self.preferences = preferences;
        }
        changed
    }
}
#[derive(Default)]
pub(super) struct Metrics {
    pub(super) total: Totals,
    pub(super) compact: i64,
    pub(super) hours: [i64; 24],
    pub(super) providers: BTreeMap<String, (String, Totals)>,
    pub(super) models: BTreeMap<String, Totals>,
}
pub(super) fn metrics(snapshot: &Snapshot, client: Option<&str>) -> Metrics {
    let today = snapshot.today();
    let mut m = Metrics::default();
    for row in &snapshot.rows {
        if row.day != today || client.is_some_and(|c| row.client != c) {
            continue;
        }
        m.total.add(&row.totals);
        if row.kind == "compact" {
            m.compact += row.totals.calls;
        }
        if let Some(hour) = m.hours.get_mut(row.hour as usize) {
            *hour += row.totals.calls;
        }
        let entry = m
            .providers
            .entry(format!("{}:{}", row.client, row.provider))
            .or_insert_with(|| (row.name.clone(), Totals::default()));
        entry.1.add(&row.totals);
        m.models
            .entry(row.model.clone())
            .or_default()
            .add(&row.totals);
    }
    m
}
pub(super) fn rate(t: &Totals) -> Option<f64> {
    let completed = t.success + t.failed + t.interrupted;
    (completed > 0).then(|| 100.0 * t.success as f64 / completed as f64)
}
