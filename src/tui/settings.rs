use super::*;

#[derive(Clone)]
pub(super) struct SettingsMenu {
    pub selected: usize,
    pub return_usage: bool,
    pub editing: bool,
    pub row: usize,
    pub draft: config::UiPreferences,
    original: config::UiPreferences,
    pub theme: theme::Theme,
    saved_theme: theme::Theme,
    pulse: theme::PulseTheme,
    saved_pulse: theme::PulseTheme,
    refresh: u64,
    saved_refresh: u64,
    reasoning: Option<String>,
    saved_reasoning: Option<String>,
    pub message: Option<String>,
}

impl SettingsMenu {
    pub fn dirty(&self) -> bool {
        self.draft != self.original
            || self.theme != self.saved_theme
            || self.pulse != self.saved_pulse
            || self.refresh != self.saved_refresh
            || self.reasoning != self.saved_reasoning
    }
    fn fields(&self) -> Vec<(&'static str, String)> {
        let options = |kind| UiOptions {
            kind,
            selected: 0,
            original: self.original.clone(),
            edited: self.draft.clone(),
            error: None,
            return_appearance: None,
        };
        match self.selected {
            0 => vec![(
                "Color theme",
                self.theme.name().split(" / ").next().unwrap().into(),
            )],
            1 => {
                let o = options(OptionsKind::Pulse);
                vec![
                    (
                        "Color theme",
                        self.pulse.name().split(" / ").next().unwrap().into(),
                    ),
                    ("View", o.value(0)),
                    ("Model details", o.value(1)),
                    ("Start page", o.value(2)),
                    ("Sessions sort", o.value(3)),
                ]
            }
            2 => vec![("Refresh interval", format!("{}s", self.refresh))],
            3 => {
                let o = options(OptionsKind::Models);
                ["Claude 1M", "Claude enable", "Codex enable", "Grok enable"]
                    .into_iter()
                    .enumerate()
                    .map(|(i, l)| (l, o.value(i)))
                    .collect()
            }
            5 => vec![(
                "Reasoning effort",
                self.reasoning.clone().unwrap_or("Default".into()),
            )],
            _ => vec![("Open settings", "Enter →".into())],
        }
    }
    fn change(&mut self, forward: bool) {
        fn cycle<T: Copy + PartialEq>(v: &mut T, all: &[T], forward: bool) {
            let i = all.iter().position(|x| x == v).unwrap_or(0);
            *v = all[(i + if forward { 1 } else { all.len() - 1 }) % all.len()];
        }
        match self.selected {
            0 => cycle(&mut self.theme, &theme::Theme::ALL, forward),
            1 if self.row == 0 => cycle(&mut self.pulse, &theme::PulseTheme::ALL, forward),
            1 | 3 => {
                let mut o = UiOptions {
                    kind: if self.selected == 1 {
                        OptionsKind::Pulse
                    } else {
                        OptionsKind::Models
                    },
                    selected: if self.selected == 1 {
                        self.row - 1
                    } else {
                        self.row
                    },
                    original: self.original.clone(),
                    edited: self.draft.clone(),
                    error: None,
                    return_appearance: None,
                };
                o.change(forward);
                self.draft = o.edited;
            }
            2 => {
                self.refresh = if forward {
                    (self.refresh + 1).min(60)
                } else {
                    self.refresh.saturating_sub(1).max(1)
                }
            }
            5 => {
                let levels = [
                    None,
                    Some("none".into()),
                    Some("minimal".into()),
                    Some("low".into()),
                    Some("medium".into()),
                    Some("high".into()),
                    Some("xhigh".into()),
                ];
                let i = levels
                    .iter()
                    .position(|v| v == &self.reasoning)
                    .unwrap_or(0);
                self.reasoning =
                    levels[(i + if forward { 1 } else { levels.len() - 1 }) % levels.len()].clone();
            }
            _ => {}
        }
        self.message = None;
    }
}

