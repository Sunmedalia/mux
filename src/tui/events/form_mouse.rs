use super::*;

impl App {
    pub(in crate::tui) fn handle_modal_mouse(
        &mut self,
        mouse: MouseEvent,
        screen: Rect,
    ) -> Result<()> {
        let Some(modal) = self.modal.as_ref() else {
            return Ok(());
        };
        let area = if self.settings_menu.is_some()
            && matches!(
                modal,
                Modal::Preferences(_) | Modal::Grok(_) | Modal::Proxy(_)
            ) {
            settings::client_editor_area(screen)
        } else {
            modal_area_for(modal, screen)
        };
        if matches!(modal, Modal::Proxy(manager) if manager.port_field.is_some() || manager.resource_fields.is_some())
        {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some(Modal::Proxy(manager)) = &mut self.modal
                    && let Some(fields) = &manager.resource_fields
                {
                    let inner = panel_inner(area);
                    let content = Rect::new(
                        inner.x,
                        inner.y,
                        inner.width,
                        inner.height.saturating_sub(4),
                    );
                    let (content, offset) = form_viewport(content, manager.resource_selected);
                    if contains(content, mouse.column, mouse.row) {
                        let row = offset + usize::from(mouse.row - content.y);
                        if row < fields.len() {
                            manager.resource_selected = row;
                        }
                    }
                }
                let buttons = modal_button_rects(area, 2);
                if let Some(index) = buttons
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
                }
            }
            return Ok(());
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if let Some(Modal::Model(form)) = self.modal.as_mut() {
                    let inner = panel_inner(area);
                    let content_area = Rect::new(
                        inner.x,
                        inner.y,
                        inner.width,
                        inner.height.saturating_sub(2),
                    );
                    let (_, api_area) = model_form_areas(content_area, form.focus_api_search);
                    let visible_height =
                        usize::from(panel_inner(api_area).height.saturating_sub(2));
                    form.scroll_api_list(false, 3, visible_height);
                    return Ok(());
                }
                self.handle_modal(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
                return Ok(());
            }
            MouseEventKind::ScrollDown => {
                if let Some(Modal::Model(form)) = self.modal.as_mut() {
                    let inner = panel_inner(area);
                    let content_area = Rect::new(
                        inner.x,
                        inner.y,
                        inner.width,
                        inner.height.saturating_sub(2),
                    );
                    let (_, api_area) = model_form_areas(content_area, form.focus_api_search);
                    let visible_height =
                        usize::from(panel_inner(api_area).height.saturating_sub(2));
                    form.scroll_api_list(true, 3, visible_height);
                    return Ok(());
                }
                self.handle_modal(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
                return Ok(());
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {}
            _ => return Ok(()),
        }
        if !contains(area, mouse.column, mouse.row) {
            return Ok(());
        }

        if let Some(Modal::Model(form)) = self.modal.as_mut() {
            let inner = panel_inner(area);
            let content = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(2),
            );
            let (_, api_area) = model_form_areas(content, form.focus_api_search);
            let api_inner = panel_inner(api_area);
            let list_area = Rect::new(
                api_inner.x,
                api_inner.y.saturating_add(2),
                api_inner.width,
                api_inner.height.saturating_sub(2),
            );
            let count = form.filtered_api_models().len();
            if let Some(index) = scrollbar_index(
                list_area,
                mouse.column,
                mouse.row,
                count,
                usize::from(list_area.height),
            ) {
                form.api_scroll = index.min(count.saturating_sub(usize::from(list_area.height)));
                form.api_selected = form.api_scroll;
                form.focus_api_search = true;
                return Ok(());
            }
        }

