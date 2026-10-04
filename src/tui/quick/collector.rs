use super::*;

pub(super) fn spawn_session_reader(
    session_send: mpsc::SyncSender<crate::sessions::Snapshot>,
    session_requests: mpsc::Receiver<()>,
    session_refresh: mpsc::SyncSender<()>,
) {
    std::thread::spawn(move || {
        let mut reader = crate::sessions::Reader::default();
        let mut watcher = None;
        let mut watched_at = Instant::now();
        loop {
            let roots = crate::sessions::roots();
            if watcher.is_none() || watched_at.elapsed() >= Duration::from_secs(30) {
                watcher = roots.as_ref().ok().and_then(|roots| {
                    crate::sessions::watch_changes(roots, session_refresh.clone())
                });
                watched_at = Instant::now();
            }
            let snapshot = match roots {
                Ok(roots) => reader.read(&roots),
                Err(_) => crate::sessions::Snapshot {
                    rows: vec![],
                    warnings: 1,
                },
            };
            if session_send.send(snapshot).is_err() {
                break;
            }
            let wait = if watcher.is_some() {
                Duration::from_secs(2)
                    .min(Duration::from_secs(30).saturating_sub(watched_at.elapsed()))
            } else {
                Duration::from_secs(2)
            };
            match session_requests.recv_timeout(wait) {
                Ok(()) => {
                    std::thread::sleep(Duration::from_millis(150));
                    while session_requests.try_recv().is_ok() {}
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });
}

pub(super) fn current_focus(
    workspace: Option<&str>,
    tab: Option<&str>,
    source: &str,
) -> Option<FocusedAgent> {
    match (workspace, tab) {
        (Some(workspace), Some(tab)) => herdr(&["pane", "list", "--workspace", workspace])
            .ok()
            .and_then(|panes| focused_or_source_agent(&panes, tab, source)),
        _ => herdr(&["pane", "get", source])
            .ok()
            .and_then(|result| focused_pane(&result["result"]["pane"], None, source)),
    }
}

pub(super) fn focus_from_event(
    event: &serde_json::Value,
    workspace: Option<&str>,
    tab: Option<&str>,
    source: &str,
) -> Option<FocusedAgent> {
    let data = &event["data"];
    match event["event"].as_str()? {
        "pane_updated" | "pane_created" | "pane_moved" => focused_pane(&data["pane"], tab, source),
        "pane_focused" | "pane_agent_detected" => {
            let id = data["pane_id"].as_str()?;
            herdr(&["pane", "get", id])
                .ok()
                .and_then(|result| focused_pane(&result["result"]["pane"], tab, source))
        }
        "pane_closed" => current_focus(workspace, tab, source),
        _ => None,
    }
}

#[cfg(unix)]
pub(super) fn focus_event_socket(path: &std::path::Path) -> Result<Box<dyn ReadWrite>> {
    let socket = std::os::unix::net::UnixStream::connect(path)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    Ok(Box::new(socket))
}

#[cfg(windows)]
pub(super) fn focus_event_socket(path: &std::path::Path) -> Result<Box<dyn ReadWrite>> {
    Ok(Box::new(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?,
    ))
}

pub(super) trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

pub(super) fn read_focus_subscription_response(reader: &mut impl BufRead) -> Result<()> {
    let mut response = String::new();
    anyhow::ensure!(
        reader.read_line(&mut response)? > 0,
        "Herdr subscription closed"
    );
    let response: serde_json::Value = serde_json::from_str(&response)?;
    anyhow::ensure!(
        response["result"].is_object(),
        "Herdr subscription rejected"
    );
    Ok(())
}

pub(super) fn follow_focus_events(
    tracker: &mut FocusTracker,
    send: &mpsc::SyncSender<FocusUpdate>,
    workspace: Option<&str>,
    tab: Option<&str>,
    source: &str,
) -> Result<bool> {
    let path = std::env::var_os("HERDR_SOCKET_PATH").context("Herdr socket unavailable")?;
    let mut socket = focus_event_socket(std::path::Path::new(&path))?;
    let request = serde_json::json!({
        "id": "mux_focus",
        "method": "events.subscribe",
        "params": {"subscriptions": [
            {"type": "pane.focused"},
            {"type": "pane.updated"},
            {"type": "pane.created"},
            {"type": "pane.moved"},
            {"type": "pane.closed"},
            {"type": "pane.agent_detected"}
        ]}
    });
    socket.write_all(request.to_string().as_bytes())?;
    socket.write_all(b"\n")?;
    let mut reader = BufReader::new(socket);
    read_focus_subscription_response(&mut reader)?;
    let mut line = String::new();
    if !tracker.observe(current_focus(workspace, tab, source), send, true) {
        return Ok(false);
    }
    loop {
        match reader.read_line(&mut line) {
            Ok(0) => anyhow::bail!("Herdr subscription closed"),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                if !tracker.observe(current_focus(workspace, tab, source), send, false) {
                    return Ok(false);
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        }
        let event: serde_json::Value = serde_json::from_str(&line)?;
        line.clear();
        if !tracker.observe(focus_from_event(&event, workspace, tab, source), send, true) {
            return Ok(false);
        }
    }
}