const SECTIONS: [(&str, &str); 8] = [
    ("Display", "Editor theme and preview"),
    ("Pulse", "Monitor theme and display"),
    ("Usage", "Refresh interval"),
    ("New models", "Claude 1M and enable defaults"),
    ("Claude", "Environment and attribution"),
    ("Codex", "Reasoning effort"),
    ("Grok", "Native model and UI settings"),
    ("Proxy", "Listen port and service"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OptionsKind {
    Pulse,
    Models,
}

#[derive(Clone)]
pub(super) struct UiOptions {
    pub kind: OptionsKind,
    pub selected: usize,
    pub original: config::UiPreferences,
    pub edited: config::UiPreferences,
    pub error: Option<String>,
    pub return_appearance: Option<theme::Appearance>,
}

#[derive(Clone)]
pub(super) struct CodexSettings {
    pub original: Option<String>,
    pub edited: String,
    pub error: Option<String>,
}

pub(super) fn settings_rows(area: Rect, selected: usize) -> Vec<(usize, Rect)> {
    let inner = panel_inner(area);
    let start = inner.y + 2;
    let visible = usize::from(area.height.saturating_sub(6)).max(1);
    let offset = selected.saturating_sub(visible - 1);
    (offset..SECTIONS.len().min(offset + visible))
        .map(|index| {
            (
                index,
                Rect::new(inner.x, start + (index - offset) as u16, inner.width, 1),
            )
        })
        .collect()
}

pub(super) fn option_rows(area: Rect, count: usize) -> Vec<Rect> {
    let inner = panel_inner(area);
    (0..count)
        .map(|index| Rect::new(inner.x, inner.y + 3 + index as u16, inner.width, 1))
        .collect()
}

fn panel_body(frame: &mut ratatui::Frame, area: Rect, title: &str, hint: &str) {
    frame.render_widget(panel(title, true), area);
    let inner = panel_inner(area);
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(MUTED)),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
}

pub(super) fn settings_content(area: Rect) -> Rect {
    let inner = panel_inner(area);
    if area.width >= 76 {
        Rect::new(
            inner.x + 23,
            inner.y,
            inner.width.saturating_sub(24).min(78),
            inner.height,
        )
    } else {
        inner
    }
}

pub(super) fn client_editor_area(screen: Rect) -> Rect {
    let page = settings_page_area(screen);
    if page.width < 76 {
        return page;
    }
    Rect::new(
        page.x + 23,
        page.y + 1,
        page.width.saturating_sub(24),
        page.height.saturating_sub(2),
    )
}

pub(super) fn draw_client_sidebar(frame: &mut ratatui::Frame, area: Rect, menu: &SettingsMenu) {
    if area.width < 76 {
        return;
    }
    frame.render_widget(panel(" Settings ", true), area);
    let inner = panel_inner(area);
    frame.render_widget(
        Paragraph::new("PREFERENCES").style(Style::default().fg(MUTED)),
        Rect::new(inner.x + 1, inner.y, 20, 1),
    );
    for (index, mut rect) in settings_rows(area, menu.selected) {
        rect.width = 20;
        frame.render_widget(
            Paragraph::new(format!(
                " {} {}",
                if menu.selected == index { "›" } else { " " },
                SECTIONS[index].0
            ))
            .style(if menu.selected == index {
                Style::default()
                    .fg(ROUTE)
                    .bg(SELECTION)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED)
            }),
            rect,
        );
    }
    frame.render_widget(
        Paragraph::new("Esc back").style(Style::default().fg(MUTED)),
        Rect::new(inner.x + 1, area.bottom().saturating_sub(3), 20, 1),
    );
}

pub(super) fn settings_fields(area: Rect, menu: &SettingsMenu) -> Vec<(usize, Rect)> {
    if area.width < 76 && !menu.editing {
        return vec![];
    }
    let body = settings_content(area);
    let stride = if area.width >= 76 && area.height >= 22 {
        2
    } else {
        1
    };
    let visible = (area.height.saturating_sub(7) / stride).max(1) as usize;
    let offset = menu.row.saturating_sub(visible - 1);
    (offset..menu.fields().len().min(offset + visible))
        .map(|i| {
            (
                i,
                Rect::new(
                    body.x,
                    body.y + 3 + (i - offset) as u16 * stride,
                    body.width,
                    1,
                ),
            )
        })
        .collect()
}

