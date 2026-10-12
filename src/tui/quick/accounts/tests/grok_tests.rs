use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const DEADLOCK_LIMIT: Duration = Duration::from_secs(30);

#[test]
fn local_grok_login_changes_while_old_quota_success_is_held() {
    run_quota_case("grok-late-success");
}

#[test]
fn local_grok_login_changes_while_old_quota_error_is_held() {
    run_quota_case("grok-late-error");
}

#[test]
fn returning_to_grok_account_rejects_its_previous_generation_success() {
    run_quota_case("grok-generation-success");
}

#[test]
fn returning_to_grok_account_rejects_its_previous_generation_error() {
    run_quota_case("grok-generation-error");
}

#[test]
fn grok_cached_quota_and_error_survive_local_ticks_and_token_rotation() {
    run_quota_case("grok-sticky-cache");
}

#[test]
fn grok_quota_uses_billing_identity_not_saved_card_identity() {
    run_quota_case("grok-billing-identity");
}

#[test]
fn grok_issuer_and_principal_changes_invalidate_old_quota() {
    run_quota_case("grok-identity-components");
}

#[test]
fn grok_rejects_quota_with_another_snapshot_account() {
    run_quota_case("grok-snapshot-mismatch");
}

#[test]
fn repeated_refreshes_coalesce_and_both_providers_share_one_worker() {
    run_quota_case("grok-coalescing");
}

#[test]
fn switching_away_discards_unstarted_grok_automatic_refresh() {
    run_quota_case("grok-switch-away");
}

#[test]
fn grok_cadence_counts_actual_attempts_and_manual_refresh_bypasses_it() {
    run_quota_case("grok-cadence");
}

#[test]
fn quota_pending_dispatch_prioritizes_manual_then_request_order() {
    run_quota_case("grok-manual-priority");
}

#[test]
fn quota_worker_disconnect_remains_visible_after_identity_changes() {
    run_quota_case("grok-worker-disconnect");
}

pub(super) fn scenario(case: &str) {
    match case {
        "grok-late-success" => stale_completion(false, false),
        "grok-late-error" => stale_completion(false, true),
        "grok-generation-success" => stale_completion(true, false),
        "grok-generation-error" => stale_completion(true, true),
        "grok-sticky-cache" => sticky_cache(),
        "grok-billing-identity" => billing_identity(),
        "grok-identity-components" => identity_components(),
        "grok-snapshot-mismatch" => snapshot_mismatch(),
        "grok-coalescing" => coalescing(),
        "grok-switch-away" => switch_away(),
        "grok-cadence" => cadence(),
        "grok-manual-priority" => manual_priority(),
        "grok-worker-disconnect" => worker_disconnect(),
        _ => panic!("Unknown Grok quota scenario: {case}"),
    }
}

