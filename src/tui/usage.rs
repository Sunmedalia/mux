use super::*;
mod chart;
mod view;
pub(super) fn page_area(screen: Rect) -> Rect {
    workspace_content_area(screen)
}
use crate::usage::{Query, Reader, Snapshot, Totals};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

struct UsageRead {
    reader: Reader,
    query: Query,
    result: Result<Option<Snapshot>, String>,
}

#[derive(Default)]
pub(super) struct UsageUi {
    pub active: bool,
    pub page: Option<UsagePage>,
    pub snapshot: Snapshot,
    pub updated: Option<Instant>,
    receiver: Option<mpsc::Receiver<UsageRead>>,
    reader: Option<Reader>,
    query: Option<Query>,
    error: Option<String>,
    sessions: crate::sessions::Snapshot,
    sessions_reader: Option<crate::sessions::Reader>,
    sessions_receiver:
        Option<mpsc::Receiver<(crate::sessions::Reader, crate::sessions::Snapshot, bool)>>,
    sessions_updated: Option<Instant>,
    titles_updated: Option<Instant>,
    sessions_watcher: Option<crate::sessions::ChangeWatch>,
    sessions_events: Option<mpsc::Receiver<()>>,
    sessions_dirty: bool,
    sessions_watch_attempt: Option<Instant>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum UsageSource {
    #[default]
    Gateway,
    Sessions,
}

#[derive(Clone)]
struct ViewPosition {
    day: String,
    follow_today: bool,
    range: usize,
    client: usize,
    provider: Option<String>,
    scroll: usize,
    section: usize,
    dashboard: view::DashboardState,
}

#[derive(Clone)]
struct SearchDraft {
    query: String,
    scroll: usize,
    dashboard: view::DashboardState,
}

#[derive(Clone)]
pub(super) struct UsagePage {
    source: UsageSource,
    inactive: Option<ViewPosition>,
    search_draft: Option<SearchDraft>,
    session_detail: bool,
    detail_return_scroll: usize,
    day: String,
    follow_today: bool,
    range: usize,
    chart_tokens: bool,
    client: usize,
    provider: Option<String>,
    scroll: usize,
    section: usize,
    session_sort_tokens: bool,
    session_follow_range: bool,
    session_search: String,
    pub(super) session_searching: bool,
    limit: std::cell::Cell<usize>,
    dashboard: std::cell::RefCell<view::DashboardState>,
}
impl UsagePage {
    fn switch_source(&mut self) {
        if self.session_searching {
            self.finish_search(true);
        }
        let saved = ViewPosition {
            day: self.day.clone(),
            follow_today: self.follow_today,
            range: self.range,
            client: self.client,
            provider: self.provider.clone(),
            scroll: self.dashboard.borrow().effective_scroll,
            section: self.section,
            dashboard: self.dashboard.borrow().clone(),
        };
        let next = self.inactive.take().unwrap_or_else(|| ViewPosition {
            day: self.day.clone(),
            follow_today: self.follow_today,
            range: self.range,
            client: 0,
            provider: None,
            scroll: 0,
            section: 5,
            dashboard: Default::default(),
        });
        self.source = if self.source == UsageSource::Gateway {
            UsageSource::Sessions
        } else {
            UsageSource::Gateway
        };
        self.day = next.day;
        self.follow_today = next.follow_today;
        self.range = next.range;
        self.client = next.client;
        self.provider = next.provider;
        self.scroll = next.scroll;
        self.section = next.section;
        *self.dashboard.borrow_mut() = next.dashboard;
        self.inactive = Some(saved);
    }
    fn begin_search(&mut self) {
        if self.source == UsageSource::Gateway {
            self.switch_source();
        }
        if self.session_detail {
            self.session_detail = false;
            self.scroll = self.detail_return_scroll;
        }
        if !self.session_searching {
            self.search_draft = Some(SearchDraft {
                query: self.session_search.clone(),
                scroll: self.scroll,
                dashboard: self.dashboard.borrow().clone(),
            });
        }
        self.session_searching = true;
        self.section = 5;
        self.jump_to_section();
    }
    fn finish_search(&mut self, cancel: bool) {
        if cancel && let Some(draft) = self.search_draft.take() {
            self.session_search = draft.query;
            self.scroll = draft.scroll;
            *self.dashboard.borrow_mut() = draft.dashboard;
        }
        self.search_draft = None;
        self.session_searching = false;
    }
    fn range_label(&self) -> &'static str {
        ["1 day", "1 week", "1 month", "All time"][self.range]
    }
    fn start_day(&self) -> Option<String> {
        let days = match self.range {
            0 => 0,
            1 => 6,
            2 => 29,
            _ => return None,
        };
        chrono::NaiveDate::parse_from_str(&self.day, "%Y-%m-%d")
            .ok()?
            .checked_sub_signed(chrono::Duration::days(days))
            .map(|d| d.to_string())
    }
    fn includes(&self, day: &str) -> bool {
        self.range == 3
            || (day <= self.day.as_str()
                && self.start_day().is_some_and(|start| day >= start.as_str()))
    }
    fn range_total(
        &self,
        snapshot: &Snapshot,
        client: Option<&str>,
        provider: Option<&str>,
        kind: &str,
    ) -> Totals {
        let mut total = Totals::default();
        for row in &snapshot.rows {
            if self.includes(&row.day)
                && client.is_none_or(|c| c == row.client)
                && provider.is_none_or(|p| p == row.provider)
                && row.kind == kind
            {
                total.add(&row.totals);
            }
        }
        total
    }
    fn client(&self) -> Option<&'static str> {
        [
            None,
            Some("Claude"),
            Some("Codex"),
            Some("Pi"),
            Some("Grok"),
        ][self.client.min(4)]
    }
    pub fn key(&mut self, key: KeyEvent, snapshot: &Snapshot) -> bool {
        self.scroll = self.dashboard.borrow().effective_scroll;
        self.scroll = self.scroll.min(self.limit.get());
        if self.session_searching {
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    self.section = 5;
                    self.select_table_row(key.code == KeyCode::Down);
                    return false;
                }
                KeyCode::Esc => {
                    self.finish_search(true);
                    return false;
                }
                KeyCode::Enter => {
                    self.finish_search(false);
                    return false;
                }
                KeyCode::Backspace => {
                    self.session_search.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.session_search.clear()
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.session_search.push(ch)
                }
                _ => {}
            }
            self.section = 5;
            self.jump_to_section();
            return false;
        }
        if self.source == UsageSource::Sessions
            && self.session_detail
            && !matches!(
                key.code,
                KeyCode::Char('g' | 'q' | 'r' | 'j' | 'k')
                    | KeyCode::Esc
                    | KeyCode::F(6)
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
            )
        {
            return false;
        }
        match key.code {
            KeyCode::Char('g') => {
                self.switch_source();
                return false;
            }
            KeyCode::Esc | KeyCode::Char('q')
                if self.source == UsageSource::Sessions && self.session_detail =>
            {
                self.session_detail = false;
                self.scroll = self.detail_return_scroll;
                self.dashboard.borrow_mut().effective_scroll = self.scroll;
                return false;
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(6) => return true,
            KeyCode::Left | KeyCode::Right => {
                if self.range == 3 {
                    return false;
                }
                if let Ok(day) = chrono::NaiveDate::parse_from_str(&self.day, "%Y-%m-%d") {
                    let next = if key.code == KeyCode::Left {
                        day.pred_opt()
                    } else {
                        day.succ_opt()
                    };
                    if let Some(next) = next.filter(|d| d.to_string() <= snapshot.today()) {
                        self.day = next.to_string();
                        self.follow_today = self.day == snapshot.today();
                    }
                }
                self.scroll = 0;
            }
            KeyCode::Char('t') => {
                self.day = snapshot.today();
                self.follow_today = true;
                self.scroll = 0;
            }
            KeyCode::Char('[' | ']') => {
                self.client =
                    (self.client + if key.code == KeyCode::Char(']') { 1 } else { 4 }) % 5;
                self.provider = None;
                self.scroll = 0;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if self.source == UsageSource::Sessions {
                    return false;
                }
                self.section = (self.section + if key.code == KeyCode::Tab { 1 } else { 4 }) % 5;
                self.jump_to_section();
            }
            KeyCode::Char(c @ '1'..='6') => {
                if (c == '6') != (self.source == UsageSource::Sessions) {
                    self.switch_source();
                }
                self.section = (c as u8 - b'1') as usize;
                self.jump_to_section();
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('f') if self.source == UsageSource::Sessions && !self.session_detail => {
                self.session_follow_range = !self.session_follow_range;
                self.section = 5;
                self.jump_to_section();
            }
            KeyCode::Char('n' | 'p')
                if !self.session_detail || self.source == UsageSource::Gateway =>
            {
                self.select_table_row(key.code == KeyCode::Char('n'))
            }
            KeyCode::Char('x') => {
                self.client = 0;
                self.provider = None;
                self.day = snapshot.today();
                self.follow_today = true;
                self.range = 0;
                if self.source == UsageSource::Sessions {
                    self.session_search.clear();
                    self.session_follow_range = false;
                    self.session_sort_tokens = false;
                }
                self.scroll = 0;
            }
            KeyCode::Char('s') if self.source == UsageSource::Sessions && !self.session_detail => {
                self.session_sort_tokens = !self.session_sort_tokens;
                self.section = 5;
            }
            KeyCode::Char('d' | 'w' | 'm' | 'y') => {
                self.range = match key.code {
                    KeyCode::Char('d') => 0,
                    KeyCode::Char('w') => 1,
                    KeyCode::Char('m') => 2,
                    _ => 3,
                };
                self.scroll = 0;
            }
            KeyCode::Char('c') if self.source == UsageSource::Sessions => {
                self.jump_to_session_charts();
            }
            KeyCode::Char('c' | 'v') if self.source == UsageSource::Gateway => {
                self.chart_tokens = key.code == KeyCode::Char('v');
                self.section = 4;
                self.jump_to_section();
            }
            KeyCode::Char('a') if self.source == UsageSource::Gateway => {
                self.provider = None;
                self.scroll = 0;
            }
            KeyCode::Up | KeyCode::Down
                if key.modifiers.contains(KeyModifiers::ALT)
                    && !(self.source == UsageSource::Sessions && self.session_detail) =>
            {
                self.select_table_row(key.code == KeyCode::Down);
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k')
                if self.source == UsageSource::Sessions && !self.session_detail =>
            {
                self.select_table_row(matches!(key.code, KeyCode::Down | KeyCode::Char('j')));
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1).min(self.limit.get())
            }
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                if self.source == UsageSource::Sessions && !self.session_detail =>
            {
                let count = if matches!(key.code, KeyCode::Home | KeyCode::End) {
                    self.dashboard.borrow().rows_len(5)
                } else {
                    usize::from(self.dashboard.borrow().viewport.height.max(2)) / 2
                };
                let count = count.min(isize::MAX as usize) as isize;
                self.select_table_delta(if matches!(key.code, KeyCode::PageDown | KeyCode::End) {
                    count
                } else {
                    -count
                });
            }
            KeyCode::PageUp => {
                self.scroll = self
                    .scroll
                    .saturating_sub(usize::from(self.dashboard.borrow().viewport.height.max(1)))
            }
            KeyCode::PageDown => {
                self.scroll = self
                    .scroll
                    .saturating_add(usize::from(self.dashboard.borrow().viewport.height.max(1)))
                    .min(self.limit.get())
            }
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.limit.get(),
            _ => {}
        }
        false
    }
}

