use super::*;

pub(super) fn initial_client(agent: Option<&str>) -> usize {
    match agent.unwrap_or("").to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claudecode" | "claude code" => 0,
        "codex" => 1,
        "grok" | "grokcli" | "grok-cli" | "grok cli" => 2,
        _ => 3,
    }
}

pub(super) fn rightmost_split_target(layout: &serde_json::Value) -> Option<(&str, u64)> {
    let layout = &layout["result"]["layout"];
    let edge = layout["area"]["x"].as_u64()? + layout["area"]["width"].as_u64()?;
    layout["panes"]
        .as_array()?
        .iter()
        .filter_map(|pane| {
            let rect = &pane["rect"];
            let x = rect["x"].as_u64()?;
            let width = rect["width"].as_u64()?;
            let height = rect["height"].as_u64()?;
            if x + width == edge && width >= 76 {
                Some((pane["pane_id"].as_str()?, width, height))
            } else {
                None
            }
        })
        .max_by_key(|(_, width, height)| (*height, *width))
        .map(|(id, width, _)| (id, width))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentSession {
    pub(super) client: &'static str,
    pub(super) id: String,
}

pub(super) fn agent_session(pane: &serde_json::Value) -> Option<AgentSession> {
    let pane = pane.get("result").map_or(pane, |result| &result["pane"]);
    let session = &pane["agent_session"];
    let agent = session["agent"].as_str()?;
    let client = match (pane["agent"].as_str().unwrap_or(agent), agent) {
        ("codex", "codex") => "Codex",
        ("claude", "claude") => "Claude",
        ("grok", "grok") => "Grok",
        _ => return None,
    };
    let value = session["value"].as_str()?;
    let id = match session["kind"].as_str()? {
        "id" => value,
        "path" => {
            let path = std::path::Path::new(value);
            // Rollout filenames contain timestamps and may include a resume suffix.
            // The session metadata carries the actual thread ID.
            let id = if client == "Codex" {
                crate::sessions::id_from_log(path)?
            } else {
                path.file_stem()?.to_str()?.to_owned()
            };
            return Some(AgentSession { client, id });
        }
        _ => return None,
    };
    (!id.is_empty()).then(|| AgentSession {
        client,
        id: id.into(),
    })
}

pub(super) fn pane_session(pane: &serde_json::Value) -> Option<AgentSession> {
    let reported = agent_session(pane);
    if pane["agent"] != "codex"
        || (pane["agent_session"]["agent"].is_string() && pane["agent_session"]["agent"] != "codex")
    {
        return reported;
    }
    // The daemon can keep reporting a different pane while this CLI changes
    // threads. Its displayed name tracks the attached thread, even then.
    let named = (|| {
        let title = pane["terminal_title_stripped"].as_str()?;
        let cwd = pane["foreground_cwd"]
            .as_str()
            .or_else(|| pane["cwd"].as_str())?;
        let id = crate::sessions::codex_id_from_title(&crate::codex::home().ok()?, title, cwd)?;
        Some(AgentSession {
            client: "Codex",
            id,
        })
    })();
    named.or(reported)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FocusedAgent {
    pub(super) pane_id: String,
    pub(super) client: usize,
    pub(super) session: Option<AgentSession>,
}

pub(super) fn focused_agent(panes: &serde_json::Value, tab_id: &str) -> Option<FocusedAgent> {
    panes["result"]["panes"]
        .as_array()?
        .iter()
        .find_map(|pane| {
            if pane["focused"] != true || pane["tab_id"].as_str()? != tab_id {
                return None;
            }
            let pane_id = pane["pane_id"].as_str()?;
            let client = initial_client(
                pane["agent"]
                    .as_str()
                    .or_else(|| pane["agent_session"]["agent"].as_str()),
            );
            (client != 3).then(|| FocusedAgent {
                pane_id: pane_id.into(),
                client,
                session: pane_session(pane),
            })
        })
}

pub(super) fn focused_or_source_agent(
    panes: &serde_json::Value,
    tab_id: &str,
    source: &str,
) -> Option<FocusedAgent> {
    focused_agent(panes, tab_id).or_else(|| {
        panes["result"]["panes"]
            .as_array()?
            .iter()
            .find(|pane| pane["pane_id"].as_str() == Some(source))
            .and_then(|pane| focused_pane(pane, None, source))
    })
}

pub(super) fn focused_pane(
    pane: &serde_json::Value,
    tab_id: Option<&str>,
    source: &str,
) -> Option<FocusedAgent> {
    if tab_id.is_some_and(|tab| pane["tab_id"].as_str() != Some(tab))
        || (tab_id.is_none() && pane["pane_id"].as_str() != Some(source))
        || (tab_id.is_some() && pane["focused"] != true)
    {
        return None;
    }
    let pane_id = pane["pane_id"].as_str()?;
    let client = initial_client(
        pane["agent"]
            .as_str()
            .or_else(|| pane["agent_session"]["agent"].as_str()),
    );
    (client != 3).then(|| FocusedAgent {
        pane_id: pane_id.into(),
        client,
        session: pane_session(pane),
    })
}

#[derive(Clone, Debug)]
pub(super) struct FocusUpdate {
    pub(super) pane_id: String,
    pub(super) client: usize,
    pub(super) session: Option<AgentSession>,
}

#[derive(Default)]
pub(super) struct FocusTracker {
    pub(super) previous: Option<AgentSession>,
    pub(super) pane: Option<String>,
    pub(super) client: Option<usize>,
    pub(super) missing: u8,
}

impl FocusTracker {
    pub(super) fn observe(
        &mut self,
        focused: Option<FocusedAgent>,
        send: &mpsc::SyncSender<FocusUpdate>,
        event_driven: bool,
    ) -> bool {
        let Some(focused) = focused else { return true };
        let changed_focus = self.pane.as_deref() != Some(focused.pane_id.as_str())
            || self.client != Some(focused.client);
        if changed_focus {
            self.pane = Some(focused.pane_id.clone());
            self.client = Some(focused.client);
            self.previous = focused.session.clone();
            self.missing = 0;
            return send
                .send(FocusUpdate {
                    pane_id: focused.pane_id,
                    client: focused.client,
                    session: focused.session,
                })
                .is_ok();
        }
        // A full pane event is authoritative. The polling fallback waits for
        // three missing observations because CLI snapshots can be transient.
        if event_driven && focused.session.is_none() && self.previous.is_some() {
            self.missing = 2;
        }
        if let Some(session) =
            stable_agent_session(&mut self.previous, &mut self.missing, focused.session)
        {
            return send
                .send(FocusUpdate {
                    pane_id: focused.pane_id,
                    client: focused.client,
                    session,
                })
                .is_ok();
        }
        true
    }
}
pub(super) fn stable_agent_session(
    previous: &mut Option<AgentSession>,
    missing: &mut u8,
    observed: Option<AgentSession>,
) -> Option<Option<AgentSession>> {
    if let Some(current) = observed {
        *missing = 0;
        if previous.as_ref() != Some(&current) {
            *previous = Some(current.clone());
            return Some(Some(current));
        }
    } else {
        *missing = missing.saturating_add(1);
        if *missing >= 3 && previous.take().is_some() {
            return Some(None);
        }
    }
    None
}

impl Monitor {
    pub(super) fn apply_focus(&mut self, update: FocusUpdate) {
        let focus_changed = self.focused_pane.as_deref() != Some(update.pane_id.as_str());
        let agent_changed = self.focused_client != Some(update.client);
        let session_changed = self.active_session != update.session;
        self.focused_pane = Some(update.pane_id);
        self.focused_client = Some(update.client);
        if focus_changed || agent_changed {
            self.client = update.client;
        }
        if focus_changed || agent_changed || session_changed {
            self.active_session = update.session;
            if self.page == config::PulseStartPage::Sessions {
                self.scroll = 0;
            }
        }
    }
}