pub(super) fn draw_menu(frame: &mut ratatui::Frame, area: Rect, menu: &SettingsMenu) {
    frame.render_widget(
        panel(
            if menu.dirty() {
                " Settings · unsaved "
            } else {
                " Settings "
            },
            true,
        ),
        area,
    );
    let inner = panel_inner(area);
    let wide = area.width >= 76;
    if wide {
        for y in inner.y..area.bottom().saturating_sub(3) {
            frame.render_widget(
                Paragraph::new("│").style(Style::default().fg(MUTED)),
                Rect::new(inner.x + 21, y, 1, 1),
            );
        }
        frame.render_widget(
            Paragraph::new("PREFERENCES").style(Style::default().fg(MUTED)),
            Rect::new(inner.x + 1, inner.y, 20, 1),
        );
    }
    if wide || !menu.editing {
        for (index, mut rect) in settings_rows(area, menu.selected) {
            if wide {
                rect.width = 20;
            }
            frame.render_widget(
                Paragraph::new(format!(
                    " {} {}",
                    if menu.selected == index { "›" } else { " " },
                    SECTIONS[index].0
                ))
                .style(if menu.selected == index {
                    Style::default()
                        .fg(ROUTE)
                        .bg(SELECTION)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED)
                }),
                rect,
            );
        }
    }
    if wide || menu.editing {
        let body = settings_content(area);
        frame.render_widget(
            Paragraph::new(SECTIONS[menu.selected].0).style(
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Rect::new(body.x, body.y, body.width, 1),
        );
        frame.render_widget(
            Paragraph::new(SECTIONS[menu.selected].1).style(Style::default().fg(MUTED)),
            Rect::new(body.x, body.y + 1, body.width, 1),
        );
        let fields = menu.fields();
        for (i, rect) in settings_fields(area, menu) {
            let (label, value) = &fields[i];
            let active = menu.editing && menu.row == i;
            let value = format!("‹ {value} ›");
            let space = usize::from(rect.width).saturating_sub(value.chars().count() + 3);
            frame.render_widget(
                Paragraph::new(format!(
                    "{} {label:space$} {value}",
                    if active { "›" } else { " " }
                ))
                .style(if active {
                    Style::default().fg(ROUTE).bg(SELECTION)
                } else {
                    Style::default().fg(Color::White)
                }),
                rect,
            );
        }
        if wide && area.height >= 22 {
            let descriptions: &[&str] = match menu.selected {
                0 => &["Preview colors immediately; save to keep your selection."],
                1 => &[
                    "Colors for the Pulse sidebar",
                    "Compact text or graphical charts",
                    "Expand per-model token usage",
                    "Page shown when Pulse opens",
                    "Order sessions by activity or token usage",
                ],
                2 => &["How often Usage requests fresh data"],
                3 => &[
                    "Check 1M context in new Claude model forms",
                    "Enable new Claude models by default",
                    "Enable new Codex models by default",
                    "Enable new Grok models by default",
                ],
                5 => &["Default inherits the client's reasoning behavior"],
                _ => &["Opens a dedicated editor; workspace drafts are retained"],
            };
            for (i, rect) in settings_fields(area, menu) {
                frame.render_widget(
                    Paragraph::new(descriptions[i]).style(Style::default().fg(MUTED)),
                    Rect::new(rect.x + 2, rect.y + 1, rect.width.saturating_sub(2), 1),
                );
            }
        }
        if area.height >= 17 {
            let note = match menu.selected {
                0 => "Live preview · Save to keep this theme.",
                1 => "Saved display changes update Pulse live. Start page applies on reopen.",
                2 => "Usage polling frequency · 1–60 seconds.",
                3 => "Defaults for new forms only. Existing models stay unchanged.",
                4 => "Edit Claude environment and attribution in its dedicated editor.",
                5 => "Save here, then Apply on the Codex tab.",
                6 => "Edit Grok models, permissions and interface preferences.",
                _ => "Manage the local proxy service and listen port.",
            };
            frame.render_widget(
                Paragraph::new(note)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(MUTED)),
                Rect::new(
                    body.x,
                    body.y + if wide && area.height >= 22 { 15 } else { 10 },
                    body.width,
                    2,
                ),
            );
        }
    }
    let hint = menu.message.as_deref().unwrap_or(if menu.editing {
        "↑↓ row · ←→ change · [ ] category"
    } else {
        "↑↓ category · Enter edit"
    });
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(MUTED)),
        Rect::new(inner.x, area.bottom().saturating_sub(3), inner.width, 1),
    );
    if !wide && menu.editing {
        let progress = format!(" {}/{} ", menu.row + 1, menu.fields().len());
        frame.render_widget(
            Paragraph::new(progress.clone()).style(Style::default().fg(MUTED)),
            Rect::new(
                area.right().saturating_sub(progress.len() as u16 + 2),
                area.y,
                progress.len() as u16,
                1,
            ),
        );
    }
    draw_modal_buttons(frame, area, &["Save ^S", "Back Esc"]);
}

