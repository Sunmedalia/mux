//! Monitor with explicit account switching, independent of the editor sync loop.
use super::*;
use crate::usage::{Query, Reader, Snapshot, Totals};
use chrono::Timelike;
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) const BG: Color = Color::Rgb(20, 30, 42);
pub(super) const INK: Color = Color::Rgb(223, 233, 240);
pub(super) const SOFT: Color = Color::Rgb(139, 161, 181);
pub(super) const BLUE: Color = Color::Rgb(123, 190, 218);
pub(super) const GOLD: Color = Color::Rgb(234, 193, 126);
pub(super) const RED: Color = Color::Rgb(236, 139, 131);
pub(super) const GREEN: Color = Color::Rgb(147, 204, 178);
pub(super) const METRIC: Color = Color::Rgb(1, 3, 1);
pub(super) const RAIL: Color = Color::Rgb(48, 67, 84);
const LABEL: &str = "Mux Pulse";
mod accounts;
mod collector;
mod focus;
mod git;
mod model;
mod picker;
mod render;
mod widgets;
#[cfg(test)]
use super::meters::digits;
use collector::*;
use focus::*;
use model::*;
pub(super) use widgets::preview_lines;
use widgets::*;
const CLIENTS: [&str; 4] = ["Claude", "Codex", "Grok", "All"];

