mod dialogs;
mod form_mouse;
mod mouse;
mod navigation;
use super::*;

impl App {
    pub(super) fn event_loop(&mut self, terminal: &mut TuiTerminal) -> Result<()> {
        self.initialize_background();
        let mut closing = false;
        let mut redraw = true;
        loop {
            redraw |= self.poll_background();
            redraw |= self.poll_codex();
            redraw |= self.poll_grok_auth();
            redraw |= self.poll_usage();
            if redraw {
                terminal.draw(|frame| self.draw(frame))?;
                redraw = false;
            }
            if closing {
                self.cancel_grok_auth();
                if !self.background.sync_running
                    && !self.background.proxy_running
                    && self.background.queued_sync.is_none()
                    && !self.codex_ui.busy
                    && !self.grok_auth.busy
                {
                    return Ok(());
                }
                if self.status != "Finishing pending changes…" {
                    self.status = "Finishing pending changes…".into();
                    redraw = true;
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
                continue;
            }
            if !event::poll(std::time::Duration::from_millis(200))? {
                continue;
            }
            let input = event::read()?;
            redraw = true;
            if self.screen.width < 40 || self.screen.height < 12 {
                if matches!(input, Event::Key(key) if key.kind == event::KeyEventKind::Press && (key.code == KeyCode::Char('q') || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))))
                {
                    closing = true;
                }
                continue;
            }
            let result = match input {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => self.handle_key(key),
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    self.handle_mouse(mouse, Rect::new(0, 0, size.width, size.height))
                        .map(|action| action == MouseAction::Quit)
                }
                _ => Ok(false),
            };
            match result {
                Ok(quit) => closing = quit,
                Err(error) => {
                    self.reload_for_edit();
                    self.init_provider_editor();
                    self.set_error(format!("Could not save changes: {error:#}"));
                }
            }
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        self.provider_card_selected = false;
        let before = self.config.clone();
        let before_client = self.config_client();
        let result = self.handle_key_inner(key);
        let mut before_sync = before;
        before_sync.usage_refresh_secs = self.config.usage_refresh_secs;
        if self.settings_menu.is_none()
            && before_sync != self.config
            && before_client == self.config_client()
        {
            self.queue_sync(false, None);
            self.sync_pi_after_edit();
            self.sync_grok_after_edit();
        }
        result
    }
}
