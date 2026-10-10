use super::*;

#[derive(Default)]
pub(super) struct Monitor {
    pub(super) picker: Option<picker::Picker>,
    pub(super) accounts: accounts::Accounts,
    pub(super) pulse_theme: theme::PulseTheme,
    pub(super) snapshot: Snapshot,
    pub(super) sessions: crate::sessions::Snapshot,
    pub(super) sessions_refreshed: Option<Instant>,
    pub(super) page: config::PulseStartPage,
    pub(super) token_page: config::PulseStartPage,
    pub(super) git: git::GitPane,
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
    pub(super) refreshed: Option<Instant>,
    pub(super) error: Option<String>,
    pub(super) notice: Option<String>,
}
impl Monitor {
    pub(super) fn apply_preferences(&mut self, preferences: config::UiPreferences) {
        self.visual_mode = preferences.pulse_visual;
        self.models = preferences.pulse_models;
        self.sessions_sort_tokens = preferences.pulse_sort_tokens;
        self.preferences = preferences;
        self.scroll = 0;
    }

    pub(super) fn apply_start_page(&mut self) {
        self.page = self.preferences.pulse_start_page;
        if self.page != config::PulseStartPage::Git {
            self.token_page = self.page;
        }
    }

    pub(super) fn select_workspace(&mut self, git: bool) {
        if git == (self.page == config::PulseStartPage::Git) {
            return;
        }
        if git {
            self.token_page = self.page;
            self.page = config::PulseStartPage::Git;
        } else {
            self.page = self.token_page;
        }
    }

    pub(super) fn workspace_shortcut(&mut self, key: KeyEvent) -> bool {
        if key.kind != event::KeyEventKind::Press
            || key.modifiers != KeyModifiers::ALT
            || !matches!(key.code, KeyCode::Char('1' | '2'))
            || self.picker.is_some()
            || (self.page == config::PulseStartPage::Git && !self.git.can_switch_workspace())
        {
            return false;
        }
        self.select_workspace(key.code == KeyCode::Char('2'));
        true
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
