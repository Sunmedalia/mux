//! Shared continuation menu and terminal handoff for the editor and Pulse.
use super::*;
use crate::sessions::{
    Session,
    takeover::{Client, Prepared},
};
use std::sync::mpsc;

#[derive(Default)]
pub(super) struct Ui {
    dialog: Option<Dialog>,
    worker: Option<mpsc::Receiver<std::result::Result<Prepared, String>>>,
}
struct Dialog {
    source: Session,
    selected: usize,
    available: [bool; 4],
    error: Option<String>,
    rows: [Rect; 4],
    launch: Rect,
    back: Rect,
}
impl Ui {
    pub fn open(&mut self, source: Session) {
        if self.active() {
            return;
        }
        let selected = Client::ALL
            .iter()
            .position(|c| c.name() == source.client)
            .unwrap_or(0);
        self.dialog = Some(Dialog {
            selected,
            available: Client::ALL.map(|c| c.executable().is_ok()),
            error: source
                .child
                .then(|| "Child-agent sessions cannot be taken over".into()),
            source,
            rows: [Rect::default(); 4],
            launch: Rect::default(),
            back: Rect::default(),
        });
    }
    pub fn active(&self) -> bool {
        self.dialog.is_some()
    }
    pub fn input(&mut self, input: &Event, state: &std::path::Path) -> bool {
        let Some(d) = &mut self.dialog else {
            return false;
        };
        if matches!(input, Event::Key(k) if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL))
        {
            return false;
        }
        if self.worker.is_some() {
            return true;
        }
        let mut choose = false;
        let mut close = false;
        match input {
            Event::Key(k) if k.kind == event::KeyEventKind::Press => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => close = true,
                KeyCode::Up | KeyCode::Char('k') => {
                    d.selected = (d.selected + 3) % 4;
                    d.error = None;
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    d.selected = (d.selected + 1) % 4;
                    d.error = None;
                }
                KeyCode::Enter => choose = true,
                _ => {}
            },
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                if let Some(i) = d.rows.iter().position(|r| contains(*r, m.column, m.row)) {
                    d.selected = i;
                    d.error = None;
                }
                choose = contains(d.launch, m.column, m.row);
                close = contains(d.back, m.column, m.row);
            }
            _ => {}
        }
        if choose && !d.source.child {
            if !d.available[d.selected] {
                d.error = Some(format!(
                    "{} is not installed in PATH",
                    Client::ALL[d.selected].program()
                ));
            } else {
                let source = d.source.clone();
                let target = Client::ALL[d.selected];
                let state = state.to_path_buf();
                let (send, receive) = mpsc::sync_channel(1);
                self.worker = Some(receive);
                d.error = None;
                std::thread::spawn(move || {
                    let result = std::panic::catch_unwind(|| {
                        crate::sessions::takeover::prepare(source, target, &state)
                    })
                    .map_err(|_| "Session preparation worker stopped".to_string())
                    .and_then(|r| r.map_err(|e| format!("{e:#}")));
                    let _ = send.send(result);
                });
            }
        }
        if close {
            self.dialog = None;
        }
        true
    }
    pub fn poll(&mut self, terminal: &mut TuiTerminal) -> Result<bool> {
        let Some(worker) = &self.worker else {
            return Ok(false);
        };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return Ok(false),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Session preparation worker stopped".into())
            }
        };
        self.worker = None;
        let result = match result {
            Ok(prepared) => {
                let local = !Prepared::in_herdr();
                if local {
                    reset_terminal();
                }
                let result = prepared.launch().map_err(|e| format!("{e:#}"));
                if local {
                    // Always restore the terminal, including launch and child failures.
                    enable_raw_mode()?;
                    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
                    terminal.autoresize()?;
                    terminal.clear()?;
                }
                result
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => self.dialog = None,
            Err(error) => {
                if let Some(d) = &mut self.dialog {
                    d.error = Some(error);
                }
            }
        }
        Ok(true)
    }
    pub fn draw(&mut self, f: &mut ratatui::Frame) {
        let Some(d) = &mut self.dialog else {
            return;
        };
        let screen = f.area();
        let width = screen.width.min(68);
        let height = screen.height.min(15);
        let area = Rect::new(
            screen.x + (screen.width - width) / 2,
            screen.y + (screen.height - height) / 2,
            width,
            height,
        );
        f.render_widget(Clear, area);
        let block = Block::default()
            .title(" AI 接管 / Take over ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ROUTE));
        let inner = block.inner(area);
        f.render_widget(block, area);
        d.rows = [Rect::default(); 4];
        d.launch = Rect::default();
        d.back = Rect::default();
        if inner.height < 7 {
            f.render_widget(Paragraph::new("Enlarge terminal · Esc back"), inner);
            return;
        }
        let title = session_browser::title(&d.source);
        f.render_widget(
            Paragraph::new(title).style(Style::default().fg(Color::White)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        for (i, client) in Client::ALL.iter().enumerate() {
            let row = Rect::new(inner.x, inner.y + 1 + i as u16, inner.width, 1);
            d.rows[i] = row;
            let action = if !d.available[i] {
                "not installed"
            } else if client.name() == d.source.client {
                "continue / 继续"
            } else {
                "handoff / 交接"
            };
            let text = format!(
                "{} {} · {}",
                if d.selected == i { "›" } else { " " },
                client.name(),
                action
            );
            f.render_widget(
                Paragraph::new(text).style(Style::default().fg(
                    if !d.available[i] || d.source.child {
                        MUTED
                    } else if d.selected == i {
                        ROUTE
                    } else {
                        Color::White
                    },
                )),
                row,
            );
        }
        let note = if self.worker.is_some() {
            "Preparing conversation…"
        } else if let Some(error) = &d.error {
            error
        } else if Client::ALL[d.selected].name() == d.source.client {
            "Resume original session; reuse an existing Herdr pane when identified."
        } else {
            "New session · full text snapshot · original session keeps running."
        };
        f.render_widget(
            Paragraph::new(note)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(if d.error.is_some() { ERROR } else { MUTED })),
            Rect::new(
                inner.x,
                inner.y + 5,
                inner.width,
                inner.height.saturating_sub(6),
            ),
        );
        let row = inner.bottom().saturating_sub(1);
        let launch_width = inner.width.min(18);
        d.launch = Rect::new(inner.x, row, launch_width, 1);
        d.back = Rect::new(
            inner.x + launch_width,
            row,
            inner.width.saturating_sub(launch_width),
            1,
        );
        let enabled = self.worker.is_none() && d.available[d.selected] && !d.source.child;
        f.render_widget(
            Paragraph::new("[Start Enter]").style(button_style(false, !enabled, false)),
            d.launch,
        );
        f.render_widget(
            Paragraph::new("[Back Esc]").style(button_style(false, self.worker.is_some(), false)),
            d.back,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_preserves_source_and_closes_without_launching_at_all_sizes() {
        for (width, height) in [(1, 1), (20, 8), (32, 12), (68, 15), (120, 36)] {
            let mut ui = Ui::default();
            ui.open(Session {
                client: "Codex",
                id: "selected-session".into(),
                project: "/project".into(),
                title: Some("中文 title".into()),
                ..Default::default()
            });
            assert_eq!(ui.dialog.as_ref().unwrap().selected, 2);
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| ui.draw(f)).unwrap();
            ui.input(
                &Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
                std::path::Path::new("/unused"),
            );
            assert_eq!(ui.dialog.as_ref().unwrap().selected, 3);
            assert_eq!(ui.dialog.as_ref().unwrap().source.id, "selected-session");
            ui.input(
                &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                std::path::Path::new("/unused"),
            );
            assert!(!ui.active());
            assert!(ui.worker.is_none());
        }
    }
    #[test]
    fn child_sessions_and_missing_clients_never_start_workers() {
        let mut ui = Ui::default();
        ui.open(Session {
            child: true,
            ..Default::default()
        });
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.input(&enter, std::path::Path::new("/unused"));
        assert!(ui.worker.is_none());
        ui.dialog.as_mut().unwrap().source.child = false;
        ui.dialog.as_mut().unwrap().available = [false; 4];
        ui.input(&enter, std::path::Path::new("/unused"));
        assert!(ui.worker.is_none());
        assert!(
            ui.dialog
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("not installed")
        );
    }
}