struct Fixture {
    paths: AppPaths,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(std::env::var_os("HOME").expect("Isolated HOME"));
        let paths = AppPaths::discover().unwrap();
        let home = grok::home().unwrap();
        assert!(home.starts_with(&root));
        assert!(paths.config.starts_with(&root));
        assert!(paths.state_dir.starts_with(&root));
        fs::create_dir_all(&home).unwrap();
        config::update(&paths.config, |_| Ok(())).unwrap();
        Self { paths, home }
    }

    fn login(&self, user: &str, email: &str, token: &str) -> String {
        self.login_components(user, email, token, "https://auth.x.ai", "default")
    }

    fn login_components(
        &self,
        user: &str,
        email: &str,
        token: &str,
        issuer: &str,
        principal: &str,
    ) -> String {
        let auth = json!({
            "auth_mode": "oidc",
            "user_id": user,
            "email": email,
            "key": token,
            "oidc_issuer": issuer,
            "principal_id": principal,
        });
        codex::atomic_write(
            &self.home.join("auth.json"),
            &serde_json::to_vec(&auth).unwrap(),
        )
        .unwrap();
        grok::usage::account(&self.home).unwrap().unwrap()
    }

    fn saved_codex(&self) -> String {
        let home = codex::home().unwrap();
        let root = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert!(home.starts_with(root));
        fs::create_dir_all(&home).unwrap();
        let document = fs::read_to_string(home.join("config.toml")).unwrap();
        let document: toml::Value = toml::from_str(&document).unwrap();
        assert_eq!(
            document["cli_auth_credentials_store"].as_str(),
            Some("file")
        );
        let claims = json!({
            "sub": "quota-codex",
            "email": "quota-codex@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "quota-workspace",
                "chatgpt_plan_type": "plus",
            },
        });
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let auth = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": format!("e30.{payload}.sig"),
                "access_token": "quota-access-fixture",
                "refresh_token": "quota-refresh-fixture",
                "account_id": "quota-workspace",
            },
            "last_refresh": "2026-01-01T00:00:00Z",
        });
        let path = home.join("auth.json");
        codex::atomic_write(&path, &serde_json::to_vec(&auth).unwrap()).unwrap();
        let id = codex::accounts::import(&self.paths, "Quota Codex", Some(&path)).unwrap();
        config::update(&self.paths.config, |config| {
            config.codex.active = Some(codex::Selection::Account { id: id.clone() });
            Ok(())
        })
        .unwrap();
        id
    }

    fn identity(&self) -> QuotaIdentity {
        QuotaIdentity::Grok {
            home: self.home.clone(),
            account: grok::usage::account(&self.home).unwrap().unwrap(),
        }
    }
}

#[derive(Clone, Debug)]
struct Accepted {
    id: u64,
    generation: u64,
    identity: QuotaIdentity,
    manual: bool,
}

struct HeldWorker {
    requests: mpsc::SyncSender<AccountRequest>,
    updates: mpsc::Receiver<Accounts>,
    accepted: mpsc::Receiver<Accepted>,
    release: mpsc::Sender<QuotaOutcome>,
    calls: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

impl HeldWorker {
    fn new(paths: AppPaths, initial: usize) -> Self {
        let (requests, request_rx) = mpsc::sync_channel(0);
        let (updates_tx, updates) = mpsc::sync_channel(64);
        let (accepted_tx, accepted) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let worker_maximum = maximum.clone();
        spawn_with_executor(paths, updates_tx, request_rx, initial, move |job| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            worker_maximum.fetch_max(count, Ordering::SeqCst);
            worker_calls.fetch_add(1, Ordering::SeqCst);
            accepted_tx
                .send(Accepted {
                    id: job.id,
                    generation: job.generation,
                    identity: job.identity.clone(),
                    manual: job.manual,
                })
                .unwrap();
            let outcome = release_rx.recv_timeout(DEADLOCK_LIMIT).unwrap();
            active.fetch_sub(1, Ordering::SeqCst);
            QuotaDone { job, outcome }
        });
        Self {
            requests,
            updates,
            accepted,
            release,
            calls,
            maximum,
        }
    }

    fn accepted(&self) -> Accepted {
        self.accepted.recv_timeout(DEADLOCK_LIMIT).unwrap()
    }

    fn update(&self) -> Accounts {
        self.updates.recv_timeout(DEADLOCK_LIMIT).unwrap()
    }

    fn until(&self, mut matches: impl FnMut(&Accounts) -> bool) -> Accounts {
        let deadline = Instant::now() + DEADLOCK_LIMIT;
        loop {
            let update = self
                .updates
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if matches(&update) {
                return update;
            }
        }
    }

    fn request(&self, request: AccountRequest) {
        self.requests.send(request).unwrap();
    }

    fn finish(&self, outcome: QuotaOutcome) {
        self.release.send(outcome).unwrap();
    }

    fn drain(&self) {
        while self.updates.try_recv().is_ok() {}
    }

    fn assert_idle(&self, expected_calls: usize) {
        // Two actual coordinator publications, without a request, cross local ticks.
        self.drain();
        self.update();
        self.update();
        assert_eq!(self.calls.load(Ordering::SeqCst), expected_calls);
        assert_eq!(self.maximum.load(Ordering::SeqCst), 1);
        assert!(self.accepted.try_recv().is_err());
    }
}