pub(super) fn run(paths: AppPaths) -> Result<()> {
    let initial = initial_client(std::env::var("MUX_MONITOR_CLIENT").ok().as_deref());
    let (account_send, account_updates) = mpsc::sync_channel(1);
    let (account_refresh, account_requests) = mpsc::sync_channel(1);
    accounts::spawn(paths.clone(), account_send, account_requests, initial);
    let (send, updates) = mpsc::sync_channel(1);
    let (refresh, requests) = mpsc::sync_channel(1);
    let (session_send, session_updates) = mpsc::sync_channel(1);
    let (session_refresh, session_requests) = mpsc::sync_channel(1);
    let source_pane = std::env::var("MUX_MONITOR_SOURCE_PANE")
        .ok()
        .filter(|id| !id.is_empty());
    let workspace = std::env::var("MUX_MONITOR_WORKSPACE").ok();
    let tab = std::env::var("MUX_MONITOR_TAB").ok();
    let theme_paths = paths.clone();
    let (active_send, active_updates) = mpsc::sync_channel(1);
    if let Some(source) = source_pane.clone() {
        std::thread::spawn(move || {
            let mut tracker = FocusTracker::default();
            loop {
                if let Ok(false) = follow_focus_events(
                    &mut tracker,
                    &active_send,
                    workspace.as_deref(),
                    tab.as_deref(),
                    &source,
                ) {
                    break;
                }
                if !tracker.observe(
                    current_focus(workspace.as_deref(), tab.as_deref(), &source),
                    &active_send,
                    false,
                ) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }
    std::thread::spawn(move || {
        let mut reader = Reader::new(paths.state_dir.join(crate::usage::FILE), paths.config);
        let mut snapshot = Snapshot::default();
        loop {
            let today = snapshot.today();
            let result = reader.read(&Query::Range {
                start: today.clone(),
                end: today,
            });
            if let Ok(Some(s)) = &result {
                snapshot = s.clone();
            }
            if send
                .send(result.map_err(|_| "Usage unavailable; check database access".to_string()))
                .is_err()
            {
                break;
            }
            if matches!(
                requests.recv_timeout(Duration::from_secs(2)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ) {
                break;
            }
        }
    });
    let (mut terminal, _guard) = setup_terminal()?;
    let mut monitor = Monitor {
        pulse_theme: theme::PulseTheme::load(&theme_paths),
        client: initial,
        source_pane,
        ..Default::default()
    };
    if let Ok(config) = config::load(&theme_paths.config) {
        monitor.apply_preferences(config.ui);
        monitor.apply_start_page();
    }
    monitor.git.start();
    spawn_session_reader(session_send, session_requests, session_refresh.clone());
    let mut redraw = true;
    let mut last_theme_check = Instant::now();
    let mut last_clock_redraw = Instant::now();
    let mut account_client = initial;
    let mut account_force = false;
    let mut switching: Option<mpsc::Receiver<std::result::Result<String, String>>> = None;
    loop {
        redraw |= monitor.git.poll();
        if last_theme_check.elapsed() >= Duration::from_secs(2) {
            let theme = theme::PulseTheme::load(&theme_paths);
            if monitor.pulse_theme != theme {
                monitor.pulse_theme = theme;
                redraw = true;
            }
            if let Ok(config) = config::load(&theme_paths.config) {
                redraw |= monitor.refresh_preferences(config.ui);
            }
            last_theme_check = Instant::now();
        }
        while let Ok(update) = active_updates.try_recv() {
            if monitor.picker.is_none() {
                monitor.apply_focus(update);
            }
            redraw = true;
        }
        while let Ok(snapshot) = session_updates.try_recv() {
            monitor.sessions = snapshot;
            monitor.sessions_refreshed = Some(Instant::now());
            redraw = true;
        }
        while let Ok(result) = updates.try_recv() {
            match result {
                Ok(snapshot) => {
                    if let Some(s) = snapshot {
                        monitor.snapshot = s;
                    }
                    monitor.refreshed = Some(Instant::now());
                    monitor.error = None;
                }
                Err(error) => monitor.error = Some(error),
            }
            redraw = true;
        }
        while let Ok(accounts) = account_updates.try_recv() {
            monitor.accounts = accounts;
            redraw = true;
        }
        let completed = switching
            .as_ref()
            .and_then(|receiver| match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                    "Account switch worker stopped; inspect the local login before retrying".into(),
                )),
                Err(mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = completed {
            if let Some(picker) = &mut monitor.picker {
                monitor.client = picker.client;
                picker.finish(result);
                monitor.scroll = monitor.scroll.min(2);
            }
            switching = None;
            account_force = true;
            redraw = true;
        }
        if (monitor.client != account_client || account_force)
            && account_refresh
                .try_send((monitor.client, account_force))
                .is_ok()
        {
            account_client = monitor.client;
            account_force = false;
        }
        if last_clock_redraw.elapsed() >= Duration::from_secs(1) {
            redraw = true;
            last_clock_redraw = Instant::now();
        }
        if redraw {
            terminal.draw(|f| {
                monitor.draw(f);
                monitor.pulse_theme.apply(f.buffer_mut());
            })?;
            redraw = false;
        }
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        redraw = true;
        let input = event::read()?;
        if let Event::Key(key) = &input
            && monitor.workspace_shortcut(*key)
        {
            continue;
        }
        if monitor.page == config::PulseStartPage::Git {
            if monitor.git.input(&input) {
                continue;
            }
            match &input {
                Event::Key(k)
                    if k.code == KeyCode::Char('q')
                        || (k.code == KeyCode::Char('c')
                            && k.modifiers.contains(KeyModifiers::CONTROL)) =>
                {
                    break;
                }
                _ => {
                    monitor.select_workspace(false);
                    continue;
                }
            }
        }
        let size = terminal.size()?;
        let body = monitor.account_body(Rect::new(0, 0, size.width, size.height));
        if let Some(picker) = &mut monitor.picker {
            let was_selecting = picker.selecting();
            let mut navigate = false;
            let action = match &input {
                Event::Key(k) if k.kind == event::KeyEventKind::Press => match k.code {
                    KeyCode::PageDown => {
                        monitor.scroll = monitor.scroll.saturating_add(10).min(monitor.limit);
                        picker::Action::None
                    }
                    KeyCode::PageUp => {
                        monitor.scroll = monitor.scroll.saturating_sub(10);
                        picker::Action::None
                    }
                    KeyCode::Up | KeyCode::Down if !picker.selecting() => {
                        if k.code == KeyCode::Down {
                            monitor.scroll = monitor.scroll.saturating_add(1).min(monitor.limit);
                        } else {
                            monitor.scroll = monitor.scroll.saturating_sub(1);
                        }
                        picker::Action::None
                    }
                    _ => {
                        navigate = picker.selecting()
                            && matches!(
                                k.code,
                                KeyCode::Up
                                    | KeyCode::Down
                                    | KeyCode::Home
                                    | KeyCode::End
                                    | KeyCode::Char('j' | 'k')
                            );
                        picker.key(k.code)
                    }
                },
                Event::Mouse(m) => {
                    let wheel = matches!(
                        m.kind,
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                    );
                    navigate = wheel && picker.list_hit(body, m.column, m.row, monitor.scroll);
                    if wheel && !navigate {
                        if m.kind == MouseEventKind::ScrollDown {
                            monitor.scroll = monitor.scroll.saturating_add(3).min(monitor.limit);
                        } else {
                            monitor.scroll = monitor.scroll.saturating_sub(3);
                        }
                        picker::Action::None
                    } else {
                        picker.mouse(*m, body, monitor.scroll)
                    }
                }
                _ => picker::Action::None,
            };
            if was_selecting && !picker.selecting() {
                monitor.scroll = monitor.scroll.min(2);
            }
            if navigate && body.height > 0 {
                let row = picker.selected_row().min(u16::MAX as usize) as u16;
                if row < monitor.scroll {
                    monitor.scroll = row;
                } else if row >= monitor.scroll.saturating_add(body.height) {
                    monitor.scroll = row.saturating_add(1).saturating_sub(body.height);
                }
            }
            match action {
                picker::Action::Close => monitor.picker = None,
                picker::Action::Switch(client, id) => {
                    let (send, receiver) = mpsc::channel();
                    let paths = theme_paths.clone();
                    std::thread::spawn(move || {
                        let result = picker::switch(&paths, client, &id).map_err(|e| e.to_string());
                        let _ = send.send(result);
                    });
                    switching = Some(receiver);
                }
                picker::Action::None => {}
            }
            continue;
        }
        let key = match input {
            Event::Key(k) if k.kind == event::KeyEventKind::Press => Some(k),
            Event::Mouse(m) => {
                let size = terminal.size()?;
                let screen = Rect::new(0, 0, size.width, size.height);
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
                if m.kind == MouseEventKind::Down(MouseButton::Left)
                    && monitor.account_hit(body, m.column, m.row)
                {
                    match picker::Picker::load(&theme_paths, monitor.client) {
                        Ok(picker) => {
                            monitor.picker = Some(picker);
                            monitor.scroll = monitor.scroll.min(1);
                        }
                        Err(error) => monitor.notice = Some(format!("! {error}")),
                    }
                    continue;
                }
                let dragging = matches!(
                    m.kind,
                    MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Drag(MouseButton::Left)
                );
                let code = if dragging {
                    if !mini
                        && let Some(target) = scrollbar_target(
                            body,
                            area.right().saturating_sub(1),
                            m.column,
                            m.row,
                            monitor.limit,
                        )
                    {
                        monitor.scroll = target;
                        None
                    } else if !mini
                        && m.kind == MouseEventKind::Down(MouseButton::Left)
                        && monitor.provider_header_hit(body, m.column, m.row)
                    {
                        monitor.models = !monitor.models;
                        None
                    } else {
                        match m.kind {
                            MouseEventKind::Down(MouseButton::Left)
                                if contains(
                                    monitor.header_switch_rect(area, mini),
                                    m.column,
                                    m.row,
                                ) =>
                            {
                                Some(KeyCode::Char('g'))
                            }
                            MouseEventKind::Down(MouseButton::Left)
                                if m.row == 0
                                    && (!mini || area.width >= 10)
                                    && m.column
                                        >= area.right().saturating_sub(if mini {
                                            2
                                        } else {
                                            4
                                        }) =>
                            {
                                Some(KeyCode::Char('?'))
                            }
                            MouseEventKind::Down(MouseButton::Left)
                                if m.row == 0
                                    && (!mini || area.width >= 10)
                                    && m.column
                                        >= area.right().saturating_sub(if mini {
                                            4
                                        } else {
                                            9
                                        }) =>
                            {
                                Some(KeyCode::Char('v'))
                            }
                            MouseEventKind::Down(MouseButton::Left) if m.row == area.y + 1 => {
                                let tabs = Layout::horizontal([Constraint::Ratio(1, 4); 4])
                                    .split(Rect::new(area.x, area.y + 1, area.width, 1));
                                if let Some(i) =
                                    tabs.iter().position(|r| contains(*r, m.column, m.row))
                                {
                                    monitor.client = i;
                                    monitor.scroll = 0;
                                }
                                None
                            }
                            MouseEventKind::Down(MouseButton::Left) => (if mini {
                                mini_buttons(area)
                            } else {
                                buttons(area)
                            })
                            .iter()
                            .position(|r| contains(*r, m.column, m.row))
                            .map(|i| {
                                [
                                    KeyCode::Char('e'),
                                    KeyCode::Char(
                                        if monitor.page == config::PulseStartPage::Sessions {
                                            't'
                                        } else {
                                            'c'
                                        },
                                    ),
                                    KeyCode::Char('s'),
                                    KeyCode::Char('r'),
                                    KeyCode::Char('q'),
                                ][i]
                            }),
                            _ => None,
                        }
                    }
                } else {
                    match m.kind {
                        MouseEventKind::ScrollDown => Some(KeyCode::Down),
                        MouseEventKind::ScrollUp => Some(KeyCode::Up),
                        _ => None,
                    }
                };
                code.map(|c| KeyEvent::new(c, KeyModifiers::NONE))
            }
            _ => None,
        };
        if let Some(key) = key {
            if monitor.workspace_shortcut(key) {
                continue;
            }
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('a') if matches!(monitor.client, 1 | 2) => {
                    match picker::Picker::load(&theme_paths, monitor.client) {
                        Ok(picker) => {
                            monitor.picker = Some(picker);
                            monitor.scroll = monitor.scroll.min(1);
                        }
                        Err(error) => monitor.notice = Some(format!("! {error}")),
                    }
                }
                KeyCode::Char('T') => {
                    monitor.select_workspace(false);
                }
                KeyCode::Char('e') => {
                    monitor.notice = Some(match open_editor() {
                        Ok(()) => "↗ Editor opened in a new tab".into(),
                        Err(e) => format!("! {e}"),
                    });
                }
                KeyCode::Char('m')
                    if monitor.page != config::PulseStartPage::Sessions
                        && monitor.page != config::PulseStartPage::Charts =>
                {
                    monitor.help = false;
                    monitor.models = !monitor.models;
                    monitor.scroll = monitor
                        .content(40)
                        .iter()
                        .position(|line| {
                            let text = line.to_string();
                            text.starts_with("MODELS / TODAY")
                                || text.starts_with("PROVIDERS / TODAY")
                        })
                        .unwrap_or(0) as u16;
                }
                KeyCode::Char('t') if monitor.page == config::PulseStartPage::Sessions => {
                    monitor.help = false;
                    monitor.sessions_sort_tokens = !monitor.sessions_sort_tokens;
                    monitor.scroll = 0;
                }
                KeyCode::Char('v') => {
                    monitor.visual_mode = !monitor.visual_mode;
                    monitor.help = false;
                    monitor.scroll = 0;
                }
                KeyCode::Char('c') => {
                    monitor.page = if monitor.page == config::PulseStartPage::Charts {
                        config::PulseStartPage::Home
                    } else {
                        config::PulseStartPage::Charts
                    };
                    monitor.help = false;
                    monitor.scroll = 0;
                }
                KeyCode::Char('s') => {
                    monitor.page = if monitor.page == config::PulseStartPage::Sessions {
                        config::PulseStartPage::Home
                    } else {
                        config::PulseStartPage::Sessions
                    };
                    monitor.help = false;
                    monitor.scroll = 0;
                    if monitor.page == config::PulseStartPage::Sessions
                        && let Some(client) = monitor.focused_client
                    {
                        monitor.client = client;
                    }
                }
                KeyCode::Char('r') => {
                    account_force = true;
                    let _ = refresh.try_send(());
                    let _ = session_refresh.try_send(());
                    monitor.notice = None;
                }
                KeyCode::Tab => {
                    monitor.client = (monitor.client + 1) % CLIENTS.len();
                    monitor.scroll = 0;
                }
                KeyCode::Char(c @ '1'..='4') => {
                    monitor.client = (c as u8 - b'1') as usize;
                    monitor.scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    monitor.scroll = monitor.scroll.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    monitor.scroll = monitor.scroll.saturating_add(1).min(monitor.limit)
                }
                KeyCode::PageDown => {
                    monitor.scroll = monitor.scroll.saturating_add(10).min(monitor.limit)
                }
                KeyCode::PageUp => monitor.scroll = monitor.scroll.saturating_sub(10),
                KeyCode::Char('?') => {
                    monitor.help = !monitor.help;
                    monitor.scroll = 0;
                }
                KeyCode::Home | KeyCode::Esc => {
                    monitor.help = false;
                    monitor.scroll = 0;
                }
                KeyCode::End => monitor.scroll = monitor.limit,
                _ => {}
            }
        }
    }
    Ok(())
}
fn herdr(args: &[&str]) -> Result<serde_json::Value> {
    let output = Command::new(std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()))
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Herdr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}
pub(super) fn open_pane() -> Result<()> {
    anyhow::ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1"),
        "Open the monitor from inside Herdr"
    );
    let current = herdr(&["pane", "current", "--current"])?;
    let pane = &current["result"]["pane"];
    // Capture the invoking agent before creating the new, agent-free monitor pane.
    let client = ["claude", "codex", "grok", "all"][initial_client(pane["agent"].as_str())];
    let client_env = format!("MUX_MONITOR_CLIENT={client}");
    let workspace = pane["workspace_id"].as_str().context("Missing workspace")?;
    let tab = pane["tab_id"].as_str().context("Missing tab")?;
    let target = pane["pane_id"].as_str().context("Missing calling pane")?;
    let source_env = format!("MUX_MONITOR_SOURCE_PANE={target}");
    let workspace_env = format!("MUX_MONITOR_WORKSPACE={workspace}");
    let tab_env = format!("MUX_MONITOR_TAB={tab}");
    let list = herdr(&["pane", "list", "--workspace", workspace])?;
    if let Some(existing) = list["result"]["panes"].as_array().and_then(|panes| {
        panes
            .iter()
            .find(|p| p["tab_id"] == tab && p["label"] == LABEL)
    }) {
        let id = existing["pane_id"]
            .as_str()
            .context("Missing monitor pane")?;
        let process = herdr(&["pane", "process-info", "--pane", id])?;
        let binary = std::env::current_exe()?;
        let plugin = herdr(&["plugin", "list", "--plugin", "mux", "--json"])?;
        let linked_binary = plugin["result"]["plugins"]
            .as_array()
            .and_then(|plugins| plugins.first())
            .and_then(|plugin| plugin["plugin_root"].as_str())
            .and_then(|root| {
                std::fs::canonicalize(std::path::Path::new(root).join("target/release/mux")).ok()
            });
        let is_monitor = process["result"]["process_info"]["foreground_processes"]
            .as_array()
            .is_some_and(|ps| {
                ps.iter().any(|p| {
                    p["argv"].as_array().is_some_and(|args| {
                        let executable = args
                            .first()
                            .and_then(|a| a.as_str())
                            .map(std::path::Path::new);
                        let executable = executable.map(|path| {
                            if path.is_absolute() {
                                path.to_path_buf()
                            } else {
                                std::path::Path::new(p["cwd"].as_str().unwrap_or("")).join(path)
                            }
                        });
                        args.len() == 2
                            && args[1] == "quick"
                            && executable
                                .and_then(|path| std::fs::canonicalize(path).ok())
                                .is_some_and(|path| {
                                    path == binary || linked_binary.as_ref() == Some(&path)
                                })
                    })
                })
            });
        anyhow::ensure!(
            is_monitor,
            "The pane named Mux Pulse is no longer running the monitor; leave it open"
        );
        return herdr(&["pane", "close", id]).map(|_| ());
    }
    let cwd = std::env::current_dir()?;
    let layout = herdr(&["pane", "layout", "--pane", target])?;
    let (split_target, width) = rightmost_split_target(&layout).context(
        "The right edge is too narrow for Pulse; widen a pane at the right edge to 76 columns",
    )?;
    let monitor_width = 48u64.min(width / 2);
    let split = herdr(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        "mux",
        "--entrypoint",
        "quick",
        "--placement",
        "split",
        "--target-pane",
        split_target,
        "--direction",
        "right",
        "--cwd",
        &cwd.to_string_lossy(),
        "--env",
        &client_env,
        "--env",
        &source_env,
        "--env",
        &workspace_env,
        "--env",
        &tab_env,
        "--no-focus",
    ])?;
    let id = split["result"]["plugin_pane"]["pane"]["pane_id"]
        .as_str()
        .context("Missing new pane")?;
    herdr(&["pane", "rename", id, LABEL])?;
    // Native plugin panes start directly (no shell echo), with a half-width split.
    // Resize the right-edge target to retain the monitor's narrow footprint.
    let amount = 0.5 - monitor_width as f64 / width as f64;
    if amount > 0.001 {
        herdr(&[
            "pane",
            "resize",
            "--pane",
            split_target,
            "--direction",
            "right",
            "--amount",
            &format!("{amount:.4}"),
        ])?;
    }
    Ok(())
}
fn open_editor() -> Result<()> {
    anyhow::ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1"),
        "Edit requires Herdr; run mux in another terminal"
    );
    let workspace = std::env::var("HERDR_WORKSPACE_ID").context("Missing Herdr workspace")?;
    let opened = herdr(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        "mux",
        "--entrypoint",
        "editor",
        "--placement",
        "tab",
        "--workspace",
        &workspace,
        "--no-focus",
    ])?;
    let pane = &opened["result"]["plugin_pane"]["pane"];
    let tab_id = pane["tab_id"]
        .as_str()
        .context("Editor opened, but Herdr returned no tab ID")?;
    let pane_id = pane["pane_id"]
        .as_str()
        .context("Editor opened, but Herdr returned no pane ID")?;
    // Explicit navigation after creation also selects the new tab in the client.
    herdr(&["tab", "focus", tab_id])?;
    herdr(&["plugin", "pane", "focus", pane_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    #[test]
    fn pulse_saved_defaults_apply_at_start_and_hot_reload_keeps_current_page() {
        let mut monitor = Monitor::default();
        let settings = config::UiPreferences {
            pulse_visual: true,
            pulse_models: true,
            pulse_start_page: config::PulseStartPage::Sessions,
            pulse_sort_tokens: true,
            ..Default::default()
        };
        monitor.apply_preferences(settings.clone());
        monitor.apply_start_page();
        assert!(monitor.visual_mode && monitor.models && monitor.sessions_sort_tokens);
        assert_eq!(monitor.page, config::PulseStartPage::Sessions);
        monitor.page = config::PulseStartPage::Charts;
        assert!(monitor.refresh_preferences(config::UiPreferences {
            pulse_visual: false,
            ..settings
        }));
        assert!(!monitor.visual_mode);
        assert_eq!(monitor.page, config::PulseStartPage::Charts);
        monitor.visual_mode = true;
        let mut unrelated = monitor.preferences.clone();
        unrelated.claude_new_model_1m = false;
        unrelated.pulse_start_page = config::PulseStartPage::Charts;
        assert!(!monitor.refresh_preferences(unrelated));
        assert!(
            monitor.visual_mode,
            "model defaults and next start page should not reset a live view"
        );
    }

    #[test]
    fn progress_bars_preserve_proportions_endpoints_and_narrow_widths() {
        for width in [1, 4, 10, 48] {
            for fraction in [0.0, 0.01, 0.25, 0.46, 0.998, 1.0] {
                let bar = Line::from(progress_spans(Some(fraction), width, BLUE));
                assert_eq!(bar.width(), width);
                let fill = &bar.spans[0].content;
                if fraction == 0.0 {
                    assert!(fill.is_empty());
                }
                if fraction == 1.0 {
                    assert_eq!(fill.as_ref(), "█".repeat(width));
                }
                if fraction < 1.0 {
                    assert!(fill.chars().filter(|&c| c == '█').count() < width);
                }
            }
        }
        assert_eq!(
            Line::from(progress_spans(Some(0.25), 20, BLUE)).to_string(),
            format!("{}{}", "█".repeat(5), "░".repeat(15))
        );
        assert!(
            Line::from(progress_spans(Some(f64::NAN), 10, BLUE)).spans[0]
                .content
                .is_empty()
        );
        for width in 0..=48 {
            assert!(compact_account_quota("codex 5h", 46.0, width).width() <= width.into());
            assert!(compact_account_meter("Weekly credits", None, width).width() <= width.into());
        }
        let rare_failure = health_meter(
            &Totals {
                success: 999,
                failed: 1,
                ..Default::default()
            },
            24,
        );
        assert_eq!(rare_failure.spans[1].content.chars().count(), 20);
        assert!(rare_failure.spans[2].content.is_empty());
    }

    #[test]
    fn reads_only_identified_agent_session() {
        let pane = json!({"result":{"pane":{"agent":"codex","agent_session":{
            "agent":"codex","kind":"id","value":"session-123"
        }}}});
        assert_eq!(agent_session(&pane).unwrap().id, "session-123");
        let mut wrong = pane.clone();
        wrong["result"]["pane"]["agent_session"]["agent"] = json!("claude");
        assert!(agent_session(&wrong).is_none());
        wrong["result"]["pane"]["agent_session"] = serde_json::Value::Null;
        assert!(agent_session(&wrong).is_none());
    }

    #[test]
    #[ignore = "read-only live check; set MUX_TEST_CODEX_PANE to the target pane"]
    fn live_daemon_codex_session_has_tokens() {
        let pane = std::env::var("MUX_TEST_CODEX_PANE").unwrap();
        let result = herdr(&["pane", "get", &pane]).unwrap();
        let current = pane_session(&result["result"]["pane"]).expect("identified session");
        let snapshot = crate::sessions::Reader::default().read(&crate::sessions::roots().unwrap());
        let session = snapshot
            .rows
            .iter()
            .find(|row| row.client == current.client && row.id == current.id)
            .expect("session log");
        assert!(session.tokens.known && session.tokens.total() > 0);
        println!(
            "pane={pane} session={} total={} input={} output={}",
            current.id,
            session.tokens.total(),
            session.tokens.input,
            session.tokens.output
        );
    }

    #[test]
    fn subscription_ack_does_not_contaminate_the_first_focus_event() {
        let messages = b"{\"id\":\"mux_focus\",\"result\":{}}\n{\"event\":\"pane_updated\",\"data\":{\"pane\":{\"pane_id\":\"source\",\"focused\":true,\"agent_session\":{\"agent\":\"codex\",\"kind\":\"id\",\"value\":\"thread-a\"}}}}\n";
        let mut reader = std::io::Cursor::new(messages);
        read_focus_subscription_response(&mut reader).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let event = serde_json::from_str(&line).unwrap();
        let focus = focus_from_event(&event, None, None, "source").unwrap();
        assert_eq!(focus.session.unwrap().id, "thread-a");
        assert!(
            read_focus_subscription_response(&mut std::io::Cursor::new(b"{\"error\":{}}\n"))
                .is_err()
        );
    }

    #[test]
    fn session_report_identifies_codex_when_process_detection_has_no_agent() {
        let pane = json!({"pane_id":"source","tab_id":"tab","focused":true,
            "agent_session":{"agent":"codex","kind":"id","value":"thread-a"}});
        let focused = focused_pane(&pane, Some("tab"), "source").unwrap();
        assert_eq!(focused.client, 1);
        assert_eq!(focused.session.unwrap().id, "thread-a");
        let panes = json!({"result":{"panes":[pane]}});
        assert_eq!(
            focused_agent(&panes, "tab").unwrap().session.unwrap().id,
            "thread-a"
        );
    }

    #[test]
    fn codex_rollout_path_matches_session_tokens_and_incremental_updates() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join("rollout-2026-09-28T10-00-00-thread-a_resume.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"session_meta","payload":{"id":"thread-a"}})
        )
        .unwrap();
        let pane = json!({"agent":"codex","agent_session":{
            "agent":"codex","kind":"path","value":path}});
        let mut monitor = Monitor {
            active_session: agent_session(&pane),
            ..Default::default()
        };
        assert_eq!(monitor.active_session.as_ref().unwrap().id, "thread-a");
        let mut reader = crate::sessions::Reader::default();
        for input in [100, 200] {
            writeln!(
                file,
                "{}",
                json!({"type":"event_msg","payload":{"type":"token_count",
                "info":{"total_token_usage":{"input_tokens":input,"output_tokens":10}}}})
            )
            .unwrap();
            file.flush().unwrap();
            monitor.sessions = reader.read(&[(temp.path().to_owned(), false)]);
            assert_eq!(monitor.active_row().unwrap().tokens.total(), input + 10);
        }
        let missing = json!({"agent":"codex","agent_session":{
            "agent":"codex","kind":"path","value":temp.path().join("missing.jsonl")}});
        assert!(agent_session(&missing).is_none());
    }

    #[test]
    fn source_agent_remains_visible_when_pulse_pane_is_focused() {
        let panes = json!({"result":{"panes":[
            {"pane_id":"source","tab_id":"tab","focused":false,"agent":"codex",
             "agent_session":{"agent":"codex","kind":"id","value":"session-123"}},
            {"pane_id":"pulse","tab_id":"tab","focused":true,"agent":null}
        ]}});
        let active = focused_or_source_agent(&panes, "tab", "source").unwrap();
        assert_eq!(active.pane_id, "source");
        assert_eq!(active.session.unwrap().id, "session-123");
    }

    #[test]
    fn pulse_split_uses_the_outer_right_edge_not_the_invoking_pane() {
        let layout = json!({"result":{"layout":{
            "area":{"x":0,"width":300},
            "panes":[
                {"pane_id":"caller","rect":{"x":0,"width":140,"height":40}},
                {"pane_id":"middle","rect":{"x":140,"width":80,"height":40}},
                {"pane_id":"right","rect":{"x":220,"width":80,"height":40}}
            ]
        }}});
        assert_eq!(rightmost_split_target(&layout), Some(("right", 80)));
        let narrow = json!({"result":{"layout":{
            "area":{"x":0,"width":300},
            "panes":[
                {"pane_id":"caller","rect":{"x":0,"width":252,"height":40}},
                {"pane_id":"right","rect":{"x":252,"width":48,"height":40}}
            ]
        }}});
        assert_eq!(rightmost_split_target(&narrow), None);
    }

    #[test]
    fn focused_agent_tracks_the_active_pane_in_the_monitor_tab() {
        let mut panes = json!({"result":{"panes":[
            {"pane_id":"codex","tab_id":"tab-a","focused":true,"agent":"codex",
             "agent_session":{"agent":"codex","kind":"id","value":"codex-one"}},
            {"pane_id":"claude","tab_id":"tab-a","focused":false,"agent":"claude",
             "agent_session":{"agent":"claude","kind":"id","value":"claude-one"}},
            {"pane_id":"other","tab_id":"tab-b","focused":false,"agent":"claude"}
        ]}});
        let current = focused_agent(&panes, "tab-a").unwrap();
        assert_eq!(current.client, 1);
        assert_eq!(current.session.unwrap().id, "codex-one");

        panes["result"]["panes"][0]["focused"] = json!(false);
        panes["result"]["panes"][1]["focused"] = json!(true);
        let current = focused_agent(&panes, "tab-a").unwrap();
        assert_eq!(current.client, 0);
        assert_eq!(current.session.unwrap().id, "claude-one");

        panes["result"]["panes"][1]["agent_session"] = serde_json::Value::Null;
        let current = focused_agent(&panes, "tab-a").unwrap();
        assert_eq!(current.client, 0);
        assert!(current.session.is_none());
        assert!(focused_agent(&panes, "tab-b").is_none());
    }

    #[test]
    fn focus_subscription_uses_pane_updates_without_a_cli_query() {
        let event = json!({"event":"pane_updated","data":{"pane":{
            "pane_id":"pane-a","tab_id":"tab-a","focused":true,"agent":"codex",
            "agent_session":{"agent":"codex","kind":"id","value":"session-a"}
        }}});
        let focused =
            focus_from_event(&event, Some("workspace-a"), Some("tab-a"), "source").unwrap();
        assert_eq!(focused.pane_id, "pane-a");
        assert_eq!(focused.session.unwrap().id, "session-a");
        assert!(focus_from_event(&event, Some("workspace-a"), Some("tab-b"), "source").is_none());

        let (send, updates) = mpsc::sync_channel(1);
        let mut tracker = FocusTracker::default();
        assert!(tracker.observe(
            focus_from_event(&event, Some("workspace-a"), Some("tab-a"), "source"),
            &send,
            true,
        ));
        assert_eq!(updates.try_recv().unwrap().pane_id, "pane-a");
        assert!(tracker.observe(
            focus_from_event(&event, Some("workspace-a"), Some("tab-a"), "source"),
            &send,
            true,
        ));
        assert!(updates.try_recv().is_err());
        let mut cleared = event;
        cleared["data"]["pane"]["agent_session"] = serde_json::Value::Null;
        assert!(tracker.observe(
            focus_from_event(&cleared, Some("workspace-a"), Some("tab-a"), "source"),
            &send,
            true,
        ));
        assert!(updates.try_recv().unwrap().session.is_none());
    }

    #[test]
    fn sessions_filter_follows_focused_agent_even_when_session_id_is_missing() {
        let mut monitor = Monitor {
            page: config::PulseStartPage::Sessions,
            client: 3,
            ..Default::default()
        };
        monitor.sessions.rows = vec![
            crate::sessions::Session {
                id: "codex-one".into(),
                client: "Codex",
                ..Default::default()
            },
            crate::sessions::Session {
                id: "claude-one".into(),
                client: "Claude",
                ..Default::default()
            },
        ];
        monitor.apply_focus(FocusUpdate {
            pane_id: "codex-pane".into(),
            client: 1,
            session: Some(AgentSession {
                client: "Codex",
                id: "codex-one".into(),
            }),
        });
        assert_eq!(monitor.client(), Some("Codex"));
        assert_eq!(monitor.session_rows().len(), 1);
        monitor.client = 3; // A manual All selection lasts until focus changes.
        monitor.apply_focus(FocusUpdate {
            pane_id: "claude-pane".into(),
            client: 0,
            session: None,
        });
        assert_eq!(monitor.client(), Some("Claude"));
        assert_eq!(monitor.session_rows()[0].id, "claude-one");
        assert!(monitor.active_session.is_none());
    }

    #[test]
    fn gateway_filter_follows_focus_and_keeps_manual_selection_until_focus_moves() {
        let mut monitor = Monitor {
            client: 3,
            ..Default::default()
        };
        monitor.apply_focus(FocusUpdate {
            pane_id: "codex-pane".into(),
            client: 1,
            session: None,
        });
        assert_eq!(monitor.client(), Some("Codex"));
        monitor.client = 3;
        monitor.apply_focus(FocusUpdate {
            pane_id: "codex-pane".into(),
            client: 1,
            session: Some(AgentSession {
                client: "Codex",
                id: "another-session".into(),
            }),
        });
        assert_eq!(monitor.client(), None);
        monitor.apply_focus(FocusUpdate {
            pane_id: "claude-pane".into(),
            client: 0,
            session: None,
        });
        assert_eq!(monitor.client(), Some("Claude"));
    }

    #[test]
    fn transient_missing_agent_id_does_not_hide_current_session() {
        let mut previous = None;
        let mut missing = 0;
        let first = AgentSession {
            client: "Codex",
            id: "first".into(),
        };
        let second = AgentSession {
            client: "Codex",
            id: "second".into(),
        };
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, Some(first.clone())),
            Some(Some(first.clone()))
        );
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, None),
            None
        );
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, None),
            None
        );
        assert_eq!(previous, Some(first));
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, Some(second.clone())),
            Some(Some(second.clone()))
        );
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, None),
            None
        );
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, None),
            None
        );
        assert_eq!(
            stable_agent_session(&mut previous, &mut missing, None),
            Some(None)
        );
        assert_eq!(previous, None);
    }

    #[test]
    fn active_session_stays_first_across_sorts() {
        let mut m = Monitor {
            client: 3,
            active_session: Some(AgentSession {
                client: "Codex",
                id: "current".into(),
            }),
            ..Default::default()
        };
        m.sessions.rows = vec![
            crate::sessions::Session {
                id: "older".into(),
                client: "Codex",
                updated: 20,
                tokens: crate::sessions::Tokens {
                    input: 500,
                    known: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            crate::sessions::Session {
                id: "current".into(),
                client: "Codex",
                updated: 10,
                ..Default::default()
            },
        ];
        assert_eq!(m.session_rows()[0].id, "current");
        m.sessions_sort_tokens = true;
        assert_eq!(m.session_rows()[0].id, "current");
    }

    #[test]
    fn home_shows_current_session_independently_of_gateway_and_history() {
        let mut m = Monitor {
            source_pane: Some("w1:p1".into()),
            active_session: Some(AgentSession {
                client: "Claude",
                id: "current-session".into(),
            }),
            client: 1,
            ..Default::default()
        };
        m.sessions.rows.push(crate::sessions::Session {
            id: "current-session".into(),
            client: "Claude",
            project: "/work/a-very-long-project-name".into(),
            tokens: crate::sessions::Tokens {
                input: 800,
                output: 200,
                read: 300,
                write: 100,
                known: true,
                cache_known: true,
            },
            ..Default::default()
        });
        let home = m
            .content(28)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(home.contains("SESSION / ALL TIME"));
        assert!(home.find("TOKENS").unwrap() < home.find("SESSION / ALL TIME").unwrap());
        assert!(home.contains(&digits("1000")[0]));
        assert!(home.contains("↑ Input 800  ·  ↓ Output 200"));
        assert!(home.contains("↺ Read 300  ·  Write 100"));
        assert!(home.contains("37.5%"));
        assert!(home.starts_with("CODEX ACCOUNT"));
        assert!(home.contains("Reading gateway usage"));
        assert!(
            m.content(28).iter().all(|line| line.width() <= 28),
            "overflow: {:?}",
            m.content(28)
                .iter()
                .filter(|line| line.width() > 28)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );

        m.page = config::PulseStartPage::Sessions;
        m.sessions_refreshed = Some(Instant::now());
        let history = m
            .content(28)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(history.contains("SESSION HISTORY"));
        assert!(history.contains("No local sessions for this client"));
        assert_eq!(history.matches(&digits("1000")[0]).count(), 1);
        m.client = 3;
        let all_history = m
            .content(28)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_history.contains("No other sessions"));
    }

    #[test]
    fn visual_view_keeps_exact_metrics_and_fits_narrow_panes() {
        let mut m = Monitor {
            visual_mode: true,
            source_pane: Some("w1:p1".into()),
            active_session: Some(AgentSession {
                client: "Codex",
                id: "current".into(),
            }),
            client: 1,
            refreshed: Some(Instant::now()),
            sessions_refreshed: Some(Instant::now()),
            ..Default::default()
        };
        m.sessions.rows.push(crate::sessions::Session {
            id: "current".into(),
            client: "Codex",
            project: "/work/project".into(),
            tokens: crate::sessions::Tokens {
                input: 800,
                output: 200,
                read: 300,
                write: 100,
                known: true,
                cache_known: true,
            },
            ..Default::default()
        });
        m.sessions.rows.push(crate::sessions::Session {
            id: "previous".into(),
            client: "Codex",
            project: "/work/project".into(),
            tokens: crate::sessions::Tokens {
                input: 80,
                output: 20,
                read: 30,
                write: 10,
                known: true,
                cache_known: true,
            },
            ..Default::default()
        });
        m.snapshot.rows.push(crate::usage::Row {
            day: m.snapshot.today(),
            hour: 12,
            client: "Codex".into(),
            provider: "test".into(),
            name: "Test provider".into(),
            kind: "generation".into(),
            totals: Totals {
                calls: 2,
                success: 1,
                failed: 1,
                input: 400,
                output: 100,
                cache_read: 200,
                cache_write: 50,
                cache_input: 400,
                cache_hits: 200,
                speed_output: 100,
                speed_ms: 2000,
                speed_samples: 1,
                ..Default::default()
            },
            model: "test-model".into(),
        });
        for width in [28, 36, 44] {
            let home = m.content(width);
            let text = home
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.find("TOKENS").unwrap() < text.find("SESSION / ALL TIME").unwrap());
            assert!(
                text.find("Calls / unknown").unwrap() < text.find("SESSION / ALL TIME").unwrap()
            );
            assert!(text.find("streams").unwrap() < text.find("SESSION / ALL TIME").unwrap());
            assert!(text.contains(&digits("500")[0]));
            for expected in [
                &digits("1000")[0],
                "I 800  O 200",
                "R 300 W 100",
                "37.5%",
                "500",
                "400",
                "100",
                "50.0%",
                "50.0 tok/s",
                "✓ 1",
                "× 1",
                "Test provider",
            ] {
                assert!(text.contains(expected), "missing {expected}: {text}");
            }
            assert!(home.iter().all(|line| line.width() <= width as usize));
            m.page = config::PulseStartPage::Sessions;
            let history = m.content(width);
            let text = history
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            for expected in ["SESSION HISTORY", "previous", "I 80  O 20", "R 30 W 10"] {
                assert!(text.contains(expected), "missing {expected}: {text}");
            }
            assert!(history.iter().all(|line| line.width() <= width as usize));
            m.page = config::PulseStartPage::Home;
        }
        let mut terminal = ratatui::Terminal::new(TestBackend::new(48, 30)).unwrap();
        terminal.draw(|frame| m.draw(frame)).unwrap();
        let compact_rows = m.content(42).len();
        let buffer = terminal.backend().buffer();
        let row = |y| (0..48).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(1).contains("Codex"));
        assert!(row(2).contains("CODEX ACCOUNT"));
        assert!(row(28).contains("Checked"));
        let mut tall = ratatui::Terminal::new(TestBackend::new(48, 40)).unwrap();
        tall.draw(|frame| m.draw(frame)).unwrap();
        assert_eq!(m.content(42).len(), compact_rows);
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "T")
        );
    }

    #[test]
    fn graphical_gateway_strips_keep_all_values_at_narrow_widths() {
        let totals = Totals {
            calls: 2,
            unknown: 1,
            input: 1536,
            output: 80,
            cache_read: 1536,
            cache_write: 0,
            cache_input: 125_000,
            cache_hits: 1500,
            speed_output: 196,
            speed_ms: 5000,
            speed_samples: 2,
            ..Default::default()
        };
        for width in [28, 36, 44] {
            let lines = gateway_cache_meter(&totals, false, width);
            let cache = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(cache.contains("HIT"));
            assert!(cache.contains("1.2%"));
            assert!(cache.contains("R 1536 W 0") || cache.contains("1536 / 0"));
            assert!(lines.iter().all(|line| line.width() <= width as usize));
        }
        let mut m = Monitor {
            visual_mode: true,
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        m.snapshot.rows.push(crate::usage::Row {
            day: m.snapshot.today(),
            hour: 12,
            client: "Claude".into(),
            provider: "test".into(),
            name: "Test".into(),
            kind: "generation".into(),
            totals,
            model: "test".into(),
        });
        let text = m
            .content(40)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "I 1536  O 80",
            "● Calls / unknown",
            "2 / 1",
            "1.2%",
            "R 1536 W 0",
            "39.2 tok/s",
            "2 measured streams",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("Cache read / write"));
        assert!(!text.contains("Measured streams"));
    }

    #[test]
    fn charts_show_gateway_calls_and_current_session_token_activity() {
        let mut m = Monitor {
            source_pane: Some("w1:p1".into()),
            active_session: Some(AgentSession {
                client: "Claude",
                id: "current".into(),
            }),
            page: config::PulseStartPage::Charts,
            ..Default::default()
        };
        m.snapshot.offset = 0;
        let noon = chrono::NaiveDate::parse_from_str(&m.snapshot.today(), "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let mut session = crate::sessions::Session {
            id: "current".into(),
            client: "Claude",
            ..Default::default()
        };
        session.activity.insert(noon, 120);
        m.sessions.rows.push(session);
        m.snapshot.rows.push(crate::usage::Row {
            day: m.snapshot.today(),
            hour: 12,
            client: "Claude".into(),
            provider: "test".into(),
            name: "Test".into(),
            kind: "generation".into(),
            totals: Totals {
                calls: 2,
                ..Default::default()
            },
            model: "test".into(),
        });
        assert_eq!(m.session_hours_today()[12], 120);
        let text = m
            .content(42)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("GATEWAY / REQUESTS BY HOUR"));
        assert!(text.contains("SESSION / TOKENS BY HOUR"));
        assert!(text.contains("120"));
        assert!(!text.contains("PROVIDERS / TODAY"));
    }

    #[test]
    fn pulse_scrollbar_reaches_the_end_of_the_content() {
        for (width, height) in [(32, 12), (48, 24), (80, 35)] {
            let area = Rect::new(0, 0, width, height);
            let mut monitor = Monitor {
                source_pane: Some("source".into()),
                ..Default::default()
            };
            monitor.page = config::PulseStartPage::Sessions;
            monitor.sessions.rows = (0..50)
                .map(|index| crate::sessions::Session {
                    id: format!("session-{index}"),
                    client: "Claude",
                    project: "/work/mux".into(),
                    ..Default::default()
                })
                .collect();
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            monitor.scroll = u16::MAX;
            terminal.draw(|frame| monitor.draw(frame)).unwrap();
            let body = content_body(area.inner(Margin::new(2, 0)));
            assert!(monitor.limit > 0);
            assert_eq!(monitor.scroll, monitor.limit);
            let bottom = &terminal.backend().buffer()[(area.right() - 3, body.bottom() - 1)];
            assert_eq!(bottom.fg, BLUE, "{width}x{height}: bottom of thumb");
            let top: String = (0..width)
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect();
            assert!(top.contains("TOKEN") && top.contains("[Git(g)]"));
            assert!(!top.contains("Mux"));
        }
    }

    #[test]
    fn scrollbar_track_and_provider_header_are_mouse_targets() {
        let body = Rect::new(2, 4, 42, 20);
        assert_eq!(scrollbar_target(body, 47, 47, 4, 100), Some(0));
        assert_eq!(scrollbar_target(body, 47, 47, 23, 100), Some(100));
        assert_eq!(scrollbar_target(body, 47, 46, 23, 100), None);
        let mut m = Monitor {
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let header = m
            .content(body.width)
            .iter()
            .position(|line| line.to_string().starts_with("PROVIDERS / TODAY"))
            .unwrap() as u16;
        m.scroll = header;
        assert!(m.provider_header_hit(body, body.x + 2, body.y));
        assert!(!m.provider_header_hit(body, body.x + 2, body.y + 1));
    }
    #[test]
    fn sidepane_sessions_render_and_filter_without_proxy_usage() {
        let mut m = Monitor {
            page: config::PulseStartPage::Sessions,
            sessions_refreshed: Some(Instant::now()),
            client: 3,
            ..Default::default()
        };
        m.sessions.rows = vec![
            crate::sessions::Session {
                id: "codex-session-123".into(),
                client: "Codex",
                project: "/work/project".into(),
                updated: 200,
                tokens: crate::sessions::Tokens {
                    input: 900,
                    output: 100,
                    read: 700,
                    known: true,
                    cache_known: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            crate::sessions::Session {
                id: "claude-session-456".into(),
                client: "Claude",
                updated: 100,
                tokens: crate::sessions::Tokens {
                    input: 10,
                    output: 2,
                    known: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        for (width, height) in [(32, 12), (40, 28), (48, 46)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| m.draw(f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(
                text.contains("(s)"),
                "Sessions button must fit {width} columns"
            );
            assert!(text.contains("SESSION"), "Session view must be visible");
            assert!(!text.contains("TODAY / USAGE"));
        }
        assert_eq!(m.session_rows().len(), 2);
        m.client = 0;
        assert_eq!(m.session_rows().len(), 1);
        assert_eq!(m.session_rows()[0].client, "Claude");
        m.client = 3;
        m.sessions_sort_tokens = true;
        assert_eq!(m.session_rows()[0].id, "codex-session-123");
        let body = m
            .content(48)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("1000 tok"));
        assert!(body.contains("700 read"));
    }
    #[test]
    fn cache_and_request_rate_display_matching_sample_totals() {
        let mut m = Monitor {
            refreshed: Some(Instant::now()),
            source_pane: Some("w1:p1".into()),
            active_session: Some(AgentSession {
                client: "Claude",
                id: "current".into(),
            }),
            ..Default::default()
        };
        m.sessions.rows.push(crate::sessions::Session {
            id: "current".into(),
            client: "Claude",
            ..Default::default()
        });
        m.snapshot.rows.push(crate::usage::Row {
            hour: 0,
            model: "test".into(),
            day: m.snapshot.today(),
            client: "Claude".into(),
            provider: "p".into(),
            name: "P".into(),
            kind: "generation".into(),
            totals: Totals {
                calls: 2,
                success: 2,
                input: 200,
                output: 100,
                cache_hits: 120448,
                cache_input: 120675,
                speed_output: 100,
                speed_ms: 4000,
                speed_samples: 2,
                ..Default::default()
            },
        });
        let text = m
            .content(48)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("99.8%"));
        assert!(text.contains("↑ Input") && text.contains("↓ Output"));
        assert!(text.contains("Output rate (E2E)"));
        assert!(text.contains("25.0 tok/s"));
        assert!(text.contains("2 measured streams"));
        assert!(text.find("REQUESTS").unwrap() < text.find("SESSION / ALL TIME").unwrap());
        assert!(
            text.find("2 measured streams").unwrap() < text.find("SESSION / ALL TIME").unwrap()
        );
    }
    #[test]
    fn invoking_agent_selects_the_initial_statistics_tab() {
        for agent in ["claude", "claude-code", "Claude Code", "claudecode"] {
            assert_eq!(initial_client(Some(agent)), 0);
        }
        assert_eq!(initial_client(Some("codex")), 1);
        for agent in [None, Some(""), Some("pi"), Some("unknown"), Some("all")] {
            assert_eq!(initial_client(agent), 3);
        }
    }
    #[test]
    fn hourly_chart_uses_six_rows_full_width_and_preserves_zero_hours() {
        for width in [28, 36, 44, 60] {
            let empty = hourly_chart(&[0; 24], width, BLUE);
            assert_eq!(empty.len(), 8);
            assert!(empty.iter().all(|line| line.width() == usize::from(width)));
            assert!(!empty.iter().any(|line| line.to_string().contains('█')));
            let mut hours = [0; 24];
            hours[12] = 10;
            let chart = hourly_chart(&hours, width, BLUE);
            assert!(chart[..6].iter().all(|line| line.to_string().contains('█')));
            assert!(chart.last().unwrap().to_string().contains("23"));
        }
    }
    #[test]
    fn scope_explanation_is_only_in_help() {
        let mut m = Monitor {
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let text = |m: &Monitor| {
            m.content(44)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(!text(&m).contains("subscription"));
        m.help = true;
        assert!(text(&m).contains("subscription"));
        assert!(text(&m).contains("gateway totals"));
    }
    #[test]
    fn independent_layout_and_scrolling_at_narrow_sizes() {
        for (width, height) in [(32, 12), (40, 28), (48, 46)] {
            let mut m = Monitor {
                refreshed: Some(Instant::now()),
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| m.draw(f)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("TOKEN"));
            assert!(text.contains("(e)"));
            assert!(text.contains("(c)"));
            assert!(text.contains("(r)"));
            assert!(text.contains("(q)"));
            assert!(!text.contains("e edit ·"));
            assert!(!text.contains("Configurations"));
            m.scroll = u16::MAX;
            terminal.draw(|f| m.draw(f)).unwrap();
            assert!(m.scroll <= m.limit);
        }
    }
    #[test]
    fn token_header_title_is_prominent_and_git_switch_is_only_in_header() {
        for (width, height) in [(20, 10), (32, 12), (48, 30), (100, 40)] {
            let mut monitor = Monitor::default();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| monitor.draw(f)).unwrap();
            let row = |y| {
                (0..width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            };
            assert!(row(0).contains("TOKEN"));
            assert!(row(0).contains("[Git(g)]"));
            assert!(!row(0).contains('●') && !row(0).contains('○'));
            let (header, reserve) = pulse_header_area(Rect::new(0, 0, width, height));
            let parts = page_header_rects(header, reserve, false);
            for x in parts[0].x..parts[0].right() {
                assert_eq!(terminal.backend().buffer()[(x, 0)].bg, BG);
            }
            let title_cell = &terminal.backend().buffer()[(parts[0].x + 2, 0)];
            assert!(title_cell.modifier.contains(Modifier::BOLD));
            assert_eq!(title_cell.fg, INK);
            for x in parts[1].x..parts[1].right() {
                let cell = &terminal.backend().buffer()[(x, 0)];
                assert_eq!(cell.fg, SOFT);
                assert_eq!(cell.bg, BG);
                assert!(!cell.modifier.contains(Modifier::BOLD));
            }
            assert!(!row(height - 1).contains("GIT"));
            let mini = width < 32 || height < 12;
            let area = if mini {
                Rect::new(0, 0, width, height)
            } else {
                Rect::new(0, 0, width, height).inner(Margin::new(2, 0))
            };
            let rect = monitor.header_switch_rect(area, mini);
            assert!(rect.right() <= area.right());
            let label = (rect.x..rect.right())
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect::<String>();
            assert_eq!(label, "[Git(g)]");
            assert_eq!(buttons(area).len(), 5);
        }
    }
    #[test]
    fn workspace_selection_is_idempotent_and_restores_token_context() {
        let mut monitor = Monitor {
            page: config::PulseStartPage::Sessions,
            scroll: 17,
            client: 1,
            sessions_sort_tokens: true,
            ..Default::default()
        };
        monitor.select_workspace(false);
        assert_eq!(monitor.page, config::PulseStartPage::Sessions);
        for _ in 0..2 {
            assert!(
                monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT))
            );
            assert_eq!(monitor.page, config::PulseStartPage::Git);
        }
        for _ in 0..2 {
            assert!(
                monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT))
            );
            assert_eq!(monitor.page, config::PulseStartPage::Sessions);
        }
        assert_eq!(monitor.scroll, 17);
        assert_eq!(monitor.client, 1);
        assert!(monitor.sessions_sort_tokens);
        for _ in 0..2 {
            assert!(
                monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            );
            assert_eq!(monitor.page, config::PulseStartPage::Git);
            assert!(
                monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            );
            assert_eq!(monitor.page, config::PulseStartPage::Sessions);
            assert_eq!(monitor.scroll, 17);
        }
        assert!(
            !monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
        );
        assert!(!monitor.workspace_shortcut(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)));
        assert_eq!(monitor.page, config::PulseStartPage::Sessions);
        let mut from_git = Monitor::default();
        from_git.preferences.pulse_start_page = config::PulseStartPage::Git;
        from_git.apply_start_page();
        from_git.select_workspace(false);
        assert_eq!(from_git.page, config::PulseStartPage::Home);
    }
    #[test]
    fn health_needs_samples_and_excludes_pending() {
        assert_eq!(rate(&Totals::default()), None);
        assert_eq!(
            rate(&Totals {
                pending: 4,
                ..Default::default()
            }),
            None
        );
        assert_eq!(
            rate(&Totals {
                success: 8,
                failed: 1,
                interrupted: 1,
                pending: 90,
                ..Default::default()
            }),
            Some(80.0)
        );
    }

    #[test]
    fn visual_health_bar_shows_failures_and_interruptions() {
        let totals = Totals {
            success: 20,
            failed: 1,
            interrupted: 1,
            pending: 90,
            ..Default::default()
        };
        let bar = health_meter(&totals, 40);
        assert_eq!(bar.width(), 40);
        assert_eq!(bar.spans[1].style.fg, Some(GREEN));
        assert_eq!(bar.spans[2].style.fg, Some(RED));
        assert_eq!(bar.spans[3].style.fg, Some(GOLD));
        assert!(!bar.spans[2].content.is_empty());
        assert!(!bar.spans[3].content.is_empty());
        let empty = health_meter(
            &Totals {
                pending: 3,
                ..Default::default()
            },
            40,
        );
        assert!(!empty.to_string().contains('█'));
    }

    #[test]
    fn unavailable_tokens_are_not_displayed_as_zero() {
        let t = Totals {
            calls: 4,
            unknown: 4,
            ..Default::default()
        };
        assert_eq!(token_label(&t), "unknown");
        assert_eq!(
            token_label(&Totals {
                calls: 4,
                unknown: 1,
                input: 100,
                ..Default::default()
            }),
            "100+?"
        );
        assert_eq!(token_label(&Totals::default()), "0");
        let mut m = Monitor {
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        m.snapshot.rows.push(crate::usage::Row {
            hour: 1,
            model: "m".into(),
            day: m.snapshot.today(),
            client: "Claude".into(),
            provider: "p".into(),
            name: "P".into(),
            kind: "generation".into(),
            totals: t,
        });
        let content = m
            .content(28)
            .into_iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.contains("4 calls lack token data"));
        assert!(content.contains("↺ Read") && content.contains("Write"));
        assert!(!content.contains("0 tok"));
        assert!(content.contains("no samples"));
    }
    #[test]
    fn today_excludes_history_and_other_clients_but_counts_compaction() {
        let mut s = Snapshot::default();
        for (day, client, kind, calls) in [
            (s.today(), "Claude", "generation", 10),
            (s.today(), "Claude", "compact", 2),
            ("".into(), "Claude", "generation", 1000),
            (s.today(), "Codex", "generation", 30),
        ] {
            s.rows.push(crate::usage::Row {
                hour: 9,
                model: "model".into(),
                day,
                client: client.into(),
                provider: "host".into(),
                name: "Host".into(),
                kind: kind.into(),
                totals: Totals {
                    calls,
                    success: calls,
                    ..Default::default()
                },
            });
        }
        let m = metrics(&s, Some("Claude"));
        assert_eq!(m.total.calls, 12);
        assert_eq!(m.compact, 2);
        assert_eq!(m.hours[9], 12);
        assert_eq!(metrics(&s, None).total.calls, 42);
    }
}

#[cfg(test)]
mod account_page_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn grok_pane_detection_selects_its_page_and_keeps_all_separate() {
        for name in ["grok", "grokcli", "grok-cli", "Grok"] {
            assert_eq!(initial_client(Some(name)), 2);
        }
        assert_eq!(initial_client(Some("all")), 3);
        let panes = serde_json::json!({"result":{"panes":[{"pane_id":"grok-pane","tab_id":"test-tab","focused":true,"agent":"grok","agent_session":{"agent":"grok","kind":"id","value":"grok-session"}}]}});
        let focus = focused_agent(&panes, "test-tab").unwrap();
        assert_eq!(focus.client, 2);
        assert_eq!(focus.session.unwrap().client, "Grok");
    }
    #[test]
    fn grok_and_codex_account_cards_share_the_same_layout() {
        for unknown in [false, true] {
            let card = accounts::Card {
                id: Some("fixture".into()),
                name: "Work account".into(),
                email: "work@example.com".into(),
                badge: "Pro".into(),
                rows: vec![
                    ("Login".into(), "● Local".into()),
                    ("Accounts".into(), "2".into()),
                    ("Updated".into(), "2m ago".into()),
                ],
                gauges: if unknown {
                    vec![]
                } else {
                    vec![("Weekly".into(), 43.0, "10/06 22:00".into())]
                },
                unknown_gauge: unknown.then(|| ("Weekly".into(), "10/06 22:00".into())),
                ..Default::default()
            };
            let mut monitor = Monitor::default();
            monitor.accounts.codex.card = Some(card.clone());
            monitor.accounts.grok.card = Some(card);
            for visual in [true, false] {
                monitor.visual_mode = visual;
                for width in [20, 32, 48] {
                    monitor.client = 1;
                    let codex = monitor.account_content(width);
                    assert!(codex.iter().all(|line| !line.to_string().trim().is_empty()));
                    let tokens = monitor.stats_content(width);
                    assert_eq!(tokens[0].spans[0].style, codex[0].spans[0].style);
                    assert!(
                        tokens
                            .iter()
                            .all(|line| !line.to_string().trim().is_empty())
                    );
                    monitor.client = 2;
                    let grok = monitor.account_content(width);
                    assert_eq!(codex[1..], grok[1..]);
                    assert!(grok.iter().all(|line| line.width() <= width as usize));
                }
            }
        }
    }
    #[test]
    fn grok_unknown_quota_keeps_a_meter_and_balance_stays_separate() {
        let mut monitor = Monitor {
            client: 2,
            ..Default::default()
        };
        monitor.accounts.grok.card = Some(accounts::Card {
            name: "Grok".into(),
            unknown_gauge: Some(("Weekly credits".into(), "10/06 22:51".into())),
            rows: vec![
                ("Quota".into(), "Usage not published".into()),
                ("Balance".into(), "$0.00".into()),
            ],
            ..Default::default()
        });
        for visual in [true, false] {
            monitor.visual_mode = visual;
            let shown = monitor
                .account_content(30)
                .iter()
                .map(Line::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(shown.contains("Weekly") && shown.contains("—") && shown.contains('▒'));
            assert!(shown.contains("Usage not published"));
            if visual {
                assert!(!shown.contains("Balance"));
            } else {
                assert!(
                    shown.contains("Balance")
                        && shown.find("Weekly credits").unwrap() < shown.find("Balance").unwrap()
                );
            }
            assert!(!shown.contains("0% used"));
        }
    }
    #[test]
    fn grok_account_is_first_without_wake_controls() {
        let mut monitor = Monitor {
            client: 2,
            ..Default::default()
        };
        monitor.accounts.grok.card = Some(accounts::Card {
            id: Some("grok-id".into()),
            name: "Grok".into(),
            ..Default::default()
        });
        for visual in [true, false] {
            monitor.visual_mode = visual;
            let lines = monitor.content(48);
            assert!(lines[0].to_string().starts_with("GROK ACCOUNT"));
            assert!(!lines.iter().any(|line| line.to_string().contains("Wake")));
        }
    }

    #[test]
    fn subscription_home_hides_gateway_and_keeps_accounts_and_local_sessions() {
        for client in [1, 2] {
            let name = if client == 1 { "Codex" } else { "Grok" };
            let mut monitor = Monitor {
                client,
                source_pane: Some("w1:p1".into()),
                active_session: Some(AgentSession {
                    client: name,
                    id: "current".into(),
                }),
                refreshed: Some(Instant::now()),
                sessions_refreshed: Some(Instant::now()),
                ..Default::default()
            };
            monitor.sessions.rows.push(crate::sessions::Session {
                id: "current".into(),
                client: name,
                tokens: crate::sessions::Tokens {
                    input: 800,
                    output: 200,
                    known: true,
                    ..Default::default()
                },
                ..Default::default()
            });
            let card = accounts::Card {
                name: "Fixture account".into(),
                gauges: vec![("Weekly".into(), 25.0, "tomorrow".into())],
                ..Default::default()
            };
            monitor.accounts.codex.card = Some(card.clone());
            monitor.accounts.grok.card = Some(card);
            for subscription in [true, false, true] {
                monitor.accounts.codex.subscription = subscription;
                monitor.accounts.grok.subscription = subscription;
                for visual in [false, true] {
                    monitor.visual_mode = visual;
                    let text = monitor
                        .content(48)
                        .iter()
                        .map(Line::to_string)
                        .collect::<Vec<_>>()
                        .join("\n");
                    assert!(text.contains("Fixture account") && text.contains("Weekly"));
                    assert!(text.contains(if client == 1 {
                        "SESSION / ALL TIME"
                    } else {
                        "SESSION TOKENS"
                    }));
                    assert_eq!(
                        text.contains("CALL HEALTH"),
                        !subscription,
                        "{name}: {text}"
                    );
                    assert_eq!(
                        text.lines()
                            .any(|line| line.starts_with("TOKENS")
                                || line.starts_with("GATEWAY TOKENS")),
                        !subscription
                    );
                    for (width, height) in [(48, 40), (28, 32), (12, 20)] {
                        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                        terminal.draw(|frame| monitor.draw(frame)).unwrap();
                        if subscription {
                            let text = terminal
                                .backend()
                                .buffer()
                                .content
                                .iter()
                                .map(|cell| cell.symbol())
                                .collect::<String>();
                            assert!(
                                !text.contains("CALL HEALTH") && !text.contains("GATEWAY / DETAIL")
                            );
                        }
                    }
                }
            }
            for page in [
                config::PulseStartPage::Sessions,
                config::PulseStartPage::Charts,
            ] {
                monitor.page = page;
                monitor.accounts.codex.subscription = false;
                monitor.accounts.grok.subscription = false;
                let api = monitor.content(48);
                monitor.accounts.codex.subscription = true;
                monitor.accounts.grok.subscription = true;
                assert_eq!(api, monitor.content(48));
            }
            monitor.page = config::PulseStartPage::Home;
            for client in [0, 3] {
                monitor.client = client;
                assert!(
                    monitor
                        .content(48)
                        .iter()
                        .any(|line| line.to_string().contains("CALL HEALTH"))
                );
            }
        }
    }

    #[test]
    fn grok_text_and_compact_views_show_measured_gateway_and_session_rates() {
        let mut monitor = Monitor {
            client: 2,
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        monitor.snapshot.rows.push(crate::usage::Row {
            hour: 9,
            model: "grok".into(),
            day: monitor.snapshot.today(),
            client: "Grok".into(),
            provider: "p".into(),
            name: "Provider".into(),
            kind: "generation".into(),
            totals: Totals {
                calls: 2,
                success: 2,
                input: 1200,
                output: 100,
                speed_output: 100,
                speed_ms: 2000,
                speed_samples: 2,
                ..Default::default()
            },
        });
        monitor.sessions.rows.push(crate::sessions::Session {
            client: "Grok",
            id: "session".into(),
            api_output: 200,
            api_ms: 1000,
            api_samples: 1,
            tokens: crate::sessions::Tokens {
                input: 1000,
                output: 200,
                known: true,
                ..Default::default()
            },
            ..Default::default()
        });
        for visual in [false, true] {
            monitor.visual_mode = visual;
            let text = monitor
                .content(48)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                text.contains("50.0 tok/s")
                    && text.contains("200.0 tok/s")
                    && text.contains("API rate")
            );
            assert!(text.contains("CALL HEALTH"));
            assert!(text.contains("Success rate"));
            assert!(text.contains("100.0%"));
            assert_eq!(text.contains("Output rate (E2E)"), !visual);
            assert_eq!(text.contains("↗ Rate"), visual);
            assert_eq!(text.matches("SESSION TOKENS").count(), 1);
            for (width, height) in [(48, 40), (28, 32), (32, 24)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| monitor.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(text.contains("50.0 tok/s"));
                if visual && width == 48 {
                    assert!(text.contains("CALL HEALTH"));
                }
            }
        }
        monitor.snapshot.rows[0].totals.speed_ms = 0;
        monitor.sessions.rows[0].api_ms = 0;
        let text = monitor
            .content(48)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("50.0 tok/s") && !text.contains("200.0 tok/s"));
        assert_eq!(monitor.client, 2);
    }

    #[test]
    fn grok_gateway_tokens_are_today_only_and_independent_of_sessions() {
        let mut monitor = Monitor {
            client: 2,
            visual_mode: true,
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        for (client, day, input) in [
            ("Grok", monitor.snapshot.today(), 12345),
            ("Claude", monitor.snapshot.today(), 98765),
            ("Grok", "2000-01-01".into(), 99999),
        ] {
            monitor.snapshot.rows.push(crate::usage::Row {
                hour: 9,
                model: "m".into(),
                day,
                client: client.into(),
                provider: "p".into(),
                name: "P".into(),
                kind: "generation".into(),
                totals: Totals {
                    calls: 1,
                    success: 1,
                    input,
                    output: 100,
                    ..Default::default()
                },
            });
        }
        let text = monitor
            .grok_gateway_tokens(48)
            .into_iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("GATEWAY TOKENS") && text.contains("12.3K") && text.contains("100"));
        assert!(!text.contains("98.8K") && !text.contains("100.0K"));
        monitor.snapshot.rows[0].totals.unknown = 1;
        let unknown = monitor
            .grok_gateway_tokens(48)
            .into_iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(unknown.contains("Unknown tokens") && !unknown.contains("12.3K"));
        monitor.snapshot.rows.clear();
        assert!(
            monitor
                .grok_gateway_tokens(48)
                .iter()
                .any(|l| l.to_string().contains("No gateway traffic"))
        );
        monitor.refreshed = None;
        assert!(
            monitor
                .grok_gateway_tokens(48)
                .iter()
                .any(|l| l.to_string().contains("Loading gateway"))
        );
        for (width, height) in [(48, 40), (28, 32), (32, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| monitor.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("GATEWAY TOKENS") && text.contains("SESSION TOKENS"));
        }
    }

    #[test]
    fn grok_dashboard_shows_real_session_tokens_and_separates_recent_from_active() {
        let mut monitor = Monitor {
            client: 2,
            visual_mode: true,
            sessions_refreshed: Some(Instant::now()),
            ..Default::default()
        };
        monitor.sessions.rows.push(crate::sessions::Session {
            id: "grok-live".into(),
            client: "Grok",
            updated: 100,
            tokens: crate::sessions::Tokens {
                input: 1200,
                output: 300,
                read: 600,
                known: true,
                cache_known: true,
                ..Default::default()
            },
            ..Default::default()
        });
        monitor.sessions.rows.push(crate::sessions::Session {
            id: "grok-empty".into(),
            client: "Grok",
            updated: 200,
            ..Default::default()
        });
        let recent = monitor
            .content(48)
            .into_iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            recent.contains("Recent")
                && recent.contains("1200")
                && recent.contains("300")
                && recent.contains("50.0%")
        );
        monitor.active_session = Some(AgentSession {
            client: "Grok",
            id: "grok-live".into(),
        });
        let active = monitor
            .content(48)
            .into_iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(active.contains("● Active") && !active.contains("Recent"));
        monitor.active_session.as_mut().unwrap().id = "missing".into();
        let missing = monitor
            .content(48)
            .into_iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(missing.contains("No token data yet") && !missing.contains("1200"));
    }

    #[test]
    fn account_pages_render_in_normal_and_mini_panes() {
        let mut monitor = Monitor {
            client: 1,
            visual_mode: true,
            accounts: accounts::Accounts {
                codex: accounts::Info {
                    lines: vec![
                        "Account: Personal".into(),
                        "Email: codex@example.com".into(),
                        "Plan: plus".into(),
                        "State: Applied by Mux".into(),
                    ],
                    card: Some(accounts::Card {
                        name: "Personal".into(),
                        email: "codex@example.com".into(),
                        badge: "plus".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                grok: accounts::Info {
                    lines: vec![
                        "Account: grok@example.com".into(),
                        "Weekly credits: 25% used · resets tomorrow".into(),
                        "Remaining allowance: 75%".into(),
                        "API providers: 2 · 6 enabled models".into(),
                    ],
                    card: Some(accounts::Card {
                        name: "Grok".into(),
                        email: "grok@example.com".into(),
                        gauges: vec![("Weekly".into(), 25.0, "tomorrow".into())],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            },
            ..Default::default()
        };
        for (width, height) in [(48, 40), (28, 32), (32, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| monitor.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.contains("CODEX ACCOUNT")
                    && text.contains("Personal")
                    && (text.contains("PLUS") || text.contains("plus"))
            );
            monitor.client = 2;
            monitor.scroll = 0;
            terminal.draw(|frame| monitor.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("GROK ACCOUNT"));
            assert!(text.contains("grok@example.com") && text.contains("25%"));

            monitor.client = 1;
            monitor.scroll = 0;
        }
    }
}