impl UiOptions {
    fn len(&self) -> usize {
        4
    }

    fn value(&self, index: usize) -> String {
        match (self.kind, index) {
            (OptionsKind::Pulse, 0) => if self.edited.pulse_visual {
                "Graphical"
            } else {
                "Text"
            }
            .into(),
            (OptionsKind::Pulse, 1) => if self.edited.pulse_models {
                "Expanded"
            } else {
                "Collapsed"
            }
            .into(),
            (OptionsKind::Pulse, 2) => match self.edited.pulse_start_page {
                config::PulseStartPage::Home => "Home",
                config::PulseStartPage::Sessions => "Sessions",
                config::PulseStartPage::Charts => "Charts",
            }
            .into(),
            (OptionsKind::Pulse, _) => if self.edited.pulse_sort_tokens {
                "Tokens"
            } else {
                "Recent"
            }
            .into(),
            (OptionsKind::Models, 0) => if self.edited.claude_new_model_1m {
                "On"
            } else {
                "Off"
            }
            .into(),
            (OptionsKind::Models, 1) => if self.edited.claude_new_model_enabled {
                "On"
            } else {
                "Off"
            }
            .into(),
            (OptionsKind::Models, 2) => if self.edited.codex_new_model_enabled {
                "On"
            } else {
                "Off"
            }
            .into(),
            (OptionsKind::Models, _) => if self.edited.grok_new_model_enabled {
                "On"
            } else {
                "Off"
            }
            .into(),
        }
    }

    pub(super) fn change(&mut self, forward: bool) {
        match (self.kind, self.selected) {
            (OptionsKind::Pulse, 0) => self.edited.pulse_visual = !self.edited.pulse_visual,
            (OptionsKind::Pulse, 1) => self.edited.pulse_models = !self.edited.pulse_models,
            (OptionsKind::Pulse, 2) => {
                self.edited.pulse_start_page = match (self.edited.pulse_start_page, forward) {
                    (config::PulseStartPage::Home, true)
                    | (config::PulseStartPage::Charts, false) => config::PulseStartPage::Sessions,
                    (config::PulseStartPage::Sessions, true) => config::PulseStartPage::Charts,
                    (config::PulseStartPage::Sessions, false) => config::PulseStartPage::Home,
                    (config::PulseStartPage::Charts, true) => config::PulseStartPage::Home,
                    (config::PulseStartPage::Home, false) => config::PulseStartPage::Charts,
                }
            }
            (OptionsKind::Pulse, _) => {
                self.edited.pulse_sort_tokens = !self.edited.pulse_sort_tokens
            }
            (OptionsKind::Models, 0) => {
                self.edited.claude_new_model_1m = !self.edited.claude_new_model_1m
            }
            (OptionsKind::Models, 1) => {
                self.edited.claude_new_model_enabled = !self.edited.claude_new_model_enabled
            }
            (OptionsKind::Models, 2) => {
                self.edited.codex_new_model_enabled = !self.edited.codex_new_model_enabled
            }
            (OptionsKind::Models, _) => {
                self.edited.grok_new_model_enabled = !self.edited.grok_new_model_enabled
            }
        }
        self.error = None;
    }
}