fn card(accounts: &Accounts) -> &Card {
    accounts.grok.card.as_ref().expect("Local Grok card")
}

fn percent(accounts: &Accounts) -> Option<f64> {
    card(accounts).gauges.first().map(|(_, value, _)| *value)
}

fn failed(accounts: &Accounts) -> bool {
    card(accounts)
        .rows
        .iter()
        .any(|(label, value)| label == "Quota" && value.starts_with('!'))
}

fn snapshot(account: &str, percent: f64) -> grok::usage::Snapshot {
    grok::usage::Snapshot {
        account: account.into(),
        credits: grok::usage::Credits {
            percent: Some(percent),
            ..Default::default()
        },
        fetched_at: chrono::Utc::now().timestamp(),
    }
}

fn success(account: &str, percent: f64) -> QuotaOutcome {
    QuotaOutcome::Grok(Ok(snapshot(account, percent)))
}

fn assert_grok(job: &Accepted, home: &Path, account: &str) {
    assert_eq!(
        job.identity,
        QuotaIdentity::Grok {
            home: home.to_path_buf(),
            account: account.into()
        }
    );
}

fn stale_completion(return_to_a: bool, error: bool) {
    let fixture = Fixture::new();
    let account_a = fixture.login("a", "a@example.test", "fixture-a");
    let worker = HeldWorker::new(fixture.paths.clone(), 2);
    let initial = worker.update();
    assert_eq!(card(&initial).email, "a@example.test");
    assert_eq!(percent(&initial), None);
    let old = worker.accepted();
    assert_grok(&old, &fixture.home, &account_a);
    assert!(!old.manual);

    let account_b = fixture.login("b", "b@example.test", "fixture-b");
    let local_b = worker.until(|accounts| card(accounts).email == "b@example.test");
    assert_eq!(percent(&local_b), None);
    assert!(!failed(&local_b));
    assert_ne!(account_a, account_b);
    let (current_account, current_email) = if return_to_a {
        assert_eq!(
            fixture.login("a", "a-return@example.test", "fixture-a-return"),
            account_a
        );
        let local_a = worker.until(|accounts| card(accounts).email == "a-return@example.test");
        assert_eq!(percent(&local_a), None);
        (account_a.as_str(), "a-return@example.test")
    } else {
        (account_b.as_str(), "b@example.test")
    };
    worker.drain();
    worker.finish(if error {
        QuotaOutcome::Grok(Err("old account controlled failure".into()))
    } else {
        success(&account_a, 91.0)
    });
    let fresh = worker.accepted();
    assert_grok(&fresh, &fixture.home, current_account);
    assert!(fresh.id > old.id);
    assert!(fresh.generation > old.generation);
    worker.request(AccountRequest::Select(2));
    let after_old = worker.until(|accounts| {
        assert_ne!(percent(accounts), Some(91.0));
        assert!(!failed(accounts));
        card(accounts).email == current_email
    });
    assert_eq!(percent(&after_old), None);
    worker.finish(success(current_account, 23.0));
    let fresh_quota = worker.until(|accounts| percent(accounts) == Some(23.0));
    assert_eq!(card(&fresh_quota).email, current_email);
    assert!(!failed(&fresh_quota));
    assert_eq!(worker.maximum.load(Ordering::SeqCst), 1);
}

