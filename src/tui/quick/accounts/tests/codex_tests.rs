use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

// These are deadlock limits, not assertions about how quickly local IO completes.
const DEADLOCK_LIMIT: Duration = Duration::from_secs(30);

#[test]
fn codex_local_identity_is_not_blocked_by_quota_and_rejects_late_errors() {
    run_quota_case("codex_local_identity_and_late_error");
}

#[test]
fn codex_returning_to_an_account_rejects_its_previous_generation() {
    run_quota_case("codex_aba_generation");
}

#[test]
fn codex_refresh_is_explicit_and_targets_active_before_saved_live() {
    run_quota_case("codex_explicit_refresh_and_target");
}

#[test]
fn codex_repeated_refreshes_coalesce_on_one_serial_worker() {
    run_quota_case("codex_coalesced_serial_worker");
}

#[test]
fn codex_same_identity_failure_survives_ticks_until_success() {
    run_quota_case("codex_sticky_failure_and_recovery");
}

pub(super) fn scenario(case: &str) {
    match case {
        "codex_local_identity_and_late_error" => local_identity_and_late_error(),
        "codex_aba_generation" => aba_generation(),
        "codex_explicit_refresh_and_target" => explicit_refresh_and_target(),
        "codex_coalesced_serial_worker" => coalesced_serial_worker(),
        "codex_sticky_failure_and_recovery" => sticky_failure_and_recovery(),
        _ => panic!("unknown Codex quota scenario: {case}"),
    }
}

fn auth(subject: &str) -> Value {
    let workspace = format!("quota-workspace-{subject}");
    let claims = json!({
        "sub": subject,
        "email": format!("{subject}@example.test"),
        "https://api.openai.com/auth": {
            "chatgpt_account_id": workspace,
            "chatgpt_plan_type": "plus"
        }
    });
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": format!("e30.{payload}.sig"),
            "access_token": format!("quota-access-secret-{subject}"),
            "refresh_token": format!("quota-refresh-secret-{subject}"),
            "account_id": workspace
        },
        "last_refresh": "2026-01-01T00:00:00Z"
    })
}

struct Fixture {
    paths: AppPaths,
    home: PathBuf,
    a: String,
    b: String,
}

impl Fixture {
    fn new() -> Self {
        assert!(
            std::env::var("MUX_TEST_QUOTA_CASE")
                .unwrap()
                .starts_with("codex")
        );
        let root = PathBuf::from(std::env::var_os("HOME").unwrap());
        let paths = AppPaths::discover().unwrap();
        let home = codex::home().unwrap();
        assert_eq!(home, root.join("codex"));
        assert_eq!(paths.config, root.join("config.toml"));
        let native: toml::Value = std::fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(native["cli_auth_credentials_store"].as_str(), Some("file"));

        let [a, b] = [("quota-a", "Account A"), ("quota-b", "Account B")].map(|(subject, name)| {
            let file = root.join(format!("{subject}.json"));
            std::fs::write(&file, auth(subject).to_string()).unwrap();
            codex::accounts::import(&paths, name, Some(&file)).unwrap()
        });
        let fixture = Self { paths, home, a, b };
        config::update(&fixture.paths.config, |config| {
            for (id, used) in [(&fixture.a, 37.5), (&fixture.b, 81.0)] {
                let account = config.codex.accounts.get_mut(id).unwrap();
                account.limits = json!({"rateLimits": {"primary": {
                    "usedPercent": used,
                    "windowDurationMins": 300,
                    "resetsAt": 4_000_000_000_u64
                }}});
                account.refreshed_at = Some(1);
            }
            Ok(())
        })
        .unwrap();
        fixture.apply(&fixture.a);
        fixture
    }

    fn install(&self, value: &Value) {
        // Atomic replacement avoids a synthetic partial JSON read during a local tick.
        let mut file = tempfile::NamedTempFile::new_in(&self.home).unwrap();
        file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
        file.persist(self.home.join("auth.json")).unwrap();
    }