pub(super) fn draw_options(frame: &mut ratatui::Frame, area: Rect, options: &UiOptions) {
    panel_body(
        frame,
        area,
        if options.kind == OptionsKind::Pulse {
            if area.width < 56 {
                " Pulse UI "
            } else {
                " Pulse display "
            }
        } else if area.width < 56 {
            " Defaults "
        } else {
            " New model defaults "
        },
        "↑↓ row · ←→ change · Enter save",
    );
    let labels = if options.kind == OptionsKind::Pulse {
        ["View", "Model details", "Start page", "Sessions sort"]
    } else {
        ["Claude 1M", "Claude enable", "Codex enable", "Grok enable"]
    };
    for (index, rect) in option_rows(area, options.len()).into_iter().enumerate() {
        frame.render_widget(
            Paragraph::new(format!(
                "{} {:<19} {}",
                if options.selected == index {
                    "▶"
                } else {
                    " "
                },
                labels[index],
                options.value(index)
            ))
            .style(if options.selected == index {
                Style::default().fg(ROUTE).bg(SELECTION)
            } else {
                Style::default().fg(Color::White)
            }),
            rect,
        );
    }
    if let Some(error) = &options.error {
        let inner = panel_inner(area);
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(ERROR)),
            Rect::new(inner.x, area.bottom().saturating_sub(3), inner.width, 1),
        );
    }
    draw_modal_buttons(frame, area, &["Save", "Back"]);
}

pub(super) fn draw_codex_settings(frame: &mut ratatui::Frame, area: Rect, form: &CodexSettings) {
    panel_body(
        frame,
        area,
        if area.width < 56 {
            " Codex "
        } else {
            " Codex settings "
        },
        "←→ effort · Enter save · Esc back",
    );
    let inner = panel_inner(area);
    frame.render_widget(
        Paragraph::new(format!("Reasoning effort    {}", form.edited))
            .style(Style::default().fg(ROUTE)),
        Rect::new(inner.x, inner.y + 3, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new("none · minimal · low · medium · high · xhigh")
            .style(Style::default().fg(MUTED)),
        Rect::new(inner.x, inner.y + 5, inner.width, 1),
    );
    if let Some(error) = &form.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(ERROR)),
            Rect::new(inner.x, area.bottom().saturating_sub(3), inner.width, 1),
        );
    }
    draw_modal_buttons(frame, area, &["Save", "Back"]);
}

impl App {
    pub(super) fn open_settings_menu(&mut self) {
        let global = match config::load(&self.paths.config) {
            Ok(config) => config,
            Err(error) => {
                self.set_error(format!("Could not read global settings: {error}"));
                return;
            }
        };
        self.config.ui = global.ui;
        self.config.usage_refresh_secs = global.usage_refresh_secs;
        self.config.codex.reasoning_effort = global.codex.reasoning_effort;
        let menu = SettingsMenu {
            selected: 0,
            editing: false,
            row: 0,
            draft: self.config.ui.clone(),
            original: self.config.ui.clone(),
            theme: self.theme,
            saved_theme: self.theme,
            pulse: theme::PulseTheme::load(&self.paths),
            saved_pulse: theme::PulseTheme::load(&self.paths),
            refresh: self.config.usage_refresh_secs,
            saved_refresh: self.config.usage_refresh_secs,
            reasoning: self.config.codex.reasoning_effort.clone(),
            saved_reasoning: self.config.codex.reasoning_effort.clone(),
            message: None,
            return_usage: self
                .settings_menu
                .as_ref()
                .map_or(self.usage.active, |menu| menu.return_usage),
        };
        self.usage.active = false;
        self.settings_menu = Some(menu.clone());
        self.modal = Some(Modal::SettingsMenu(menu));
    }

    pub(super) fn return_settings_menu(&mut self) {
        if let Some(menu) = &self.settings_menu {
            self.modal = Some(Modal::SettingsMenu(menu.clone()));
        }
    }

    pub(super) fn open_settings_section(&mut self, index: usize) {
        if let Some(menu) = &mut self.settings_menu {
            menu.selected = index.min(SECTIONS.len() - 1);
        }
        match index {
            0..=2 => {
                self.open_appearance();
                if let Some(Modal::Appearance(form)) = &mut self.modal {
                    form.return_usage = self.settings_menu.as_ref().is_some_and(|m| m.return_usage);
                    form.pulse_selected = index == 1;
                    form.refresh_selected = index == 2;
                }
            }
            3 => {
                self.modal = Some(Modal::UiOptions(UiOptions {
                    kind: OptionsKind::Models,
                    selected: 0,
                    original: self.config.ui.clone(),
                    edited: self.config.ui.clone(),
                    error: None,
                    return_appearance: None,
                }))
            }
            4 => self.open_preferences(),
            5 => {
                self.modal = Some(Modal::CodexSettings(CodexSettings {
                    original: self.config.codex.reasoning_effort.clone(),
                    edited: self
                        .config
                        .codex
                        .reasoning_effort
                        .clone()
                        .unwrap_or_else(|| "medium".into()),
                    error: None,
                }))
            }
            6 => self.open_grok_preferences(None),
            7 => self.open_proxy_manager(),
            _ => {}
        }
    }