fn sticky_cache() {
    let fixture = Fixture::new();
    let account = fixture.login("a", "a@example.test", "fixture-original");
    let worker = HeldWorker::new(fixture.paths.clone(), 2);
    assert_eq!(card(&worker.update()).email, "a@example.test");
    assert_grok(&worker.accepted(), &fixture.home, &account);
    worker.finish(success(&account, 37.0));
    worker.until(|accounts| percent(accounts) == Some(37.0));
    worker.request(AccountRequest::Refresh(2));
    let refresh = worker.accepted();
    assert!(refresh.manual);
    worker.finish(QuotaOutcome::Grok(Err(
        "controlled current account failure".into(),
    )));
    worker.until(|accounts| percent(accounts) == Some(37.0) && failed(accounts));
    worker.drain();
    for _ in 0..2 {
        let tick = worker.update();
        assert_eq!(percent(&tick), Some(37.0));
        assert!(failed(&tick));
    }
    assert_eq!(worker.calls.load(Ordering::SeqCst), 2);

    assert_eq!(
        fixture.login("a", "rotated@example.test", "fixture-rotated"),
        account
    );
    worker.request(AccountRequest::Select(2));
    let rotated = worker.until(|accounts| card(accounts).email == "rotated@example.test");
    assert_eq!(percent(&rotated), Some(37.0));
    assert!(failed(&rotated));
    assert_eq!(worker.calls.load(Ordering::SeqCst), 2);
    worker.request(AccountRequest::Refresh(2));
    let manual = worker.accepted();
    assert!(manual.manual);
    assert_grok(&manual, &fixture.home, &account);
    worker.finish(success(&account, 55.0));
    let recovered = worker.until(|accounts| percent(accounts) == Some(55.0));
    assert!(!failed(&recovered));
}

fn billing_identity() {
    let fixture = Fixture::new();
    let claims =
        URL_SAFE_NO_PAD.encode(json!({"iss": "https://claims-only.example.test"}).to_string());
    let auth = json!({
        "auth_mode": "oidc",
        "user_id": "billing-user",
        "email": "billing@example.test",
        "key": format!("e30.{claims}.sig"),
        "principal_id": "billing-project",
    });
    codex::atomic_write(
        &fixture.home.join("auth.json"),
        &serde_json::to_vec(&auth).unwrap(),
    )
    .unwrap();
    let account = grok::usage::account(&fixture.home).unwrap().unwrap();
    let saved = grok::accounts::capture(&fixture.paths, &fixture.home)
        .unwrap()
        .unwrap();
    assert_ne!(saved, account);
    let worker = HeldWorker::new(fixture.paths.clone(), 2);
    let local = worker.update();
    assert_eq!(card(&local).id.as_deref(), Some(saved.as_str()));
    let job = worker.accepted();
    assert_grok(&job, &fixture.home, &account);
    worker.finish(success(&account, 42.0));
    let loaded = worker.until(|accounts| percent(accounts) == Some(42.0));
    assert_eq!(card(&loaded).id.as_deref(), Some(saved.as_str()));
    assert!(!failed(&loaded));
}

fn identity_components() {
    let fixture = Fixture::new();
    let first = fixture.login_components(
        "same-user",
        "first@example.test",
        "fixture",
        "https://auth.x.ai",
        "one",
    );
    let worker = HeldWorker::new(fixture.paths.clone(), 2);
    worker.update();
    assert_grok(&worker.accepted(), &fixture.home, &first);
    worker.finish(success(&first, 31.0));
    worker.until(|accounts| percent(accounts) == Some(31.0));

    let issuer = fixture.login_components(
        "same-user",
        "issuer@example.test",
        "fixture",
        "https://accounts.x.ai",
        "one",
    );
    assert_ne!(issuer, first);
    worker.request(AccountRequest::Select(2));
    let local = worker.until(|accounts| card(accounts).email == "issuer@example.test");
    assert_eq!(percent(&local), None);
    let old = worker.accepted();
    assert_grok(&old, &fixture.home, &issuer);

    let principal = fixture.login_components(
        "same-user",
        "principal@example.test",
        "fixture",
        "https://accounts.x.ai",
        "two",
    );
    assert_ne!(principal, issuer);
    worker.request(AccountRequest::Select(2));
    let local = worker.until(|accounts| card(accounts).email == "principal@example.test");
    assert_eq!(percent(&local), None);
    worker.drain();
    worker.finish(success(&issuer, 77.0));
    let current = worker.accepted();
    assert_grok(&current, &fixture.home, &principal);
    assert!(current.generation > old.generation);
    worker.request(AccountRequest::Select(2));
    worker.until(|accounts| {
        assert_eq!(percent(accounts), None);
        !failed(accounts) && card(accounts).email == "principal@example.test"
    });
    worker.finish(success(&principal, 19.0));
    worker.until(|accounts| percent(accounts) == Some(19.0));
}

