//! Read-only session browsing; identity survives refreshes and sorting.
use super::*;
use crate::sessions::Session;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    client: &'static str,
    id: String,
}
impl Identity {
    fn of(row: &Session) -> Self {
        Self {
            client: row.client,
            id: row.id.clone(),
        }
    }
    fn matches(&self, row: &Session) -> bool {
        self.client == row.client && self.id == row.id
    }
}
struct SearchDraft {
    query: String,
    selected: Option<Identity>,
    index: usize,
    scroll: u16,
}
#[derive(Default)]
pub(super) struct Browser {
    query: String,
    editing: Option<SearchDraft>,
    selected: Option<Identity>,
    selected_index: usize,
    detail: Option<Identity>,
    return_scroll: u16,
    reveal: bool,
    anchor: Option<(Identity, i64)>,
}
struct Layout {
    lines: Vec<Line<'static>>,
    rows: Vec<(usize, Identity)>,
    search: Option<usize>,
}
impl Monitor {
    fn history_rows(&self) -> Vec<&Session> {
        let query = self.session_browser.query.to_lowercase();
        self.session_rows()
            .into_iter()
            .filter(|s| {
                !self
                    .active_session
                    .as_ref()
                    .is_some_and(|a| a.client == s.client && a.id == s.id)
                    && super::super::session_browser::matches(s, &query)
            })
            .collect()
    }
    pub(super) fn reconcile_sessions(&mut self) {
        let rows = self.history_rows();
        let index = self
            .session_browser
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|row| id.matches(row)))
            .unwrap_or(
                self.session_browser
                    .selected_index
                    .min(rows.len().saturating_sub(1)),
            );
        let selected = rows.get(index).map(|row| Identity::of(row));
        if self.session_browser.selected == selected && index != self.session_browser.selected_index
        {
            self.session_browser.reveal = true;
        }
        self.session_browser.selected = selected;
        self.session_browser.selected_index = index;
    }
    pub(super) fn reveal_session_selection(&mut self, screen: Rect) {
        if self.page != config::PulseStartPage::Sessions
            || self.help
            || self.session_browser.detail.is_some()
        {
            return;
        }
        let body = self.account_body(screen);
        let layout = self.session_layout(body.width);
        if let Some((id, offset)) = self.session_browser.anchor.take()
            && let Some((row, _)) = layout.rows.iter().find(|(_, row_id)| row_id == &id)
        {
            self.scroll = (*row as i64 - offset).clamp(0, i64::from(u16::MAX)) as u16;
        }
        let target = if self.session_browser.editing.is_some() {
            layout.search.map(|row| (row, 1))
        } else if self.session_browser.reveal {
            layout
                .rows
                .get(self.session_browser.selected_index)
                .map(|(row, _)| (*row, 2))
        } else {
            None
        };
        if let Some((row, height)) = target {
            let row = row.min(u16::MAX as usize) as u16;
            if row < self.scroll {
                self.scroll = row;
            } else if row.saturating_add(height) > self.scroll.saturating_add(body.height) {
                self.scroll = row.saturating_add(height).saturating_sub(body.height);
            }
        }
        self.session_browser.reveal = false;
    }
    pub(super) fn token_footer(&self, area: Rect) -> Vec<FooterButton> {
        if let Some(picker) = &self.picker {
            return picker.footer(area);
        }
        if self.page != config::PulseStartPage::Sessions {
            return footer_buttons(area, self.page, self.sessions_sort_tokens);
        }
        if self.session_browser.editing.is_some() {
            return layout_footer(
                area,
                vec![
                    footer_control(["Done(Enter)", "Done", "↵"], KeyCode::Enter, true),
                    footer_control(
                        ["Clear(Ctrl+U)", "Clear", "^U"],
                        KeyCode::Char('u'),
                        !self.session_browser.query.is_empty(),
                    )
                    .modified(KeyModifiers::CONTROL),
                    footer_control(["Cancel(Esc)", "Cancel", "Esc"], KeyCode::Esc, true),
                ],
            );
        }
        if self.session_browser.detail.is_some() {
            return layout_footer(
                area,
                vec![
                    footer_control(["Refresh(r)", "Refresh", "r"], KeyCode::Char('r'), true),
                    footer_control(["Back(q)", "Back", "q"], KeyCode::Char('q'), true),
                    footer_control(["Home(H)", "Home", "H"], KeyCode::Char('H'), true),
                ],
            );
        }
        let mut buttons = footer_buttons(area, self.page, self.sessions_sort_tokens);
        buttons[1].enabled = self.session_browser.selected.is_some();
        buttons
    }
    fn begin_search(&mut self) {
        self.session_browser.editing = Some(SearchDraft {
            query: self.session_browser.query.clone(),
            selected: self.session_browser.selected.clone(),
            index: self.session_browser.selected_index,
            scroll: self.scroll,
        });
    }
    pub(super) fn capture_session_position(&mut self, screen: Rect) {
        if self.page != config::PulseStartPage::Sessions
            || self.help
            || self.session_browser.editing.is_some()
            || self.session_browser.detail.is_some()
        {
            return;
        }
        let layout = self.session_layout(self.account_body(screen).width);
        if let Some((row, id)) = layout
            .rows
            .iter()
            .find(|(row, _)| *row + 2 > usize::from(self.scroll))
        {
            self.session_browser.anchor = Some((id.clone(), *row as i64 - i64::from(self.scroll)));
        }
    }
    pub(super) fn reset_session_browser(&mut self) {
        self.session_browser = Browser::default();
    }
    fn session_layout(&self, width: u16) -> Layout {
        let mut layout = Layout {
            lines: vec![],
            rows: vec![],
            search: None,
        };
        if let Some(id) = &self.session_browser.detail {
            layout.lines.push(line(
                clipped("SESSION DETAILS · Back(Esc)", width.into()),
                BLUE,
            ));
            let Some(s) = self.sessions.rows.iter().find(|s| id.matches(s)) else {
                layout.lines.push(line("Session no longer available", GOLD));
                return layout;
            };
            let token = |n: i64, known: bool| {
                if known {
                    format!("{n}{}", if s.incomplete { "+?" } else { "" })
                } else {
                    "—".into()
                }
            };
            let fields = [
                format!("Title: {}", display_title(s)),
                format!("Client: {}", s.client),
                format!("ID: {}", s.id),
                format!("Project: {}", s.project),
                format!(
                    "Models: {}",
                    if s.models.is_empty() {
                        "—".into()
                    } else {
                        s.models.iter().cloned().collect::<Vec<_>>().join(", ")
                    }
                ),
                format!("Activity: {}", self.session_time(s.updated)),
                format!("Total: {} tok", token(s.tokens.total(), s.tokens.known)),
                format!(
                    "Input: {} · Output: {}",
                    token(s.tokens.input, s.tokens.known),
                    token(s.tokens.output, s.tokens.known)
                ),
                format!(
                    "Cache read: {} · Write: {}",
                    token(s.tokens.read, s.tokens.cache_known),
                    token(s.tokens.write, s.tokens.cache_known)
                ),
                format!(
                    "Cache reuse: {}",
                    s.tokens
                        .cache_reuse_percent()
                        .map_or("—".into(), |p| format!("{p:.1}%"))
                ),
            ];
            for field in fields {
                layout.lines.extend(wrap_field(&field, width));
            }
            if s.client == "Grok" {
                layout.lines.extend(wrap_field(
                    &format!(
                        "API rate: {} · samples {}",
                        if s.api_ms > 0 {
                            format!(
                                "{:.1} tok/s",
                                s.api_output as f64 * 1000.0 / s.api_ms as f64
                            )
                        } else {
                            "—".into()
                        },
                        s.api_samples
                    ),
                    width,
                ));
            }
            if s.child {
                layout
                    .lines
                    .extend(wrap_field("[child] Separate child session", width));
            }
            if s.fork {
                layout
                    .lines
                    .extend(wrap_field("Fork * · may include inherited usage", width));
            }
            if s.incomplete {
                layout
                    .lines
                    .extend(wrap_field("Partial log · totals may be incomplete", width));
            }
        } else {
            layout.lines = if width < 32 {
                match (self.active_session.as_ref(), self.active_row()) {
                    (_, Some(s)) => vec![
                        line(
                            clipped(
                                &format!(
                                    "● {} · {}",
                                    s.client,
                                    std::path::Path::new(&s.project)
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                ),
                                width.into(),
                            ),
                            GREEN,
                        ),
                        line(
                            clipped(
                                &format!(
                                    "{}{} tok",
                                    if s.tokens.known {
                                        short(s.tokens.total())
                                    } else {
                                        "?".into()
                                    },
                                    if s.incomplete { "+?" } else { "" }
                                ),
                                width.into(),
                            ),
                            INK,
                        ),
                    ],
                    (Some(a), None) => vec![line(
                        clipped(&format!("● {} · loading log", a.client), width.into()),
                        GREEN,
                    )],
                    _ => vec![line(clipped("Waiting for agent pane", width.into()), SOFT)],
                }
            } else if self.visual_mode {
                self.visual_active_content(width)
            } else {
                self.active_content(width)
            };
            layout.lines.push(section("SESSION HISTORY", width));
            layout.search = Some(layout.lines.len());
            layout.lines.push(line(
                search_line(
                    &self.session_browser.query,
                    self.session_browser.editing.is_some(),
                    width,
                ),
                BLUE,
            ));
            let rows = self.history_rows();
            layout.lines.push(line(
                clipped(
                    &format!(
                        "{} matches · {} · ↑↓ Enter",
                        rows.len(),
                        if self.sessions_sort_tokens {
                            "Tokens(t)"
                        } else {
                            "Recent(t)"
                        }
                    ),
                    width.into(),
                ),
                SOFT,
            ));
            if self.sessions_refreshed.is_none() {
                layout.lines.push(line("Reading local session logs…", SOFT));
            } else if rows.is_empty() {
                layout.lines.push(line(
                    if self.session_browser.query.is_empty() && self.session_rows().is_empty() {
                        "No local sessions for this client"
                    } else if self.session_browser.query.is_empty() {
                        "No other sessions for this client"
                    } else {
                        "No matching sessions"
                    },
                    SOFT,
                ));
            }
            if let Some(s) = rows.iter().find(|s| {
                self.session_browser
                    .selected
                    .as_ref()
                    .is_some_and(|id| id.matches(s))
            }) {
                layout
                    .lines
                    .extend(wrap_field(&format!("ID: {}", s.id), width));
                layout.lines.extend(wrap_field(
                    &format!("Activity: {}", self.session_time(s.updated)),
                    width,
                ));
            }
            for s in rows {
                let id = Identity::of(s);
                let selected = self.session_browser.selected.as_ref() == Some(&id);
                layout.rows.push((layout.lines.len(), id));
                let project = std::path::Path::new(&s.project)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                layout.lines.push(line(
                    clipped(
                        &format!(
                            "{} {}{}{}",
                            if selected { "›" } else { " " },
                            display_title(s),
                            if s.child { " [child]" } else { "" },
                            if s.fork { " *" } else { "" }
                        ),
                        width.into(),
                    ),
                    if selected { BLUE } else { INK },
                ));
                layout.lines.push(pair(
                    &format!("{} · {project}", s.client),
                    format!(
                        "{}{} tok",
                        if s.tokens.known {
                            short(s.tokens.total())
                        } else {
                            "?".into()
                        },
                        if s.incomplete { "+?" } else { "" }
                    ),
                    width,
                    if selected { BLUE } else { SOFT },
                ));
            }
        }
        if self.sessions.warnings > 0 {
            layout.lines.extend(wrap_field(
                &format!("! {} logs unavailable / partial", self.sessions.warnings),
                width,
            ));
        }
        layout
    }
    fn session_time(&self, timestamp: i64) -> String {
        let zone = chrono::FixedOffset::east_opt(self.snapshot.offset)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap());
        chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|t| t.with_timezone(&zone).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "time ?".into())
    }
    pub(super) fn browser_content(&self, width: u16) -> Vec<Line<'static>> {
        self.session_layout(width).lines
    }
    fn open_session_detail(&mut self) {
        if self.session_browser.selected.is_some() {
            self.session_browser.detail = self.session_browser.selected.clone();
            self.session_browser.return_scroll = self.scroll;
            self.scroll = 0;
        }
    }
    pub(super) fn sessions_input(&mut self, input: &Event, screen: Rect) -> bool {
        if self.page != config::PulseStartPage::Sessions || self.help {
            return false;
        }
        self.reconcile_sessions();
        if self.session_browser.editing.is_some() {
            if let Event::Key(key) = input {
                if key.kind != event::KeyEventKind::Press {
                    return true;
                }
                match key.code {
                    KeyCode::Esc => {
                        let draft = self.session_browser.editing.take().unwrap();
                        self.session_browser.query = draft.query;
                        self.session_browser.selected = draft.selected;
                        self.session_browser.selected_index = draft.index;
                        self.scroll = draft.scroll;
                        self.session_browser.reveal = false;
                        self.reconcile_sessions();
                        return true;
                    }
                    KeyCode::Enter => {
                        self.session_browser.editing = None;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.session_browser.query.clear()
                    }
                    KeyCode::Backspace => {
                        self.session_browser.query.pop();
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return false;
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.session_browser.query.push(c)
                    }
                    _ => {}
                }
                self.reconcile_sessions();
                self.session_browser.reveal = true;
            }
            return true;
        }
        let body = self.account_body(screen);
        match input {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                KeyCode::Char('H') => self.return_home(),
                KeyCode::Esc | KeyCode::Char('q') if self.session_browser.detail.is_some() => {
                    self.session_browser.detail = None;
                    self.scroll = self.session_browser.return_scroll;
                }
                KeyCode::Char('q') => self.return_home(),
                KeyCode::Esc if !self.session_browser.query.is_empty() => {
                    self.session_browser.query.clear();
                    self.reconcile_sessions();
                    self.scroll = 0;
                }
                KeyCode::Char('/') if self.session_browser.detail.is_none() => self.begin_search(),
                KeyCode::Enter if self.session_browser.detail.is_none() => {
                    self.capture_session_position(screen);
                    self.open_session_detail()
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k')
                    if self.session_browser.detail.is_none() =>
                {
                    let count = self.history_rows().len();
                    let index = self.session_browser.selected_index;
                    let next = if matches!(key.code, KeyCode::Up | KeyCode::Char('k')) {
                        index.saturating_sub(1)
                    } else {
                        index.saturating_add(1).min(count.saturating_sub(1))
                    };
                    self.session_browser.selected = None;
                    self.session_browser.selected_index = next;
                    self.reconcile_sessions();
                    let layout = self.session_layout(body.width);
                    if let Some((start, _)) = layout.rows.get(next) {
                        let start = (*start).min(u16::MAX as usize) as u16;
                        if start < self.scroll {
                            self.scroll = start;
                        } else if start.saturating_add(2) > self.scroll.saturating_add(body.height)
                        {
                            self.scroll = start.saturating_add(2).saturating_sub(body.height);
                        }
                    }
                }
                _ => return false,
            },
            Event::Mouse(mouse) if contains(body, mouse.column, mouse.row) => match mouse.kind {
                MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_add(3).min(self.limit)
                }
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                MouseEventKind::Down(MouseButton::Left)
                    if self.session_browser.detail.is_none()
                        && mouse.column < body.right().saturating_sub(1) =>
                {
                    let layout = self.session_layout(body.width);
                    let row = usize::from(mouse.row - body.y) + usize::from(self.scroll);
                    if layout.search == Some(row) {
                        self.begin_search();
                    } else if let Some((_, id)) = layout
                        .rows
                        .iter()
                        .find(|(start, _)| row >= *start && row < start + 2)
                    {
                        self.session_browser.selected = Some(id.clone());
                        self.capture_session_position(screen);
                        self.open_session_detail();
                    }
                }
                MouseEventKind::Down(MouseButton::Left)
                    if self.session_browser.detail.is_some()
                        && mouse.column < body.right().saturating_sub(1)
                        && usize::from(mouse.row - body.y) + usize::from(self.scroll) == 0 =>
                {
                    self.session_browser.detail = None;
                    self.scroll = self.session_browser.return_scroll;
                }
                _ => return false,
            },
            _ => return false,
        }
        true
    }
}
pub(super) fn display_title(s: &Session) -> String {
    super::super::session_browser::title(s)
}
fn search_line(query: &str, editing: bool, width: u16) -> String {
    let prefix = if width >= 16 { "Search(/): " } else { "/ " };
    let cursor = if editing { "▏" } else { "" };
    let available = usize::from(width).saturating_sub(prefix.width() + cursor.width());
    let suffix = super::super::session_browser::query_tail(query, available);
    clipped(&format!("{prefix}{suffix}{cursor}"), width.into())
}
pub(super) fn wrap_field(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width).max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = ch.width().unwrap_or(0);
        if used + size > width && !current.is_empty() {
            lines.push(line(std::mem::take(&mut current), INK));
            used = 0;
        }
        current.push(ch);
        used += size;
    }
    if !current.is_empty() {
        lines.push(line(current, INK));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitor() -> Monitor {
        let mut m = Monitor {
            page: config::PulseStartPage::Sessions,
            client: 3,
            sessions_refreshed: Some(Instant::now()),
            ..Default::default()
        };
        for (client, id, project, model, updated) in [
            (
                "Codex",
                "alpha-full-id",
                "/work/中文/项目",
                "GPT-Custom",
                30,
            ),
            ("Claude", "beta-full-id", "/other/项目", "Claude-Model", 20),
            ("Codex", "gamma-full-id", "/work/another", "GPT-Custom", 10),
        ] {
            m.sessions.rows.push(Session {
                client,
                id: id.into(),
                project: project.into(),
                models: [model.into()].into(),
                updated,
                ..Default::default()
            });
        }
        m.reconcile_sessions();
        m
    }
    fn key(m: &mut Monitor, code: KeyCode) {
        m.sessions_input(
            &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            Rect::new(0, 0, 40, 15),
        );
    }
    #[test]
    fn search_unicode_identifiers_models_and_cancel() {
        let mut m = monitor();
        for (query, count) in [
            ("中文", 1),
            ("ALPHA-FULL", 1),
            ("gpt-custom", 2),
            ("", 3),
            ("missing", 0),
        ] {
            m.session_browser.query = query.into();
            assert_eq!(m.history_rows().len(), count);
        }
        m.session_browser.query = "GPT".into();
        m.client = 0;
        assert!(m.history_rows().is_empty());
        m.client = 3;
        key(&mut m, KeyCode::Char('/'));
        key(&mut m, KeyCode::Char('s'));
        key(&mut m, KeyCode::Char('q'));
        assert_eq!(m.page, config::PulseStartPage::Sessions);
        key(&mut m, KeyCode::Esc);
        assert_eq!(m.session_browser.query, "GPT");
        key(&mut m, KeyCode::Esc);
        assert!(m.session_browser.query.is_empty());
    }
    #[test]
    fn navigation_details_refresh_removal_and_focus() {
        let mut m = monitor();
        key(&mut m, KeyCode::Down);
        assert_eq!(
            m.session_browser.selected.as_ref().unwrap().id,
            "beta-full-id"
        );
        m.scroll = 3;
        key(&mut m, KeyCode::Enter);
        assert_eq!(m.scroll, 0);
        m.apply_focus(FocusUpdate {
            pane_id: "new-pane".into(),
            client: 1,
            session: Some(AgentSession {
                client: "Codex",
                id: "new-session".into(),
            }),
        });
        assert_eq!(m.client, 3);
        assert!(m.session_browser.detail.is_some());
        m.sessions.rows[1].updated = 100;
        m.reconcile_sessions();
        assert_eq!(m.session_browser.selected_index, 0);
        key(&mut m, KeyCode::Esc);
        assert_eq!(m.scroll, 3);
        key(&mut m, KeyCode::Enter);
        m.sessions.rows.retain(|s| s.id != "beta-full-id");
        m.reconcile_sessions();
        assert!(
            m.browser_content(40)
                .iter()
                .any(|l| l.to_string().contains("no longer available"))
        );
        key(&mut m, KeyCode::Esc);
        assert!(m.session_browser.selected.is_some());
    }
    #[test]
    fn layout_mouse_and_wrapped_details_across_sizes() {
        for (width, height) in [(80, 30), (40, 15), (31, 11), (12, 8)] {
            let mut m = monitor();
            let screen = Rect::new(0, 0, width, height);
            let body = m.account_body(screen);
            let layout = m.session_layout(body.width);
            let (row, id) = &layout.rows[0];
            let expected = id.clone();
            m.scroll = *row as u16;
            assert!(m.sessions_input(
                &Event::Mouse(event::MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: body.x,
                    row: body.y,
                    modifiers: KeyModifiers::NONE
                }),
                screen
            ));
            assert_eq!(m.session_browser.detail, Some(expected));
            let lines = m.browser_content(body.width);
            let text = lines.iter().map(|l| l.to_string()).collect::<String>();
            assert!(text.contains("alpha-full-id"));
            assert!(text.contains("/work/中文/项目"));
            assert!(text.contains("Total: —"));
            assert!(
                lines
                    .iter()
                    .skip(1)
                    .all(|line| line.width() <= usize::from(body.width))
            );
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| m.draw(f)).unwrap();
        }
    }
    #[test]
    fn footer_and_partial_grok_details_preserve_metadata() {
        let mut m = monitor();
        let screen = Rect::new(0, 0, 40, 15);
        let search = m.token_footer(screen)[0].key;
        key(&mut m, search);
        assert!(m.session_browser.editing.is_some());
        key(&mut m, KeyCode::Enter);
        let row = &mut m.sessions.rows[0];
        row.client = "Grok";
        row.child = true;
        row.fork = true;
        row.incomplete = true;
        row.tokens = crate::sessions::Tokens {
            known: true,
            cache_known: false,
            input: 10,
            output: 2,
            ..Default::default()
        };
        row.api_ms = 1000;
        row.api_output = 2;
        row.api_samples = 1;
        m.sessions.warnings = 1;
        m.reconcile_sessions();
        key(&mut m, KeyCode::Enter);
        let text = m
            .browser_content(80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Total: 12+?",
            "Cache read: —",
            "API rate: 2.0",
            "[child]",
            "Fork *",
            "Partial log",
            "1 logs unavailable",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        let back = m.token_footer(screen)[0].key;
        assert_eq!(back, KeyCode::Char('r'));
        let back = m.token_footer(screen)[1].key;
        assert_eq!(back, KeyCode::Char('q'));
        key(&mut m, back);
        assert!(m.session_browser.detail.is_none());
    }
    fn click_footer(m: &mut Monitor, screen: Rect, index: usize) {
        let area = screen;
        let button = &m.token_footer(area)[index];
        let input = Event::Mouse(event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.rect.x,
            row: button.rect.y,
            modifiers: KeyModifiers::NONE,
        });
        if let Some(Some(key)) = m.footer_key(&input, screen) {
            m.sessions_input(&Event::Key(key), screen);
        }
    }
    #[test]
    fn footer_changes_by_depth_and_search_buttons_work_with_the_mouse() {
        let screen = Rect::new(0, 0, 60, 20);
        let mut m = monitor();
        assert_eq!(m.token_footer(screen).len(), 5);
        click_footer(&mut m, screen, 1);
        assert!(m.session_browser.detail.is_some());
        assert_eq!(m.token_footer(screen).len(), 3);
        assert!(
            m.token_footer(screen)
                .iter()
                .all(|b| !b.label.contains("Close") && !b.label.contains('×'))
        );
        click_footer(&mut m, screen, 1);
        assert!(m.session_browser.detail.is_none());
        m.scroll = 4;
        let original = m.session_browser.selected.clone();
        click_footer(&mut m, screen, 0);
        assert_eq!(m.token_footer(screen).len(), 3);
        assert!(!m.token_footer(screen)[1].enabled);
        key(&mut m, KeyCode::Char('q'));
        assert_eq!(m.session_browser.query, "q");
        click_footer(&mut m, screen, 1);
        assert!(m.session_browser.query.is_empty());
        key(&mut m, KeyCode::Char('g'));
        click_footer(&mut m, screen, 2);
        assert!(m.session_browser.query.is_empty());
        assert_eq!(m.session_browser.selected, original);
        assert_eq!(m.scroll, 4);
        click_footer(&mut m, screen, 0);
        key(&mut m, KeyCode::Char('g'));
        click_footer(&mut m, screen, 0);
        assert!(m.session_browser.editing.is_none());
        assert_eq!(m.session_browser.query, "g");
        key(&mut m, KeyCode::Char('q'));
        assert_eq!(m.page, config::PulseStartPage::Home);
        assert!(m.token_footer(screen).last().unwrap().label.contains('×'));
    }
    #[test]
    fn empty_and_narrow_footers_fit_and_disabled_actions_do_nothing() {
        let mut m = monitor();
        m.sessions.rows.clear();
        m.reconcile_sessions();
        for width in [0, 1, 5, 12, 20, 31, 40, 80] {
            let area = Rect::new(0, 0, width, 8);
            for state in 0..3 {
                m.session_browser.editing = None;
                m.session_browser.detail = None;
                if state == 1 {
                    m.begin_search();
                }
                if state == 2 {
                    m.session_browser.detail = Some(Identity {
                        client: "Codex",
                        id: "missing".into(),
                    });
                }
                let buttons = m.token_footer(area);
                for b in &buttons {
                    assert!(b.rect.right() <= area.right());
                    assert!(b.label.width() <= usize::from(b.rect.width));
                }
                assert!(
                    buttons
                        .windows(2)
                        .all(|pair| pair[0].rect.right() <= pair[1].rect.x)
                );
                assert!(buttons.iter().all(|b| !b.label.contains('×')));
            }
        }
        m.session_browser.detail = None;
        let screen = Rect::new(0, 0, 40, 15);
        let button = &m.token_footer(screen)[1];
        assert!(!button.enabled);
        assert_eq!(
            m.footer_key(
                &Event::Mouse(event::MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: button.rect.x,
                    row: button.rect.y,
                    modifiers: KeyModifiers::NONE
                }),
                screen
            ),
            Some(None)
        );
        m.page = config::PulseStartPage::Charts;
        assert_eq!(m.token_footer(screen).len(), 2);
    }
    #[test]
    fn title_search_long_query_and_viewport_anchor_keep_identity() {
        let mut m = monitor();
        m.sessions.rows[0].title = Some("自定义会话标题 Native".into());
        m.session_browser.query = "会话标题".into();
        m.reconcile_sessions();
        assert_eq!(m.history_rows().len(), 1);
        assert!(
            m.browser_content(80)
                .iter()
                .any(|l| l.to_string().contains("自定义会话标题 Native"))
        );
        for width in [5, 12, 20, 40] {
            let text = search_line("abcdefghijklmnopqrstuvwxyz中文末尾", true, width);
            assert!(text.width() <= usize::from(width));
            assert!(text.ends_with('▏'));
            if width >= 12 {
                assert!(text.contains("末尾"));
            }
        }
        m.session_browser.query.clear();
        m.reconcile_sessions();
        m.source_pane = Some("pane".into());
        m.active_session = Some(AgentSession {
            client: "Codex",
            id: "alpha-full-id".into(),
        });
        m.reconcile_sessions();
        let screen = Rect::new(0, 0, 48, 20);
        let body = m.account_body(screen);
        let row = m.session_layout(body.width).rows[0].0;
        m.scroll = row as u16 + 1;
        m.capture_session_position(screen);
        m.visual_mode = true;
        m.reveal_session_selection(screen);
        let next = m.session_layout(body.width).rows[0].0;
        assert_eq!(m.scroll, next as u16 + 1);
        key(&mut m, KeyCode::Enter);
        assert!(
            m.browser_content(80)
                .iter()
                .any(|l| l.to_string().starts_with("Title:"))
        );
    }
    #[test]
    fn search_accept_and_selection_stays_visible_after_reordering() {
        let mut m = monitor();
        let screen = Rect::new(0, 0, 40, 12);
        key(&mut m, KeyCode::Char('/'));
        key(&mut m, KeyCode::Char('g'));
        key(&mut m, KeyCode::Enter);
        assert_eq!(m.history_rows().len(), 2);
        assert!(m.session_browser.editing.is_none());
        key(&mut m, KeyCode::Down);
        let selected = m.session_browser.selected.clone();
        m.sessions.rows[2].updated = 200;
        m.reconcile_sessions();
        m.reveal_session_selection(screen);
        assert_eq!(m.session_browser.selected, selected);
        let body = m.account_body(screen);
        let layout = m.session_layout(body.width);
        let (row, _) = &layout.rows[m.session_browser.selected_index];
        assert!(*row >= usize::from(m.scroll));
        assert!(*row + 2 <= usize::from(m.scroll + body.height));
    }
    #[test]
    fn home_button_returns_directly_to_token_home_from_details() {
        let screen = Rect::new(0, 0, 40, 12);
        let mut m = monitor();
        key(&mut m, KeyCode::Enter);
        assert!(m.session_browser.detail.is_some());
        let index = m
            .token_footer(screen)
            .iter()
            .position(|b| b.key == KeyCode::Char('H'))
            .unwrap();
        click_footer(&mut m, screen, index);
        assert_eq!(m.page, config::PulseStartPage::Home);
        assert!(m.session_browser.detail.is_none());
        assert!(m.session_browser.query.is_empty());
        assert_eq!(m.scroll, 0);
    }
}