    pub(super) fn settings_menu_key(&mut self, menu: &mut SettingsMenu, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut saved_groups = Vec::new();
                let mut failures = Vec::new();
                let result = (|| -> Result<()> {
                    let saved = config::try_update(&self.paths.config, |latest| {
                        macro_rules! merge {
                            ($field:ident) => {
                                if menu.draft.$field != menu.original.$field {
                                    if latest.ui.$field != menu.original.$field
                                        && latest.ui.$field != menu.draft.$field
                                    {
                                        anyhow::bail!("{} changed elsewhere", stringify!($field));
                                    }
                                    latest.ui.$field = menu.draft.$field;
                                }
                            };
                        }
                        merge!(pulse_visual);
                        merge!(pulse_models);
                        merge!(pulse_start_page);
                        merge!(pulse_sort_tokens);
                        merge!(claude_new_model_1m);
                        merge!(claude_new_model_enabled);
                        merge!(codex_new_model_enabled);
                        merge!(grok_new_model_enabled);
                        if menu.refresh != menu.saved_refresh {
                            if latest.usage_refresh_secs != menu.saved_refresh
                                && latest.usage_refresh_secs != menu.refresh
                            {
                                anyhow::bail!("Refresh interval changed elsewhere");
                            }
                            latest.usage_refresh_secs = menu.refresh;
                        }
                        if menu.reasoning != menu.saved_reasoning {
                            if latest.codex.reasoning_effort != menu.saved_reasoning
                                && latest.codex.reasoning_effort != menu.reasoning
                            {
                                anyhow::bail!("Reasoning changed elsewhere");
                            }
                            latest.codex.reasoning_effort = menu.reasoning.clone();
                        }
                        Ok(())
                    })?;
                    menu.draft = saved.ui;
                    menu.refresh = saved.usage_refresh_secs;
                    menu.reasoning = saved.codex.reasoning_effort;
                    self.config.ui = menu.draft.clone();
                    self.config.usage_refresh_secs = menu.refresh;
                    self.config.codex.reasoning_effort = menu.reasoning.clone();
                    menu.original = menu.draft.clone();
                    menu.saved_refresh = menu.refresh;
                    menu.saved_reasoning = menu.reasoning.clone();
                    Ok(())
                })();
                match result {
                    Ok(()) => saved_groups.push("preferences"),
                    Err(error) => failures.push(format!("preferences: {error}")),
                }
                if menu.theme != menu.saved_theme {
                    match menu.theme.save(&self.paths) {
                        Ok(()) => {
                            self.theme = menu.theme;
                            menu.saved_theme = menu.theme;
                            saved_groups.push("theme");
                        }
                        Err(error) => failures.push(format!("theme: {error}")),
                    }
                }
                if menu.pulse != menu.saved_pulse {
                    match menu.pulse.save(&self.paths) {
                        Ok(()) => {
                            menu.saved_pulse = menu.pulse;
                            saved_groups.push("Pulse theme");
                        }
                        Err(error) => failures.push(format!("Pulse theme: {error}")),
                    }
                }
                menu.message = Some(if failures.is_empty() {
                    "Saved · all changes are up to date".into()
                } else if saved_groups.is_empty() {
                    format!("Cannot save: {} · drafts retained", failures.join("; "))
                } else {
                    format!(
                        "Partially saved: {} · failed: {} · Ctrl+S retry",
                        saved_groups.join(", "),
                        failures.join("; ")
                    )
                });
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                if menu.editing {
                    menu.editing = false;
                } else if menu.dirty() {
                    menu.message = Some("Unsaved · Ctrl+S save · Ctrl+R discard".into());
                } else {
                    self.usage.active = menu.return_usage;
                    self.settings_menu = None;
                    return true;
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                menu.draft = menu.original.clone();
                menu.theme = menu.saved_theme;
                menu.pulse = menu.saved_pulse;
                menu.refresh = menu.saved_refresh;
                menu.reasoning = menu.saved_reasoning.clone();
                menu.message = Some("Unsaved changes discarded".into());
            }
            KeyCode::Char('[' | ']') => {
                menu.selected =
                    (menu.selected + if key.code == KeyCode::Char(']') { 1 } else { 7 }) % 8;
                menu.row = 0;
            }
            KeyCode::Tab | KeyCode::BackTab => menu.editing = !menu.editing,
            KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') => {
                let forward = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
                let field_count = menu.fields().len();
                let (index, len) = if menu.editing {
                    (&mut menu.row, field_count)
                } else {
                    (&mut menu.selected, 8)
                };
                *index = (*index + if forward { 1 } else { len - 1 }) % len;
                if !menu.editing {
                    menu.row = 0;
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Left | KeyCode::Char(' ') => {
                if matches!(menu.selected, 4 | 6 | 7) {
                    self.settings_menu = Some(menu.clone());
                    self.open_settings_section(menu.selected);
                    return true;
                }
                if !menu.editing {
                    menu.editing = true;
                } else {
                    menu.change(key.code != KeyCode::Left);
                }
            }
            _ => {}
        }
        self.settings_menu = Some(menu.clone());
        false
    }