impl App {
    fn usage_query(&self) -> Query {
        if self.usage.active
            && let Some(page) = &self.usage.page
        {
            let gateway = if page.source == UsageSource::Sessions {
                page.inactive.as_ref()
            } else {
                None
            };
            let (start, end) = gateway.map_or_else(
                || (page.start_day(), page.day.clone()),
                |saved| {
                    let days = match saved.range {
                        0 => 0,
                        1 => 6,
                        2 => 29,
                        _ => return (None, saved.day.clone()),
                    };
                    let start = chrono::NaiveDate::parse_from_str(&saved.day, "%Y-%m-%d")
                        .ok()
                        .and_then(|d| d.checked_sub_signed(chrono::Duration::days(days)))
                        .map(|d| d.to_string());
                    (start, saved.day.clone())
                },
            );
            match start {
                Some(start) => Query::Range { start, end },
                None => Query::All,
            }
        } else {
            Query::Summary
        }
    }

    fn accept_usage(&mut self, query: Query, result: Result<Option<Snapshot>, String>) -> bool {
        // A date/range switch can happen while the old reader is still working.
        if self.usage.query.as_ref() != Some(&query) {
            return false;
        }
        // A fast A → B → A switch may return "unchanged" for a snapshot
        // cleared during B. Ask a fresh reader rather than displaying zeros.
        if matches!(result, Ok(None)) && self.usage.updated.is_none() {
            self.usage.reader = None;
            return false;
        }
        match result {
            Ok(snapshot) => {
                if let Some(snapshot) = snapshot {
                    self.usage.snapshot = snapshot;
                }
                self.usage.error = None;
            }
            Err(error) => self.usage.error = Some(error),
        }
        self.usage.updated = Some(Instant::now());
        true
    }

