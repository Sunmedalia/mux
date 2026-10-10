use super::*;

impl Monitor {
    fn subscription_mode(&self) -> bool {
        match self.client {
            1 => self.accounts.codex.subscription,
            2 => self.accounts.grok.subscription,
            _ => false,
        }
    }
    pub(super) fn header_switch_rect(&self, area: Rect, mini: bool) -> Rect {
        page_header_rects(
            area,
            if mini { 4 } else { 9 },
            self.page == config::PulseStartPage::Git,
        )[1]
    }
    fn draw_token_header(&self, f: &mut ratatui::Frame, area: Rect, mini: bool) {
        let reserve = if mini { 4 } else { 9 };
        draw_page_header(f, area, reserve, false, self.picker.is_none());
        let suffix = if self.help {
            "HELP"
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
    pub(super) fn grok_gateway_tokens(&self, width: u16) -> Vec<Line<'static>> {
        if !self.visual_mode {
            let mut out = self.stats_content(width);
            if let Some(title) = out.first_mut() {
                *title = section_meta("GATEWAY TOKENS", &self.snapshot.today(), width);
            }
            if self.refreshed.is_some() && metrics(&self.snapshot, Some("Grok")).total.calls == 0 {
                out.push(line(
                    "Use a Mux API model via /model to record Gateway usage",
                    SOFT,
                ));
            }
            return out;
        }
        let totals = metrics(&self.snapshot, Some("Grok")).total;
        let ready = self.refreshed.is_some();
        let unknown = !ready || (totals.calls > 0 && totals.unknown == totals.calls);
        let value = if unknown {
            "—".into()
        } else {
            short(totals.input + totals.output)
        };
        let mut out = vec![section_meta(
            "GATEWAY TOKENS",
            &self.snapshot.today(),
            width,
        )];
        out.extend(if width < 30 {
            mini_token_total(&value, width, BLUE)
        } else {
            token_digits(&value, BLUE)
        });
        let input = if unknown {
            "—".into()
        } else {
            short(totals.input)
        };
        let output = if unknown {
            "—".into()
        } else {
            short(totals.output)
        };
        out.push(duo_line(
            width,
            ("↑ Input", "↑", &input, BLUE),
            ("↓ Output", "↓", &output, METRIC),
        ));
        if ready {
            out.extend(gateway_cache_meter(&totals, unknown, width));
            out.push(pair("Requests", totals.calls.to_string(), width, INK));
            out.push(speed_pair("↗ Rate", &totals, width));
            out.push(line(
                format!("{} measured streams", totals.speed_samples),
                SOFT,
            ));
            out.push(section("CALL HEALTH", width));
            out.push(health_meter(&totals, width));
            out.push(pair(
                "Success rate",
                rate(&totals).map_or("— no samples".into(), |n| format!("{n:.1}%")),
                width,
                GREEN,
            ));
            out.push(line(
                format!(
                    "✓ {}   × {}   ! {}   ◌ {}",
                    totals.success, totals.failed, totals.interrupted, totals.pending
                ),
                INK,
            ));
            if totals.unknown > 0 {
                out.push(pair(
                    "Unknown tokens",
                    totals.unknown.to_string(),
                    width,
                    GOLD,
                ));
            }
            if totals.calls == 0 {
                out.push(line("○ No gateway traffic", SOFT));
                out.push(line("Select a Mux API model with /model", SOFT));
            }
        } else {
            out.push(line("◌ Loading gateway…", SOFT));
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
        if self.client == 2 {
            if self.page == config::PulseStartPage::Sessions {
                return self.session_content(width);
            }
            if self.page == config::PulseStartPage::Charts {
                return self.chart_content(width);
            }
            let mut out = self.account_content(width);
            if !self.subscription_mode() {
                out.extend(self.grok_gateway_tokens(width));
            }
            out.extend(self.grok_tokens(width));
            return out;
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
        if self.client == 2 {
            return self.content(width);
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
            let mut out = vec![
                mini_line("↑ input  ↓ output", width, BLUE),
                mini_line("↺ cache · R read · W write", width, SOFT),
            ];
            out.extend(
                self.content(width)
                    .into_iter()
                    .map(|l| mini_line(l.to_string(), width, SOFT)),
            );
            return out;
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
            let gateway = metrics(&self.snapshot, self.client());
            let session = self.session_hours_today();
            let mut out = vec![
                mini_line("CHARTS / TODAY", width, BLUE),
                mini_line(format!("Calls  {}", gateway.total.calls), width, INK),
                mini_line(mini_sparkline(&gateway.hours), width, BLUE),
                mini_line("00   06   12   18   24", width, SOFT),
                mini_line(
                    format!("Session  {} tok", short(session.iter().sum())),
                    width,
                    GREEN,
                ),
                mini_line(mini_sparkline(&session), width, GREEN),
            ];
            out.push(mini_line("GATEWAY / HOURLY CALLS", width, BLUE));
            out.extend(mini_hourly_rows(&gateway.hours, width, BLUE));
            out.push(mini_line("SESSION / HOURLY TOKENS", width, GREEN));
            out.extend(mini_hourly_rows(&session, width, GREEN));
            return out;
        }
        if self.subscription_mode() {
            return if self.visual_mode {
                self.visual_active_content(width)
            } else {
                self.active_content(width)
            };
        }
        let m = metrics(&self.snapshot, self.client());
        let t = &m.total;
        let unknown = t.calls > 0 && t.unknown == t.calls;
        let total = if self.refreshed.is_none() || unknown {
            "—".into()
        } else {
            short(t.input + t.output)
        };
        let mut out = vec![mini_line(
            format!("TODAY  {}", self.snapshot.today()),
            width,
            BLUE,
        )];
        out.extend(mini_token_total(&total, width, BLUE));
        out.extend([
            mini_line(format!("{} calls", t.calls), width, SOFT),
            mini_line(
                format!(
                    "↑ {}  ↓ {}",
                    if unknown { "?".into() } else { short(t.input) },
                    if unknown { "?".into() } else { short(t.output) }
                ),
                width,
                SOFT,
            ),
            mini_line(
                format!(
                    "OK {}  ×{}  !{}",
                    rate(t).map_or("—".into(), |r| format!("{r:.0}%")),
                    t.failed,
                    t.interrupted
                ),
                width,
                GREEN,
            ),
        ]);
        if let Some(active) = self.active_row() {
            out.push(mini_line("CURRENT SESSION", width, METRIC));
            out.push(mini_line(
                format!("● {} session", active.client),
                width,
                GREEN,
            ));
            let total = if active.tokens.known {
                short(active.tokens.total())
            } else {
                "?".into()
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
            }
        }
        let entries: Vec<(String, &Totals)> = if self.models {
            m.models.iter().map(|(name, t)| (name.clone(), t)).collect()
        } else {
            m.providers
                .values()
                .map(|(name, t)| (name.clone(), t))
                .collect()
        };
        out.push(mini_line(
            if self.models {
                "MODELS · m switch"
            } else {
                "PROVIDERS · m switch"
            },
            width,
            BLUE,
        ));
        if entries.is_empty() {
            out.push(mini_line("No tracked requests", width, SOFT));
        }
        for (name, totals) in entries {
            out.push(mini_line(
                format!("{}  {} calls", name, totals.calls),
                width,
                INK,
            ));
            out.push(mini_line(
                format!(
                    "  {} tok  ×{}",
                    token_label(totals),
                    totals.failed + totals.interrupted
                ),
                width,
                SOFT,
            ));
        }
        out.push(mini_line("GATEWAY / DETAIL", width, BLUE));
        out.push(mini_line(
            format!(
                "↺ R {}  W {}",
                if unknown {
                    "?".into()
                } else {
                    short(t.cache_read)
                },
                if unknown {
                    "?".into()
                } else {
                    short(t.cache_write)
                }
            ),
            width,
            SOFT,
        ));
        out.push(mini_line(
            format!(
                "↺ hit {}",
                if t.cache_input > 0 {
                    format!("{:.1}%", 100.0 * t.cache_hits as f64 / t.cache_input as f64)
                } else {
                    "—".into()
                }
            ),
            width,
            SOFT,
        ));
        out.push(mini_speed_line(t, width));
        out.push(mini_line(
            format!(
                "✓{}  ×{}  !{}  ◌{}",
                t.success, t.failed, t.interrupted, t.pending
            ),
            width,
            GREEN,
        ));
        out.push(mini_line(format!("Compactions {}", m.compact), width, SOFT));
        if t.unknown > 0 {
            out.push(mini_line(
                format!("Unknown tokens: {} calls", t.unknown),
                width,
                GOLD,
            ));
        }
        out.push(mini_line("CALLS / EACH HOUR", width, BLUE));
        out.extend(mini_hourly_rows(&m.hours, width, BLUE));
        out
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
        if area.width >= 10 {
            f.render_widget(
                Paragraph::new("v ?").style(Style::default().fg(SOFT)),
                Rect::new(area.right().saturating_sub(3), area.y, 3, 1),
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
        let mut content = self.mini_content(body.width);
        if self.client != 2
            && !self.subscription_mode()
            && self.visual_mode
            && !self.help
            && self.page != config::PulseStartPage::Sessions
            && self.page != config::PulseStartPage::Charts
        {
            let totals = metrics(&self.snapshot, self.client()).total;
            content.insert(
                self.account_content(body.width).len() + 4,
                health_meter(&totals, body.width),
            );
        }
        self.limit = (content.len() as u16).saturating_sub(body.height);
        self.scroll = self.scroll.min(self.limit);
        f.render_widget(Paragraph::new(content).scroll((self.scroll, 0)), body);
        if area.height >= 4 {
            let status = if self.client == 2 && self.error.is_none() {
                "● Auto-update · r refresh".into()
            } else if let Some(error) = &self.error {
                format!("! {error}")
            } else if let Some(notice) = &self.notice {
                notice.clone()
            } else if self.page == config::PulseStartPage::Sessions
                && self.sessions_refreshed.is_none()
            {
                "◌ sessions".into()
            } else if self.page != config::PulseStartPage::Sessions && self.refreshed.is_none() {
                "◌ loading".into()
            } else if self.limit > 0 {
                "↑↓ scroll".into()
            } else {
                "● live".into()
            };
            f.render_widget(
                Paragraph::new(clipped(&status, area.width.into()))
                    .style(Style::default().fg(if self.error.is_some() { RED } else { SOFT })),
                Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 1),
            );
        }
        for (label, rect) in [
            "e",
            if self.page == config::PulseStartPage::Sessions {
                "t"
            } else {
                "c"
            },
            "s",
            "r",
            "q",
        ]
        .into_iter()
        .zip(mini_buttons(area))
        {
            f.render_widget(
                Paragraph::new(label)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(BLUE).bg(RAIL)),
                rect,
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
        let gateway = metrics(&self.snapshot, self.client());
        let session = self.session_hours_today();
        let mut out = vec![
            section("CHARTS / TODAY", width),
            line("Gateway calls + active session tokens", SOFT),
            Line::default(),
            section("GATEWAY / REQUESTS BY HOUR", width),
            pair("Calls today", gateway.total.calls.to_string(), width, BLUE),
        ];
        out.extend(hourly_chart(&gateway.hours, width, BLUE));
        out.extend([Line::default(), section("SESSION / TOKENS BY HOUR", width)]);
        if self.active_session.is_none() {
            out.push(line("Waiting for agent session ID…", SOFT));
        } else if self.active_row().is_none() {
            out.push(line("Waiting for local session log…", SOFT));
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
    pub(super) fn visual_home_content(&self, width: u16) -> Vec<Line<'static>> {
        if self.subscription_mode() {
            return self.visual_active_content(width);
        }
        let m = metrics(&self.snapshot, self.client());
        let t = &m.total;
        let mut out = vec![section_meta("TOKENS", &self.snapshot.today(), width)];
        if self.refreshed.is_none() {
            out.push(line("◌ Reading gateway usage…", SOFT));
            let active = self.visual_active_content(width);
            if !active.is_empty() {
                out.extend(active);
            }
            return out;
        }
        let unknown = t.calls > 0 && t.unknown == t.calls;
        let total = if unknown {
            "—".into()
        } else {
            short(t.input + t.output)
        };
        out.extend(token_digits(&total, BLUE));
        out.push(if unknown {
            line("I/O ░░░░░░░░░░ I ? O ?", SOFT)
        } else {
            compact_token_meter(t.input, t.output, width)
        });
        out.push(pair(
            "● Calls / unknown",
            format!("{} / {}", t.calls, t.unknown),
            width,
            INK,
        ));
        out.extend(gateway_cache_meter(t, unknown, width));
        out.push(speed_pair("↗ Rate", t, width));
        out.push(line(format!("{} measured streams", t.speed_samples), SOFT));
        out.push(section("CALL HEALTH", width));
        let health = rate(t);
        out.push(health_meter(t, width));
        out.push(pair(
            "Success rate",
            health.map_or("— no samples".into(), |n| format!("{n:.1}%")),
            width,
            GREEN,
        ));
        out.push(line(
            format!(
                "✓ {}   × {}   ! {}   ◌ {}",
                t.success, t.failed, t.interrupted, t.pending
            ),
            INK,
        ));
        out.push(pair("Compaction", m.compact.to_string(), width, SOFT));
        let active = self.visual_active_content(width);
        if !active.is_empty() {
            out.extend(active);
        }
        out.push(if self.models {
            section_action("MODELS / TODAY", "[Providers m]", width)
        } else {
            section_action("PROVIDERS / TODAY", "[Models m]", width)
        });
        let mut entries: Vec<(String, &Totals)> = if self.models {
            m.models
                .iter()
                .map(|(name, totals)| {
                    (
                        if name.is_empty() {
                            "Unknown model".into()
                        } else {
                            name.clone()
                        },
                        totals,
                    )
                })
                .collect()
        } else {
            m.providers
                .values()
                .map(|(name, totals)| (name.clone(), totals))
                .collect()
        };
        entries.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(&b.0)));
        if entries.is_empty() {
            out.push(line("No tracked requests today", SOFT));
            out.push(line("Waiting for gateway traffic…", SOFT));
        }
        for (name, totals) in entries {
            out.push(pair(&name, format!("{} calls", totals.calls), width, INK));
            out.push(pair(
                &format!("{} tok", token_label(totals)),
                format!("× {}", totals.failed + totals.interrupted),
                width,
                if totals.failed + totals.interrupted > 0 {
                    RED
                } else {
                    SOFT
                },
            ));
        }
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
            return vec![
                section("ABOUT THIS DATA", width),
                line("a: Codex / Grok accounts", BLUE),
                line("↑↓ select · Enter confirm", SOFT),
                Line::default(),
                line("Current session: local log", INK),
                line("for the focused agent pane.", INK),
                Line::default(),
                line("Today's gateway totals: only", SOFT),
                line("this Mux config's traffic.", SOFT),
                line("Direct API and subscription", SOFT),
                line("traffic aren't in that total.", SOFT),
                Line::default(),
                line("Success rate excludes pending", SOFT),
                line("requests; failures and", SOFT),
                line("interruptions count against it.", SOFT),
                Line::default(),
                line("Charts: hourly today; each", SOFT),
                line("series scales to its peak.", SOFT),
                line("Visual I/O: blue input,", SOFT),
                line("gold output; bars show share.", SOFT),
                line("HIT: cache hit share;", SOFT),
                line("R/W: cache read/write.", SOFT),
                line("Calls / unknown: requests", SOFT),
                line("with missing token counts.", SOFT),
                Line::default(),
                line("Cache hit = cached reads /", SOFT),
                line("all input in measured", SOFT),
                line("successful generation calls.", SOFT),
                line("Cache writes are not hits.", SOFT),
                line("Write uses upstream-reported", SOFT),
                line("cache creation tokens;", SOFT),
                line("cache misses aren't writes.", SOFT),
                Line::default(),
                line("Output rate: successful", SOFT),
                line("streams, all output tokens /", SOFT),
                line("request start to completion.", SOFT),
                line("Includes first-token wait,", SOFT),
                line("reasoning and network time.", SOFT),
                line("Weighted average today;", SOFT),
                line("not pure model decode speed.", SOFT),
                line("Old/unmeasured calls excluded.", SOFT),
                line("Speed colors (tok/s):", SOFT),
                line("<50 red · 50–99 purple", SOFT),
                line("100–199 blue · 200–299 green", SOFT),
                line("300+ rainbow", SOFT),
                Line::default(),
                line("? / Esc to return", BLUE),
                line("v: text / visual view", BLUE),
            ];
        }
        if self.visual_mode {
            return self.visual_home_content(width);
        }
        if self.subscription_mode() {
            return self.active_content(width);
        }
        let mut out = vec![];
        let m = metrics(&self.snapshot, self.client());
        let t = &m.total;
        let ready = self.refreshed.is_some();
        if !ready {
            out.push(section_meta("TOKENS", &self.snapshot.today(), width));
            out.push(line("◌ Reading gateway usage…", SOFT));
            let active = if self.client == 2 {
                vec![]
            } else {
                self.active_content(width)
            };
            if !active.is_empty() {
                out.extend(active);
            }
            return out;
        }
        let unknown = t.calls > 0 && t.unknown == t.calls;
        let value = if !ready || (t.calls > 0 && t.unknown == t.calls) {
            "—".into()
        } else {
            short(t.input + t.output)
        };
        out.extend([section_meta("TOKENS", &self.snapshot.today(), width)]);
        out.extend(token_digits(&value, BLUE));
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
        if t.unknown > 0 {
            out.push(line(
                format!(
                    "! {} {} lack token data",
                    t.unknown,
                    if t.unknown == 1 { "call" } else { "calls" }
                ),
                GOLD,
            ));
        }
        out.push(pair(
            "REQUESTS",
            if ready {
                t.calls.to_string()
            } else {
                "—".into()
            },
            width,
            INK,
        ));
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
        out.push(speed_pair("Output rate (E2E)", t, width));
        out.push(line(
            format!("  ↳ {} measured streams", t.speed_samples),
            SOFT,
        ));
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
        out.push(Line::from(progress_spans(
            health.map(|r| r / 100.0),
            width.into(),
            color,
        )));
        out.push(pair("✓ Success", t.success.to_string(), width, GREEN));
        out.push(pair(
            "× Failed",
            t.failed.to_string(),
            width,
            if t.failed + t.interrupted > 0 {
                RED
            } else {
                SOFT
            },
        ));
        out.push(pair(
            "! Interrupted",
            t.interrupted.to_string(),
            width,
            if t.interrupted > 0 { GOLD } else { SOFT },
        ));
        out.push(pair("◌ Pending", t.pending.to_string(), width, GOLD));
        out.push(pair("↘ Compaction", m.compact.to_string(), width, SOFT));
        let active = if self.client == 2 {
            vec![]
        } else {
            self.active_content(width)
        };
        if !active.is_empty() {
            out.extend(active);
        }
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
            return vec![
                section("ABOUT SESSIONS", width),
                line("Current: focused agent pane.", INK),
                line("Local Claude / Codex logs.", INK),
                line("Each row: whole session", SOFT),
                line("tokens, not today's usage.", SOFT),
                line("Input includes cached tokens.", SOFT),
                line("Cache reuse = read / input.", SOFT),
                line("Writes are not cache hits.", SOFT),
                line("Forks may include inherited", SOFT),
                line("tokens; children are separate.", SOFT),
                line("Current follows pane focus.", SOFT),
                line("Visual I/O: blue input,", SOFT),
                line("gold output; bars show share.", SOFT),
                line("s: Usage / Sessions", BLUE),
                line("Alt+1/2: Token/Git · g: Git · T: Token", BLUE),
                line("t: recent / tokens sort", BLUE),
                line("? / Esc to return", BLUE),
                line("v: text / visual view", BLUE),
            ];
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
            Paragraph::new("?(?)").style(Style::default().fg(SOFT)),
            Rect::new(inner.right() - 4, inner.y, 4, 1),
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
        let status = if self.page == config::PulseStartPage::Sessions {
            if self.sessions.warnings > 0 {
                format!("! {} logs unavailable · r retry", self.sessions.warnings)
            } else if let Some(time) = self.sessions_refreshed {
                format!(
                    "● Sessions updated {}s ago · on change",
                    time.elapsed().as_secs()
                )
            } else {
                "◌ Reading local sessions…".into()
            }
        } else if self.page == config::PulseStartPage::Charts {
            if self.sessions.warnings > 0 {
                format!(
                    "! {} session logs unavailable · r retry",
                    self.sessions.warnings
                )
            } else {
                "● Charts auto-update · c: Home".into()
            }
        } else if let Some(note) = &self.notice {
            note.clone()
        } else if self.client == 2 && self.error.is_none() {
            "● Auto-update · r refresh".into()
        } else if let Some(error) = &self.error {
            format!("! STALE · {error}")
        } else if let Some(time) = self.refreshed {
            format!("● Checked {}s ago · every 2s", time.elapsed().as_secs())
        } else {
            "◌ Reading local usage…".into()
        };
        f.render_widget(
            Paragraph::new(status).style(Style::default().fg(
                if ((self.page == config::PulseStartPage::Sessions
                    || self.page == config::PulseStartPage::Charts)
                    && self.sessions.warnings > 0)
                    || (self.page != config::PulseStartPage::Sessions && self.error.is_some())
                {
                    RED
                } else {
                    SOFT
                },
            )),
            Rect::new(inner.x, area.bottom() - 2, inner.width, 1),
        );
        for (label, rect) in [
            if inner.width >= 44 {
                "↗ Edit(e)"
            } else {
                "↗(e)"
            },
            if inner.width < 44 {
                if self.page == config::PulseStartPage::Sessions {
                    "T(t)"
                } else {
                    "C(c)"
                }
            } else if self.page == config::PulseStartPage::Sessions {
                if self.sessions_sort_tokens {
                    "Recent(t)"
                } else {
                    "Tokens(t)"
                }
            } else if self.page == config::PulseStartPage::Charts {
                "Home(c)"
            } else {
                "Chart(c)"
            },
            if inner.width < 36 {
                if self.page == config::PulseStartPage::Sessions {
                    "H(s)"
                } else {
                    "S(s)"
                }
            } else if inner.width < 44 {
                if self.page == config::PulseStartPage::Sessions {
                    "Home(s)"
                } else {
                    "Sess(s)"
                }
            } else if self.page == config::PulseStartPage::Sessions {
                "Home(s)"
            } else {
                "Sessions(s)"
            },
            "↻(r)",
            "×(q)",
        ]
        .into_iter()
        .zip(buttons(inner))
        {
            let compact;
            let label = if label.width() > usize::from(rect.width) {
                compact = label.find('(').map(|i| &label[i..]).unwrap_or(label);
                compact
            } else {
                label
            };
            f.render_widget(
                Paragraph::new(label)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(BLUE).bg(RAIL)),
                rect,
            );
        }
    }
}