    fn selection(&self, id: Option<&str>) {
        config::update(&self.paths.config, |config| {
            config.codex.active = id.map(|id| codex::Selection::Account { id: id.into() });
            Ok(())
        })
        .unwrap();
    }

    fn apply(&self, id: &str) {
        self.selection(Some(id));
        let subject = if id == self.a {
            "quota-a"
        } else {
            assert_eq!(id, self.b);
            "quota-b"
        };
        self.install(&auth(subject));
    }
}

#[derive(Debug)]
struct Accepted {
    id: u64,
    generation: u64,
    identity: QuotaIdentity,
    manual: bool,
    thread: std::thread::ThreadId,
}

#[derive(Default)]
struct Counts {
    active: AtomicUsize,
    maximum: AtomicUsize,
    total: AtomicUsize,
}

struct Harness {
    updates: mpsc::Receiver<Accounts>,
    requests: mpsc::SyncSender<AccountRequest>,
    accepted: mpsc::Receiver<Accepted>,
    release: mpsc::Sender<Result<(), String>>,
    counts: Arc<Counts>,
}

impl Harness {
    fn start(fixture: &Fixture) -> Self {
        let (updates_send, updates) = mpsc::sync_channel(32);
        let (requests, requests_recv) = mpsc::sync_channel(32);
        let (accepted_send, accepted) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let counts = Arc::new(Counts::default());
        let worker_counts = Arc::clone(&counts);
        spawn_with_executor(
            fixture.paths.clone(),
            updates_send,
            requests_recv,
            1,
            move |job| {
                assert!(matches!(&job.identity, QuotaIdentity::Codex { .. }));
                let active = worker_counts.active.fetch_add(1, Ordering::SeqCst) + 1;
                worker_counts.maximum.fetch_max(active, Ordering::SeqCst);
                worker_counts.total.fetch_add(1, Ordering::SeqCst);
                assert_eq!(active, 1, "quota executors overlapped");
                accepted_send
                    .send(Accepted {
                        id: job.id,
                        generation: job.generation,
                        identity: job.identity.clone(),
                        manual: job.manual,
                        thread: std::thread::current().id(),
                    })
                    .unwrap();
                let outcome = releases.recv_timeout(DEADLOCK_LIMIT).unwrap();
                worker_counts.active.fetch_sub(1, Ordering::SeqCst);
                QuotaDone {
                    job,
                    outcome: QuotaOutcome::Codex(outcome),
                }
            },
        );
        Self {
            updates,
            requests,
            accepted,
            release,
            counts,
        }
    }

    fn receive(&self) -> Accounts {
        let update = self.updates.recv_timeout(DEADLOCK_LIMIT).unwrap();
        assert_no_credentials(&update);
        update
    }

    fn until(&self, predicate: impl Fn(&Accounts) -> bool) -> Accounts {
        let deadline = Instant::now() + DEADLOCK_LIMIT;
        loop {
            let update = self
                .updates
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            assert_no_credentials(&update);
            if predicate(&update) {
                return update;
            }
        }
    }

    fn account(&self, id: &str) -> Accounts {
        self.until(|update| displayed_id(update) == Some(id))
    }

    fn drain(&self) {
        while let Ok(update) = self.updates.try_recv() {
            assert_no_credentials(&update);
        }
    }

    fn send(&self, request: AccountRequest) {
        assert!(
            self.requests.send(request).is_ok(),
            "coordinator disconnected"
        );
    }

    fn started(&self) -> Accepted {
        self.accepted.recv_timeout(DEADLOCK_LIMIT).unwrap()
    }

