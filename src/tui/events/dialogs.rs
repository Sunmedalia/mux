use super::*;

impl App {
    pub(in crate::tui) fn handle_modal(&mut self, key: KeyEvent) -> Result<()> {
        let saved = self.modal.clone();
        if let Err(error) = self.handle_modal_inner(key) {
            self.modal = saved;
            self.set_error(format!("Could not save changes: {error:#}"));
        }
        Ok(())
    }

    pub(in crate::tui) fn handle_modal_inner(&mut self, key: KeyEvent) -> Result<()> {
        let Some(mut modal) = self.modal.take() else {
            return Ok(());
        };
        match &mut modal {
            Modal::SettingsMenu(menu) => {
                if self.settings_menu_key(menu, key) {
                    return Ok(());
                }
            }
            Modal::UiOptions(options) => {
                if self.ui_options_key(options, key) {
                    return Ok(());
                }
            }
            Modal::CodexSettings(form) => {
                if self.codex_settings_key(form, key) {
                    return Ok(());
                }
            }
            Modal::Grok(dialog) => {
                if self.grok_dialog_key(dialog, key)? {
                    if self.modal.is_none() && self.settings_menu.is_some() {
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('s')
                            && let Some(menu) = &mut self.settings_menu
                        {
                            menu.message = Some("Grok settings saved".into());
                        }
                        self.return_settings_menu();
                    }
                    return Ok(());
                }
            }
            Modal::Appearance(form) => match self.appearance_key(form, key) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    form.error = Some(format!("Cannot save: {error}"));
                    self.modal = Some(modal);
                    return Ok(());
                }
            },
            Modal::Preferences(form) => {
                if self.preferences_key(form, key) {
                    if let Some(appearance) = &form.return_appearance {
                        self.modal = Some(Modal::Appearance(appearance.clone()));
                    } else if self.settings_menu.is_some() {
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('s')
                            && let Some(menu) = &mut self.settings_menu
                        {
                            menu.message = Some("Claude settings saved".into());
                        }
                        self.return_settings_menu();
                    } else if let Some(theme) = form.return_theme {
                        self.modal = Some(Modal::Appearance(theme::Appearance {
                            theme,
                            original_theme: self.theme,
                            original_pulse_theme: theme::PulseTheme::load(&self.paths),
                            return_usage: false,
                            error: None,
                            pulse_theme: form.return_pulse_theme.unwrap_or_default(),
                            pulse_selected: form.return_pulse_selected,
                            refresh_selected: false,
                            usage_refresh_secs: self.config.usage_refresh_secs,
                            original_usage_refresh_secs: self.config.usage_refresh_secs,
                        }));
                    }
                    return Ok(());
                }
            }
            Modal::Import(candidate) => match key.code {
                KeyCode::Char('i') | KeyCode::Enter => {
                    let profile = candidate.profile.clone();
                    self.config = self.update_client_config(|latest| {
                        let id = unique_profile_id("imported", &latest.profiles);
                        latest.profiles.insert(id, profile);
                        Ok(())
                    })?;
                    self.profile_idx = 0;
                    self.model_idx = self.default_model_index();
                    self.status =
                        "Imported existing Claude settings; the source file was not changed".into();
                    self.status_error = false;
                    return Ok(());
                }
                KeyCode::Char('s') | KeyCode::Esc => return Ok(()),
                _ => {}
            },
            Modal::Help(help) => {
                let limit = help_scroll_limit(
                    help,
                    modal_area_for(&Modal::Help(help.clone()), self.screen),
                );
                help.scroll = help.scroll.min(limit);
                if let Some(action) = help_key_action(help, key) {
                    self.run_help_action(action)?;
                    return Ok(());
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q' | 'h') => {
                        self.modal = self.help_return.take().map(|modal| *modal);
                        return Ok(());
                    }
                    KeyCode::Tab | KeyCode::Right => help.move_section(true),
                    KeyCode::BackTab | KeyCode::Left => help.move_section(false),
                    KeyCode::Down | KeyCode::Char('j') => help.scroll(true),
                    KeyCode::PageDown => help.scroll = help.scroll.saturating_add(10),
                    KeyCode::Up | KeyCode::Char('k') => help.scroll(false),
                    KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(10),
                    KeyCode::Char(c @ '1'..='9') => help.select((c as u8 - b'1') as usize),
                    KeyCode::Home => help.scroll = 0,
                    KeyCode::End => help.scroll = limit,
                    _ => {}
                }
                help.scroll = help.scroll.min(limit);
            }
            Modal::DeleteProfile => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(id) = self.selected_profile_id() {
                        let original = self.config.profiles.get(&id).cloned();
                        self.config = self.update_client_config(
                            |latest| {
                                if latest.profiles.get(&id) != original.as_ref() {
                                    anyhow::bail!(
                                        "provider changed in another Mux instance; reopen before deleting"
                                    );
                                }
                                latest.profiles.remove(&id);
                                Ok(())
                            },
                        )?;
                        let cache_result = discovery::update_cache(
                            &self.client_cache_path(self.config_client()),
                            |cache| {
                                cache.profiles.remove(&id);
                            },
                        );
                        self.cache.profiles.remove(&id);
                        self.profile_idx = self
                            .profile_idx
                            .min(self.config.profiles.len().saturating_sub(1));
                        self.model_idx = 0;
                        if self.view_mode == ViewMode::Provider {
                            self.return_home();
                        }
                        self.status_error = false;
                        self.status = format!("Deleted profile {id}");
                        if let Err(error) = cache_result {
                            self.set_error(format!(
                                "Provider deleted, but cache cleanup failed: {error:#}"
                            ));
                        }
                    }
                    return Ok(());
                }
                KeyCode::Char('n') | KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                _ => {}
            },
            Modal::DeleteModel => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let (Some(profile_id), Some(model)) =
                        (self.selected_profile_id(), self.selected_model())
                    {
                        let model_base = canonical_model_id(&model.id);
                        let model_id = model.id.clone();
                        let deleting_default =
                            canonical_model_id(&self.config.profiles[&profile_id].default_model)
                                == model_base;
                        let replacement = self.provider_editor.as_ref().and_then(|editor| {
                            editor
                                .catalog
                                .iter()
                                .find(|entry| {
                                    canonical_model_id(&entry.id) != model_base
                                        && editor.is_enabled(&entry.id)
                                })
                                .or_else(|| {
                                    editor
                                        .catalog
                                        .iter()
                                        .find(|entry| canonical_model_id(&entry.id) != model_base)
                                })
                                .map(|entry| editor.effective_id(&entry.id))
                        });
                        if deleting_default && replacement.is_none() {
                            self.set_error(
                                "The only model cannot be deleted · add another model first",
                            );
                            return Ok(());
                        }
                        let original = self.config.profiles[&profile_id].clone();
                        let mut edited = original.clone();
                        {
                            let profile = &mut edited;
                            profile
                                .models
                                .retain(|entry| canonical_model_id(&entry.id) != model_base);
                            profile
                                .enabled_models
                                .retain(|id| canonical_model_id(id) != model_base);
                            profile
                                .disabled_models
                                .retain(|id| canonical_model_id(id) != model_base);
                            for alias in [
                                &mut profile.aliases.opus,
                                &mut profile.aliases.sonnet,
                                &mut profile.aliases.haiku,
                                &mut profile.aliases.fable,
                                &mut profile.subagent_model,
                            ] {
                                if alias
                                    .as_ref()
                                    .is_some_and(|id| canonical_model_id(id) == model_base)
                                {
                                    *alias = None;
                                }
                            }
                            profile
                                .fallback_models
                                .retain(|id| canonical_model_id(id) != model_base);
                            if let Some(replacement) = &replacement
                                && deleting_default
                            {
                                let replacement_base = canonical_model_id(replacement);
                                profile
                                    .disabled_models
                                    .retain(|id| canonical_model_id(id) != replacement_base);
                                profile.default_model = replacement.clone();
                            }
                        }
                        self.config =
                            self.update_client_profile(&profile_id, &original, &edited)?;
                        self.model_idx = self.model_idx.min(self.models().len().saturating_sub(1));
                        self.status_error = false;
                        self.status = format!("Deleted model {model_id}");
                        self.init_provider_editor();
                    }
                    return Ok(());
                }
                KeyCode::Char('n') | KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                _ => {}
            },
            Modal::Proxy(manager) => {
                if let Some(fields) = &mut manager.resource_fields {
                    match handle_form_key(fields, &mut manager.resource_selected, key) {
                        FormOutcome::Close => {
                            manager.resource_fields = None;
                            manager.resource_original = None;
                            manager.error = false;
                            manager.message = "Resources edit cancelled".into();
                        }
                        FormOutcome::Submit => {
                            if let Err(error) = manager.save_resources(&self.paths) {
                                manager.error = true;
                                manager.message = format!("Cannot save: {error}");
                            }
                        }
                        FormOutcome::Stay => {}
                    }
                    self.modal = Some(modal);
                    return Ok(());
                }
                if let Some(port) = &mut manager.port_field {
                    if !self.background.proxy_running {
                        match handle_form_key(std::slice::from_mut(port), &mut 0, key) {
                            FormOutcome::Close => {
                                manager.port_field = None;
                                manager.error = false;
                                manager.message = "Port edit cancelled".into();
                            }
                            FormOutcome::Submit => {
                                self.modal = Some(modal);
                                self.start_proxy_action(ProxyControl::Port);
                                return Ok(());
                            }
                            FormOutcome::Stay => {}
                        }
                    }
                    self.modal = Some(modal);
                    return Ok(());
                }
                let control = match key.code {
                    KeyCode::Esc | KeyCode::Char('P') | KeyCode::Char('q') => {
                        if let Some(form) = &manager.return_appearance {
                            self.modal = Some(Modal::Appearance(form.clone()));
                        } else if self.settings_menu.is_some() {
                            self.return_settings_menu();
                        }
                        return Ok(());
                    }
                    KeyCode::Tab | KeyCode::Right | KeyCode::Down | KeyCode::Char('j' | 'l') => {
                        manager.move_selection(true);
                        None
                    }
                    KeyCode::BackTab | KeyCode::Left | KeyCode::Up | KeyCode::Char('h' | 'k') => {
                        manager.move_selection(false);
                        None
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => Some(manager.selected_control()),
                    KeyCode::Char('s') => Some(ProxyControl::Start),
                    KeyCode::Char('x') => Some(ProxyControl::Stop),
                    KeyCode::Char('r') => Some(ProxyControl::Refresh),
                    KeyCode::Char('i') => Some(ProxyControl::EnableAtLogin),
                    KeyCode::Char('u') => Some(ProxyControl::DisableAtLogin),
                    KeyCode::Char('e') => Some(ProxyControl::Port),
                    KeyCode::Char('L') => Some(ProxyControl::Resources),
                    _ => None,
                };
                if let Some(control) = control {
                    manager.selected = proxy_control_index(control);
                    if control == ProxyControl::Resources {
                        if !self.background.proxy_running
                            && let Err(error) = manager.edit_resources(&self.paths)
                        {
                            manager.error = true;
                            manager.message = format!("Cannot load resources: {error}");
                        }
                        self.modal = Some(modal);
                        return Ok(());
                    }
                    if control == ProxyControl::Port {
                        if !self.background.proxy_running {
                            manager.edit_port();
                        }
                        self.modal = Some(modal);
                        return Ok(());
                    }
                    if control == ProxyControl::Close {
                        if let Some(form) = &manager.return_appearance {
                            self.modal = Some(Modal::Appearance(form.clone()));
                        } else if self.settings_menu.is_some() {
                            self.return_settings_menu();
                        }
                        return Ok(());
                    }
                    self.modal = Some(modal);
                    self.start_proxy_action(control);
                    return Ok(());
                }
            }
            Modal::Profile(form) => {
                if let Some(selected) = &mut form.template_selected {
                    match key.code {
                        KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                            *selected = selected.checked_sub(1).unwrap_or(PROVIDER_TEMPLATES.len())
                        }
                        KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                            *selected = (*selected + 1) % (PROVIDER_TEMPLATES.len() + 1)
                        }
                        KeyCode::Enter | KeyCode::Char('l') => {
                            let index = *selected;
                            self.use_provider_template(index);
                            return Ok(());
                        }
                        KeyCode::Esc | KeyCode::Char('h') => return Ok(()),
                        _ => {}
                    }
                    self.modal = Some(modal);
                    return Ok(());
                }
                if (key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('f'))
                    || (key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('r'))
                {
                    let force = key.modifiers == KeyModifiers::CONTROL;
                    self.modal = Some(modal);
                    self.fetch_profile_models(force);
                    return Ok(());
                }
                if let Some(picker) = &mut form.picker {
                    let mut key = key;
                    if key.modifiers.is_empty() {
                        if form.picker_search {
                            if key.code == KeyCode::Esc {
                                form.picker_search = false;
                                self.modal = Some(modal);
                                return Ok(());
                            }
                        } else {
                            key.code = match key.code {
                                KeyCode::Char('j') => KeyCode::Down,
                                KeyCode::Char('k') => KeyCode::Up,
                                KeyCode::Char('h') => KeyCode::Esc,
                                KeyCode::Char('l') => KeyCode::Enter,
                                KeyCode::Char('/') => {
                                    form.picker_search = true;
                                    self.modal = Some(modal);
                                    return Ok(());
                                }
                                KeyCode::Char(c) => {
                                    form.picker_search = true;
                                    KeyCode::Char(c)
                                }
                                other => other,
                            };
                        }
                    }
                    match key.code {
                        KeyCode::Esc => form.picker = None,
                        KeyCode::Enter => {
                            if let Some(model) =
                                picker.filtered_api_models().get(picker.api_selected)
                            {
                                let id = model.id.clone();
                                form.fill_selected_model(&id);
                                form.picker = None;
                            }
                        }
                        KeyCode::Tab | KeyCode::BackTab => {}
                        _ => {
                            picker.handle_key(
                                key,
                                usize::from(modal_area(self.screen).height.saturating_sub(8))
                                    .max(1),
                            );
                        }
                    }
                    self.modal = Some(modal);
                    return Ok(());
                }
                if key.code == KeyCode::F(5) {
                    if form.fields[form.selected].label == "Fetch models URL" {
                        self.status = "Use Fetch models to test the model catalog URL".into();
                        self.modal = Some(modal);
                        return Ok(());
                    }
                    let connection = form.selected == 3;
                    self.modal = Some(modal);
                    if connection {
                        self.start_profile_connection_test();
                    } else {
                        self.start_profile_model_test();
                    }
                    return Ok(());
                }
                if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('1') {
                    form.toggle_model_field_1m(form.selected);
                    self.modal = Some(modal);
                    return Ok(());
                }
                let old_default = (form.selected == 6 && form.fields.len() > 10)
                    .then(|| form.fields[6].value.clone());
                let outcome = handle_form_key(&mut form.fields, &mut form.selected, key);
                if let Some(old_default) = old_default
                    && form.fields[6].value != old_default
                {
                    form.sync_default_aliases(&old_default);
                }
                if outcome == FormOutcome::Close {
                    return Ok(());
                }
                if outcome == FormOutcome::Submit
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('s'))
                {
                    match form.to_profile() {
                        Ok((id, profile)) => {
                            let original = form.original_id.clone();
                            let original_profile = form.original_profile.clone();
                            let update = self.update_client_config(|latest| {
                                let profile = if let (Some(original_id), Some(original_profile)) =
                                    (&original, &original_profile)
                                {
                                    let current = latest
                                        .profiles
                                        .get(original_id)
                                        .context("provider was removed in another Mux instance")?;
                                    config::merge_profile(original_profile, &profile, current)?
                                } else {
                                    profile
                                };
                                if latest.profiles.contains_key(&id)
                                    && original.as_deref() != Some(id.as_str())
                                {
                                    anyhow::bail!("profile id '{id}' already exists");
                                }
                                if let Some(original) = &original
                                    && original != &id
                                {
                                    latest.profiles.remove(original);
                                }
                                latest.profiles.insert(id.clone(), profile);
                                Ok(())
                            });
                            self.config = match update {
                                Ok(config) => config,
                                Err(error) => {
                                    self.set_error(format!("Cannot save profile: {error:#}"));
                                    self.modal = Some(modal);
                                    return Ok(());
                                }
                            };
                            let current = &self.config.profiles[&id];
                            let connection_changed =
                                form.original_profile.as_ref().is_some_and(|old| {
                                    old.base_url != current.base_url
                                        || old.models_url != current.models_url
                                        || old.api_format != current.api_format
                                        || old.credential != current.credential
                                });
                            let fetched = form
                                .discovery_profile()
                                .ok()
                                .filter(|profile| form.fetched_profile.as_deref() == Some(profile))
                                .map(|_| CachedModels {
                                    fetched_at: now_epoch(),
                                    models: form.fetched_models.clone(),
                                });
                            let mut cache_error = None;
                            if form
                                .original_id
                                .as_ref()
                                .is_some_and(|original| original != &id || connection_changed)
                                || fetched.is_some()
                            {
                                let result = discovery::update_cache(
                                    &self.client_cache_path(self.config_client()),
                                    |cache| {
                                        let previous = form
                                            .original_id
                                            .as_ref()
                                            .and_then(|original| cache.profiles.remove(original));
                                        if let Some(cached) = fetched.or_else(|| {
                                            (!connection_changed).then_some(previous).flatten()
                                        }) {
                                            cache.profiles.insert(id.clone(), cached);
                                        }
                                    },
                                );
                                match result {
                                    Ok(cache) => self.cache = cache,
                                    Err(error) => {
                                        if let Some(original) = &form.original_id {
                                            self.cache.profiles.remove(original);
                                        }
                                        cache_error = Some(error);
                                    }
                                }
                            }
                            self.profile_idx = self
                                .profile_ids()
                                .iter()
                                .position(|candidate| candidate == &id)
                                .unwrap_or(0);
                            self.model_idx = self.default_model_index();
                            self.status_error = false;
                            self.status = format!("Saved profile {id}");
                            if let Some(error) = cache_error {
                                self.set_error(format!(
                                    "Provider saved, but cache update failed: {error:#}"
                                ));
                            }
                            self.init_provider_editor();
                            return Ok(());
                        }
                        Err(error) => self.set_error(format!("Cannot save profile: {error:#}")),
                    }
                }
            }
            Modal::Model(form) => {
                if (key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('f'))
                    || (key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('r'))
                {
                    self.modal = Some(modal);
                    self.fetch_api_models_for_form();
                    return Ok(());
                }

                let modal_rect = centered_rect(
                    86.min(self.screen.width.saturating_sub(2)),
                    20.min(self.screen.height.saturating_sub(2)),
                    self.screen,
                );
                let inner = panel_inner(modal_rect);
                let content = Rect::new(
                    inner.x,
                    inner.y,
                    inner.width,
                    inner.height.saturating_sub(2),
                );
                let (_, api_area) = model_form_areas(content, form.focus_api_search);
                let visible = usize::from(panel_inner(api_area).height.saturating_sub(3)).max(1);
                let outcome = form.handle_key(key, visible);
                if outcome == FormOutcome::Close {
                    return Ok(());
                }
                if outcome == FormOutcome::Submit
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('s'))
                {
                    let base_id = canonical_model_id(form.fields[0].value.trim());
                    if base_id.trim().is_empty() {
                        self.set_error("Model id cannot be empty");
                    } else if let Err(error) = form.validate_tokens() {
                        self.set_error(error.to_string());
                    } else if let Some(profile_id) = self.selected_profile_id() {
                        let model = form.to_model();
                        let saved_model = model.clone();
                        let enable_now = form.enable_now();
                        let original = form
                            .original_profile
                            .as_deref()
                            .cloned()
                            .unwrap_or_else(|| self.config.profiles[&profile_id].clone());
                        let mut edited = original.clone();
                        {
                            let profile = &mut edited;
                            let saved_base = canonical_model_id(&saved_model.id);
                            let old_base = form.original_model_id.as_deref().unwrap_or(&saved_base);
                            profile.models.retain(|entry| {
                                canonical_model_id(&entry.id) != saved_base
                                    && canonical_model_id(&entry.id) != old_base
                            });
                            profile.models.push(saved_model.clone());
                            for reference in std::iter::once(&mut profile.default_model)
                                .chain(
                                    [
                                        &mut profile.aliases.opus,
                                        &mut profile.aliases.sonnet,
                                        &mut profile.aliases.haiku,
                                        &mut profile.aliases.fable,
                                        &mut profile.subagent_model,
                                    ]
                                    .into_iter()
                                    .flatten(),
                                )
                                .chain(profile.fallback_models.iter_mut())
                            {
                                if canonical_model_id(reference) == saved_base
                                    || canonical_model_id(reference) == old_base
                                {
                                    *reference = saved_model.id.clone();
                                }
                            }
                            profile.enabled_models.retain(|id| {
                                canonical_model_id(id) != saved_base
                                    && canonical_model_id(id) != old_base
                            });
                            profile.disabled_models.retain(|id| {
                                canonical_model_id(id) != saved_base
                                    && canonical_model_id(id) != old_base
                            });
                            if enable_now {
                                if !profile.required_model_ids().contains(&saved_model.id) {
                                    profile.enabled_models.push(saved_model.id.clone());
                                }
                            } else {
                                profile.disabled_models.push(saved_model.id.clone());
                            }
                        }
                        self.config =
                            self.update_client_profile(&profile_id, &original, &edited)?;
                        self.select_model_id(&model.id);
                        self.status_error = false;
                        self.status = format!(
                            "Saved model {} · {}",
                            model.id,
                            if self.pi_enabled {
                                "saved to models.json"
                            } else if enable_now {
                                "enabled"
                            } else {
                                "disabled"
                            }
                        );
                        self.init_provider_editor();
                        if let Some(editor) = &mut self.provider_editor
                            && let Some(index) = editor
                                .catalog
                                .iter()
                                .position(|entry| canonical_model_id(&entry.id) == base_id)
                        {
                            editor.selected = index;
                        }
                        return Ok(());
                    }
                }
            }
        }
        self.modal = Some(modal);
        Ok(())
    }
}
