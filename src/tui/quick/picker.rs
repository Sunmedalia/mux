//! Explicit account switching; the selected client stays fixed while this view is open.
use super::*;

pub(super) struct Choice {
    id: String,
    name: String,
    email: String,
    active: bool,
}
pub(super) struct Picker {
    pub(super) client: usize,
    choices: Vec<Choice>,
    selected: usize,
    confirming: bool,
    busy: bool,
    pub(super) result: Option<String>,
}
pub(super) enum Action {
    None,
    Close,
    Switch(usize, String),
}
fn safe(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}
impl Picker {
    pub(super) fn load(paths: &AppPaths, client: usize) -> Result<Self> {
        anyhow::ensure!(matches!(client, 1 | 2), "Select Codex or Grok first");
        let active = if client == 1 {
            crate::codex::accounts::live_login()?.0
        } else {
            // Import the native login only after the user explicitly opens Accounts.
            crate::grok::accounts::capture(paths, &crate::grok::home()?)?
        };
        let config = config::load(&paths.config)?;
        let choices = if client == 1 {
            config
                .codex
                .accounts
                .into_iter()
                .map(|(id, account)| Choice {
                    active: active.as_deref() == Some(&id),
                    id,
                    name: safe(&account.name),
                    email: safe(&account.email),
                })
                .collect()
        } else {
            config
                .grok
                .accounts
                .into_iter()
                .map(|(id, account)| Choice {
                    active: active.as_deref() == Some(&id),
                    id,
                    name: safe(&account.name),
                    email: safe(account.email.as_deref().unwrap_or("")),
                })
                .collect()
        };
        Ok(Self::new(client, choices))
    }
    fn new(client: usize, choices: Vec<Choice>) -> Self {
        let selected = choices.iter().position(|c| c.active).unwrap_or(0);
        Self {
            client,
            choices,
            selected,
            confirming: false,
            busy: false,
            result: None,
        }
    }
    pub(super) fn key(&mut self, code: KeyCode) -> Action {
        if self.busy {
            return Action::None;
        }
        if self.result.is_some() {
            return if matches!(code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                Action::Close
            } else {
                Action::None
            };
        }
        if self.confirming {
            match code {
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.confirming = false;
                }
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(choice) = self.choices.get(self.selected) {
                        self.busy = true;
                        return Action::Switch(self.client, choice.id.clone());
                    }
                }
                _ => {}
            }
        } else {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => return Action::Close,
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.choices.len().saturating_sub(1))
                }
                KeyCode::Home => self.selected = 0,
                KeyCode::End => self.selected = self.choices.len().saturating_sub(1),
                KeyCode::Enter if !self.choices.is_empty() => self.confirming = true,
                _ => {}
            }
        }
        Action::None
    }
    fn start(&self) -> usize {
        self.selected.saturating_sub(11)
    }
    pub(super) fn selecting(&self) -> bool {
        !self.confirming && !self.busy && self.result.is_none()
    }
    pub(super) fn list_hit(&self, body: Rect, x: u16, y: u16, scroll: u16) -> bool {
        if !self.selecting() || !contains(body, x, y) {
            return false;
        }
        let row = usize::from(y - body.y) + usize::from(scroll);
        row >= 3 && row < 3 + self.choices.len().min(12)
    }
    pub(super) fn selected_row(&self) -> usize {
        if self.confirming || self.result.is_some() {
            2
        } else {
            3 + self.selected - self.start()
        }
    }
    pub(super) fn mouse(&mut self, event: MouseEvent, body: Rect, scroll: u16) -> Action {
        match event.kind {
            MouseEventKind::ScrollDown => return self.key(KeyCode::Down),
            MouseEventKind::ScrollUp => return self.key(KeyCode::Up),
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return Action::None,
        }
        let rows = self.content(body.width);
        let row = event.row.saturating_sub(body.y) as usize + usize::from(scroll);
        if !contains(body, event.column, event.row) || row < 2 || row >= 2 + rows.len() {
            return if self.busy {
                Action::None
            } else {
                Action::Close
            };
        }
        let row = row - 2;
        if row == rows.len().saturating_sub(2) {
            let code = if event.column < body.x + body.width / 2 {
                KeyCode::Enter
            } else {
                KeyCode::Esc
            };
            return self.key(code);
        }
        if !self.confirming && !self.busy && self.result.is_none() && row >= 1 {
            let index = self.start() + row - 1;
            if row <= self.choices.len().min(12) && index < self.choices.len() {
                self.selected = index;
                self.confirming = true;
            }
        }
        Action::None
    }
    pub(super) fn finish(&mut self, result: std::result::Result<String, String>) {
        self.busy = false;
        self.result = Some(match result {
            Ok(message) => format!("✓ {message}"),
            Err(error) => format!("! {error}"),
        });
    }
    pub(super) fn content(&self, width: u16) -> Vec<Line<'static>> {
        let mut out = vec![line(
            clipped(
                &format!("┌ {} ACCOUNTS", CLIENTS[self.client].to_uppercase()),
                width.into(),
            ),
            BLUE,
        )];
        let wrap = |text: &str, color| {
            let mut rows = Vec::new();
            let mut current = String::new();
            let mut used = 0;
            for ch in text.chars() {
                let size = ch.width().unwrap_or(0);
                if used + size > usize::from(width.max(1)) && !current.is_empty() {
                    rows.push(line(std::mem::take(&mut current), color));
                    used = 0;
                }
                current.push(ch);
                used += size;
            }
            if !current.is_empty() {
                rows.push(line(current, color));
            }
            rows
        };
        if let Some(message) = &self.result {
            out.extend(wrap(message, INK));
        } else if self.confirming {
            if self.busy {
                out.push(line("◌ Switching…", BLUE));
            }
            if let Some(choice) = self.choices.get(self.selected) {
                out.extend(wrap(&format!("Switch to {}?", choice.name), GOLD));
                out.extend(wrap(&choice.email, INK));
            }
            out.extend(wrap(if self.client == 1 {
                "Applies ChatGPT login and Codex configuration. Existing tasks may need a new session."
            } else {
                "Changes the local Grok login. Restart existing Grok sessions to use it."
            }, SOFT));
        } else if self.choices.is_empty() {
            out.extend(wrap(
                "No saved accounts. Use e → Accounts to add a login.",
                SOFT,
            ));
        } else {
            for (i, c) in self.choices.iter().enumerate().skip(self.start()).take(12) {
                out.push(line(
                    clipped(
                        &format!(
                            "{} {} {} · {}",
                            if i == self.selected { "›" } else { " " },
                            if c.active { "●" } else { "○" },
                            c.name,
                            c.email
                        ),
                        width.into(),
                    ),
                    if i == self.selected { BLUE } else { INK },
                ));
            }
        }
        let accept = if self.busy {
            "Switching…"
        } else if self.result.is_some() {
            "Close ↵"
        } else if self.confirming {
            "Confirm [y/↵]"
        } else {
            "Select ↵"
        };
        let left_width = usize::from(width / 2);
        let left = clipped(accept, left_width);
        let right = clipped("Back [Esc]", usize::from(width) - left_width);
        out.push(Line::from(vec![Span::styled(
            format!(
                "{left}{}{right}",
                " ".repeat(left_width.saturating_sub(left.width()))
            ),
            Style::default().fg(BLUE).bg(RAIL),
        )]));
        out.push(line("└", BLUE));
        out
    }
}
pub(super) fn switch(paths: &AppPaths, client: usize, id: &str) -> Result<String> {
    match client {
        1 => {
            crate::codex::accounts::verify_for_switch(paths, id)?;
            Ok(crate::codex::accounts::activate_and_sync(paths, id)?.into())
        }
        2 => {
            crate::grok::accounts::activate(paths, &crate::grok::home()?, id)?;
            Ok("Grok account switched · restart existing Grok sessions".into())
        }
        _ => anyhow::bail!("Unsupported account client"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn picker(client: usize) -> Picker {
        Picker::new(
            client,
            (0..20)
                .map(|i| Choice {
                    id: format!("id-{i}"),
                    name: format!("Account {i}"),
                    email: format!("user{i}@example.test"),
                    active: i == 1,
                })
                .collect(),
        )
    }
    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }
    #[test]
    fn selection_needs_confirmation_and_busy_blocks_duplicate_switches() {
        for client in [1, 2] {
            let mut p = picker(client);
            assert_eq!(p.selected, 1);
            assert!(matches!(p.key(KeyCode::Down), Action::None));
            assert!(matches!(p.key(KeyCode::Enter), Action::None));
            assert!(p.confirming);
            p.key(KeyCode::Esc);
            assert!(!p.confirming);
            p.key(KeyCode::Enter);
            assert!(
                matches!(p.key(KeyCode::Char('y')), Action::Switch(c, id) if c == client && id == "id-2")
            );
            assert!(matches!(p.key(KeyCode::Enter), Action::None));
            assert!(matches!(p.key(KeyCode::Esc), Action::None));
            p.finish(Err("Credentials expired; reopen Accounts".into()));
            assert!(!p.busy);
            assert!(p.result.as_ref().unwrap().contains("Credentials expired"));
            assert!(matches!(p.key(KeyCode::Esc), Action::Close));
        }
    }
    #[test]
    fn mouse_selects_from_scrolled_dropdown_and_requires_confirm_button() {
        let body = Rect::new(2, 5, 40, 30);
        let mut p = picker(2);
        p.key(KeyCode::End);
        let expected = p.start();
        let scroll = 2;
        assert!(matches!(
            p.mouse(click(body.x, body.y + 1), body, scroll),
            Action::None
        ));
        assert_eq!(p.selected, expected);
        assert!(p.confirming);
        let button_row = body.y + p.content(body.width).len() as u16 - 2;
        assert!(
            matches!(p.mouse(click(body.x, button_row), body, scroll), Action::Switch(2, id) if id == format!("id-{expected}"))
        );
        assert!(matches!(p.mouse(click(0, 0), body, 0), Action::None));
    }
    #[test]
    fn outside_click_closes_and_empty_accounts_cannot_switch() {
        let area = Rect::new(2, 5, 40, 12);
        let mut p = Picker::new(1, vec![]);
        assert!(matches!(p.key(KeyCode::Enter), Action::None));
        assert!(!p.confirming);
        assert!(matches!(p.mouse(click(0, 0), area, 0), Action::Close));
    }
    #[test]
    fn dropdown_anchors_to_account_and_preserves_background_at_all_sizes() {
        for (width, height) in [(0, 0), (1, 1), (8, 6), (20, 12), (32, 24), (48, 54)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut monitor = Monitor {
                client: 2,
                picker: Some(picker(2)),
                ..Default::default()
            };
            let area = monitor.account_body(Rect::new(0, 0, width, height));
            assert!(area.right() <= width && area.bottom() <= height);
            assert!(
                !monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            );
            terminal.draw(|f| monitor.draw(f)).unwrap();
            monitor.picker.as_mut().unwrap().key(KeyCode::Enter);
            terminal.draw(|f| monitor.draw(f)).unwrap();
            monitor
                .picker
                .as_mut()
                .unwrap()
                .finish(Ok("Account switched".into()));
            terminal.draw(|f| monitor.draw(f)).unwrap();
            if width == 48 {
                let buffer = terminal.backend().buffer();
                let first: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
                assert!(
                    first.contains("TOKEN") && first.contains("[Git(g)]"),
                    "Background header stays visible: {first}"
                );
            }
        }
    }
    #[test]
    fn inline_menu_pushes_all_following_account_content_down_and_collapses_cleanly() {
        for client in [1, 2] {
            for visual_mode in [false, true] {
                for width in [8, 20, 48] {
                    let mut monitor = Monitor {
                        client,
                        visual_mode,
                        ..Default::default()
                    };
                    let info = accounts::Info {
                        card: Some(accounts::Card {
                            name: "Account".into(),
                            email: "user@example.test".into(),
                            gauges: vec![("Weekly".into(), 25.0, "".into())],
                            ..Default::default()
                        }),
                        ..Default::default()
                    };
                    if client == 1 {
                        monitor.accounts.codex = info;
                    } else {
                        monitor.accounts.grok = info;
                    }
                    let collapsed = monitor.account_content(width);
                    monitor.picker = Some(picker(client));
                    let added = monitor.picker.as_ref().unwrap().content(width).len();
                    let expanded = monitor.account_content(width);
                    assert_eq!(expanded.len(), collapsed.len() + added);
                    assert_eq!(&expanded[2 + added..], &collapsed[2..]);
                    monitor.picker.as_mut().unwrap().key(KeyCode::Enter);
                    let confirming = monitor.account_content(width);
                    let added = monitor.picker.as_ref().unwrap().content(width).len();
                    assert_eq!(&confirming[2 + added..], &collapsed[2..]);
                    monitor.picker = None;
                    assert_eq!(monitor.account_content(width), collapsed);
                }
            }
        }
    }
    #[test]
    fn account_hit_tracks_scroll_and_ignores_other_views() {
        let mut monitor = Monitor {
            client: 1,
            ..Default::default()
        };
        let body = Rect::new(2, 4, 40, 30);
        assert!(monitor.account_hit(body, 3, 5));
        assert!(!monitor.account_hit(body, 3, 6));
        monitor.scroll = 1;
        assert!(monitor.account_hit(body, 3, 4));
        monitor.page = config::PulseStartPage::Charts;
        assert!(!monitor.account_hit(body, 3, 4));
        monitor.page = config::PulseStartPage::Home;
        monitor.client = 0;
        assert!(!monitor.account_hit(body, 3, 4));
    }
}