    fn no_job(&self) {
        assert!(matches!(
            self.accepted.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    fn finish(&self, result: Result<(), String>) {
        self.release.send(result).unwrap();
    }
}

fn displayed_id(update: &Accounts) -> Option<&str> {
    update
        .codex
        .card
        .as_ref()
        .and_then(|card| card.id.as_deref())
}

fn quota_warning(update: &Accounts) -> bool {
    update.codex.card.as_ref().is_some_and(|card| {
        card.rows
            .iter()
            .any(|(label, value)| label == "Quota" && value.starts_with('!'))
    })
}

fn assert_used(update: &Accounts, expected: f64) {
    let card = update.codex.card.as_ref().unwrap();
    assert!(card.gauges.iter().any(|(_, used, _)| *used == expected));
}

fn assert_no_credentials(update: &Accounts) {
    let public = format!("{update:?}");
    for secret in [
        "quota-access-secret",
        "quota-refresh-secret",
        "rotated-secret",
        "e30.",
    ] {
        assert!(
            !public.contains(secret),
            "credential entered account UI state"
        );
    }
}

fn assert_target(job: &Accepted, account: &str, live: &str, selection: Option<&str>) {
    let QuotaIdentity::Codex {
        account: target,
        live: observed_live,
        selection: observed_selection,
    } = &job.identity
    else {
        panic!("non-Codex job dispatched");
    };
    assert_eq!(target, account);
    assert_eq!(observed_live.as_deref(), Some(live));
    assert_eq!(
        observed_selection,
        &selection.map(|id| codex::Selection::Account { id: id.into() })
    );
    assert!(job.manual);
}

fn local_identity_and_late_error() {
    let fixture = Fixture::new();
    let harness = Harness::start(&fixture);
    let initial = harness.account(&fixture.a);
    assert_used(&initial, 37.5);
    harness.no_job();
    harness.drain();

    harness.send(AccountRequest::Refresh(1));
    let job = harness.started();
    assert_target(&job, &fixture.a, &fixture.a, Some(&fixture.a));
    // Publication is already available while the quota executor waits at its barrier.
    let before_quota = harness.updates.try_recv().unwrap();
    assert_no_credentials(&before_quota);
    assert_eq!(displayed_id(&before_quota), Some(fixture.a.as_str()));
    assert_eq!(harness.counts.active.load(Ordering::SeqCst), 1);

    fixture.apply(&fixture.b);
    // No Select/Refresh is sent: the independent local deadline must discover B.
    let changed = harness.account(&fixture.b);
    assert_used(&changed, 81.0);
    assert!(!quota_warning(&changed));
    assert_eq!(harness.counts.active.load(Ordering::SeqCst), 1);
    harness.drain();
    harness.finish(Err("quota-access-secret-late-A failure".into()));
    for _ in 0..3 {
        let update = harness.receive();
        assert_eq!(displayed_id(&update), Some(fixture.b.as_str()));
        assert!(!quota_warning(&update));
        assert_used(&update, 81.0);
    }
    harness.no_job();
    assert_eq!(harness.counts.total.load(Ordering::SeqCst), 1);
}

fn aba_generation() {
    let fixture = Fixture::new();
    let harness = Harness::start(&fixture);
    harness.account(&fixture.a);
    harness.send(AccountRequest::Refresh(1));
    let old = harness.started();
    harness.account(&fixture.a);

    fixture.apply(&fixture.b);
    harness.send(AccountRequest::Select(1));
    assert!(!quota_warning(&harness.account(&fixture.b)));
    fixture.apply(&fixture.a);
    harness.send(AccountRequest::Select(1));
    assert!(!quota_warning(&harness.account(&fixture.a)));
    harness.send(AccountRequest::Refresh(1));
    assert!(!quota_warning(&harness.receive()));
    harness.no_job();
    harness.drain();

    harness.finish(Err("quota-refresh-secret-old-generation".into()));
    let current = harness.started();
    assert_eq!(old.identity, current.identity);
    assert_ne!(old.generation, current.generation);
    assert!(current.id > old.id);
    assert_eq!(old.thread, current.thread);
    // The current job stays blocked, so any failure here came from the old A job.
    for _ in 0..3 {
        let update = harness.receive();
        assert_eq!(displayed_id(&update), Some(fixture.a.as_str()));
        assert!(!quota_warning(&update));
        assert_used(&update, 37.5);
    }
    harness.finish(Ok(()));
    assert!(!quota_warning(&harness.account(&fixture.a)));
    assert_eq!(harness.counts.maximum.load(Ordering::SeqCst), 1);
}

fn explicit_refresh_and_target() {
    let fixture = Fixture::new();
    fixture.install(&auth("quota-b"));
    let harness = Harness::start(&fixture);
    let initial = harness.account(&fixture.b);
    assert_used(&initial, 81.0);
    harness.no_job();

    for client in [0, 1, 3, 1] {
        harness.drain();
        harness.send(AccountRequest::Select(client));
        assert_eq!(displayed_id(&harness.receive()), Some(fixture.b.as_str()));
        harness.no_job();
    }
    harness.drain();
    for _ in 0..2 {
        harness.receive(); // Local ticks cannot initiate Codex RPC.
        harness.no_job();
    }
    for client in [0, 3] {
        harness.drain();
        harness.send(AccountRequest::Refresh(client));
        harness.receive();
        harness.no_job();
    }

    harness.send(AccountRequest::Refresh(1));
    let active = harness.started();
    assert_target(&active, &fixture.a, &fixture.b, Some(&fixture.a));
    assert_eq!(
        displayed_id(&harness.account(&fixture.b)),
        Some(fixture.b.as_str())
    );
    harness.drain();
    harness.finish(Err("quota-access-secret-configured-A".into()));
    for _ in 0..2 {
        let update = harness.receive();
        assert_eq!(displayed_id(&update), Some(fixture.b.as_str()));
        assert!(
            !quota_warning(&update),
            "A refresh error decorated the live B card"
        );
    }

    fixture.selection(None);
    harness.drain();
    harness.send(AccountRequest::Select(1));
    harness.until(|update| {
        displayed_id(update) == Some(fixture.b.as_str())
            && update
                .codex
                .card
                .as_ref()
                .unwrap()
                .rows
                .iter()
                .all(|(label, _)| label != "Switch")
    });
    harness.no_job();
    harness.send(AccountRequest::Refresh(1));
    let saved_live = harness.started();
    assert_target(&saved_live, &fixture.b, &fixture.b, None);
    assert_eq!(active.thread, saved_live.thread);
    harness.finish(Ok(()));
    assert!(!quota_warning(&harness.account(&fixture.b)));
    assert_eq!(harness.counts.total.load(Ordering::SeqCst), 2);
}

fn coalesced_serial_worker() {
    let fixture = Fixture::new();
    let harness = Harness::start(&fixture);
    harness.account(&fixture.a);
    harness.send(AccountRequest::Refresh(1));
    let first = harness.started();
    harness.account(&fixture.a);

    // Exercise the pending bound deterministically with the same job-channel contract.
    let now = Instant::now();
    let mut state = QuotaState::default();
    state.observe(Some(first.identity.clone()), None);
    let (jobs, work) = mpsc::sync_channel(1);
    state.schedule(1, false, now);
    assert!(state.dispatch(&jobs, now));
    assert!(matches!(work.try_recv(), Err(mpsc::TryRecvError::Empty)));
    state.schedule(1, true, now);
    assert!(state.dispatch(&jobs, now));
    let running = work.recv_timeout(DEADLOCK_LIMIT).unwrap();
    for _ in 0..32 {
        state.schedule(1, true, now);
        assert!(state.dispatch(&jobs, now));
        assert!(matches!(work.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
    state.accept(QuotaDone {
        job: running,
        outcome: QuotaOutcome::Codex(Ok(())),
    });
    assert!(state.dispatch(&jobs, now));
    let pending = work.recv_timeout(DEADLOCK_LIMIT).unwrap();
    assert!(pending.manual);
    state.accept(QuotaDone {
        job: pending,
        outcome: QuotaOutcome::Codex(Ok(())),
    });
    assert!(state.dispatch(&jobs, now));
    assert!(matches!(work.try_recv(), Err(mpsc::TryRecvError::Empty)));

    // Real coordinator requests are published and coalesced while the executor is held.
    for _ in 0..12 {
        harness.drain();
        harness.send(AccountRequest::Refresh(1));
        assert_eq!(displayed_id(&harness.receive()), Some(fixture.a.as_str()));
        harness.no_job();
    }
    harness.drain();
    for _ in 0..2 {
        harness.receive();
        harness.no_job();
    }
    assert_eq!(harness.counts.total.load(Ordering::SeqCst), 1);
    harness.finish(Ok(()));
    let second = harness.started();
    assert!(second.id > first.id);
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.identity, first.identity);
    assert_eq!(second.thread, first.thread);
    assert!(second.manual);
    harness.account(&fixture.a);
    harness.drain();
    harness.finish(Ok(()));
    for _ in 0..3 {
        assert!(!quota_warning(&harness.receive()));
    }
    harness.no_job();
    assert_eq!(harness.counts.total.load(Ordering::SeqCst), 2);
    assert_eq!(harness.counts.maximum.load(Ordering::SeqCst), 1);
    assert_eq!(harness.counts.active.load(Ordering::SeqCst), 0);
}

fn sticky_failure_and_recovery() {
    let fixture = Fixture::new();
    let harness = Harness::start(&fixture);
    harness.account(&fixture.a);
    harness.send(AccountRequest::Refresh(1));
    let failed = harness.started();
    harness.account(&fixture.a);
    harness.finish(Err("quota-refresh-secret-sensitive-service-error".into()));
    let failure = harness.until(quota_warning);
    assert_eq!(displayed_id(&failure), Some(fixture.a.as_str()));
    assert_used(&failure, 37.5);
    harness.drain();
    for _ in 0..3 {
        let update = harness.receive();
        assert_eq!(displayed_id(&update), Some(fixture.a.as_str()));
        assert!(quota_warning(&update));
        assert_used(&update, 37.5);
        harness.no_job();
    }

    let mut rotated = auth("quota-a");
    rotated["tokens"]["access_token"] = json!("rotated-secret-access");
    rotated["tokens"]["refresh_token"] = json!("rotated-secret-refresh");
    harness.drain();
    fixture.install(&rotated);
    let rotated_update = harness.receive();
    assert_eq!(displayed_id(&rotated_update), Some(fixture.a.as_str()));
    assert!(quota_warning(&rotated_update));
    assert_used(&rotated_update, 37.5);
    harness.no_job();

    harness.send(AccountRequest::Refresh(1));
    let recovered = harness.started();
    assert_eq!(recovered.identity, failed.identity);
    assert_eq!(recovered.generation, failed.generation);
    assert_eq!(recovered.thread, failed.thread);
    assert!(quota_warning(&harness.account(&fixture.a)));
    // A concurrent local configuration edit must survive quota completion/reload.
    config::update(&fixture.paths.config, |config| {
        config.codex.accounts.get_mut(&fixture.a).unwrap().name = "Renamed A".into();
        Ok(())
    })
    .unwrap();
    harness.until(|update| {
        update
            .codex
            .card
            .as_ref()
            .is_some_and(|card| card.name == "Renamed A")
    });
    harness.drain();
    harness.finish(Ok(()));
    let success = harness.until(|update| !quota_warning(update));
    assert_eq!(displayed_id(&success), Some(fixture.a.as_str()));
    assert_eq!(success.codex.card.as_ref().unwrap().name, "Renamed A");
    assert_used(&success, 37.5);
    for _ in 0..2 {
        assert!(!quota_warning(&harness.receive()));
    }
    assert_eq!(
        config::load(&fixture.paths.config).unwrap().codex.accounts[&fixture.a].name,
        "Renamed A"
    );
    harness.no_job();
    assert_eq!(harness.counts.total.load(Ordering::SeqCst), 2);
}