fn snapshot_mismatch() {
    let fixture = Fixture::new();
    let account = fixture.login("a", "a@example.test", "fixture-a");
    let worker = HeldWorker::new(fixture.paths.clone(), 2);
    worker.update();
    worker.accepted();
    worker.finish(success(&account, 21.0));
    worker.until(|accounts| percent(accounts) == Some(21.0));
    worker.request(AccountRequest::Refresh(2));
    assert!(worker.accepted().manual);
    worker.drain();
    worker.finish(success("another-billing-account", 98.0));
    // Observe the completion and subsequent real local ticks, not just a
    // pre-completion publication that happens to carry the previous cache.
    for _ in 0..3 {
        let update = worker.update();
        assert_eq!(percent(&update), Some(21.0));
        assert_eq!(card(&update).email, "a@example.test");
    }
    assert_eq!(worker.calls.load(Ordering::SeqCst), 2);
    assert!(worker.accepted.try_recv().is_err());
}

fn coalescing() {
    let fixture = Fixture::new();
    let account = fixture.login("a", "initial@example.test", "fixture");
    let codex_id = fixture.saved_codex();
    let worker = HeldWorker::new(fixture.paths.clone(), 2);
    worker.update();
    let running = worker.accepted();
    assert_grok(&running, &fixture.home, &account);
    for round in 0..8 {
        let email = format!("refresh-{round}@example.test");
        assert_eq!(fixture.login("a", &email, "fixture"), account);
        worker.request(AccountRequest::Refresh(2));
        worker.until(|accounts| card(accounts).email == email);
        assert_eq!(worker.calls.load(Ordering::SeqCst), 1);
    }
    fixture.login("a", "codex-queued@example.test", "fixture");
    worker.request(AccountRequest::Refresh(1));
    worker.until(|accounts| card(accounts).email == "codex-queued@example.test");
    fixture.login("a", "all-queued@example.test", "fixture");
    worker.request(AccountRequest::Select(0));
    worker.until(|accounts| card(accounts).email == "all-queued@example.test");
    // The rendezvous send proves the previous request's scheduling finished.
    worker.request(AccountRequest::Select(0));
    assert!(worker.accepted.try_recv().is_err());

    worker.finish(success(&account, 10.0));
    let grok_manual = worker.accepted();
    assert_grok(&grok_manual, &fixture.home, &account);
    assert!(grok_manual.manual);
    assert!(grok_manual.id > running.id);
    worker.finish(success(&account, 20.0));
    let codex_manual = worker.accepted();
    assert!(codex_manual.manual);
    assert!(codex_manual.id > grok_manual.id);
    assert!(matches!(
        &codex_manual.identity,
        QuotaIdentity::Codex { account, .. } if account == &codex_id
    ));
    worker.drain();
    worker.finish(QuotaOutcome::Codex(Ok(())));
    worker.update();
    worker.assert_idle(3);
}

fn switch_away() {
    let fixture = Fixture::new();
    let account = fixture.login("a", "initial@example.test", "fixture");
    fixture.saved_codex();
    let worker = HeldWorker::new(fixture.paths.clone(), 1);
    worker.update();
    assert_eq!(worker.calls.load(Ordering::SeqCst), 0);
    worker.request(AccountRequest::Refresh(1));
    assert!(matches!(
        worker.accepted().identity,
        QuotaIdentity::Codex { .. }
    ));
    fixture.login("a", "grok-selected@example.test", "fixture");
    worker.request(AccountRequest::Select(2));
    worker.until(|accounts| card(accounts).email == "grok-selected@example.test");
    fixture.login("a", "away@example.test", "fixture");
    worker.request(AccountRequest::Select(0));
    worker.until(|accounts| card(accounts).email == "away@example.test");
    worker.request(AccountRequest::Select(0));
    worker.drain();
    worker.finish(QuotaOutcome::Codex(Ok(())));
    worker.update();
    worker.assert_idle(1);
    worker.request(AccountRequest::Select(2));
    let automatic = worker.accepted();
    assert_grok(&automatic, &fixture.home, &account);
    assert!(!automatic.manual);
    worker.finish(success(&account, 33.0));
    worker.until(|accounts| percent(accounts) == Some(33.0));
}

