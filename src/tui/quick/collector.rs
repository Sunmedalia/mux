use super::*;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub(super) struct SessionRefresh {
    wake: mpsc::SyncSender<()>,
    manual: Arc<AtomicBool>,
}

impl SessionRefresh {
    pub(super) fn request(&self) {
        self.manual.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }

    fn take_manual(&self, wakes: &mpsc::Receiver<()>) -> bool {
        if !self.manual.swap(false, Ordering::AcqRel) {
            return false;
        }
        drain_session_wakes(wakes);
        true
    }
}

fn drain_session_wakes(wakes: &mpsc::Receiver<()>) {
    while wakes.try_recv().is_ok() {}
}

fn session_interval(complete: bool) -> Duration {
    Duration::from_secs(if complete { 30 } else { 2 })
}

struct SessionSchedule {
    last_read: Instant,
    watch_attempt: Instant,
    event_due: Option<Instant>,
}

impl SessionSchedule {
    fn wait(&self, now: Instant, complete: bool) -> Duration {
        let interval = session_interval(complete);
        let mut deadline = (self.last_read + interval).min(self.watch_attempt + interval);
        if let Some(event_due) = self.event_due {
            deadline = deadline.min(event_due);
        }
        deadline.saturating_duration_since(now)
    }

    fn on_event(&mut self, now: Instant) {
        if self.event_due.is_none() {
            self.event_due = Some(now + Duration::from_millis(150));
        }
    }

    fn on_read(&mut self, now: Instant) {
        self.last_read = now;
        self.event_due = None;
    }

    fn read_due(&self, now: Instant, complete: bool) -> bool {
        now >= self.last_read + session_interval(complete)
            || self.event_due.is_some_and(|deadline| now >= deadline)
    }

    fn watch_due(&self, now: Instant, complete: bool) -> bool {
        now >= self.watch_attempt + session_interval(complete)
    }
}

