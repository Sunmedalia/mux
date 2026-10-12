//! Account summaries and explicit refresh actions. No credentials enter UI state.
use crate::{
    codex,
    config::{self, AppPaths},
    grok,
};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub(super) struct Info {
    pub lines: Vec<String>,
    pub card: Option<Card>,
    pub subscription: bool,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Card {
    pub id: Option<String>,
    pub name: String,
    pub email: String,
    pub badge: String,
    pub rows: Vec<(String, String)>,
    pub gauges: Vec<(String, f64, String)>,
    pub unknown_gauge: Option<(String, String)>,
    pub models: Vec<String>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Accounts {
    pub codex: Info,
    pub grok: Info,
}
fn safe(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(300).collect()
}
fn codex_info(
    config: &config::Config,
    live: Option<&str>,
    local: &str,
    subscription: bool,
) -> Info {
    let active = match &config.codex.active {
        Some(codex::Selection::Account { id }) => Some(id.as_str()),
        _ => None,
    };
    let mut lines = vec![format!("Saved accounts: {}", config.codex.accounts.len())];
    lines.push(safe(local));
    let chosen = live
        .and_then(|id| config.codex.accounts.get(id).map(|account| (id, account)))
        .or_else(|| {
            active.and_then(|id| config.codex.accounts.get(id).map(|account| (id, account)))
        });
    if let Some((id, account)) = chosen {
        lines.push(format!("Account: {}", safe(&account.name)));
        lines.push(format!("Email: {}", safe(&account.email)));
        lines.push(format!(
            "Plan: {}",
            safe(account.plan.as_deref().unwrap_or("unknown"))
        ));
        lines.push(format!(
            "State: {}{}",
            if active == Some(id) {
                "Applied by Mux"
            } else if active.is_some() {
                "Local login differs from Mux selection"
            } else {
                "Saved local login"
            },
            if live == Some(id) {
                " · Local login"
            } else {
                ""
            }
        ));
        let mut public = account.clone();
        public.error = None;
        lines.extend(
            codex::accounts::cached_summary(&public)
                .lines()
                .skip(1)
                .map(safe),
        );
        if active.is_some() && active != live {
            lines.push(format!(
                "Configured: {} · local login differs · reopen Accounts and apply it",
                active
                    .and_then(|id| config.codex.accounts.get(id))
                    .map(|account| safe(&account.name))
                    .unwrap_or("unknown".into())
            ));
        }
        if account.error.is_some() {
            lines.push("Last quota refresh failed · cached data".into());
        }
    } else if active.is_none() {
        lines.push("Mux mode: API providers".into());
    }
    for (id, account) in &config.codex.accounts {
        if chosen.is_some_and(|(chosen, _)| chosen == id) {
            continue;
        }
        lines.push(format!(
            "Saved: {} · {}{}",
            safe(&account.name),
            safe(&account.email),
            if live == Some(id.as_str()) {
                " · Local login"
            } else if active == Some(id.as_str()) {
                " · Configured"
            } else {
                ""
            }
        ));
    }
    lines.push("Quota is cached · e → Accounts to refresh".into());
    let mut card = Card {
        name: "API providers".into(),
        badge: "API".into(),
        ..Default::default()
    };
    if let Some((id, account)) = chosen {
        card.id = Some(id.into());
        card.name = safe(&account.name);
        card.email = safe(&account.email);
        card.badge = safe(account.plan.as_deref().unwrap_or("Account"));
        card.rows.push((
            "Login".into(),
            if live == Some(id) {
                "● Local"
            } else {
                "○ Saved"
            }
            .into(),
        ));
        if active.is_some() && active != live {
            card.rows
                .push(("Switch".into(), "! Login differs · apply again".into()));
        }
        if let Some(error) = &account.error {
            card.rows.push((
                "Quota".into(),
                if error.contains("expired or rejected") {
                    "! Login expired · re-import"
                } else {
                    "! Cached"
                }
                .into(),
            ));
        }
        card.rows
            .push(("Accounts".into(), config.codex.accounts.len().to_string()));
        card.rows.push((
            "Updated".into(),
            account
                .refreshed_at
                .map(|t| {
                    format!(
                        "{}m ago",
                        chrono::Utc::now().timestamp().max(0) as u64 / 60
                            - t.min(chrono::Utc::now().timestamp().max(0) as u64) / 60
                    )
                })
                .unwrap_or("—".into()),
        ));
        for text in &lines {
            if let Some((label, tail)) = text.split_once(": ")
                && let Some((percent, reset)) = tail.split_once("% used · resets ")
                && let Ok(percent) = percent.parse::<f64>()
            {
                card.gauges.push((
                    label.into(),
                    percent,
                    reset.split(" (").next().unwrap_or(reset).into(),
                ));
            }
        }
    }
    Info {
        lines,
        card: Some(card),
        subscription,
    }
}
fn grok_info(
    config: &config::Config,
    home: &std::path::Path,
    usage: Option<&grok::usage::Snapshot>,
    error: Option<&str>,
) -> Info {
    let mut lines = vec![
        grok::auth::status(home)
            .map(|s| s.description())
            .unwrap_or_else(|_| "Cannot read Grok login".into()),
    ];
    if let Some(snapshot) = usage {
        lines.extend(snapshot.summary().lines().map(safe));
    } else {
        lines.push("Account usage not loaded · r refresh".into());
    }
    if let Some(error) = error {
        lines.push(format!(
            "Usage: {}{}",
            safe(error),
            if usage.is_some() { " · cached" } else { "" }
        ));
    }
    let count: usize = config
        .grok
        .profiles
        .values()
        .map(|profile| crate::discovery::active_models(profile, &[]).len())
        .sum();
    lines.push(format!(
        "API providers: {} · {count} enabled models",
        config.grok.profiles.len()
    ));
    lines.push(format!(
        "Default: {}",
        safe(
            config
                .grok
                .preferences
                .default
                .as_deref()
                .unwrap_or("Native default")
        )
    ));
    for (id, profile) in &config.grok.profiles {
        for model in crate::discovery::active_models(profile, &[]) {
            lines.push(format!(
                "{} · {}",
                safe(&profile.name),
                safe(&grok::model_key(&config.grok, id, &model.id))
            ));
        }
    }
    lines.push("Direct API traffic is not in the gateway ledger".into());
    let status = grok::auth::status(home).unwrap_or_default();
    let id = grok::accounts::current_id(home)
        .ok()
        .flatten()
        .or_else(|| grok::usage::account(home).ok().flatten());
    let saved = id.as_ref().and_then(|id| config.grok.accounts.get(id));
    let mut card = Card {
        id: id.clone(),
        name: saved
            .filter(|account| Some(&account.name) != account.email.as_ref())
            .map(|account| safe(&account.name))
            .unwrap_or("Grok".into()),
        email: status.email.clone().unwrap_or_default(),
        badge: if status.expired {
            "Expired"
        } else if status.saved {
            "OAuth"
        } else {
            "API"
        }
        .into(),
        ..Default::default()
    };
    if status.saved {
        card.rows.push((
            "Login".into(),
            if status.expired {
                "● Local · Expired"
            } else {
                "● Local"
            }
            .into(),
        ));
        card.rows.push((
            "Accounts".into(),
            (config.grok.accounts.len() + usize::from(saved.is_none())).to_string(),
        ));
        card.rows.push((
            "Updated".into(),
            usage
                .map(|snapshot| {
                    format!(
                        "{}m ago",
                        (chrono::Utc::now().timestamp() - snapshot.fetched_at).max(0) / 60
                    )
                })
                .unwrap_or("—".into()),
        ));
    }
    if let Some(snapshot) = usage {
        let c = &snapshot.credits;
        if !status.expired
            && let Some(plan) = &c.plan
        {
            card.badge = safe(plan);
        }
        let label = match c.period.as_deref().unwrap_or("") {
            period if period.to_lowercase().contains("week") => "Weekly",
            period if period.to_lowercase().contains("month") => "Monthly",
            period if period.to_lowercase().contains("day") => "Daily",
            _ => "Credits",
        }
        .to_owned();
        let reset = c
            .reset_at
            .as_deref()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%m/%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        if let Some(percent) = c.percent {
            card.gauges.push((label, percent, reset));
        } else {
            card.unknown_gauge = Some((label, reset));
            card.rows
                .push(("Quota".into(), "○ Usage not published · r refresh".into()));
        }
        if let Some(balance) = c.prepaid_cents {
            card.rows
                .push(("Balance".into(), format!("${:.2}", balance as f64 / 100.0)));
        }
        if let Some(used) = c.on_demand_used_cents {
            card.rows
                .push(("On demand".into(), format!("${:.2}", used as f64 / 100.0)));
        }
    } else {
        if status.saved {
            card.unknown_gauge = Some(("Credits".into(), String::new()));
        }
        card.rows
            .push(("Quota".into(), "○ Usage not loaded · r refresh".into()));
    }
    if error.is_some() {
        card.rows.retain(|(label, _)| label != "Quota");
        card.rows.push((
            "Quota".into(),
            if usage.is_some() {
                "! Cached"
            } else {
                "! Unavailable"
            }
            .into(),
        ));
    }
    card.rows.push(("Models".into(), count.to_string()));
    card.models = config
        .grok
        .profiles
        .iter()
        .flat_map(|(id, profile)| {
            crate::discovery::active_models(profile, &[])
                .into_iter()
                .map(move |model| safe(&grok::model_key(&config.grok, id, &model.id)))
        })
        .collect();
    card.rows.push((
        "Default".into(),
        safe(
            config
                .grok
                .preferences
                .default
                .as_deref()
                .unwrap_or("Native"),
        ),
    ));
    Info {
        lines,
        card: Some(card),
        subscription: grok::uses_subscription(home, &config.grok).unwrap_or(false),
    }
}
pub(super) enum AccountRequest {
    Select(usize),
    Refresh(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum QuotaIdentity {
    Codex {
        selection: Option<codex::Selection>,
        live: Option<String>,
        account: String,
    },
    Grok {
        home: PathBuf,
        account: String,
    },
}
impl QuotaIdentity {
    fn provider(&self) -> usize {
        match self {
            Self::Codex { .. } => 0,
            Self::Grok { .. } => 1,
        }
    }
    fn account(&self) -> &str {
        match self {
            Self::Codex { account, .. } | Self::Grok { account, .. } => account,
        }
    }
}
struct QuotaJob {
    id: u64,
    generation: u64,
    identity: QuotaIdentity,
    manual: bool,
}
enum QuotaOutcome {
    Codex(Result<(), String>),
    Grok(Result<grok::usage::Snapshot, String>),
}
struct QuotaDone {
    job: QuotaJob,
    outcome: QuotaOutcome,
}
#[derive(Default)]
struct QuotaState {
    codex_identity: Option<QuotaIdentity>,
    grok_identity: Option<QuotaIdentity>,
    codex_generation: u64,
    grok_generation: u64,
    next_id: u64,
    in_flight: Option<(u64, usize)>,
    pending: [Option<QuotaJob>; 2],
    grok_cache: Option<grok::usage::Snapshot>,
    grok_error: Option<String>,
    codex_error: Option<String>,
    grok_checked: Option<Instant>,
}
impl QuotaState {
    fn observe(&mut self, codex: Option<QuotaIdentity>, grok: Option<QuotaIdentity>) {
        if self.codex_identity != codex {
            self.codex_identity = codex;
            self.codex_generation = self.codex_generation.wrapping_add(1);
            self.pending[0] = None;
            self.codex_error = None;
        }
        if self.grok_identity != grok {
            self.grok_identity = grok;
            self.grok_generation = self.grok_generation.wrapping_add(1);
            self.pending[1] = None;
            self.grok_cache = None;
            self.grok_error = None;
            self.grok_checked = None;
        }
    }
    fn queue(&mut self, provider: usize, manual: bool) {
        if self.pending[provider]
            .as_ref()
            .is_some_and(|job| job.manual || !manual)
        {
            return;
        }
        let (identity, generation) = if provider == 0 {
            (&self.codex_identity, self.codex_generation)
        } else {
            (&self.grok_identity, self.grok_generation)
        };
        let Some(identity) = identity else {
            return;
        };
        self.next_id = self.next_id.wrapping_add(1);
        self.pending[provider] = Some(QuotaJob {
            id: self.next_id,
            generation,
            identity: identity.clone(),
            manual,
        });
    }
    fn schedule(&mut self, client: usize, manual: bool, now: Instant) {
        if client != 2 && self.pending[1].as_ref().is_some_and(|job| !job.manual) {
            self.pending[1] = None;
        }
        match client {
            1 if manual => self.queue(0, true),
            // An automatic attempt already running must not enqueue another.
            2 if manual
                || (self.in_flight.is_none_or(|(_, provider)| provider != 1)
                    && self.grok_checked.is_none_or(|checked| {
                        now.saturating_duration_since(checked) >= Duration::from_secs(60)
                    })) =>
            {
                self.queue(1, manual)
            }
            _ => {}
        }
    }
    fn dispatch(&mut self, jobs: &mpsc::SyncSender<QuotaJob>, now: Instant) -> bool {
        if self.in_flight.is_some() {
            return true;
        }
        let next = (0..2)
            .filter_map(|provider| {
                self.pending[provider]
                    .as_ref()
                    .map(|job| (provider, !job.manual, job.id))
            })
            .min_by_key(|&(_, automatic, id)| (automatic, id));
        let Some((provider, _, id)) = next else {
            return true;
        };
        let job = self.pending[provider].take().unwrap();
        if jobs.try_send(job).is_err() {
            self.unavailable();
            return false;
        }
        self.in_flight = Some((id, provider));
        if provider == 1 {
            self.grok_checked = Some(now);
        }
        true
    }
    fn accept(&mut self, done: QuotaDone) {
        let provider = done.job.identity.provider();
        if self.in_flight != Some((done.job.id, provider)) {
            return;
        }
        self.in_flight = None;
        let (identity, generation) = if provider == 0 {
            (&self.codex_identity, self.codex_generation)
        } else {
            (&self.grok_identity, self.grok_generation)
        };
        if identity.as_ref() != Some(&done.job.identity) || generation != done.job.generation {
            return;
        }
        match done.outcome {
            QuotaOutcome::Codex(result) if provider == 0 => {
                self.codex_error = result
                    .err()
                    .map(|_| "! Refresh failed; cached limits may be stale".into());
            }
            QuotaOutcome::Grok(Ok(snapshot))
                if provider == 1 && snapshot.account == done.job.identity.account() =>
            {
                self.grok_cache = Some(snapshot);
                self.grok_error = None;
            }
            QuotaOutcome::Grok(Err(error)) if provider == 1 => self.grok_error = Some(safe(&error)),
            _ => {}
        }
    }
    fn unavailable(&mut self) {
        self.in_flight = None;
        self.pending = [None, None];
        if self.codex_identity.is_some() {
            self.codex_error = Some("! Quota worker unavailable".into());
        }
        if self.grok_identity.is_some() {
            self.grok_error = Some("Quota worker unavailable".into());
        }
    }
}

fn codex_identity(
    config: &config::Config,
    login: &codex::accounts::LiveLogin,
) -> Option<QuotaIdentity> {
    let account = match &config.codex.active {
        Some(codex::Selection::Account { id }) => Some(id.as_str()),
        _ => login.id.as_deref(),
    }?;
    config
        .codex
        .accounts
        .contains_key(account)
        .then(|| QuotaIdentity::Codex {
            selection: config.codex.active.clone(),
            live: login.id.clone(),
            account: account.into(),
        })
}
fn current_quota_identity(paths: &AppPaths, provider: usize) -> Option<QuotaIdentity> {
    if provider == 0 {
        let config = config::load(&paths.config).ok()?;
        let login = codex::accounts::live_login_state().ok()?;
        codex_identity(&config, &login)
    } else {
        let home = grok::home().ok()?;
        let account = grok::usage::account(&home).ok()??;
        Some(QuotaIdentity::Grok { home, account })
    }
}
fn local_accounts(
    paths: &AppPaths,
    state: &mut QuotaState,
    done: Option<QuotaDone>,
    worker_live: bool,
) -> Accounts {
    let Ok(config) = config::load(&paths.config) else {
        state.observe(None, None);
        if let Some(done) = done {
            state.accept(done);
        }
        return Accounts {
            codex: Info {
                lines: vec!["Cannot read Mux account configuration".into()],
                ..Default::default()
            },
            grok: Info {
                lines: vec!["Cannot read Grok configuration".into()],
                ..Default::default()
            },
        };
    };
    let login = codex::accounts::live_login_state();
    let home = grok::home();
    let codex_key = login
        .as_ref()
        .ok()
        .and_then(|login| codex_identity(&config, login));
    let grok_key = home.as_ref().ok().and_then(|home| {
        grok::usage::account(home)
            .ok()
            .flatten()
            .map(|account| QuotaIdentity::Grok {
                home: home.clone(),
                account,
            })
    });
    state.observe(codex_key, grok_key);
    if let Some(done) = done {
        state.accept(done);
    }
    if !worker_live {
        state.unavailable();
    }
    let mut codex = match login {
        Ok(login) => codex_info(
            &config,
            login.id.as_deref(),
            &login.summary,
            login.subscription,
        ),
        Err(_) => codex_info(&config, None, "Local Codex login unavailable", false),
    };
    if let Some(error) = &state.codex_error
        && let Some(identity) = &state.codex_identity
        && let Some(card) = &mut codex.card
        && card.id.as_deref() == Some(identity.account())
    {
        card.rows.retain(|(label, _)| label != "Quota");
        card.rows.push(("Quota".into(), error.clone()));
        codex.lines.push(error.clone());
    }
    let grok = match home {
        Ok(home) => grok_info(
            &config,
            &home,
            state.grok_cache.as_ref(),
            state.grok_error.as_deref(),
        ),
        Err(_) => Info {
            lines: vec!["Grok home unavailable".into()],
            ..Default::default()
        },
    };
    Accounts { codex, grok }
}
fn run_quota_job(paths: &AppPaths, job: QuotaJob) -> QuotaDone {
    let matches =
        current_quota_identity(paths, job.identity.provider()).as_ref() == Some(&job.identity);
    let outcome = match &job.identity {
        QuotaIdentity::Codex { account, .. } => QuotaOutcome::Codex(if matches {
            codex::accounts::refresh(paths, account).map_err(|_| "Refresh failed".into())
        } else {
            Err("Account changed · r refresh".into())
        }),
        QuotaIdentity::Grok { home, .. } => QuotaOutcome::Grok(if matches {
            grok::usage::fetch(home).map_err(|error| error.to_string())
        } else {
            Err("Account changed · r refresh".into())
        }),
    };
    QuotaDone { job, outcome }
}
fn spawn_with_executor<F>(
    paths: AppPaths,
    updates: mpsc::SyncSender<Accounts>,
    requests: mpsc::Receiver<AccountRequest>,
    initial: usize,
    mut executor: F,
) where
    F: FnMut(QuotaJob) -> QuotaDone + Send + 'static,
{
    let (jobs, work) = mpsc::sync_channel(1);
    let (results, completed) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        while let Ok(job) = work.recv() {
            if results.send(executor(job)).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let mut client = initial;
        let mut state = QuotaState::default();
        let mut local_due = Instant::now();
        let mut request = None;
        let mut worker_live = true;
        loop {
            let now = Instant::now();
            let mut collect = now >= local_due;
            let mut manual = false;
            if let Some(next) = request.take() {
                match next {
                    AccountRequest::Select(selected) => client = selected,
                    AccountRequest::Refresh(selected) => {
                        client = selected;
                        manual = true;
                    }
                }
                collect = true;
            }
            let done = if worker_live {
                match completed.try_recv() {
                    Ok(done) => {
                        collect = true;
                        Some(done)
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        worker_live = false;
                        state.unavailable();
                        collect = true;
                        None
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                }
            } else {
                None
            };
            if collect {
                let info = local_accounts(&paths, &mut state, done, worker_live);
                if updates.send(info).is_err() {
                    break;
                }
                if now >= local_due {
                    local_due = Instant::now() + Duration::from_secs(2);
                }
            }
            if worker_live {
                state.schedule(client, manual, Instant::now());
                worker_live = state.dispatch(&jobs, Instant::now());
                if !worker_live {
                    let info = local_accounts(&paths, &mut state, None, false);
                    if updates.send(info).is_err() {
                        break;
                    }
                }
            }
            let mut deadline = local_due;
            if worker_live
                && client == 2
                && state.grok_identity.is_some()
                && state.in_flight.is_none()
                && state.pending[1].is_none()
                && let Some(checked) = state.grok_checked
            {
                deadline = deadline.min(checked + Duration::from_secs(60));
            }
            let mut wait = deadline.saturating_duration_since(Instant::now());
            if worker_live && state.in_flight.is_some() {
                wait = wait.min(Duration::from_millis(100));
            }
            match requests.recv_timeout(wait) {
                Ok(next) => request = Some(next),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}
pub(super) fn spawn(
    paths: AppPaths,
    updates: mpsc::SyncSender<Accounts>,
    requests: mpsc::Receiver<AccountRequest>,
    initial: usize,
) {
    let quota_paths = paths.clone();
    spawn_with_executor(paths, updates, requests, initial, move |job| {
        run_quota_job(&quota_paths, job)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    mod codex_tests;
    mod grok_tests;

    fn run_quota_case(case: &str) {
        let root = tempfile::tempdir().unwrap();
        for directory in [
            "codex",
            "grok",
            "claude",
            "config-root",
            "state",
            "cache",
            "tmp",
        ] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        std::fs::write(
            root.path().join("codex/config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "tui::quick::accounts::tests::quota_scenario_child",
            "--ignored",
            "--nocapture",
        ]);
        for key in [
            "HOME",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "APPDATA",
            "LOCALAPPDATA",
            "MUX_CONFIG",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "GROK_HOME",
            "PI_CODING_AGENT_DIR",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "MUX_CODEX_BIN",
            "MUX_CLAUDE_BIN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "OPENAI_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "CODEX_AUTH",
            "HERDR_ENV",
            "HERDR_SOCKET_PATH",
            "HERDR_CONFIG_PATH",
            "HERDR_BIN_PATH",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
        ] {
            command.env_remove(key);
        }
        for (key, child) in [
            ("MUX_CONFIG", "config.toml"),
            ("CODEX_HOME", "codex"),
            ("GROK_HOME", "grok"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("PI_CODING_AGENT_DIR", "pi"),
            ("XDG_CONFIG_HOME", "config-root"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("APPDATA", "roaming"),
            ("LOCALAPPDATA", "local"),
            ("TMP", "tmp"),
            ("TEMP", "tmp"),
            ("TMPDIR", "tmp"),
        ] {
            command.env(key, root.path().join(child));
        }
        let output = command
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("MUX_TEST_QUOTA_CASE", case)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "isolated subprocess driver"]
    fn quota_scenario_child() {
        let case = std::env::var("MUX_TEST_QUOTA_CASE").expect("quota scenario required");
        if case.starts_with("codex") {
            codex_tests::scenario(&case);
        } else if case.starts_with("grok") {
            grok_tests::scenario(&case);
        } else {
            panic!("Unknown quota scenario: {case}");
        }
    }

    #[test]
    fn grok_subscription_mode_follows_native_model_even_with_saved_oauth() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("auth.json"),
            r#"{"auth_mode":"oidc","key":"fixture","expires_at":"2000-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let mut config = config::Config::default();
        config.grok.active_mode = Some(grok::Mode::Api);
        config.grok.preferences.default = Some("mux::provider::model".into());
        for (native, subscription) in [
            ("", false),
            ("[models]\ndefault='grok-build'", true),
            ("[models]\ndefault='mux::provider::model'", false),
            (
                "[models]\ndefault='custom'\n[model.custom]\nbase_url='https://example.invalid/v1'",
                false,
            ),
            (
                "[models]\ndefault='grok-build'\n[endpoints]\nmodels_base_url='https://example.invalid'",
                false,
            ),
        ] {
            std::fs::write(home.path().join("config.toml"), native).unwrap();
            assert_eq!(
                grok_info(&config, home.path(), None, None).subscription,
                subscription,
                "{native}"
            );
        }
        std::fs::write(
            home.path().join("config.toml"),
            "[models]\ndefault='grok-build'",
        )
        .unwrap();
        std::fs::write(
            home.path().join("auth.json"),
            r#"{"auth_mode":"api_key","key":"fixture"}"#,
        )
        .unwrap();
        assert!(!grok_info(&config, home.path(), None, None).subscription);
    }

    #[test]
    fn saved_codex_account_summary_distinguishes_applied_and_local_and_omits_errors() {
        let mut config = config::Config::default();
        config.codex.accounts.insert(
            "one".into(),
            codex::accounts::Account {
                name: "Personal".into(),
                email: "one@example.com".into(),
                plan: Some("plus".into()),
                error: Some("SECRET-TOKEN".into()),
                ..Default::default()
            },
        );
        config.codex.accounts.insert(
            "two".into(),
            codex::accounts::Account {
                name: "Work".into(),
                email: "two@example.com".into(),
                ..Default::default()
            },
        );
        config.codex.active = Some(codex::Selection::Account { id: "one".into() });
        let summary = codex_info(
            &config,
            Some("two"),
            "Provider: openai · Local login: two@example.com",
            true,
        )
        .lines
        .join("\n");
        for text in [
            "Personal",
            "one@example.com",
            "Local login differs from Mux selection",
            "Configured: Personal",
            "two@example.com",
            "Local login",
            "cached",
        ] {
            assert!(summary.contains(text), "missing {text}: {summary}");
        }
        assert!(!summary.contains("SECRET-TOKEN"));
        assert_eq!(
            codex_info(&config, Some("two"), "Local login: two@example.com", true)
                .card
                .unwrap()
                .name,
            "Work"
        );
    }
    #[test]
    fn grok_summary_shows_login_usage_and_models_without_credentials() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("auth.json"),
            r#"{"auth_mode":"oidc","key":"SECRET","email":"grok@example.com"}"#,
        )
        .unwrap();
        let snapshot = grok::usage::Snapshot {
            account: "test".into(),
            credits: grok::usage::Credits {
                percent: Some(30.0),
                ..Default::default()
            },
            fetched_at: 1,
        };
        let text = grok_info(
            &config::Config::default(),
            home.path(),
            Some(&snapshot),
            Some("Unavailable"),
        )
        .lines
        .join("\n");
        assert!(
            text.contains("grok@example.com")
                && text.contains("30.0% used")
                && text.contains("70.0%")
        );
        assert!(text.contains("cached") && !text.contains("SECRET"));
    }
}