fn dispatch(state: &mut QuotaState, now: Instant) -> QuotaJob {
    let (send, receive) = mpsc::sync_channel(1);
    assert!(state.dispatch(&send, now));
    receive.try_recv().expect("One dispatched quota job")
}

fn assert_no_dispatch(state: &mut QuotaState, now: Instant) {
    let (send, receive) = mpsc::sync_channel(1);
    assert!(state.dispatch(&send, now));
    assert!(receive.try_recv().is_err());
}

fn cadence() {
    let fixture = Fixture::new();
    let account = fixture.login("a", "a@example.test", "fixture");
    let mut state = QuotaState::default();
    state.observe(None, Some(fixture.identity()));
    let now = Instant::now();
    state.schedule(0, false, now);
    assert_no_dispatch(&mut state, now);
    state.schedule(2, false, now);
    let pending_id = state.pending[1].as_ref().unwrap().id;
    assert_eq!(state.grok_checked, None);
    state.schedule(2, false, now + Duration::from_secs(20));
    assert_eq!(state.pending[1].as_ref().unwrap().id, pending_id);
    assert_eq!(state.grok_checked, None);
    let actual = now + Duration::from_secs(30);
    let automatic = dispatch(&mut state, actual);
    assert!(!automatic.manual);
    assert_eq!(state.grok_checked, Some(actual));
    state.schedule(2, false, actual + Duration::from_secs(1));
    assert!(state.pending[1].is_none());
    state.accept(QuotaDone {
        job: automatic,
        outcome: QuotaOutcome::Grok(Err("controlled billing failure".into())),
    });
    for seconds in [2, 10, 59] {
        state.schedule(2, false, actual + Duration::from_secs(seconds));
        assert_no_dispatch(&mut state, actual + Duration::from_secs(seconds));
    }
    state.schedule(2, false, actual + Duration::from_secs(60));
    assert!(state.pending[1].as_ref().is_some_and(|job| !job.manual));
    assert_eq!(state.grok_checked, Some(actual));
    let second_attempt = actual + Duration::from_secs(67);
    let automatic = dispatch(&mut state, second_attempt);
    assert_eq!(state.grok_checked, Some(second_attempt));
    state.accept(QuotaDone {
        job: automatic,
        outcome: success(&account, 40.0),
    });
    let manual_time = second_attempt + Duration::from_secs(1);
    state.schedule(2, true, manual_time);
    let manual_id = state.pending[1].as_ref().unwrap().id;
    for _ in 0..10 {
        state.schedule(2, true, manual_time);
    }
    assert_eq!(state.pending[1].as_ref().unwrap().id, manual_id);
    let manual = dispatch(&mut state, manual_time);
    assert!(manual.manual);
    assert_eq!(state.grok_checked, Some(manual_time));
    state.accept(QuotaDone {
        job: manual,
        outcome: success(&account, 50.0),
    });
    state.schedule(2, false, manual_time + Duration::from_secs(59));
    assert_no_dispatch(&mut state, manual_time + Duration::from_secs(59));
    state.schedule(2, false, manual_time + Duration::from_secs(60));
    assert!(state.pending[1].is_some());
    state.schedule(0, false, manual_time + Duration::from_secs(60));
    assert!(state.pending[1].is_none());
    assert_eq!(
        state.grok_cache.as_ref().unwrap().credits.percent,
        Some(50.0)
    );
    assert_eq!(state.grok_checked, Some(manual_time));
}