        if matches!(self.modal, Some(Modal::Proxy(_))) {
            if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
                return Ok(());
            }
            if let Some((control, _)) = proxy_controls(area)
                .into_iter()
                .find(|(_, rect)| contains(*rect, mouse.column, mouse.row))
            {
                self.handle_modal(proxy_control_key(control))?;
                return Ok(());
            }
        } else if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            return Ok(());
        }

        if matches!(&self.modal, Some(Modal::Preferences(form)) if form.discard) {
            if let Some(button) = modal_button_rects(area, 2)
                .iter()
                .position(|rect| contains(*rect, mouse.column, mouse.row))
            {
                self.handle_modal(KeyEvent::new(
                    KeyCode::Char(if button == 0 { 'y' } else { 'n' }),
                    KeyModifiers::NONE,
                ))?;
            }
            return Ok(());
        }
        if matches!(self.modal, Some(Modal::Grok(_))) {
            self.grok_dialog_mouse(mouse, area)?;
            return Ok(());
        }
        if matches!(self.modal, Some(Modal::Preferences(_))) {
            if let Some(action) = preference_actions(area)
                .iter()
                .position(|rect| contains(*rect, mouse.column, mouse.row))
            {
                self.handle_modal(KeyEvent::new(
                    KeyCode::Char(['d', 'v', 'x'][action]),
                    KeyModifiers::ALT,
                ))?;
                return Ok(());
            }
            if let Some(button) = modal_button_rects(area, 4)
                .iter()
                .position(|rect| contains(*rect, mouse.column, mouse.row))
            {
                let key = match button {
                    0 => KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
                    1 => KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT),
                    2 => KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                    _ => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                };
                self.handle_modal(key)?;
            } else if let Some(Modal::Preferences(form)) = self.modal.as_mut() {
                let inner = panel_inner(area);
                let content = Rect::new(
                    inner.x,
                    inner.y,
                    inner.width,
                    inner.height.saturating_sub(4),
                );
                if contains(content, mouse.column, mouse.row) {
                    let (_, offset) = form_viewport(content, form.selected);
                    let index = offset + usize::from(mouse.row - content.y);
                    if index < form.fields.len() {
                        form.selected = index;
                        if !form.fields[index].choices.is_empty() {
                            cycle_choice(&mut form.fields[index], true);
                        }
                    }
                }
            }
            return Ok(());
        }
        if let Some(Modal::Help(help)) = self.modal.as_ref() {
            if let Some(index) = help_tab_at(area, mouse.column, mouse.row) {
                if let Some(Modal::Help(help)) = &mut self.modal {
                    help.select(index);
                }
            } else if let Some(action) = help_action_at(help, area, mouse.column, mouse.row) {
                self.run_help_action(action)?;
            } else if let Some(index) = help_navigation_at(area, mouse.column, mouse.row) {
                let code = [KeyCode::BackTab, KeyCode::Tab, KeyCode::Esc][index];
                self.handle_modal(KeyEvent::new(code, KeyModifiers::NONE))?;
            }
            return Ok(());
        }
        let button_count = match self.modal.as_ref() {
            Some(Modal::Help(_)) => 1,
            Some(Modal::Proxy(_)) => 0,
            Some(Modal::Model(_) | Modal::Profile(_)) => 3,
            Some(_) => 2,
            None => 0,
        };
        if let Some(button) = modal_button_rects(area, button_count)
            .iter()
            .position(|rect| contains(*rect, mouse.column, mouse.row))
        {
            let key = match (self.modal.as_ref(), button) {
                (Some(Modal::Profile(form)), button) if form.template_selected.is_some() => {
                    match button {
                        0 => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                        1 => {
                            self.use_provider_template(0);
                            return Ok(());
                        }
                        _ => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    }
                }
                (Some(Modal::Profile(form)), 0) => {
                    self.fetch_profile_models(form.picker.is_some());
                    return Ok(());
                }
                (Some(Modal::Profile(form)), 1) if form.picker.is_some() => {
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
                }
                (Some(Modal::Profile(_)), 2) => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                (Some(Modal::Model(_)), 0) => {
                    self.fetch_api_models_for_form();
                    return Ok(());
                }
                (Some(Modal::Model(_)), 1) => {
                    KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
                }
                (Some(Modal::Model(_)), 2) => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                (Some(Modal::Import(_)), 0)
                | (Some(Modal::DeleteProfile | Modal::DeleteModel), 0) => {
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
                }
                (Some(Modal::Profile(_)), 1) => {
                    KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
                }
                (Some(Modal::Help(_)), 0) => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                (Some(_), 1) => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                _ => return Ok(()),
            };
            self.handle_modal(key)?;
            return Ok(());
        }

        let inner = panel_inner(area);
        let clicked_field = contains(inner, mouse.column, mouse.row)
            .then(|| usize::from(mouse.row.saturating_sub(inner.y)));
        match self.modal.as_mut() {
            Some(Modal::Profile(form)) => {
                if let Some(selected) = &mut form.template_selected {
                    if contains(inner, mouse.column, mouse.row) && mouse.row >= inner.y + 2 {
                        let index = usize::from(mouse.row - inner.y - 2);
                        if index <= PROVIDER_TEMPLATES.len() {
                            *selected = index;
                        }
                    }
                    return Ok(());
                }
                if let Some(picker) = &mut form.picker {
                    let api_area = Rect::new(
                        inner.x,
                        inner.y,
                        inner.width,
                        inner.height.saturating_sub(2),
                    );
                    let api_inner = panel_inner(api_area);
                    if contains(api_inner, mouse.column, mouse.row) && mouse.row == api_inner.y {
                        form.picker_search = true;
                    }
                    if contains(api_inner, mouse.column, mouse.row)
                        && mouse.row >= api_inner.y.saturating_add(2)
                    {
                        let index = picker.api_scroll + usize::from(mouse.row - api_inner.y - 2);
                        if let Some(id) = picker
                            .filtered_api_models()
                            .get(index)
                            .map(|m| m.id.clone())
                        {
                            form.fill_selected_model(&id);
                            form.picker = None;
                        }
                    }
                    return Ok(());
                }
                let content = Rect::new(
                    inner.x,
                    inner.y,
                    inner.width,
                    inner.height.saturating_sub(4),
                );
                let (_, offset) = form_viewport(content, form.selected);
                if let Some(index) = clicked_field
                    .filter(|_| contains(content, mouse.column, mouse.row))
                    .map(|index| index + offset)
                    && index < form.fields.len()
                {
                    form.selected = index;
                    if index == 3
                        && contains(
                            profile_test_rect(content, (index - offset) as u16),
                            mouse.column,
                            mouse.row,
                        )
                    {
                        self.start_profile_connection_test();
                        return Ok(());
                    }
                    if (6..=12).contains(&index)
                        && form.fields[index].label != "Fetch models URL"
                        && contains(
                            profile_test_rect(content, (index - offset) as u16),
                            mouse.column,
                            mouse.row,
                        )
                    {
                        self.start_profile_model_test();
                        return Ok(());
                    }
                    if (6..=12).contains(&index)
                        && form.fields[index].label != "Fetch models URL"
                        && contains(
                            profile_1m_rect(content, (index - offset) as u16),
                            mouse.column,
                            mouse.row,
                        )
                    {
                        form.toggle_model_field_1m(index);
                    } else if !form.fields[index].choices.is_empty() {
                        cycle_choice(&mut form.fields[index], true);
                    }
                }
            }
            Some(Modal::Model(form)) => {
                let content_area = Rect::new(
                    inner.x,
                    inner.y,
                    inner.width,
                    inner.height.saturating_sub(2),
                );
                let (form_area, api_area) = model_form_areas(content_area, form.focus_api_search);
                if contains(form_area, mouse.column, mouse.row) {
                    form.focus_api_search = false;
                    let form_inner = panel_inner(form_area);
                    let clicked_field = contains(form_inner, mouse.column, mouse.row)
                        .then(|| usize::from(mouse.row.saturating_sub(form_inner.y)));
                    let (_, offset) = form_viewport(form_inner, form.selected);
                    if let Some(index) = clicked_field.map(|index| index + offset)
                        && index < form.fields.len()
                    {
                        form.selected = index;
                        if form.fields[index].toggle {
                            toggle_form_field(&mut form.fields[index]);
                        } else if !form.fields[index].choices.is_empty() {
                            let value_x = form_inner.x + (form_inner.width / 3).min(17) + 2;
                            cycle_choice(&mut form.fields[index], mouse.column > value_x);
                        }
                    }
                } else if contains(api_area, mouse.column, mouse.row) {
                    let api_inner = panel_inner(api_area);
                    if mouse.row == api_inner.y {
                        form.focus_api_search = true;
                    } else if contains(api_inner, mouse.column, mouse.row)
                        && mouse.row >= api_inner.y.saturating_add(2)
                    {
                        let list_row =
                            usize::from(mouse.row.saturating_sub(api_inner.y.saturating_add(2)));
                        let item_idx = form.api_scroll + list_row;
                        if form.click_api_model(item_idx).is_some() {
                            form.pick_api_model(item_idx);
                            form.focus_api_search = false;
                            form.api_clicked = None;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
