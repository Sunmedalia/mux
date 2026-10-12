use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceStatus {
    Loading,
    Ready,
    Stale,
    Unavailable,
    Partial,
}

impl Monitor {
    fn draw_help_page(&mut self, f: &mut ratatui::Frame) {
        let screen = f.area();
        f.render_widget(
            Block::default().style(Style::default().bg(BG).fg(INK)),
            screen,
        );
        let (area, body) = help_area(screen);
        draw_page_header(f, area, 0, false, true);
        let content = self.content(body.width);
        self.limit = (content.len() as u16).saturating_sub(body.height);
        self.scroll = self.scroll.min(self.limit);
        f.render_widget(Paragraph::new(content).scroll((self.scroll, 0)), body);
        if self.limit > 0 {
            let mut state = scroll_state(
                usize::from(self.limit + body.height),
                usize::from(self.scroll),
                usize::from(body.height),
            );
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(Style::default().fg(BLUE))
                    .track_style(Style::default().fg(RAIL)),
                Rect::new(area.right().saturating_sub(1), body.y, 1, body.height),
                &mut state,
            );
        }
        if area.height >= 2 {
            f.render_widget(
                Paragraph::new(self.help_language.text(
                    "l 切换中 / 英 · ? / Esc 返回",
                    "l: 中文 / English · ? / Esc: back",
                ))
                .style(Style::default().fg(SOFT)),
                Rect::new(area.x, area.bottom() - 2, area.width, 1),
            );
        }
        for button in help_footer(area, self.help_language) {
            f.render_widget(
                Paragraph::new(button.label)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(BLUE).bg(RAIL)),
                button.rect,
            );
        }
    }

    fn subscription_mode(&self) -> bool {
        match self.client {
            1 => self.accounts.codex.subscription,
            2 => self.accounts.grok.subscription,
            _ => false,
        }
    }
    fn gateway_status(&self) -> SourceStatus {
        match (self.error.is_some(), self.refreshed.is_some()) {
            (true, true) => SourceStatus::Stale,
            (true, false) => SourceStatus::Unavailable,
            (false, false) => SourceStatus::Loading,
            (false, true) => SourceStatus::Ready,
        }
    }
    fn session_status(&self) -> SourceStatus {
        if self.sessions.warnings > 0 {
            if self.sessions.rows.is_empty() {
                SourceStatus::Unavailable
            } else {
                SourceStatus::Partial
            }
        } else if self.sessions_refreshed.is_none() {
            SourceStatus::Loading
        } else {
            SourceStatus::Ready
        }
    }
    fn gateway_placeholder(&self) -> Option<(&'static str, Color)> {
        match self.gateway_status() {
            SourceStatus::Loading => Some(("Reading gateway usage…", SOFT)),
            SourceStatus::Unavailable => Some(("Gateway unavailable · r retry", RED)),
            _ => None,
        }
    }
    fn page_status(&self) -> (String, Color) {
        let charts = self.page == config::PulseStartPage::Charts;
        let sessions = self.page == config::PulseStartPage::Sessions;
        let gateway =
            (!sessions && (charts || !self.subscription_mode())).then(|| self.gateway_status());
        let local = (charts || sessions || self.client == 2 || self.source_pane.is_some())
            .then(|| self.session_status());
        let mut errors = String::new();
        let mut color = SOFT;
        for (name, state) in [("GW", gateway), ("Sessions", local)] {
            let label = match state {
                Some(SourceStatus::Stale) => "stale",
                Some(SourceStatus::Unavailable) => "unavailable",
                Some(SourceStatus::Partial) => "partial",
                _ => continue,
            };
            if !errors.is_empty() {
                errors.push_str(" · ");
            }
            errors.push_str(name);
            errors.push(' ');
            errors.push_str(label);
            if state == Some(SourceStatus::Partial) {
                if color != RED {
                    color = GOLD;
                }
            } else {
                color = RED;
            }
        }
        if !errors.is_empty() {
            return (format!("! {errors} · r retry"), color);
        }
        if gateway == Some(SourceStatus::Loading) {
            return ("Reading gateway usage…".into(), SOFT);
        }
        if local == Some(SourceStatus::Loading) {
            return ("Reading local sessions…".into(), SOFT);
        }
        if let Some(notice) = &self.notice {
            return (notice.clone(), SOFT);
        }
        let text = if sessions {
            self.sessions_refreshed
                .map_or("Reading local sessions…".into(), |time| {
                    format!(
                        "● Sessions updated {}s ago · on change",
                        time.elapsed().as_secs()
                    )
                })
        } else if charts {
            "● Charts auto-update · c: Home".into()
        } else {
            "● Auto-update · r refresh".into()
        };
        (text, SOFT)
    }
    pub(super) fn header_switch_rect(&self, area: Rect, mini: bool) -> Rect {
        page_header_rects(
            area,
            pulse_header_reserve(area, mini),
            self.page == config::PulseStartPage::Git,
        )[1]
    }
    fn draw_token_header(&self, f: &mut ratatui::Frame, area: Rect, mini: bool) {
        let reserve = pulse_header_reserve(area, mini);
        draw_page_header(f, area, reserve, false, self.picker.is_none());
        let suffix = if self.help {
            "帮助"
        } else if self.page == config::PulseStartPage::Sessions {
            "SESSIONS"
        } else if self.page == config::PulseStartPage::Charts {
            "CHARTS"
        } else {
            ""
        };
        let x = page_header_rects(area, reserve, false)[1]
            .right()
            .saturating_add(2);
        let width = area.right().saturating_sub(reserve).saturating_sub(x);
        if suffix.width() <= usize::from(width) {
            f.render_widget(
                Paragraph::new(suffix).style(Style::default().fg(SOFT)),
                Rect::new(x, area.y, width, 1),
            );
        }
    }
    pub(super) fn account_content(&self, width: u16) -> Vec<Line<'static>> {
        let (title, info) = match self.client {
            1 => ("CODEX ACCOUNT", &self.accounts.codex),
            2 => ("GROK ACCOUNT", &self.accounts.grok),
            _ => return vec![],
        };
        let mut out = vec![section(title, width)];
        let Some(card) = &info.card else {
            out.push(line(
                if info.lines.is_empty() {
                    "Select account ▾ [a]"
                } else {
                    "No active account ▾ [a]"
                },
                SOFT,
            ));
            if let Some(picker) = &self.picker {
                out.extend(picker.content(width));
            }
            return out;
        };
        if self.visual_mode {
            let local = card
                .rows
                .iter()
                .any(|(label, value)| label == "Login" && value.contains("Local"));
            let mut identity = pair(
                &format!("{} {}", if local { "●" } else { "○" }, card.name),
                format!(
                    "{} {}",
                    card.badge.to_uppercase(),
                    if self.picker.is_some() { "▴" } else { "▾" }
                ),
                width,
                GREEN,
            );
            identity.spans[0].style = Style::default().fg(INK).add_modifier(Modifier::BOLD);
            out.push(identity);
        } else {
            out.push(pair(
                &card.name,
                format!(
                    "{} {}",
                    card.badge,
                    if self.picker.is_some() { "▴" } else { "▾" }
                ),
                width,
                GREEN,
            ));
        }
        if let Some(picker) = &self.picker {
            out.extend(picker.content(width));
        }
        if !card.email.is_empty() {
            out.push(line(clipped(&card.email, width.into()), INK));
        }
        if self.visual_mode {
            if !card.gauges.is_empty() || card.unknown_gauge.is_some() {
                out.push(line("LIMITS · USED", SOFT));
            }
            for (label, percent, reset) in &card.gauges {
                out.extend(account_quota_rows(label, Some(*percent), reset, width));
            }
            if let Some((label, reset)) = &card.unknown_gauge {
                out.extend(account_quota_rows(label, None, reset, width));
            }
            let row = |name| {
                card.rows
                    .iter()
                    .find(|(label, _)| label == name)
                    .map(|(_, value)| value.as_str())
            };
            if let Some(quota) = row("Quota") {
                out.push(line(
                    clipped(quota, width.into()),
                    if quota.starts_with('!') { GOLD } else { SOFT },
                ));
            }
            if let Some(switch) = row("Switch") {
                out.push(line(clipped(switch, width.into()), GOLD));
            }
            let login = row("Login")
                .map(|value| {
                    if value.contains("Expired") {
                        "Expired"
                    } else if value.contains("Local") {
                        "Local"
                    } else {
                        "Saved"
                    }
                })
                .unwrap_or("API");
            let state = row("Accounts")
                .map(|count| format!("{login} · {count} accounts"))
                .unwrap_or(login.into());
            let age = compact_age(row("Updated").unwrap_or("—"));
            out.push(pair(&state, age, width, SOFT));
            if card.id.is_some() && card.gauges.is_empty() && card.unknown_gauge.is_none() {
                out.push(line("○ Limits unavailable · r refresh", SOFT));
            }
            return out;
        }
        for (label, percent, reset) in &card.gauges {
            out.push(pair(label, format!("{percent:.0}% used"), width, GOLD));
            out.push(Line::from(progress_spans(
                Some(percent / 100.0),
                width.into(),
                quota_color(*percent),
            )));
            if !reset.is_empty() {
                out.push(pair(
                    "Reset",
                    clipped(reset, width.saturating_sub(7).into()),
                    width,
                    SOFT,
                ));
            }
        }
        if let Some((label, reset)) = &card.unknown_gauge {
            out.push(pair(label, "— used", width, SOFT));
            out.push(line("▒".repeat(width.into()), RAIL));
            if !reset.is_empty() {
                out.push(pair("Reset", reset, width, SOFT));
            }
        }
        for (label, value) in &card.rows {
            out.push(pair(
                label,
                clipped(value, width.saturating_sub(label.len() as u16 + 1).into()),
                width,
                INK,
            ));
        }
        if !card.models.is_empty() {
            out.push(section("MODELS", width));
            for model in &card.models {
                out.push(line(clipped(&format!("· {model}"), width.into()), SOFT));
            }
        }
        out
    }
    pub(super) fn grok_tokens(&self, width: u16) -> Vec<Line<'static>> {
        let selected = self.active_session.as_ref().filter(|s| s.client == "Grok");
        let row = if selected.is_some() {
            self.active_row()
        } else {
            self.sessions
                .rows
                .iter()
                .filter(|s| s.client == "Grok" && s.tokens.known)
                .max_by_key(|s| s.updated)
        };
        let mut out = vec![section_meta(
            "SESSION TOKENS",
            if selected.is_some() {
                "● Active"
            } else {
                "Recent"
            },
            width,
        )];
        let value = row
            .filter(|s| s.tokens.known)
            .map(|s| short(s.tokens.total()))
            .unwrap_or("—".into());
        out.extend(if width < 30 {
            mini_token_total(&value, width, BLUE)
        } else {
            token_digits(&value, BLUE)
        });
        if let Some(row) = row.filter(|s| s.tokens.known) {
            let t = &row.tokens;
            out.push(duo_line(
                width,
                ("↑ Input", "↑", &short(t.input), BLUE),
                ("↓ Output", "↓", &short(t.output), GOLD),
            ));
            if self.visual_mode {
                out.push(compact_token_meter(t.input, t.output, width));
                out.push(compact_cache_meter(
                    t.read,
                    t.write,
                    t.cache_reuse_percent(),
                    t.cache_known,
                    width,
                ));
            } else {
                out.push(duo_line(
                    width,
                    ("↺ Read", "R", &short(t.read), GREEN),
                    ("Write", "W", &short(t.write), SOFT),
                ));
                out.push(pair(
                    "Cache hit",
                    t.cache_reuse_percent()
                        .map(|p| format!("{p:.0}%"))
                        .unwrap_or("—".into()),
                    width,
                    GREEN,
                ));
            }
            out.push(speed_pair(
                "API rate",
                &Totals {
                    speed_output: row.api_output,
                    speed_ms: row.api_ms,
                    ..Default::default()
                },
                width,
            ));
            out.push(line(format!("{} timed prompts", row.api_samples), SOFT));
            out.push(line(
                clipped(&row.id.chars().take(12).collect::<String>(), width.into()),
                SOFT,
            ));
        } else {
            out.push(line(
                if self.sessions_refreshed.is_none() {
                    "◌ Reading sessions…"
                } else {
                    "○ No token data yet"
                },
                SOFT,
            ));
        }
        out
    }
    pub(super) fn content(&self, width: u16) -> Vec<Line<'static>> {
        if self.help {
            return self.stats_content(width);
        }
        let mut out = if self.page != config::PulseStartPage::Sessions
            && self.page != config::PulseStartPage::Charts
        {
            self.account_content(width)
        } else {
            vec![]
        };
        out.extend(self.stats_content(width));
        out
    }
    pub(super) fn mini_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.help {
            return self.mini_stats_content(width);
        }
        let mut out = if self.page != config::PulseStartPage::Sessions
            && self.page != config::PulseStartPage::Charts
        {
            self.account_content(width)
        } else {
            vec![]
        };
        out.extend(self.mini_stats_content(width));
        out
    }
    pub(super) fn mini_stats_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.help {
            return self.stats_content(width);
        }
        if self.page == config::PulseStartPage::Sessions {
            let mut out = vec![mini_line("SESSIONS / ALL TIME", width, BLUE)];
            match (self.active_session.as_ref(), self.active_row()) {
                (None, _) => out.push(mini_line("◌ Waiting for agent pane", width, SOFT)),
                (Some(active), None) => out.push(mini_line(
                    format!("● {} · loading log", active.client),
                    width,
                    GREEN,
                )),
                (_, Some(session)) => {
                    let project = std::path::Path::new(&session.project)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    out.push(mini_line(
                        format!("● {} / {}", session.client, project),
                        width,
                        GREEN,
                    ));
                    out.push(mini_line(
                        format!(
                            "{} tok{}",
                            if session.tokens.known {
                                short(session.tokens.total())
                            } else {
                                "?".into()
                            },
                            if session.incomplete { "+?" } else { "" }
                        ),
                        width,
                        INK,
                    ));
                    if session.tokens.known {
                        out.push(mini_line(
                            format!(
                                "↑ {}  ↓ {}",
                                short(session.tokens.input),
                                short(session.tokens.output)
                            ),
                            width,
                            SOFT,
                        ));
                        out.push(mini_line(
                            format!(
                                "↺ R {}  W {}",
                                short(session.tokens.read),
                                short(session.tokens.write)
                            ),
                            width,
                            SOFT,
                        ));
                    }
                }
            }
            out.push(mini_line("HISTORY · t sort", width, BLUE));
            let rows = self.session_rows();
            if rows.is_empty() {
                out.push(mini_line("No local sessions yet", width, SOFT));
            }
            for session in rows.into_iter().filter(|s| {
                !self
                    .active_session
                    .as_ref()
                    .is_some_and(|a| a.client == s.client && a.id == s.id)
            }) {
                let project = std::path::Path::new(&session.project)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                out.push(mini_line(
                    format!(
                        "{} {} · {} tok",
                        session.client,
                        project,
                        if session.tokens.known {
                            short(session.tokens.total())
                        } else {
                            "?".into()
                        }
                    ),
                    width,
                    INK,
                ));
                if session.tokens.known {
                    out.push(mini_line(
                        format!(
                            "  ↑ {} · ↓ {}",
                            short(session.tokens.input),
                            short(session.tokens.output)
                        ),
                        width,
                        SOFT,
                    ));
                }
            }
            return out;
        }
        if self.page == config::PulseStartPage::Charts {
            let mut out = vec![mini_line("CHARTS / TODAY", width, BLUE)];
            if let Some((message, color)) = self.gateway_placeholder() {
                out.push(mini_line(message, width, color));
            } else {
                let gateway = metrics(&self.snapshot, self.client());
                out.extend([
                    mini_line(format!("Calls  {}", gateway.total.calls), width, INK),
                    mini_line(mini_sparkline(&gateway.hours), width, BLUE),
                    mini_line("00   06   12   18   24", width, SOFT),
                    mini_line("GATEWAY / HOURLY CALLS", width, BLUE),
                ]);
                out.extend(mini_hourly_rows(&gateway.hours, width, BLUE));
            }
            out.push(mini_line("SESSION / HOURLY TOKENS", width, GREEN));
            if self.active_session.is_none() {
                out.push(mini_line("Waiting for agent session ID…", width, SOFT));
            } else if let Some(row) = self.active_row() {
                if row.tokens.known {
                    let session = self.session_hours_today();
                    out.push(mini_line(
                        format!("Session  {} tok", short(session.iter().sum())),
                        width,
                        GREEN,
                    ));
                    out.push(mini_line(mini_sparkline(&session), width, GREEN));
                    out.extend(mini_hourly_rows(&session, width, GREEN));
                } else {
                    out.push(mini_line("Session tokens unavailable", width, GOLD));
                }
            } else {
                out.push(mini_line("Waiting for local session log…", width, SOFT));
            }
            return out;
        }
        self.home_stats_content(width, true)
    }
    pub(super) fn draw_mini(&mut self, f: &mut ratatui::Frame, area: Rect) {
        if area.height < 4 {
            f.render_widget(
                Paragraph::new(clipped("q close", area.width.into()))
                    .style(Style::default().fg(BLUE)),
                area,
            );
            self.limit = 0;
            self.scroll = 0;
            return;
        }
        self.draw_token_header(f, area, true);
        if area.width >= 24 {
            f.render_widget(
                Paragraph::new("v").style(Style::default().fg(SOFT)),
                Rect::new(area.right() - 7, area.y, 1, 1),
            );
        }
        if area.width >= 10 {
            f.render_widget(
                Paragraph::new("Help").style(Style::default().fg(SOFT)),
                pulse_help_rect(area),
            );
        }
        let tabs = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            1,
        ));
        for (i, name) in CLIENTS.into_iter().enumerate() {
            let label = if area.width < 21 {
                ["Cl", "Cx", "Gk", "All"][i]
            } else {
                name
            };
            f.render_widget(
                Paragraph::new(label).alignment(Alignment::Center).style(
                    Style::default()
                        .fg(if self.client == i { BG } else { SOFT })
                        .bg(if self.client == i { BLUE } else { RAIL }),
                ),
                tabs[i],
            );
        }
        let body = mini_body(area);
        let content = self.mini_content(body.width);
        self.limit = (content.len() as u16).saturating_sub(body.height);
        self.scroll = self.scroll.min(self.limit);
        f.render_widget(Paragraph::new(content).scroll((self.scroll, 0)), body);
        if area.height >= 4 {
            let (status, color) = self.page_status();
            f.render_widget(
                Paragraph::new(clipped(&status, area.width.into()))
                    .style(Style::default().fg(color)),
                Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 1),
            );
        }
        for button in footer_buttons(area, self.page, self.sessions_sort_tokens) {
            f.render_widget(
                Paragraph::new(button.label)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(BLUE).bg(RAIL)),
                button.rect,
            );
        }
    }
    pub(super) fn client(&self) -> Option<&'static str> {
        [Some("Claude"), Some("Codex"), Some("Grok"), None][self.client]
    }
    pub(super) fn provider_header_hit(&self, body: Rect, column: u16, row: u16) -> bool {
        if self.page == config::PulseStartPage::Sessions
            || self.page == config::PulseStartPage::Charts
            || !contains(body, column, row)
        {
            return false;
        }
        let Some(index) = self.content(body.width).iter().position(|line| {
            let text = line.to_string();
            text.starts_with("PROVIDERS / TODAY") || text.starts_with("MODELS / TODAY")
        }) else {
            return false;
        };
        let index = index as u16;
        index >= self.scroll && row == body.y + index - self.scroll
    }
    pub(super) fn active_row(&self) -> Option<&crate::sessions::Session> {
        let current = self.active_session.as_ref()?;
        self.sessions
            .rows
            .iter()
            .find(|s| s.client == current.client && s.id == current.id)
    }
    pub(super) fn session_hours_today(&self) -> [i64; 24] {
        let mut hours = [0; 24];
        let Some(session) = self.active_row() else {
            return hours;
        };
        let zone = chrono::FixedOffset::east_opt(self.snapshot.offset)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap());
        let today = self.snapshot.today();
        for (&timestamp, &tokens) in &session.activity {
            if let Some(time) = chrono::DateTime::from_timestamp(timestamp, 0) {
                let local = time.with_timezone(&zone);
                if local.format("%Y-%m-%d").to_string() == today {
                    hours[local.hour() as usize] += tokens;
                }
            }
        }
        hours
    }
    pub(super) fn chart_content(&self, width: u16) -> Vec<Line<'static>> {
        let session = self.session_hours_today();
        let mut out = vec![
            section("CHARTS / TODAY", width),
            line("Gateway calls + active session tokens", SOFT),
            Line::default(),
            section("GATEWAY / REQUESTS BY HOUR", width),
        ];
        if let Some((message, color)) = self.gateway_placeholder() {
            out.push(line(message, color));
        } else {
            let gateway = metrics(&self.snapshot, self.client());
            out.push(pair(
                "Calls today",
                gateway.total.calls.to_string(),
                width,
                BLUE,
            ));
            out.extend(hourly_chart(&gateway.hours, width, BLUE));
        }
        out.extend([Line::default(), section("SESSION / TOKENS BY HOUR", width)]);
        if self.active_session.is_none() {
            out.push(line("Waiting for agent session ID…", SOFT));
        } else if self.active_row().is_none() {
            out.push(line("Waiting for local session log…", SOFT));
        } else if self.active_row().is_some_and(|row| !row.tokens.known) {
            out.push(line("Session tokens unavailable", GOLD));
        } else {
            out.push(pair(
                "Tokens today",
                short(session.iter().sum()),
                width,
                GREEN,
            ));
            out.extend(hourly_chart(&session, width, GREEN));
            out.push(line("Input + output from local log", SOFT));
        }
        out
    }
    pub(super) fn visual_active_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.source_pane.is_none() {
            return vec![];
        }
        let mut out = vec![section("SESSION / ALL TIME", width)];
        let Some(current) = &self.active_session else {
            out.push(line("◌ Waiting for agent session ID", SOFT));
            out.push(line("Open or resume a session in the focused pane", SOFT));
            return out;
        };
        let Some(s) = self.active_row() else {
            out.push(pair(
                &format!(
                    "● {} · {}",
                    current.client,
                    current.id.chars().take(12).collect::<String>()
                ),
                "— tok",
                width,
                METRIC,
            ));
            out.push(line("Waiting for local session log…", SOFT));
            return out;
        };
        let project = std::path::Path::new(&s.project)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        out.push(line(
            clipped(
                &format!(
                    "● {} / {}",
                    s.client,
                    if project.is_empty() {
                        "Unknown project"
                    } else {
                        &project
                    }
                ),
                width.into(),
            ),
            METRIC,
        ));
        out.push(pair(
            "Session",
            s.id.chars().take(12).collect::<String>(),
            width,
            SOFT,
        ));
        if !s.tokens.known {
            out.push(line("Token usage unavailable in local log", SOFT));
            return out;
        }
        out.extend(token_digits(&short(s.tokens.total()), METRIC));
        if s.incomplete {
            out.push(line("+? partial token log", GOLD));
        }
        out.push(compact_token_meter(s.tokens.input, s.tokens.output, width));
        out.push(compact_cache_meter(
            s.tokens.read,
            s.tokens.write,
            s.tokens.cache_reuse_percent(),
            s.tokens.cache_known,
            width,
        ));
        out
    }
    pub(super) fn active_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.source_pane.is_none() {
            return vec![];
        }
        let mut out = vec![section("SESSION / ALL TIME", width)];
        let Some(current) = &self.active_session else {
            out.push(line("◌ Waiting for agent session ID", SOFT));
            out.push(line(
                clipped("Open or resume a session in the focused pane", width.into()),
                SOFT,
            ));
            return out;
        };
        let Some(s) = self.active_row() else {
            out.push(pair(
                &format!(
                    "● {} · {}",
                    current.client,
                    current.id.chars().take(12).collect::<String>()
                ),
                "— tok",
                width,
                METRIC,
            ));
            out.push(line(
                clipped("Waiting for local session log…", width.into()),
                SOFT,
            ));
            return out;
        };
        let project = std::path::Path::new(&s.project)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        out.push(line(
            clipped(
                &format!(
                    "● {} / {}",
                    s.client,
                    if project.is_empty() {
                        "Unknown project"
                    } else {
                        &project
                    }
                ),
                width.into(),
            ),
            METRIC,
        ));
        out.push(pair(
            "Session",
            s.id.chars().take(12).collect::<String>(),
            width,
            SOFT,
        ));
        if !s.tokens.known {
            out.push(line(
                clipped("Token usage unavailable in local log", width.into()),
                SOFT,
            ));
            return out;
        }
        let suffix = if s.incomplete { "+?" } else { "" };
        out.extend(token_digits(&short(s.tokens.total()), METRIC));
        if !suffix.is_empty() {
            out.push(line("+? partial token log", GOLD));
        }
        let input = short(s.tokens.input);
        let output = short(s.tokens.output);
        out.push(duo_line(
            width,
            ("↑ Input", "↑", &input, BLUE),
            ("↓ Output", "↓", &output, METRIC),
        ));
        let read = if s.tokens.cache_known {
            short(s.tokens.read)
        } else {
            "—".into()
        };
        let write = if s.tokens.cache_known {
            short(s.tokens.write)
        } else {
            "—".into()
        };
        out.push(duo_line(
            width,
            ("↺ Read", "R", &read, METRIC),
            ("Write", "W", &write, SOFT),
        ));
        out.push(pair(
            "Cache reuse",
            s.tokens
                .cache_reuse_percent()
                .map_or("—".into(), |rate| format!("{rate:.1}%")),
            width,
            METRIC,
        ));
        out
    }
    pub(super) fn stats_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.page == config::PulseStartPage::Sessions {
            return self.session_content(width);
        }
        if self.page == config::PulseStartPage::Charts && !self.help {
            return self.chart_content(width);
        }
        if self.help {
            return help_content(
                if self.page == config::PulseStartPage::Charts {
                    HelpPage::Charts
                } else {
                    HelpPage::Usage
                },
                self.help_language,
                width,
            );
        }
        self.home_stats_content(width, false)
    }
    fn home_stats_content(&self, width: u16, mini: bool) -> Vec<Line<'static>> {
        let active = if self.client == 2 {
            self.grok_tokens(width)
        } else if mini {
            self.mini_active_content(width)
        } else if self.visual_mode {
            self.visual_active_content(width)
        } else {
            self.active_content(width)
        };
        if self.subscription_mode() {
            return active;
        }
        let metrics = metrics(&self.snapshot, self.client());
        let mut out = self.gateway_summary(width, &metrics.total, mini);
        out.extend(active);
        if self.gateway_placeholder().is_none() {
            out.extend(self.gateway_details(width, &metrics, mini));
        }
        out
    }
    fn mini_active_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.source_pane.is_none() {
            return vec![];
        }
        let mut out = vec![mini_line("SESSION / ALL TIME", width, METRIC)];
        let Some(active) = self.active_row() else {
            out.push(mini_line(
                if self.active_session.is_none() {
                    "Waiting for agent session ID…"
                } else {
                    "Waiting for local session log…"
                },
                width,
                SOFT,
            ));
            return out;
        };
        out.push(mini_line(
            format!("● {} session", active.client),
            width,
            GREEN,
        ));
        let total = if active.tokens.known {
            short(active.tokens.total())
        } else {
            "—".into()
        };
        out.extend(mini_token_total(&total, width, METRIC));
        if active.tokens.known {
            out.push(mini_line(
                format!(
                    "↑ {}  ↓ {}",
                    short(active.tokens.input),
                    short(active.tokens.output)
                ),
                width,
                SOFT,
            ));
            out.push(mini_line(
                format!(
                    "↺ R {}  W {}",
                    short(active.tokens.read),
                    short(active.tokens.write)
                ),
                width,
                SOFT,
            ));
        } else {
            out.push(mini_line("Session tokens unavailable", width, GOLD));
        }
        out
    }
    fn gateway_summary(&self, width: u16, t: &Totals, mini: bool) -> Vec<Line<'static>> {
        let mut out = if mini {
            vec![
                mini_line("GATEWAY / TODAY", width, BLUE),
                mini_line(self.snapshot.today(), width, SOFT),
            ]
        } else {
            vec![section_meta(
                "GATEWAY TOKENS",
                &self.snapshot.today(),
                width,
            )]
        };
        if let Some((message, color)) = self.gateway_placeholder() {
            out.push(if mini {
                mini_line(message, width, color)
            } else {
                line(message, color)
            });
            return out;
        }
        let unknown = t.calls > 0 && t.unknown == t.calls;
        let value = if unknown {
            "—".into()
        } else {
            short(t.input + t.output)
        };
        out.extend(if mini {
            mini_token_total(&value, width, BLUE)
        } else {
            token_digits(&value, BLUE)
        });
        if self.visual_mode {
            out.push(if unknown {
                line("I/O ░░░░░░░░░░ I ? O ?", SOFT)
            } else {
                compact_token_meter(t.input, t.output, width)
            });
        } else {
            let input = if unknown {
                "—".into()
            } else {
                short(t.input)
            };
            let output = if unknown {
                "—".into()
            } else {
                short(t.output)
            };
            out.push(duo_line(
                width,
                ("↑ Input", "↑", &input, BLUE),
                ("↓ Output", "↓", &output, METRIC),
            ));
        }
        out.push(pair(
            if self.visual_mode {
                "● Calls / unknown"
            } else {
                "REQUESTS"
            },
            if self.visual_mode {
                format!("{} / {}", t.calls, t.unknown)
            } else {
                t.calls.to_string()
            },
            width,
            INK,
        ));
        if !self.visual_mode && t.unknown > 0 {
            out.push(line(
                format!(
                    "! {} {} lack token data",
                    t.unknown,
                    if t.unknown == 1 { "call" } else { "calls" }
                ),
                GOLD,
            ));
        }
        out
    }
    fn gateway_details(&self, width: u16, m: &Metrics, mini: bool) -> Vec<Line<'static>> {
        let t = &m.total;
        let unknown = t.calls > 0 && t.unknown == t.calls;
        let mut out = vec![];
        if self.visual_mode {
            out.extend(gateway_cache_meter(t, unknown, width));
            out.push(if mini {
                mini_speed_line(t, width)
            } else {
                speed_pair("↗ Rate", t, width)
            });
            out.push(line(format!("{} measured streams", t.speed_samples), SOFT));
        } else {
            let read = if unknown {
                "—".into()
            } else {
                short(t.cache_read)
            };
            let write = if unknown {
                "—".into()
            } else {
                short(t.cache_write)
            };
            out.push(duo_line(
                width,
                ("↺ Read", "R", &read, GREEN),
                ("Write", "W", &write, SOFT),
            ));
            out.push(pair(
                "Cache hit",
                if t.cache_input > 0 {
                    format!("{:.1}%", 100.0 * t.cache_hits as f64 / t.cache_input as f64)
                } else {
                    "—".into()
                },
                width,
                BLUE,
            ));
            out.push(if mini {
                mini_speed_line(t, width)
            } else {
                speed_pair("Output rate (E2E)", t, width)
            });
            out.push(line(
                format!("  ↳ {} measured streams", t.speed_samples),
                SOFT,
            ));
        }
        out.push(section("CALL HEALTH", width));
        let health = rate(t);
        let color = if t.failed + t.interrupted > 0 {
            GOLD
        } else {
            GREEN
        };
        out.push(pair(
            "Success rate",
            health.map_or("— no samples".into(), |r| format!("{r:.1}%")),
            width,
            if health.is_some() { color } else { SOFT },
        ));
        if self.visual_mode {
            out.push(health_meter(t, width));
        } else {
            out.push(Line::from(progress_spans(
                health.map(|r| r / 100.0),
                width.into(),
                color,
            )));
        }
        out.push(line(
            format!(
                "✓ {} · × {} · ! {} · ◌ {}",
                t.success, t.failed, t.interrupted, t.pending
            ),
            INK,
        ));
        out.push(pair("↘ Compaction", m.compact.to_string(), width, SOFT));
        out.push(if self.models {
            section_action("MODELS / TODAY", "[Providers m]", width)
        } else {
            section_action("PROVIDERS / TODAY", "[Models m]", width)
        });
        let mut entries: Vec<(String, &Totals)> = if self.models {
            m.models
                .iter()
                .map(|(name, t)| {
                    (
                        if name.is_empty() {
                            "Unknown model".into()
                        } else {
                            name.clone()
                        },
                        t,
                    )
                })
                .collect()
        } else {
            m.providers
                .values()
                .map(|(name, t)| (name.clone(), t))
                .collect()
        };
        entries.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(&b.0)));
        if entries.is_empty() {
            out.push(line("No tracked requests today", SOFT));
            out.push(line("Waiting for gateway traffic…", SOFT));
        }
        for (name, t) in entries {
            out.push(line(name, INK));
            out.push(pair(
                &format!("{} calls · {} tok", t.calls, token_label(t)),
                format!("× {}", t.failed + t.interrupted),
                width,
                if t.failed + t.interrupted > 0 {
                    RED
                } else {
                    SOFT
                },
            ));
        }
        if mini {
            out.push(mini_line("CALLS / EACH HOUR", width, BLUE));
            out.extend(mini_hourly_rows(&m.hours, width, BLUE));
        }
        out
    }
    pub(super) fn session_rows(&self) -> Vec<&crate::sessions::Session> {
        let mut rows: Vec<_> = self
            .sessions
            .rows
            .iter()
            .filter(|s| self.client().is_none_or(|c| s.client == c))
            .collect();
        rows.sort_by(|a, b| {
            let active = |s: &crate::sessions::Session| {
                self.active_session
                    .as_ref()
                    .is_some_and(|current| s.client == current.client && s.id == current.id)
            };
            active(b).cmp(&active(a)).then_with(|| {
                if self.sessions_sort_tokens {
                    b.tokens
                        .known
                        .cmp(&a.tokens.known)
                        .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
                        .then_with(|| b.updated.cmp(&a.updated))
                } else {
                    b.updated.cmp(&a.updated)
                }
                .then_with(|| a.id.cmp(&b.id))
            })
        });
        rows
    }
    pub(super) fn visual_session_content(&self, width: u16) -> Vec<Line<'static>> {
        let rows = self.session_rows();
        let history_count = rows
            .iter()
            .filter(|s| {
                !self
                    .active_session
                    .as_ref()
                    .is_some_and(|current| s.client == current.client && s.id == current.id)
            })
            .count();
        let mut out = self.visual_active_content(width);
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.push(section("SESSION HISTORY", width));
        out.push(pair(
            "Earlier sessions",
            history_count.to_string(),
            width,
            BLUE,
        ));
        out.push(line(
            if self.sessions_sort_tokens {
                "Sorted by tokens · t: recent"
            } else {
                "Sorted by recent · t: tokens"
            },
            SOFT,
        ));
        if self.sessions_refreshed.is_none() {
            out.push(line("Reading local session logs…", SOFT));
        } else if rows.is_empty() {
            out.push(line("No local sessions for this client", SOFT));
        } else if history_count == 0 {
            out.push(line("No other sessions for this client", SOFT));
        }
        let zone = chrono::FixedOffset::east_opt(self.snapshot.offset)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap());
        for s in rows {
            if self
                .active_session
                .as_ref()
                .is_some_and(|current| s.client == current.client && s.id == current.id)
            {
                continue;
            }
            let project = std::path::Path::new(&s.project)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let id: String = s.id.chars().take(8).collect();
            let detail = format!(
                "{project} · {id}{}{}",
                if s.child { " [child]" } else { "" },
                if s.fork { " *" } else { "" }
            );
            out.push(Line::default());
            out.push(pair(
                &detail,
                format!(
                    "{} tok",
                    if s.tokens.known {
                        format!(
                            "{}{}",
                            short(s.tokens.total()),
                            if s.incomplete { "+?" } else { "" }
                        )
                    } else {
                        "?".into()
                    }
                ),
                width,
                BLUE,
            ));
            let time = chrono::DateTime::from_timestamp(s.updated, 0)
                .map(|t| t.with_timezone(&zone).format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "time ?".into());
            out.push(pair(s.client, time, width, SOFT));
            if s.tokens.known {
                out.push(compact_token_meter(s.tokens.input, s.tokens.output, width));
                out.push(compact_cache_meter(
                    s.tokens.read,
                    s.tokens.write,
                    s.tokens.cache_reuse_percent(),
                    s.tokens.cache_known,
                    width,
                ));
            } else {
                out.push(line("In ? · Out ? · Cache — · R ? / W ?", SOFT));
            }
        }
        if self.sessions.warnings > 0 {
            if history_count > 0 {
                out.push(Line::default());
            }
            out.push(line(
                format!("! {} logs unavailable / partial", self.sessions.warnings),
                GOLD,
            ));
        }
        out
    }
    pub(super) fn session_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.help {
            return help_content(HelpPage::Sessions, self.help_language, width);
        }
        if self.visual_mode {
            return self.visual_session_content(width);
        }
        let rows = self.session_rows();
        let history_count = rows
            .iter()
            .filter(|s| {
                !self
                    .active_session
                    .as_ref()
                    .is_some_and(|current| s.client == current.client && s.id == current.id)
            })
            .count();
        let mut out = self.active_content(width);
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend([
            section("SESSION HISTORY", width),
            pair("Earlier sessions", history_count.to_string(), width, BLUE),
            line(
                if self.sessions_sort_tokens {
                    "Sorted by tokens · t: recent"
                } else {
                    "Sorted by recent · t: tokens"
                },
                SOFT,
            ),
        ]);
        if self.sessions_refreshed.is_none() {
            out.push(line("Reading local session logs…", SOFT));
        } else if rows.is_empty() {
            out.push(line("No local sessions for this client", SOFT));
        } else if history_count == 0 {
            out.push(line("No other sessions for this client", SOFT));
        }
        let format_tokens = |s: &crate::sessions::Session, n| {
            if !s.tokens.known {
                "?".into()
            } else {
                format!("{}{}", short(n), if s.incomplete { "+?" } else { "" })
            }
        };
        let zone = chrono::FixedOffset::east_opt(self.snapshot.offset)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap());
        for s in rows {
            if self
                .active_session
                .as_ref()
                .is_some_and(|current| s.client == current.client && s.id == current.id)
            {
                continue;
            }
            out.push(Line::default());
            let project = std::path::Path::new(&s.project)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let id: String = s.id.chars().take(8).collect();
            let detail = format!(
                "{project} · {id}{}{}",
                if s.child { " [child]" } else { "" },
                if s.fork { " *" } else { "" }
            );
            out.push(line(clipped(&detail, width.into()), INK));
            out.push(pair(
                s.client,
                format!("{} tok", format_tokens(s, s.tokens.total())),
                width,
                BLUE,
            ));
            out.push(line(
                format!(
                    "In {} · Out {}",
                    format_tokens(s, s.tokens.input),
                    format_tokens(s, s.tokens.output)
                ),
                SOFT,
            ));
            let time = chrono::DateTime::from_timestamp(s.updated, 0)
                .map(|t| t.with_timezone(&zone).format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "time ?".into());
            let cache = format!(
                "↺ {} · {} read",
                s.tokens
                    .cache_reuse_percent()
                    .map_or("—".into(), |rate| format!("{rate:.1}%")),
                if s.tokens.cache_known {
                    format_tokens(s, s.tokens.read)
                } else {
                    "—".into()
                }
            );
            out.push(pair(&cache, time, width, SOFT));
        }
        if self.sessions.warnings > 0 {
            if history_count > 0 {
                out.push(Line::default());
            }
            out.push(line(
                format!("! {} logs unavailable / partial", self.sessions.warnings),
                GOLD,
            ));
        }
        out
    }
    pub(super) fn draw(&mut self, f: &mut ratatui::Frame) {
        if self.page == config::PulseStartPage::Git {
            self.git.draw(f);
        } else {
            self.draw_content(f);
        }
    }
    pub(super) fn account_body(&self, screen: Rect) -> Rect {
        let mini = screen.width < 32 || screen.height < 12;
        let area = if mini {
            screen
        } else {
            screen.inner(Margin::new(2, 0))
        };
        let body = if mini {
            mini_body(area)
        } else {
            content_body(area)
        };
        let x = body.x.min(screen.right());
        let y = body.y.min(screen.bottom());
        Rect::new(
            x,
            y,
            body.right().min(screen.right()).saturating_sub(x),
            body.bottom().min(screen.bottom()).saturating_sub(y),
        )
    }
    pub(super) fn account_hit(&self, body: Rect, x: u16, y: u16) -> bool {
        matches!(self.client, 1 | 2)
            && !self.help
            && self.page != config::PulseStartPage::Sessions
            && self.page != config::PulseStartPage::Charts
            && contains(body, x, y)
            && usize::from(y - body.y) + usize::from(self.scroll) == 1
    }
    fn draw_content(&mut self, f: &mut ratatui::Frame) {
        if self.help {
            self.draw_help_page(f);
            return;
        }
        let area = f.area();
        f.render_widget(
            Block::default().style(Style::default().bg(BG).fg(INK)),
            area,
        );
        if area.width < 32 || area.height < 12 {
            self.draw_mini(f, area);
            return;
        }
        let inner = area.inner(Margin::new(2, 0));
        self.draw_token_header(f, inner, false);
        f.render_widget(
            Paragraph::new(if self.visual_mode { "T(v)" } else { "V(v)" })
                .style(Style::default().fg(BLUE).add_modifier(Modifier::BOLD)),
            Rect::new(inner.right() - 9, inner.y, 4, 1),
        );
        f.render_widget(
            Paragraph::new("Help").style(Style::default().fg(SOFT)),
            pulse_help_rect(inner),
        );
        let tabs = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            1,
        ));
        for (i, name) in CLIENTS.into_iter().enumerate() {
            f.render_widget(
                Paragraph::new(if i == 2 && tabs[i].width < 8 {
                    "Grok"
                } else {
                    name
                })
                .alignment(Alignment::Center)
                .style(
                    Style::default()
                        .fg(if self.client == i { BG } else { SOFT })
                        .bg(if self.client == i { BLUE } else { BG }),
                ),
                tabs[i],
            );
        }
        let body = content_body(inner);
        let content = self.content(body.width);
        self.limit = (content.len() as u16).saturating_sub(body.height);
        self.scroll = self.scroll.min(self.limit);
        f.render_widget(Paragraph::new(content).scroll((self.scroll, 0)), body);
        if self.limit > 0 {
            let mut state = scroll_state(
                usize::from(self.limit) + usize::from(body.height),
                usize::from(self.scroll),
                usize::from(body.height),
            );
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(Style::default().fg(BLUE))
                    .track_style(Style::default().fg(RAIL)),
                Rect::new(inner.right() - 1, body.y, 1, body.height),
                &mut state,
            );
        }
        let (status, color) = self.page_status();
        f.render_widget(
            Paragraph::new(clipped(&status, inner.width.into())).style(Style::default().fg(color)),
            Rect::new(inner.x, area.bottom() - 2, inner.width, 1),
        );
        for button in footer_buttons(inner, self.page, self.sessions_sort_tokens) {
            f.render_widget(
                Paragraph::new(button.label)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(BLUE).bg(RAIL)),
                button.rect,
            );
        }
    }
}
