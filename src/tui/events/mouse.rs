use super::*;

impl App {
    pub(in crate::tui) fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
    ) -> Result<MouseAction> {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && (self.modal.is_none()
                || matches!(
                    self.modal,
                    Some(
                        Modal::Appearance(_)
                            | Modal::Proxy(_)
                            | Modal::SettingsMenu(_)
                            | Modal::UiOptions(_)
                            | Modal::CodexSettings(_)
                    )
                ))
            && let Some((tab, _)) = client_tabs(area)
                .into_iter()
                .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
        {
            self.select_client_tab(tab);
            return Ok(MouseAction::None);
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && (matches!(
                self.modal,
                Some(Modal::Appearance(_) | Modal::UiOptions(_) | Modal::CodexSettings(_))
            ) || matches!(self.modal, Some(Modal::Proxy(_))) && self.settings_menu.is_none()
                || (self.modal.is_none()
                    && !provider_workspace(area)
                    && (self.codex_ui.accounts || self.grok_auth.page.is_some())))
        {
            let page = self.modal.as_ref().map_or_else(
                || workspace_content_area(area),
                |modal| modal_area_for(modal, area),
            );
            let [(help, back)] = page_header_actions(page);
            if contains(help, mouse.column, mouse.row) {
                self.open_help();
                return Ok(MouseAction::None);
            }
            if contains(back, mouse.column, mouse.row) {
                return Ok(if self.back_one_level()? {
                    MouseAction::Quit
                } else {
                    MouseAction::None
                });
            }
        }
        if matches!(self.modal, Some(Modal::Proxy(_)))
            && self.settings_menu.is_none()
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && contains(
                settings_appearance_button(modal_area_for(self.modal.as_ref().unwrap(), area)),
                mouse.column,
                mouse.row,
            )
        {
            self.open_appearance();
            return Ok(MouseAction::None);
        }
        if let Some(Modal::SettingsMenu(menu)) = &mut self.modal {
            let page = settings_page_area(area);
            let mut key = None;
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                key = Some(KeyEvent::new(
                    if mouse.kind == MouseEventKind::ScrollUp {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    KeyModifiers::NONE,
                ));
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some(index) = modal_button_rects(page, 2)
                    .iter()
                    .position(|r| contains(*r, mouse.column, mouse.row))
                {
                    key = Some(if index == 0 {
                        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
                    } else {
                        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
                    });
                } else if let Some((i, _)) = settings::settings_fields(page, menu)
                    .into_iter()
                    .find(|(_, r)| contains(*r, mouse.column, mouse.row))
                {
                    menu.row = i;
                    menu.editing = true;
                    key = Some(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
                } else if (area.width >= 76 || !menu.editing)
                    && let Some((i, _)) = settings::settings_rows(page, menu.selected)
                        .into_iter()
                        .find(|(_, r)| {
                            contains(*r, mouse.column, mouse.row)
                                && (area.width < 76 || mouse.column < r.x + 20)
                        })
                {
                    menu.selected = i;
                    menu.row = 0;
                    menu.editing = false;
                    key = Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            if let Some(key) = key {
                self.handle_modal(key)?;
            }
            return Ok(MouseAction::None);
        }
        if matches!(
            self.modal,
            Some(Modal::SettingsMenu(_) | Modal::UiOptions(_) | Modal::CodexSettings(_))
        ) {
            let modal_area = modal_area_for(self.modal.as_ref().unwrap(), area);
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                self.handle_modal(KeyEvent::new(
                    if mouse.kind == MouseEventKind::ScrollUp {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    KeyModifiers::NONE,
                ))?;
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some(index) = modal_button_rects(modal_area, 2)
                    .iter()
                    .position(|rect| contains(*rect, mouse.column, mouse.row))
                {
                    self.handle_modal(KeyEvent::new(
                        if index == 0 {
                            KeyCode::Enter
                        } else {
                            KeyCode::Esc
                        },
                        KeyModifiers::NONE,
                    ))?;
                } else if let Some(Modal::SettingsMenu(menu)) = &self.modal {
                    if let Some((index, _)) = settings::settings_rows(modal_area, menu.selected)
                        .into_iter()
                        .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                    {
                        if let Some(Modal::SettingsMenu(menu)) = &mut self.modal {
                            menu.selected = index;
                            self.settings_menu = Some(menu.clone());
                        }
                        self.handle_modal(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
                    }
                } else if let Some(Modal::UiOptions(options)) = &mut self.modal {
                    if let Some(index) = settings::option_rows(modal_area, 4)
                        .iter()
                        .position(|rect| contains(*rect, mouse.column, mouse.row))
                    {
                        options.selected = index;
                        options.change(true);
                    }
                } else if matches!(self.modal, Some(Modal::CodexSettings(_))) {
                    let row = settings::option_rows(modal_area, 1)[0];
                    if contains(row, mouse.column, mouse.row) {
                        self.handle_modal(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
                    }
                }
            }
            return Ok(MouseAction::None);
        }
        if matches!(self.modal, Some(Modal::Appearance(_))) {
            let modal_area = modal_area_for(self.modal.as_ref().unwrap(), area);
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                self.handle_modal(KeyEvent::new(
                    if mouse.kind == MouseEventKind::ScrollUp {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    KeyModifiers::NONE,
                ))?;
                return Ok(MouseAction::None);
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if matches!(&self.modal, Some(Modal::Appearance(form)) if form.pulse_selected)
                    && contains(
                        Rect::new(
                            modal_area.x.saturating_add(1),
                            modal_area.bottom().saturating_sub(3),
                            modal_area.width.saturating_sub(2),
                            1,
                        ),
                        mouse.column,
                        mouse.row,
                    )
                {
                    self.handle_modal(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE))?;
                    return Ok(MouseAction::None);
                }
                if contains(settings_proxy_button(modal_area), mouse.column, mouse.row) {
                    self.open_proxy_manager();
                    return Ok(MouseAction::None);
                }
                if matches!(&self.modal, Some(Modal::Appearance(form)) if form.refresh_selected)
                    && let Some((seconds, _)) = theme::refresh_presets(modal_area)
                        .into_iter()
                        .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                {
                    if let Some(Modal::Appearance(form)) = &mut self.modal {
                        form.usage_refresh_secs = seconds;
                    }
                } else if let Some((index, _)) = theme::rows(
                    modal_area,
                    match self.modal.as_ref().unwrap() {
                        Modal::Appearance(form) => form,
                        _ => unreachable!(),
                    },
                )
                .into_iter()
                .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                {
                    if let Some(Modal::Appearance(form)) = self.modal.as_mut() {
                        form.refresh_selected = false;
                        if form.pulse_selected {
                            form.pulse_theme = theme::PulseTheme::ALL[index];
                        } else {
                            form.theme = theme::Theme::ALL[index];
                        }
                    }
                } else if matches!(&self.modal, Some(Modal::Appearance(form)) if form.refresh_selected)
                    && contains(theme::refresh_row(modal_area), mouse.column, mouse.row)
                {
                    if let Some(Modal::Appearance(form)) = self.modal.as_mut() {
                        form.refresh_selected = true;
                        form.pulse_selected = false;
                        if let Some(index) = theme::refresh_buttons(modal_area)
                            .iter()
                            .position(|rect| contains(*rect, mouse.column, mouse.row))
                        {
                            form.usage_refresh_secs = if index == 0 {
                                form.usage_refresh_secs.saturating_sub(1).max(1)
                            } else {
                                (form.usage_refresh_secs + 1).min(60)
                            };
                        }
                    }
                } else if let Some(index) = theme::target_tabs(modal_area)
                    .iter()
                    .position(|rect| contains(*rect, mouse.column, mouse.row))
                {
                    if let Some(Modal::Appearance(form)) = self.modal.as_mut() {
                        form.pulse_selected = index == 1;
                        form.refresh_selected = index == 2;
                    }
                } else {
                    let client_settings = !self.pi_enabled && !self.codex_ui.enabled;
                    if let Some(index) =
                        modal_button_rects(modal_area, if client_settings { 3 } else { 2 })
                            .iter()
                            .position(|rect| contains(*rect, mouse.column, mouse.row))
                    {
                        let code = if index == 0 {
                            KeyCode::Enter
                        } else if client_settings && index == 1 {
                            KeyCode::Char('c')
                        } else {
                            KeyCode::Esc
                        };
                        self.handle_modal(KeyEvent::new(code, KeyModifiers::NONE))?;
                    }
                }
            }
            return Ok(MouseAction::None);
        }
        if self.modal.is_some() {
            if self.settings_menu.is_some()
                && matches!(
                    self.modal,
                    Some(Modal::Preferences(_) | Modal::Grok(_) | Modal::Proxy(_))
                )
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && area.width >= 76
                && mouse.column < settings::client_editor_area(area).x
            {
                let page = settings_page_area(area);
                if let Some((selected, _)) =
                    settings::settings_rows(page, self.settings_menu.as_ref().unwrap().selected)
                        .into_iter()
                        .find(|(_, row)| contains(*row, mouse.column, mouse.row))
                {
                    self.handle_modal(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
                    if matches!(self.modal, Some(Modal::Proxy(_))) {
                        self.handle_modal(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
                    }
                    if let Some(Modal::SettingsMenu(menu)) = &mut self.modal {
                        menu.selected = selected;
                        menu.editing = true;
                        menu.row = 0;
                        self.settings_menu = Some(menu.clone());
                        if matches!(selected, 4 | 6 | 7) {
                            self.open_settings_section(selected);
                        }
                    }
                }
                return Ok(MouseAction::None);
            }
            let before = self.config.clone();
            let before_client = self.config_client();
            self.handle_modal_mouse(mouse, area)?;
            if self.settings_menu.is_none()
                && before != self.config
                && before_client == self.config_client()
            {
                self.queue_sync(false, None);
                self.sync_pi_after_edit();
                self.sync_grok_after_edit();
            }
            return Ok(MouseAction::None);
        }
        if area.width >= 40
            && area.height >= 12
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some((tab, _)) = client_tabs(area)
                .into_iter()
                .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
        {
            self.select_client_tab(tab);
            return Ok(MouseAction::None);
        }
        if self.usage.active {
            return Ok(self.usage_mouse(mouse, area));
        }
        if self.codex_mouse(mouse, area)? {
            return Ok(MouseAction::None);
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && self.modal.is_none()
            && self.view_mode == ViewMode::Provider
        {
            let is_provider_card = self
                .provider_ui_areas(area)
                .details
                .map(provider_detail_cards)
                .is_some_and(|(_, card)| {
                    contains(card, mouse.column, mouse.row)
                        && !detail_controls(card)
                            .iter()
                            .any(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                });
            if !is_provider_card {
                self.provider_card_selected = false;
            }
        }
        let before = self.config.clone();
        let before_client = self.config_client();
        let result = self.handle_mouse_inner(mouse, area);
        if before != self.config && before_client == self.config_client() {
            self.queue_sync(false, None);
            self.sync_pi_after_edit();
            self.sync_grok_after_edit();
        }
        result
    }

    pub(in crate::tui) fn handle_mouse_inner(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
    ) -> Result<MouseAction> {
        if self.modal.is_some() {
            self.handle_modal_mouse(mouse, area)?;
            return Ok(MouseAction::None);
        }

        if self.grok_enabled && self.grok_auth.page.is_some() && !provider_workspace(area) {
            self.grok_auth_page_mouse(mouse, area)?;
            return Ok(MouseAction::None);
        }
        if self.grok_enabled
            && self.home_grok_oauth_selected()
            && provider_workspace(area)
            && let Some(panel) = self.provider_ui_areas(area).details
            && contains(panel, mouse.column, mouse.row)
        {
            if self.grok_auth.page.is_none()
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            {
                self.open_grok_auth();
            }
            let content = panel_inner(panel);
            let rows = embedded_account_rows(content, self.grok_auth.busy);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if self
                    .grok_auth
                    .page
                    .as_ref()
                    .is_some_and(|page| page.confirm_logout)
                {
                    if let Some(index) =
                        modal_button_rects(grok_auth::confirmation_area(content), 2)
                            .iter()
                            .position(|rect| contains(*rect, mouse.column, mouse.row))
                    {
                        self.grok_auth_page_key(KeyEvent::new(
                            KeyCode::Char(if index == 0 { 'y' } else { 'n' }),
                            KeyModifiers::NONE,
                        ))?;
                    }
                    return Ok(MouseAction::None);
                }
                if let Some(index) = grok_auth::account_actions(content)
                    .iter()
                    .position(|rect| contains(*rect, mouse.column, mouse.row))
                {
                    if index == 6 {
                        return Ok(MouseAction::None);
                    }
                    if self.grok_auth.busy {
                        if index == 6 {
                            self.cancel_grok_auth();
                        }
                    } else {
                        if let Some(page) = self.grok_auth.page.as_mut() {
                            page.selected = index + 1;
                        }
                        self.grok_auth_page_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
                    }
                    return Ok(MouseAction::None);
                }
                let inner = panel_inner(rows[2]);
                if mouse.row == inner.y
                    && contains(inner, mouse.column, mouse.row)
                    && let Some(page) = self.grok_auth.page.as_mut()
                {
                    page.selected = 0;
                }
            } else if contains(rows[2], mouse.column, mouse.row)
                && let Some(page) = self.grok_auth.page.as_mut()
            {
                page.scroll = if mouse.kind == MouseEventKind::ScrollDown {
                    page.scroll.saturating_add(1)
                } else if mouse.kind == MouseEventKind::ScrollUp {
                    page.scroll.saturating_sub(1)
                } else {
                    page.scroll
                };
            }
            return Ok(MouseAction::None);
        }
        self.screen = area;
        let pi = self.pi_enabled;
        let ui = self.provider_ui_areas(area);
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(panel) = ui.profiles
                && contains(panel, mouse.column, mouse.row)
            {
                self.focus = Focus::Profiles;
                self.all_models_filter.active = false;
                if let Some(editor) = &mut self.provider_editor {
                    editor.search_active = false;
                }
                if header_add_button_rect(panel)
                    .is_some_and(|button| contains(button, mouse.column, mouse.row))
                {
                    self.new_profile();
                    return Ok(MouseAction::None);
                }
            } else if let Some(panel) = ui.models
                && contains(panel, mouse.column, mouse.row)
            {
                self.focus = Focus::Models;
                let list_y = if self.view_mode == ViewMode::AllEnabled {
                    all_models::areas(panel).1.y
                } else {
                    panel.y + 3
                };
                if mouse.row >= list_y {
                    self.all_models_filter.active = false;
                    if let Some(editor) = &mut self.provider_editor {
                        editor.search_active = false;
                    }
                }
                if model_add_button_rect(panel, self.view_mode)
                    .is_some_and(|button| contains(button, mouse.column, mouse.row))
                {
                    self.all_models_filter.active = false;
                    if let Some(editor) = &mut self.provider_editor {
                        editor.search_active = false;
                    }
                    self.open_add_model_modal();
                    return Ok(MouseAction::None);
                }
            } else if ui
                .details
                .is_some_and(|panel| contains(panel, mouse.column, mouse.row))
            {
                self.focus = Focus::Details;
                self.all_models_filter.active = false;
                if let Some(editor) = &mut self.provider_editor {
                    editor.search_active = false;
                }
            }
        }
        if self.view_mode == ViewMode::AllEnabled {
            if let Some(panel) = ui.models
                && contains(panel, mouse.column, mouse.row)
            {
                let (search, list) = all_models::areas(panel);
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        self.focus = Focus::Models;
                        self.move_selection(if mouse.kind == MouseEventKind::ScrollUp {
                            -1
                        } else {
                            1
                        });
                    }
                    MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left) => {
                        self.focus = Focus::Models;
                        if contains(search, mouse.column, mouse.row) {
                            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                                if !self.all_models_filter.query.is_empty()
                                    && all_models::clear_area(search)
                                        .is_some_and(|r| contains(r, mouse.column, mouse.row))
                                {
                                    self.update_global_filter(String::new());
                                    self.all_models_filter.active = false;
                                } else {
                                    self.all_models_filter.active = true;
                                }
                            }
                        } else {
                            let models = self.filtered_global_models();
                            let index = scrollbar_index(
                                list,
                                mouse.column,
                                mouse.row,
                                models.len(),
                                usize::from(list.height.saturating_sub(2) / 2),
                            )
                            .or_else(|| {
                                clicked_list_index(
                                    list,
                                    mouse.column,
                                    mouse.row,
                                    self.model_offset,
                                    2,
                                )
                            });
                            if let Some(index) = index.filter(|index| *index < models.len()) {
                                let repeated = self.model_idx == index;
                                self.model_idx = index;
                                self.all_models_filter.active = false;
                                self.all_models_filter.detail_scroll = 0;
                                let marker_x =
                                    list.x + 1 + u16::from(self.theme.terminal_background());
                                if !pi
                                    && mouse.column == marker_x
                                    && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                                {
                                    self.toggle_selected_global_model()?;
                                } else if !provider_workspace(area)
                                    && repeated
                                    && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                                {
                                    self.open_selected_global_model();
                                } else {
                                    self.status_error = false;
                                    self.status = format!(
                                        "{} · {} · Enter open provider",
                                        models[index].profile_name,
                                        models[index].model.label()
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
                return Ok(MouseAction::None);
            }
            if let Some(details) = ui.details
                && contains(details, mouse.column, mouse.row)
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        self.all_models_filter.detail_scroll =
                            self.all_models_filter.detail_scroll.saturating_sub(1)
                    }
                    MouseEventKind::ScrollDown => {
                        self.all_models_filter.detail_scroll =
                            self.all_models_filter.detail_scroll.saturating_add(1)
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.all_models_filter.active = false
                    }
                    _ => {}
                }
                self.focus = Focus::Details;
                return Ok(MouseAction::None);
            }
        }
        // The wide sidebar navigates in place and shares compact two-line hit targets.
        if provider_workspace(area)
            && let Some(panel) = ui.profiles
            && contains(panel, mouse.column, mouse.row)
        {
            match mouse.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.focus = Focus::Profiles;
                    self.move_selection(if mouse.kind == MouseEventKind::ScrollUp {
                        -1
                    } else {
                        1
                    });
                }
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left) => {
                    let count = self.config.profiles.len() + self.home_prefix_count();
                    let visible = usize::from(panel.height.saturating_sub(2) / 2);
                    let index = scrollbar_index(panel, mouse.column, mouse.row, count, visible)
                        .or_else(|| {
                            clicked_list_index(
                                panel,
                                mouse.column,
                                mouse.row,
                                self.profile_offset,
                                2,
                            )
                        });
                    if let Some(index) = index.filter(|index| *index < count) {
                        let repeated = index == self.home_selected_index();
                        if !repeated {
                            self.select_sidebar_index(index);
                        }
                        self.focus = Focus::Profiles;
                        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                            // Only the status dot toggles provider availability.
                            let marker_x =
                                panel.x + 1 + u16::from(self.theme.terminal_background());
                            if !pi && index >= self.home_prefix_count() && mouse.column == marker_x
                            {
                                self.toggle_selected_provider()?;
                                self.init_provider_editor();
                            } else if repeated
                                && (self.home_account_selected() || self.home_grok_oauth_selected())
                            {
                                self.focus = Focus::Details;
                            }
                        }
                    }
                }
                _ => {}
            }
            return Ok(MouseAction::None);
        }
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
                if ui
                    .profiles
                    .is_some_and(|panel| contains(panel, mouse.column, mouse.row))
                {
                    self.focus = Focus::Profiles;
                    self.move_selection(delta);
                } else if ui
                    .models
                    .is_some_and(|panel| contains(panel, mouse.column, mouse.row))
                {
                    self.focus = Focus::Models;
                    if self.view_mode == ViewMode::Provider {
                        if let Some(editor) = self.ensure_provider_editor() {
                            let filtered = editor.filtered_indices();
                            if !filtered.is_empty() {
                                if delta < 0 {
                                    if editor.selected > 0 {
                                        editor.selected -= 1;
                                    }
                                } else if editor.selected + 1 < filtered.len() {
                                    editor.selected += 1;
                                }
                            }
                        }
                    } else {
                        self.move_selection(delta);
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(panel) = ui.profiles {
                    let visible = if self.view_mode == ViewMode::Home {
                        visible_variable_items(
                            &self.home_profile_item_heights(panel),
                            self.profile_offset,
                            usize::from(panel.height.saturating_sub(2)),
                        )
                    } else {
                        usize::from(panel.height.saturating_sub(2))
                    };
                    if let Some(index) = scrollbar_index(
                        panel,
                        mouse.column,
                        mouse.row,
                        if self.view_mode == ViewMode::Home {
                            self.config
                                .profiles
                                .len()
                                .saturating_add(self.home_prefix_count())
                        } else {
                            self.config.profiles.len()
                        },
                        visible,
                    ) {
                        self.focus = Focus::Profiles;
                        if self.view_mode == ViewMode::Home {
                            self.select_home_index(index);
                        } else {
                            self.profile_idx = index;
                            self.model_idx = self.default_model_index();
                            self.model_offset = 0;
                        }
                        return Ok(MouseAction::None);
                    }
                }
                if let Some(panel) = ui.models {
                    if self.view_mode == ViewMode::Provider {
                        let search_area = Rect::new(panel.x, panel.y, panel.width, 3);
                        let list_area = Rect::new(
                            panel.x,
                            panel.y + 3,
                            panel.width,
                            panel.height.saturating_sub(3),
                        );
                        if let Some(editor) = self.ensure_provider_editor() {
                            let count = editor.filtered_indices().len();
                            if let Some(index) = scrollbar_index(
                                list_area,
                                mouse.column,
                                mouse.row,
                                count,
                                usize::from(list_area.height.saturating_sub(2)),
                            ) {
                                editor.selected = index.min(count.saturating_sub(1));
                                editor.search_active = false;
                                self.model_idx = editor.selected;
                                self.focus = Focus::Models;
                                return Ok(MouseAction::None);
                            }
                        }

                        if contains(search_area, mouse.column, mouse.row) {
                            self.focus = Focus::Models;
                            if let Some(editor) = self.ensure_provider_editor() {
                                editor.search_active = true;
                            }
                            return Ok(MouseAction::None);
                        }
                        if contains(list_area, mouse.column, mouse.row) {
                            self.focus = Focus::Models;
                            let clicked = if let Some(editor) = self.ensure_provider_editor() {
                                let offset =
                                    route_editor_offset(editor, list_area.height.saturating_sub(2));
                                let inner_y = list_area.y + 1;
                                if mouse.row >= inner_y {
                                    let index =
                                        offset + usize::from(mouse.row.saturating_sub(inner_y));
                                    let filtered = editor.filtered_indices();
                                    if index < filtered.len() {
                                        let was_selected =
                                            editor.selected == index && !editor.search_active;
                                        editor.selected = index;
                                        let should_toggle = !pi && mouse.column < list_area.x + 4;
                                        if should_toggle {
                                            editor.toggle_selected();
                                        } else {
                                            editor.search_active = false;
                                        }
                                        Some((index, should_toggle, was_selected))
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            if let Some((index, should_toggle, was_selected)) = clicked {
                                self.model_idx = index;
                                if should_toggle {
                                    self.commit_provider_editor()?;
                                } else if was_selected
                                    && mouse.column >= list_area.x + 4
                                    && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                                {
                                    self.edit_model();
                                }
                                return Ok(MouseAction::None);
                            }
                        }
                    } else {
                        let models = self.models();
                        if let Some(index) = scrollbar_index(
                            panel,
                            mouse.column,
                            mouse.row,
                            models.len(),
                            usize::from(panel.height.saturating_sub(2) / 2),
                        ) {
                            self.focus = Focus::Models;
                            self.model_idx = index;
                            self.status_error = false;
                            self.status = format!(
                                "Selected {} · {}",
                                models[index].label(),
                                if self.pi_enabled {
                                    "e edit · p default · x delete"
                                } else {
                                    "Space toggles availability"
                                }
                            );
                            return Ok(MouseAction::None);
                        }
                    }
                }

                if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
                    return Ok(MouseAction::None);
                }
                for (control, rect) in self.provider_page_layout(area).controls {
                    if !contains(rect, mouse.column, mouse.row) {
                        continue;
                    }
                    if let Some(editor) = &mut self.provider_editor {
                        editor.search_active = false;
                    }
                    self.all_models_filter.active = false;
                    return Ok(match control {
                        FooterControl::Back => {
                            if self.back_one_level()? {
                                MouseAction::Quit
                            } else {
                                MouseAction::None
                            }
                        }
                        FooterControl::Models => {
                            self.focus = Focus::Models;
                            MouseAction::None
                        }
                        FooterControl::Details => {
                            self.focus = Focus::Details;
                            MouseAction::None
                        }
                        FooterControl::DeleteProfile => {
                            self.modal = Some(Modal::DeleteProfile);
                            MouseAction::None
                        }
                        FooterControl::AddModel => {
                            self.open_add_model_modal();
                            MouseAction::None
                        }
                        FooterControl::AddProfile => {
                            self.new_profile();
                            MouseAction::None
                        }
                        FooterControl::Sync => {
                            self.sync_all_to_claude();
                            MouseAction::None
                        }
                        FooterControl::Disconnect => {
                            self.run_help_action(HelpAction::Disconnect)?;
                            MouseAction::None
                        }
                        FooterControl::Proxy => {
                            self.open_proxy_manager();
                            MouseAction::None
                        }
                        FooterControl::Settings => {
                            self.open_settings_menu();
                            MouseAction::None
                        }
                        FooterControl::Help => {
                            self.open_help();
                            MouseAction::None
                        }
                        FooterControl::Quit => MouseAction::Quit,
                    });
                }

                if let Some(panel) = ui.profiles
                    && contains(panel, mouse.column, mouse.row)
                {
                    let index = if self.view_mode == ViewMode::Home {
                        clicked_variable_item(
                            panel,
                            mouse.row,
                            self.profile_offset,
                            &self.home_profile_item_heights(panel),
                        )
                    } else {
                        clicked_list_index(panel, mouse.column, mouse.row, self.profile_offset, 1)
                    };
                    let item_count = if self.view_mode == ViewMode::Home {
                        self.config
                            .profiles
                            .len()
                            .saturating_add(self.home_prefix_count())
                    } else {
                        self.config.profiles.len()
                    };
                    if let Some(index) = index.filter(|index| *index < item_count) {
                        self.focus = Focus::Profiles;
                        if self.view_mode == ViewMode::Home {
                            if !pi
                                && self.codex_ui.enabled
                                && index == 1
                                && mouse.column < panel.x.saturating_add(5)
                            {
                                self.select_home_index(index);
                                self.toggle_codex_subscription();
                                return Ok(MouseAction::None);
                            }
                            if !pi
                                && index >= self.home_prefix_count()
                                && mouse.column < panel.x.saturating_add(5)
                            {
                                self.select_home_index(index);
                                self.toggle_selected_provider()?;
                                return Ok(MouseAction::None);
                            }
                            if self.home_selected_index() == index {
                                if index < self.home_prefix_count() {
                                    self.enter_all_enabled_view();
                                } else {
                                    self.enter_provider_view();
                                }
                            } else {
                                self.select_home_index(index);
                            }
                        } else {
                            self.profile_idx = index;
                            self.model_idx = self.default_model_index();
                            self.model_offset = 0;
                        }
                    }
                } else if let Some(panel) = ui.models
                    && self.view_mode != ViewMode::Provider
                    && let Some(index) =
                        clicked_list_index(panel, mouse.column, mouse.row, self.model_offset, 2)
                {
                    if index < self.models().len() {
                        self.focus = Focus::Models;
                        self.model_idx = index;
                        if let Some(model) = self.selected_model() {
                            self.status_error = false;
                            self.status = format!(
                                "Selected {} · {}",
                                model.label(),
                                if self.pi_enabled {
                                    "e edit · p default · x delete"
                                } else {
                                    "Space toggles availability"
                                }
                            );
                        }
                    }
                } else if let Some(details) = ui.details
                    && contains(details, mouse.column, mouse.row)
                {
                    self.focus = Focus::Details;
                    if self.view_mode == ViewMode::Provider {
                        let (showcase_card, provider_card) = provider_detail_cards(details);
                        if contains(showcase_card, mouse.column, mouse.row) {
                            if let Some((control, _)) =
                                showcase_controls(showcase_card, self.pi_enabled)
                                    .into_iter()
                                    .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                            {
                                match control {
                                    ShowcaseControl::Toggle => {
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
                                        return Ok(MouseAction::None);
                                    }
                                    ShowcaseControl::Default => {
                                        self.set_selected_as_default();
                                        return Ok(MouseAction::None);
                                    }
                                    ShowcaseControl::OneM => {
                                        self.toggle_selected_model_1m();
                                        return Ok(MouseAction::None);
                                    }
                                    ShowcaseControl::Test => {
                                        self.start_model_test();
                                        return Ok(MouseAction::None);
                                    }
                                    ShowcaseControl::Delete => {
                                        self.delete_selected_model();
                                        return Ok(MouseAction::None);
                                    }
                                }
                            }
                        } else if contains(provider_card, mouse.column, mouse.row) {
                            if let Some((control, _)) = detail_controls(provider_card)
                                .into_iter()
                                .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                            {
                                self.provider_card_selected = false;
                                match control {
                                    DetailControl::Delete => {
                                        self.modal = Some(Modal::DeleteProfile)
                                    }
                                    DetailControl::Edit => self.edit_profile(),
                                }
                                return Ok(MouseAction::None);
                            }
                            if self.provider_card_selected {
                                self.provider_card_selected = false;
                                self.edit_profile();
                            } else {
                                self.provider_card_selected = true;
                                self.status_error = false;
                                self.status = "Provider selected · click again to edit".into();
                            }
                            return Ok(MouseAction::None);
                        }
                    } else if let Some((control, _)) = detail_controls(details)
                        .into_iter()
                        .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
                    {
                        match control {
                            DetailControl::Delete => self.modal = Some(Modal::DeleteProfile),
                            DetailControl::Edit => {
                                self.edit_profile();
                            }
                        }
                        return Ok(MouseAction::None);
                    }
                }
            }
            _ => {}
        }
        Ok(MouseAction::None)
    }
}