    pub(super) fn poll_usage(&mut self) -> bool {
        let sessions_changed = self.poll_sessions();
        if let Some(page) = &mut self.usage.page
            && page.follow_today
        {
            page.day = self.usage.snapshot.today();
        }
        if let Some(page) = &mut self.usage.page
            && let Some(saved) = &mut page.inactive
            && saved.follow_today
        {
            saved.day = self.usage.snapshot.today();
        }
        let query = self.usage_query();
        let mut changed = self.usage.query.as_ref() != Some(&query);
        if changed {
            self.usage.query = Some(query.clone());
            self.usage.snapshot.rows.clear();
            self.usage.updated = None;
            self.usage.error = None;
        }
        if let Some(receiver) = &self.usage.receiver {
            match receiver.try_recv() {
                Ok(completed) => {
                    self.usage.receiver = None;
                    self.usage.reader = Some(completed.reader);
                    changed |= self.accept_usage(completed.query, completed.result);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.usage.receiver = None;
                    self.usage.updated = Some(Instant::now());
                    self.usage.error = Some("Usage reader stopped".into());
                    changed = true;
                }
                _ => {}
            }
        }
        if self.usage.receiver.is_none()
            && self
                .usage
                .updated
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(self.config.usage_refresh_secs))
        {
            let (sender, receiver) = mpsc::channel();
            let path = self.paths.state_dir.join(crate::usage::FILE);
            let config = self.paths.config.clone();
            let mut reader = self
                .usage
                .reader
                .take()
                .unwrap_or_else(|| Reader::new(path, config));
            std::thread::spawn(move || {
                let result = reader.read(&query).map_err(|_| {
                    "Usage database unavailable; check permissions or disk space".into()
                });
                let _ = sender.send(UsageRead {
                    reader,
                    query,
                    result,
                });
            });
            self.usage.receiver = Some(receiver);
        }
        changed || sessions_changed
    }

    fn poll_sessions(&mut self) -> bool {
        let mut changed = false;
        let showing_sessions = self.usage.active
            && self
                .usage
                .page
                .as_ref()
                .is_some_and(|p| p.source == UsageSource::Sessions);
        if !showing_sessions {
            self.usage.sessions_watcher = None;
            self.usage.sessions_events = None;
            self.usage.sessions_watch_attempt = None;
            self.usage.sessions_dirty = true;
            return false;
        }
        if let Some(events) = &self.usage.sessions_events {
            while events.try_recv().is_ok() {
                self.usage.sessions_dirty = true;
            }
        }
        let watch_interval = if self
            .usage
            .sessions_watcher
            .as_ref()
            .is_some_and(crate::sessions::ChangeWatch::is_complete)
        {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(2)
        };
        if self
            .usage
            .sessions_watch_attempt
            .is_none_or(|t| t.elapsed() >= watch_interval)
        {
            self.usage.sessions_watch_attempt = Some(Instant::now());
            if let Ok(roots) = crate::sessions::roots() {
                let (sender, receiver) = mpsc::sync_channel(1);
                self.usage.sessions_watcher = Some(crate::sessions::watch_changes(&roots, sender));
                self.usage.sessions_events = Some(receiver);
            } else {
                self.usage.sessions_watcher = None;
                self.usage.sessions_events = None;
            }
        }
        if let Some(receiver) = &self.usage.sessions_receiver {
            match receiver.try_recv() {
                Ok((reader, snapshot, full)) => {
                    self.usage.sessions_reader = Some(reader);
                    if snapshot.rows.is_empty()
                        && snapshot.warnings > 0
                        && !self.usage.sessions.rows.is_empty()
                    {
                        self.usage.sessions.warnings = snapshot.warnings;
                    } else {
                        self.usage.sessions = snapshot;
                    }
                    if full {
                        self.usage.sessions_updated = Some(Instant::now());
                    }
                    self.usage.titles_updated = Some(Instant::now());
                    self.usage.sessions_receiver = None;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.usage.sessions_receiver = None;
                    self.usage.sessions_updated = Some(Instant::now());
                    self.usage.sessions.warnings = 1;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let interval = Duration::from_secs(self.config.usage_refresh_secs);
        let full = self
            .usage
            .sessions_updated
            .is_none_or(|t| t.elapsed() >= interval)
            || (self.usage.sessions_dirty
                && self
                    .usage
                    .sessions_updated
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(1)));
        let titles = self
            .usage
            .titles_updated
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
        if self.usage.sessions_receiver.is_none() && (full || titles) {
            if full {
                self.usage.sessions_dirty = false;
            }
            let (sender, receiver) = mpsc::channel();
            let mut reader = self.usage.sessions_reader.take().unwrap_or_default();
            let cached = self.usage.sessions.clone();
            std::thread::spawn(move || {
                let snapshot = if full {
                    match crate::sessions::roots() {
                        Ok(roots) => reader.read(&roots),
                        Err(_) => crate::sessions::Snapshot {
                            rows: vec![],
                            warnings: 1,
                        },
                    }
                } else {
                    let mut snapshot = cached;
                    reader.refresh_titles(&mut snapshot);
                    snapshot
                };
                let _ = sender.send((reader, snapshot, full));
            });
            self.usage.sessions_receiver = Some(receiver);
        }
        changed
    }

    pub(super) fn open_usage(&mut self) {
        if self.modal.is_some() || self.codex_navigation_blocked() {
            return;
        }
        self.usage.updated = None;
        self.usage.active = true;
        if self.usage.page.is_some() {
            return;
        }
        self.usage.page = Some(UsagePage {
            source: UsageSource::Gateway,
            inactive: None,
            search_draft: None,
            session_detail: false,
            detail_return_scroll: 0,
            day: self.usage.snapshot.today(),
            follow_today: true,
            range: 0,
            chart_tokens: false,
            client: 0,
            provider: None,
            scroll: 0,
            section: 0,
            session_sort_tokens: false,
            session_follow_range: false,
            session_search: String::new(),
            session_searching: false,
            limit: Default::default(),
            dashboard: Default::default(),
        });
    }

    pub(super) fn usage_key(&mut self, mut key: KeyEvent) -> bool {
        let searching = self
            .usage
            .page
            .as_ref()
            .is_some_and(|p| p.session_searching);
        if !searching && key.modifiers.is_empty() {
            key.code = match key.code {
                KeyCode::Char('h') => KeyCode::Esc,
                KeyCode::Char('l') => KeyCode::Enter,
                other => other,
            };
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if !searching && key.code == KeyCode::Char('?') {
            self.open_help();
            return false;
        }
        if !searching && key.code == KeyCode::F(2) {
            self.select_client_tab(ClientTab::Claude);
            return false;
        }
        if let Some(mut page) = self.usage.page.take() {
            if !searching {
                self.usage_enter(&mut page, key);
            }
            if page.key(key, &self.usage.snapshot) {
                self.usage.active = false;
            }
            if !searching && key.code == KeyCode::Char('r') {
                self.usage.updated = None;
                self.usage.sessions_updated = None;
            }
            self.usage.page = Some(page);
        }
        false
    }

    pub(super) fn provider_usage_label(&self, tokens: bool) -> String {
        if self.pi_enabled
            && self.selected_profile_id().is_none_or(|id| {
                crate::pi::native::proxy_endpoint(&self.pi_home, &id)
                    .ok()
                    .flatten()
                    .is_none()
            })
        {
            return "Not tracked (direct API)".into();
        }
        if self.usage.error.is_some() {
            return "Unavailable · F6 Usage".into();
        }
        if self.usage.updated.is_none() {
            return "Loading · F6 Usage".into();
        }
        let Some(provider) = self.selected_profile_id() else {
            return "F6 Usage".into();
        };
        let client = Some(if self.pi_enabled {
            "Pi"
        } else if self.codex_ui.enabled {
            "Codex"
        } else {
            "Claude"
        });
        let today = self.usage.snapshot.today();
        let daily = self
            .usage
            .snapshot
            .total(client, Some(&provider), Some(&today), "generation");
        let total = self
            .usage
            .snapshot
            .total(client, Some(&provider), None, "generation");
        if tokens {
            format!(
                "{} today / {} total",
                daily.tokens_label(),
                total.tokens_label()
            )
        } else {
            format!("{} today / {} total · F6", daily.calls, total.calls)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_switches_reject_old_results_and_refresh_immediately() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        assert_eq!(app.usage_query(), Query::Summary);
        app.open_usage();
        let daily = app.usage_query();
        assert!(matches!(daily, Query::Range { .. }));
        app.usage.page.as_mut().unwrap().range = 3;
        let all = app.usage_query();
        assert_eq!(all, Query::All);
        app.usage.query = Some(all.clone());
        assert!(!app.accept_usage(daily, Err("stale error".into())));
        assert!(app.usage.error.is_none());
        assert!(app.usage.updated.is_none());
        assert!(app.accept_usage(all, Ok(Some(Snapshot::default()))));
        assert!(app.usage.updated.is_some());
        app.usage.active = false;
        assert_eq!(app.usage_query(), Query::Summary);
        assert!(app.poll_usage());
        assert!(app.usage.updated.is_none());
        assert!(app.usage.receiver.is_some());
    }
    #[test]
    fn unchanged_result_cannot_reuse_a_cleared_snapshot() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        app.usage.query = Some(Query::Summary);
        assert!(!app.accept_usage(Query::Summary, Ok(None)));
        assert!(app.usage.updated.is_none());
        assert!(app.usage.reader.is_none());
    }
}