fn manual_priority() {
    let fixture = Fixture::new();
    fixture.login("a", "a@example.test", "fixture");
    let codex_id = fixture.saved_codex();
    let codex_identity = QuotaIdentity::Codex {
        selection: Some(codex::Selection::Account {
            id: codex_id.clone(),
        }),
        live: Some(codex_id.clone()),
        account: codex_id,
    };
    let mut state = QuotaState::default();
    state.observe(Some(codex_identity.clone()), Some(fixture.identity()));
    let now = Instant::now();
    state.queue(0, true);
    let held = dispatch(&mut state, now);
    state.queue(1, false);
    let automatic_id = state.pending[1].as_ref().unwrap().id;
    state.queue(0, true);
    let manual_id = state.pending[0].as_ref().unwrap().id;
    assert!(automatic_id < manual_id);
    for _ in 0..10 {
        state.queue(0, true);
        state.queue(1, false);
    }
    assert_eq!(state.pending[0].as_ref().unwrap().id, manual_id);
    assert_eq!(state.pending[1].as_ref().unwrap().id, automatic_id);
    assert_no_dispatch(&mut state, now);
    state.accept(QuotaDone {
        job: held,
        outcome: QuotaOutcome::Codex(Ok(())),
    });
    let first = dispatch(&mut state, now);
    assert_eq!(first.id, manual_id);
    assert!(first.manual);
    assert_eq!(first.identity, codex_identity);
    state.accept(QuotaDone {
        job: first,
        outcome: QuotaOutcome::Codex(Ok(())),
    });
    let second = dispatch(&mut state, now);
    assert_eq!(second.id, automatic_id);
    assert!(!second.manual);
    state.accept(QuotaDone {
        job: second,
        outcome: success(fixture.identity().account(), 12.0),
    });

    state.queue(1, false);
    let superseded_auto = state.pending[1].as_ref().unwrap().id;
    state.queue(1, true);
    let earlier = state.pending[1].as_ref().unwrap().id;
    assert!(earlier > superseded_auto);
    assert!(state.pending[1].as_ref().unwrap().manual);
    state.schedule(0, false, now);
    assert_eq!(state.pending[1].as_ref().unwrap().id, earlier);
    state.queue(0, true);
    assert!(state.pending[0].as_ref().unwrap().id > earlier);
    let first = dispatch(&mut state, now);
    assert!(first.manual);
    assert_eq!(first.id, earlier);
    assert!(matches!(&first.identity, QuotaIdentity::Grok { .. }));
    state.accept(QuotaDone {
        job: first,
        outcome: success(fixture.identity().account(), 13.0),
    });
    let second = dispatch(&mut state, now);
    assert!(second.manual);
    assert_eq!(second.identity, codex_identity);
    state.accept(QuotaDone {
        job: second,
        outcome: QuotaOutcome::Codex(Ok(())),
    });
    assert_no_dispatch(&mut state, now);
}

fn worker_disconnect() {
    let fixture = Fixture::new();
    fixture.login("a", "a@example.test", "fixture-a");
    let (requests, request_rx) = mpsc::sync_channel(0);
    let (updates_tx, updates) = mpsc::sync_channel(16);
    let (accepted_tx, accepted) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    spawn_with_executor(
        fixture.paths.clone(),
        updates_tx,
        request_rx,
        2,
        move |_| {
            accepted_tx.send(()).unwrap();
            release_rx.recv_timeout(DEADLOCK_LIMIT).unwrap();
            panic!("controlled quota worker disconnect")
        },
    );
    let initial = updates.recv_timeout(DEADLOCK_LIMIT).unwrap();
    assert_eq!(card(&initial).email, "a@example.test");
    accepted.recv_timeout(DEADLOCK_LIMIT).unwrap();
    release.send(()).unwrap();
    let deadline = Instant::now() + DEADLOCK_LIMIT;
    loop {
        let update = updates
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if failed(&update) {
            break;
        }
    }
    fixture.login("b", "b@example.test", "fixture-b");
    requests.send(AccountRequest::Refresh(2)).unwrap();
    loop {
        let update = updates
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if card(&update).email == "b@example.test" {
            assert!(failed(&update));
            assert_eq!(percent(&update), None);
            break;
        }
    }
    requests.send(AccountRequest::Select(2)).unwrap();
    for _ in 0..2 {
        let update = updates.recv_timeout(DEADLOCK_LIMIT).unwrap();
        assert_eq!(card(&update).email, "b@example.test");
        assert!(failed(&update));
    }
    assert!(matches!(
        accepted.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}