pub(super) fn spawn_session_reader(
    session_send: mpsc::SyncSender<crate::sessions::Snapshot>,
) -> SessionRefresh {
    let (wake, wakes) = mpsc::sync_channel(1);
    let refresh = SessionRefresh {
        wake,
        manual: Arc::new(AtomicBool::new(false)),
    };
    let worker_refresh = refresh.clone();
    std::thread::spawn(move || {
        let mut reader = crate::sessions::Reader::default();
        let mut watcher: Option<crate::sessions::ChangeWatch> = None;
        let mut watched_roots = None;
        let mut watched_dirs = Vec::new();
        let now = Instant::now();
        let mut schedule = SessionSchedule {
            last_read: now,
            watch_attempt: now,
            event_due: None,
        };
        let mut startup = true;
        let mut last_titles = Instant::now();
        let mut last_snapshot: Option<crate::sessions::Snapshot> = None;
        loop {
            // Consume before reading, never after: a request arriving during read
            // remains sticky even if the bounded wake channel is already full.
            let manual = worker_refresh.take_manual(&wakes);
            let roots = crate::sessions::roots().ok();
            let roots_changed = roots != watched_roots
                || roots.as_ref().is_some_and(|roots| {
                    roots
                        .iter()
                        .zip(&watched_dirs)
                        .any(|((root, _), existed)| root.is_dir() != *existed)
                });
            let now = Instant::now();
            let complete = watcher
                .as_ref()
                .is_some_and(crate::sessions::ChangeWatch::is_complete);
            if startup || roots_changed || schedule.watch_due(now, complete) {
                watched_dirs = roots
                    .as_ref()
                    .map(|roots| roots.iter().map(|(root, _)| root.is_dir()).collect())
                    .unwrap_or_default();
                watcher = roots.as_ref().map(|roots| {
                    crate::sessions::watch_changes(roots, worker_refresh.wake.clone())
                });
                watched_roots = roots;
                schedule.watch_attempt = Instant::now();
            }
            let complete = watcher
                .as_ref()
                .is_some_and(crate::sessions::ChangeWatch::is_complete);
            if startup || roots_changed || manual || schedule.read_due(Instant::now(), complete) {
                // Only queued, pre-read events belong to this collection. Events
                // and manual requests arriving during read are left for next time.
                drain_session_wakes(&wakes);
                let snapshot = match &watched_roots {
                    Some(roots) => reader.read(roots),
                    None => crate::sessions::Snapshot {
                        rows: vec![],
                        warnings: 1,
                    },
                };
                schedule.on_read(Instant::now());
                startup = false;
                last_titles = Instant::now();
                last_snapshot = Some(snapshot.clone());
                if session_send.send(snapshot).is_err() {
                    break;
                }
                continue;
            }
            if last_titles.elapsed() >= Duration::from_secs(2) {
                if let Some(snapshot) = &mut last_snapshot
                    && reader.refresh_titles(snapshot)
                    && session_send.send(snapshot.clone()).is_err()
                {
                    break;
                }
                last_titles = Instant::now();
            }
            let title_wait = Duration::from_secs(2).saturating_sub(last_titles.elapsed());
            match wakes.recv_timeout(schedule.wait(Instant::now(), complete).min(title_wait)) {
                Ok(()) => schedule.on_event(Instant::now()),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });
    refresh
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
#[cfg(test)]
mod session_tests {
    use super::*;

    fn schedule(now: Instant) -> SessionSchedule {
        SessionSchedule {
            last_read: now,
            watch_attempt: now,
            event_due: None,
        }
    }

    fn refresh_channel() -> (SessionRefresh, mpsc::Receiver<()>) {
        let (wake, wakes) = mpsc::sync_channel(1);
        (
            SessionRefresh {
                wake,
                manual: Arc::new(AtomicBool::new(false)),
            },
            wakes,
        )
    }

    #[test]
    fn complete_silent_schedule_waits_thirty_seconds() {
        let start = Instant::now();
        let schedule = schedule(start);
        for seconds in [2, 10, 29] {
            let now = start + Duration::from_secs(seconds);
            assert!(!schedule.read_due(now, true));
            assert!(!schedule.watch_due(now, true));
            assert_eq!(schedule.wait(now, true), Duration::from_secs(30 - seconds));
        }
        let due = start + Duration::from_secs(30);
        assert!(schedule.read_due(due, true));
        assert!(schedule.watch_due(due, true));
        assert_eq!(schedule.wait(due, true), Duration::ZERO);
        assert_eq!(
            schedule.wait(due + Duration::from_secs(1), true),
            Duration::ZERO
        );
    }

    #[test]
    fn event_bursts_keep_the_first_deadline() {
        let start = Instant::now();
        let mut schedule = schedule(start);
        let first = start + Duration::from_secs(1);
        schedule.on_event(first);
        for millis in [50, 100, 149] {
            let now = first + Duration::from_millis(millis);
            schedule.on_event(now);
            assert!(!schedule.read_due(now, true));
            assert_eq!(
                schedule.wait(now, true),
                Duration::from_millis(150 - millis)
            );
        }
        let due = first + Duration::from_millis(150);
        schedule.on_event(due);
        schedule.on_event(due + Duration::from_millis(50));
        assert!(schedule.read_due(due, true));
        assert_eq!(schedule.wait(due, true), Duration::ZERO);
        schedule.on_read(due);
        assert!(schedule.event_due.is_none());
        assert!(!schedule.read_due(due, true));
        assert!(!schedule.read_due(due + Duration::from_millis(150), true));
    }

    #[test]
    fn incomplete_and_unhealthy_schedules_fall_back_to_two_seconds() {
        let start = Instant::now();
        let mut schedule = schedule(start);
        let before = start + Duration::from_millis(1_999);
        assert!(!schedule.read_due(before, false));
        assert_eq!(schedule.wait(before, false), Duration::from_millis(1));
        let due = start + Duration::from_secs(2);
        assert!(schedule.read_due(due, false));
        assert!(schedule.watch_due(due, false));
        assert_eq!(schedule.wait(due, false), Duration::ZERO);
        assert!(!schedule.read_due(due, true));

        // A successful rebuild and completed read restore the long interval.
        schedule.watch_attempt = due;
        schedule.on_read(due);
        assert_eq!(schedule.wait(due, true), Duration::from_secs(30));
        assert!(!schedule.read_due(due + Duration::from_secs(29), true));
        assert!(schedule.read_due(due + Duration::from_secs(30), true));
    }

    #[test]
    fn read_completion_and_watch_attempt_have_independent_deadlines() {
        let start = Instant::now();
        let mut schedule = schedule(start);
        let read_done = start + Duration::from_secs(5);
        schedule.on_read(read_done);
        assert_eq!(schedule.watch_attempt, start);
        assert_eq!(schedule.wait(read_done, true), Duration::from_secs(25));
        let watch_due = start + Duration::from_secs(30);
        assert!(schedule.watch_due(watch_due, true));
        assert!(!schedule.read_due(watch_due, true));
        schedule.watch_attempt = watch_due;
        assert_eq!(schedule.wait(watch_due, true), Duration::from_secs(5));
        assert!(schedule.read_due(read_done + Duration::from_secs(30), true));
    }

    #[test]
    fn manual_request_interrupts_debounce_and_drains_its_wake() {
        let start = Instant::now();
        let mut schedule = schedule(start);
        let (refresh, wakes) = refresh_channel();
        schedule.on_event(start);
        let now = start + Duration::from_millis(50);
        assert!(!schedule.read_due(now, true));
        refresh.request();
        assert!(refresh.take_manual(&wakes));
        assert!(matches!(wakes.try_recv(), Err(mpsc::TryRecvError::Empty)));
        schedule.on_read(now);
        assert!(schedule.event_due.is_none());
        assert!(!refresh.take_manual(&wakes));
        assert!(!schedule.read_due(start + Duration::from_millis(150), true));
    }

    #[test]
    fn manual_request_stays_sticky_when_the_wake_channel_is_full() {
        let (refresh, wakes) = refresh_channel();
        refresh.wake.try_send(()).unwrap();
        for _ in 0..3 {
            refresh.request();
        }
        assert!(refresh.take_manual(&wakes));
        assert!(!refresh.take_manual(&wakes));
        assert!(matches!(wakes.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn manual_request_during_read_survives_completion() {
        let start = Instant::now();
        let mut schedule = schedule(start);
        let (refresh, wakes) = refresh_channel();
        refresh.request();
        assert!(refresh.take_manual(&wakes));
        drain_session_wakes(&wakes);

        let (reading, read_started) = mpsc::sync_channel(0);
        let (requested, request_done) = mpsc::sync_channel(0);
        let requester = refresh.clone();
        let thread = std::thread::spawn(move || {
            read_started.recv().unwrap();
            // An event already queued during collection must not prevent manual
            // refresh from being observed in the next collector iteration.
            requester.wake.try_send(()).unwrap();
            requester.request();
            requested.send(()).unwrap();
        });
        reading.send(()).unwrap();
        request_done.recv().unwrap();
        schedule.on_read(start + Duration::from_secs(1));
        assert!(refresh.take_manual(&wakes));
        assert!(matches!(wakes.try_recv(), Err(mpsc::TryRecvError::Empty)));
        schedule.on_read(start + Duration::from_secs(2));
        assert!(!refresh.take_manual(&wakes));
        thread.join().unwrap();
    }

    #[test]
    fn event_during_read_remains_queued_for_the_next_debounce() {
        let start = Instant::now();
        let mut schedule = schedule(start);
        let (refresh, wakes) = refresh_channel();
        drain_session_wakes(&wakes);
        refresh.wake.try_send(()).unwrap();
        let read_done = start + Duration::from_secs(1);
        schedule.on_read(read_done);
        assert!(!refresh.take_manual(&wakes));
        wakes.try_recv().unwrap();
        schedule.on_event(read_done);
        assert_eq!(schedule.wait(read_done, true), Duration::from_millis(150));
        assert!(schedule.read_due(read_done + Duration::from_millis(150), true));
    }

    #[test]
    fn disconnected_wake_receiver_does_not_clear_a_manual_request() {
        let (refresh, wakes) = refresh_channel();
        drop(wakes);
        refresh.request();
        assert!(refresh.manual.load(Ordering::Acquire));
    }
}
