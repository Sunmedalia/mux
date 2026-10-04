use super::*;

impl App {
    pub(in crate::tui) fn handle_key_inner(&mut self, key: KeyEvent) -> Result<bool> {
        if self.grok_auth.busy
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            self.cancel_grok_auth();
            return Ok(false);
        }
        if matches!(
            self.modal,
            Some(
                Modal::Appearance(_)
                    | Modal::Proxy(_)
                    | Modal::SettingsMenu(_)
                    | Modal::UiOptions(_)
                    | Modal::CodexSettings(_)
            )
        ) {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                self.back_one_level()?;
            } else if key.code == KeyCode::Char('?') {
                self.open_help();
            } else if matches!(self.modal, Some(Modal::Appearance(_)))
                && key.code == KeyCode::Char('P')
            {
                self.open_proxy_manager();
            } else if matches!(self.modal, Some(Modal::Proxy(_))) && key.code == KeyCode::F(4) {
                self.open_appearance();
            } else {
                self.handle_modal(key)?;
            }
            return Ok(false);
        }
        if self.modal.is_none() && key.code == KeyCode::F(4) {
            self.open_settings_menu();
            return Ok(false);
        }
        if self.usage.active {
            if self.modal.is_some() {
                self.handle_modal(key)?;
                return Ok(false);
            }
            return Ok(self.usage_key(key));
        }
        if self.modal.is_none() && key.code == KeyCode::F(6) {
            self.open_usage();
            return Ok(false);
        }
        if self.view_mode == ViewMode::AllEnabled
            && self.all_models_filter.active
            && self.modal.is_none()
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(true);
            }
            self.global_filter_key(key);
            return Ok(false);
        }
        if self.modal.is_none()
            && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
            && !self.codex_input_active()
            && !(self.view_mode == ViewMode::AllEnabled
                && !self.all_models_filter.query.is_empty()
                && key.code == KeyCode::Esc)
            && !self
                .provider_editor
                .as_ref()
                .is_some_and(|editor| editor.search_active)
            && !self
                .usage
                .page
                .as_ref()
                .is_some_and(|page| self.usage.active && page.session_searching)
            && !(self
                .grok_auth
                .page
                .as_ref()
                .is_some_and(|page| page.selected == 0)
                && key.code == KeyCode::Char('q'))
        {
            return self.back_one_level();
        }
        if self.grok_enabled && self.grok_auth.page.is_some() && self.modal.is_none() {
            if key.code == KeyCode::F(2) && !self.grok_auth.busy {
                self.select_client_tab(self.client_tab().next());
            } else if key.code == KeyCode::Char('?') {
                self.open_help();
            } else {
                self.grok_auth_page_key(key)?;
            }
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
            && (self.view_mode == ViewMode::Home
                || (provider_workspace(self.screen) && self.focus == Focus::Profiles))
            && self.modal.is_none()
            && !self.codex_ui.accounts
        {
            self.codex_ui
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(true);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(false);
        }
        if self.modal.is_none() && self.handle_grok_key(key)? {
            return Ok(false);
        }
        if self.modal.is_none()
            && let Some(quit) = self.handle_pi_key(key)?
        {
            return Ok(quit);
        }
        if self.modal.is_none()
            && let Some(quit) = self.handle_codex_key(key)?
        {
            return Ok(quit);
        }
        if self.modal.is_some() {
            self.handle_modal(key)?;
            return Ok(false);
        }
        if self.view_mode == ViewMode::AllEnabled && key.code == KeyCode::Char('/') {
            self.focus = Focus::Models;
            self.all_models_filter.active = true;
            return Ok(false);
        }
        if self.view_mode == ViewMode::AllEnabled
            && self.focus == Focus::Models
            && matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Char('j' | 'k')
            )
        {
            self.all_models_filter.detail_scroll = 0;
        }
        if self.view_mode == ViewMode::AllEnabled
            && key.code == KeyCode::Esc
            && !self.all_models_filter.query.is_empty()
        {
            self.update_global_filter(String::new());
            return Ok(false);
        }
        let searching = self.view_mode == ViewMode::Provider
            && self
                .provider_editor
                .as_ref()
                .is_some_and(|editor| editor.search_active);

        if !searching && matches!(key.code, KeyCode::Char('h' | 'l')) {
            let forward = key.code == KeyCode::Char('l');
            if !forward
                && (self.view_mode == ViewMode::Home
                    || (provider_workspace(self.screen) && self.focus == Focus::Profiles))
            {
                return self.back_one_level();
            }
            match (self.view_mode, self.focus, forward) {
                (ViewMode::Provider | ViewMode::AllEnabled, Focus::Details, false) => {
                    self.focus = Focus::Models
                }
                (ViewMode::Provider | ViewMode::AllEnabled, Focus::Models, false)
                    if provider_workspace(self.screen) =>
                {
                    self.focus = Focus::Profiles
                }
                (_, _, false) => self.return_home(),
                (ViewMode::Home, _, true) | (_, Focus::Profiles, true) => {
                    if self.home_all_selected
                        || self.home_account_selected()
                        || self.home_grok_oauth_selected()
                    {
                        self.enter_all_enabled_view();
                    } else if self.view_mode == ViewMode::Home {
                        self.enter_provider_view();
                    } else {
                        self.focus = Focus::Models;
                    }
                }
                (ViewMode::AllEnabled, Focus::Models, true) if !provider_workspace(self.screen) => {
                    self.open_selected_global_model()
                }
                (_, Focus::Models, true) => self.focus = Focus::Details,
                (ViewMode::AllEnabled, Focus::Details, true) => self.open_selected_global_model(),
                _ => {}
            }
            return Ok(false);
        }
        if !searching && key.code == KeyCode::Char('D') && self.client_tab() == ClientTab::Claude {
            if let Err(error) = self.disconnect_claude() {
                self.set_error(format!("Cannot disconnect: {error:#}"));
            }
            return Ok(false);
        }
        if key.code == KeyCode::Char('a') && !searching {
            if self.view_mode == ViewMode::Home
                || (provider_workspace(self.screen) && self.focus == Focus::Profiles)
            {
                self.new_profile();
            } else {
                self.open_add_model_modal();
            }
            return Ok(false);
        }
        if key.code == KeyCode::F(5) {
            self.start_model_test();
            return Ok(false);
        }
        if provider_workspace(self.screen) && self.focus == Focus::Profiles {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Home => self.select_sidebar_index(0),
                KeyCode::End => self.select_sidebar_index(
                    self.config.profiles.len() + self.home_prefix_count() - 1,
                ),
                KeyCode::PageUp => self.move_selection(-5),
                KeyCode::PageDown => self.move_selection(5),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    if self.home_account_selected() || self.home_grok_oauth_selected() {
                        self.focus = Focus::Details;
                    } else {
                        self.focus = Focus::Models;
                    }
                }
                KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
                KeyCode::Char(' ') if self.selected_profile().is_some() => {
                    self.toggle_selected_provider()?;
                    self.init_provider_editor();
                }
                KeyCode::Char('e') | KeyCode::Char('E') => self.edit_profile(),
                KeyCode::Char('x') if self.selected_profile().is_some() => {
                    self.modal = Some(Modal::DeleteProfile)
                }
                KeyCode::Char('p') => self.sync_all_to_claude(),
                KeyCode::Char('P') => self.open_proxy_manager(),
                KeyCode::Char('?') => self.open_help(),
                KeyCode::Char('q') => return Ok(true),
                _ => {}
            }
            return Ok(false);
        }
        match self.view_mode {
            ViewMode::Home => match key.code {
                KeyCode::Char('q') => return Ok(true),
                KeyCode::Char('?') => self.open_help(),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Enter | KeyCode::Char('l') if self.home_all_selected => {
                    self.enter_all_enabled_view();
                }
                KeyCode::Enter | KeyCode::Char('l') if self.selected_profile().is_some() => {
                    self.enter_provider_view();
                }
                KeyCode::Char('e') | KeyCode::Char('E') => self.edit_profile(),
                KeyCode::Char('x') if self.selected_profile().is_some() => {
                    self.modal = Some(Modal::DeleteProfile);
                }
                KeyCode::Char(' ') if self.selected_profile().is_some() => {
                    self.toggle_selected_provider()?;
                }
                KeyCode::Char('A') => self.enable_all_models(),
                KeyCode::Char('p') => self.sync_all_to_claude(),
                KeyCode::Char('P') => self.open_proxy_manager(),
                _ => {}
            },
            ViewMode::AllEnabled => match key.code {
                KeyCode::Esc if !self.all_models_filter.query.is_empty() => {
                    self.update_global_filter(String::new())
                }
                KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Details => {
                    self.all_models_filter.detail_scroll =
                        self.all_models_filter.detail_scroll.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Details => {
                    self.all_models_filter.detail_scroll =
                        self.all_models_filter.detail_scroll.saturating_add(1)
                }
                KeyCode::PageUp if self.focus == Focus::Details => {
                    self.all_models_filter.detail_scroll =
                        self.all_models_filter.detail_scroll.saturating_sub(10)
                }
                KeyCode::PageDown if self.focus == Focus::Details => {
                    self.all_models_filter.detail_scroll =
                        self.all_models_filter.detail_scroll.saturating_add(10)
                }
                KeyCode::Home if self.focus == Focus::Details => {
                    self.all_models_filter.detail_scroll = 0
                }
                KeyCode::End if self.focus == Focus::Details => {
                    self.all_models_filter.detail_scroll = usize::MAX
                }
                KeyCode::Tab | KeyCode::BackTab if provider_workspace(self.screen) => {
                    self.toggle_focus()
                }
                KeyCode::Char('?') => self.open_help(),
                KeyCode::Esc | KeyCode::Char('h') => self.return_home(),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::PageUp => {
                    self.model_idx = self.model_idx.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.model_idx = (self.model_idx + 10)
                        .min(self.filtered_global_models().len().saturating_sub(1));
                }
                KeyCode::Home => self.model_idx = 0,
                KeyCode::End => {
                    self.model_idx = self.filtered_global_models().len().saturating_sub(1);
                }
                KeyCode::Char(' ') => {
                    self.toggle_selected_global_model()?;
                }
                KeyCode::Enter | KeyCode::Char('l') => self.open_selected_global_model(),
                KeyCode::Char('p') => self.sync_all_to_claude(),
                KeyCode::Char('P') => self.open_proxy_manager(),
                _ => {}
            },
            ViewMode::Provider => {
                let mut search_active = false;
                if let Some(editor) = &self.provider_editor {
                    search_active = editor.search_active;
                }
                if search_active {
                    let pi = self.pi_enabled;
                    let editor = self.ensure_provider_editor().unwrap();
                    match key.code {
                        KeyCode::Esc => {
                            if !editor.query.is_empty() {
                                editor.query.clear();
                                editor.selected = 0;
                            } else {
                                editor.search_active = false;
                            }
                        }
                        KeyCode::Char(c) => {
                            editor.query.push(c);
                            editor.selected = 0;
                        }
                        KeyCode::Backspace => {
                            editor.query.pop();
                            editor.selected = 0;
                        }
                        KeyCode::Tab | KeyCode::Down => {
                            editor.search_active = false;
                        }
                        KeyCode::Enter if pi => {
                            editor.search_active = false;
                        }
                        KeyCode::Enter => {
                            editor.toggle_selected();
                            self.commit_provider_editor()?;
                        }
                        KeyCode::Up if editor.selected > 0 => {
                            editor.selected -= 1;
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('?') => self.open_help(),
                        KeyCode::Esc => {
                            self.return_home();
                        }
                        KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
                        KeyCode::Left | KeyCode::Char('h') => {
                            self.focus =
                                if provider_workspace(self.screen) && self.focus == Focus::Models {
                                    Focus::Profiles
                                } else {
                                    Focus::Models
                                };
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            self.focus = Focus::Details;
                        }
                        KeyCode::Char('/') => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.search_active = true;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let name = if let Some(editor) = self.ensure_provider_editor() {
                                let filtered = editor.filtered_indices();
                                if !filtered.is_empty() {
                                    if editor.selected > 0 {
                                        editor.selected -= 1;
                                    } else {
                                        editor.selected = filtered.len().saturating_sub(1);
                                    }
                                    filtered
                                        .get(editor.selected)
                                        .map(|&idx| editor.catalog[idx].label().to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            if let Some(name) = name {
                                self.status_error = false;
                                self.status = if self.pi_enabled {
                                    format!("Selected {name} · e edit · p default · 1 1M")
                                } else {
                                    format!("Selected {name} · Space toggle · d default · 1 1M")
                                };
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let name = if let Some(editor) = self.ensure_provider_editor() {
                                let filtered = editor.filtered_indices();
                                if !filtered.is_empty() {
                                    if editor.selected + 1 < filtered.len() {
                                        editor.selected += 1;
                                    } else {
                                        editor.selected = 0;
                                    }
                                    filtered
                                        .get(editor.selected)
                                        .map(|&idx| editor.catalog[idx].label().to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            if let Some(name) = name {
                                self.status_error = false;
                                self.status = if self.pi_enabled {
                                    format!("Selected {name} · e edit · p default · 1 1M")
                                } else {
                                    format!("Selected {name} · Space toggle · d default · 1 1M")
                                };
                            }
                        }
                        KeyCode::PageUp => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.selected = editor.selected.saturating_sub(10);
                            }
                        }
                        KeyCode::PageDown => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                let filtered = editor.filtered_indices();
                                editor.selected =
                                    (editor.selected + 10).min(filtered.len().saturating_sub(1));
                            }
                        }
                        KeyCode::Home => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.selected = 0;
                            }
                        }
                        KeyCode::End => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                let filtered = editor.filtered_indices();
                                editor.selected = filtered.len().saturating_sub(1);
                            }
                        }
                        KeyCode::Char(' ') => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.toggle_selected();
                            }
                            self.commit_provider_editor()?;
                            if let Some(editor) = &self.provider_editor {
                                let filtered = editor.filtered_indices();
                                if let Some(&idx) = filtered.get(editor.selected) {
                                    let model = &editor.catalog[idx];
                                    let enabled = editor.is_enabled(&model.id);
                                    self.status = format!(
                                        "Model {} is now {}",
                                        model.label(),
                                        if enabled { "enabled" } else { "disabled" }
                                    );
                                }
                            }
                        }
                        KeyCode::Char('1') => {
                            self.toggle_selected_model_1m();
                        }
                        KeyCode::Char('d') => {
                            self.set_selected_as_default();
                        }
                        KeyCode::Char('A') => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.enable_all_filtered();
                            }
                            self.commit_provider_editor()?;
                            self.status = "Enabled all filtered models".into();
                        }
                        KeyCode::Char('C') => {
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.disable_all_filtered();
                            }
                            self.commit_provider_editor()?;
                            self.status = "Cleared non-essential enabled models".into();
                        }
                        KeyCode::Char('x') if self.focus == Focus::Details => {
                            self.modal = Some(Modal::DeleteProfile);
                        }
                        KeyCode::Char('x') => {
                            self.delete_selected_model();
                        }
                        KeyCode::Char('e') => self.edit_model(),
                        KeyCode::Char('E') => self.edit_profile(),
                        KeyCode::Char('p') => self.sync_all_to_claude(),
                        KeyCode::Char('P') => self.open_proxy_manager(),
                        _ => {}
                    }
                }
            }
        }
        Ok(false)
    }
}