    pub(super) fn ui_options_key(&mut self, options: &mut UiOptions, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                if let Some(form) = &options.return_appearance {
                    self.modal = Some(Modal::Appearance(form.clone()));
                } else {
                    self.return_settings_menu();
                }
                true
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                options.selected = (options.selected + options.len() - 1) % options.len();
                false
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                options.selected = (options.selected + 1) % options.len();
                false
            }
            KeyCode::Left => {
                options.change(false);
                false
            }
            KeyCode::Right | KeyCode::Char(' ') => {
                options.change(true);
                false
            }
            KeyCode::Enter | KeyCode::Char('s') => {
                let original = options.original.clone();
                let edited = options.edited.clone();
                match config::try_update(&self.paths.config, |latest| {
                    if latest.ui != original && latest.ui != edited {
                        anyhow::bail!(
                            "UI preferences changed in another instance; reopen settings"
                        );
                    }
                    latest.ui = edited.clone();
                    Ok(())
                }) {
                    Ok(_) => {
                        self.config.ui = edited;
                        self.status = "UI defaults saved".into();
                        if let Some(form) = &options.return_appearance {
                            self.modal = Some(Modal::Appearance(form.clone()));
                        } else {
                            self.return_settings_menu();
                        }
                        true
                    }
                    Err(error) => {
                        options.error = Some(format!("Cannot save: {error}"));
                        false
                    }
                }
            }
            _ => false,
        }
    }

    pub(super) fn codex_settings_key(&mut self, form: &mut CodexSettings, key: KeyEvent) -> bool {
        const LEVELS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];
        match key.code {
            KeyCode::Esc => {
                self.return_settings_menu();
                true
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                let current = LEVELS
                    .iter()
                    .position(|value| *value == form.edited)
                    .unwrap_or(3);
                let next = if key.code == KeyCode::Left {
                    (current + LEVELS.len() - 1) % LEVELS.len()
                } else {
                    (current + 1) % LEVELS.len()
                };
                form.edited = LEVELS[next].into();
                form.error = None;
                false
            }
            KeyCode::Enter | KeyCode::Char('s') => {
                let edited = form.edited.clone();
                let original = form.original.clone();
                match crate::codex::validate_reasoning(&edited).and_then(|_| {
                    config::try_update(&self.paths.config, |latest| {
                        if latest.codex.reasoning_effort != original
                            && latest.codex.reasoning_effort.as_deref() != Some(edited.as_str())
                        {
                            anyhow::bail!(
                                "Codex reasoning changed in another instance; reopen settings"
                            );
                        }
                        latest.codex.reasoning_effort = Some(edited.clone());
                        Ok(())
                    })
                }) {
                    Ok(_) => {
                        self.config.codex.reasoning_effort = Some(edited);
                        self.status = "Codex reasoning saved · p applies to Codex".into();
                        self.return_settings_menu();
                        true
                    }
                    Err(error) => {
                        form.error = Some(format!("Cannot save: {error}"));
                        false
                    }
                }
            }
            _ => false,
        }
    }
}
