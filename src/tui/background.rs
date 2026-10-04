use super::*;
use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

pub(super) struct SyncRequest {
    explicit: bool,
    preferred: Option<String>,
}
#[derive(Clone)]
enum TaskKind {
    Test,
    ProfileDiscover(uuid::Uuid),
    Discover(String, u64),
    Inspect,
    Sync,
    Proxy,
}

enum Completion {
    ConnectionTest(uuid::Uuid, Result<(u16, u128)>),
    ModelTest {
        instance: Option<uuid::Uuid>,
        name: String,
        result: Result<u128>,
    },
    ProfileDiscover {
        instance: uuid::Uuid,
        profile: Box<Profile>,
        result: Result<Vec<ModelEntry>>,
    },
    Inspect(Result<sync::Status>, Option<proxy::ProxyStatus>),
    Discover {
        request: u64,
        id: String,
        profile: Box<Profile>,
        form: Option<uuid::Uuid>,
        result: Result<Vec<ModelEntry>>,
    },
    Sync(
        Result<claude_config::ApplyResult>,
        Result<sync::Status>,
        Option<proxy::ProxyStatus>,
    ),
    Proxy(Box<ProxyManager>),
    Panicked(TaskKind),
    Finished(uuid::Uuid, Box<Completion>),
}
pub(super) struct Background {
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    requests: BTreeMap<String, u64>,
    sequence: u64,
    active: BTreeMap<uuid::Uuid, TaskKind>,
    model_test_running: bool,
    pub(super) status: sync::Status,
    pub(super) sync_running: bool,
    pub(super) proxy_running: bool,
    pub(super) queued_sync: Option<SyncRequest>,
    due: Instant,
    pub(super) connected: bool,
}
impl Default for Background {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            requests: BTreeMap::new(),
            sequence: 0,
            active: BTreeMap::new(),
            model_test_running: false,
            status: sync::Status::NotConnected,
            sync_running: false,
            proxy_running: false,
            queued_sync: None,
            due: Instant::now(),
            connected: false,
        }
    }
}
impl Background {
    fn spawn(&mut self, kind: TaskKind, work: impl FnOnce() -> Completion + Send + 'static) {
        if matches!(kind, TaskKind::Sync | TaskKind::Proxy) {
            self.active
                .retain(|_, active| !matches!(active, TaskKind::Inspect));
        }
        let id = uuid::Uuid::new_v4();
        self.active.insert(id, kind.clone());
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or(Completion::Panicked(kind));
            let _ = sender.send(Completion::Finished(id, Box::new(result)));
        });
    }
}
impl App {
    fn record_profile_test_message(&mut self, instance: Option<uuid::Uuid>) {
        if let Some(Modal::Profile(form)) = &mut self.modal
            && instance == Some(form.instance)
        {
            form.test_message = Some((self.status.clone(), self.status_error));
        }
    }

    pub(super) fn start_model_test(&mut self) {
        if self.background.model_test_running {
            self.status = "A model test is already running".into();
            return;
        }
        let (Some(profile), Some(model)) =
            (self.selected_profile().cloned(), self.selected_model())
        else {
            self.set_error("Select a provider model to test");
            return;
        };
        let name = format!("{} / {}", profile.name, model.id);
        self.status = format!("Testing {name}…");
        self.status_error = false;
        self.background.model_test_running = true;
        self.background
            .spawn(TaskKind::Test, move || Completion::ModelTest {
                instance: None,
                name,
                result: discovery::test_model(&profile, &model.id),
            });
    }

    pub(super) fn start_profile_connection_test(&mut self) {
        let instance = match &self.modal {
            Some(Modal::Profile(form)) => form.instance,
            _ => return,
        };
        self.start_profile_connection_test_inner();
        self.record_profile_test_message(Some(instance));
    }

    fn start_profile_connection_test_inner(&mut self) {
        if self.background.model_test_running {
            self.status = "A test is already running".into();
            return;
        }
        let Some(Modal::Profile(form)) = &self.modal else {
            return;
        };
        let instance = form.instance;
        let profile = match form.connection_test_profile() {
            Ok(profile) => profile,
            Err(error) => {
                self.set_error(format!("Cannot test Base URL: {error}"));
                return;
            }
        };
        self.status_error = false;
        self.status = "Testing Base URL connectivity…".into();
        self.background.model_test_running = true;
        self.background.spawn(TaskKind::Test, move || {
            Completion::ConnectionTest(instance, discovery::test_connection(&profile))
        });
    }

    pub(super) fn start_profile_model_test(&mut self) {
        let instance = match &self.modal {
            Some(Modal::Profile(form)) => form.instance,
            _ => return,
        };
        self.start_profile_model_test_inner();
        self.record_profile_test_message(Some(instance));
    }

    fn start_profile_model_test_inner(&mut self) {
        if self.background.model_test_running {
            self.status = "A model test is already running".into();
            return;
        }
        let Some(Modal::Profile(form)) = &self.modal else {
            return;
        };
        let instance = form.instance;
        let (profile, models) = match form.model_test_request() {
            Ok(request) => request,
            Err(error) => {
                self.set_error(format!("Cannot test model: {error}"));
                return;
            }
        };
        let name = format!("{} / {}", profile.name, models.join(", "));
        self.status = format!("Testing {name}…");
        self.status_error = false;
        self.background.model_test_running = true;
        self.background.spawn(TaskKind::Test, move || {
            let started = Instant::now();
            let result = models
                .iter()
                .try_for_each(|model| {
                    discovery::test_model(&profile, model)
                        .map(|_| ())
                        .with_context(|| format!("{model} failed"))
                })
                .map(|_| started.elapsed().as_millis());
            Completion::ModelTest {
                instance: Some(instance),
                name,
                result,
            }
        });
    }

    pub(super) fn fetch_profile_models(&mut self, force: bool) {
        let Some(Modal::Profile(form)) = &mut self.modal else {
            return;
        };
        let profile = match form.discovery_profile() {
            Ok(profile) => profile,
            Err(error) => {
                self.set_error(format!("Check URL and credentials: {error:#}"));
                return;
            }
        };
        if !force && let Some(models) = form.cached_models_for(&profile) {
            let mut picker = ModelForm::with_api_models(models.to_vec());
            picker.focus_api_search = true;
            picker.api_status = format!("{} cached models · Ctrl+R refresh", models.len());
            form.picker = Some(picker);
            form.picker_search = false;
            return;
        }
        if form.fetching_profile.as_deref() == Some(&profile) {
            return;
        }
        let existing = form
            .cached_models_for(&profile)
            .unwrap_or_default()
            .to_vec();
        let mut picker = ModelForm::with_api_models(existing);
        picker.focus_api_search = true;
        picker.api_status = "Fetching models…".into();
        form.picker = Some(picker);
        form.picker_search = false;
        form.fetching_profile = Some(Box::new(profile.clone()));
        form.instance = uuid::Uuid::new_v4();
        let instance = form.instance;
        self.background
            .spawn(TaskKind::ProfileDiscover(instance), move || {
                Completion::ProfileDiscover {
                    instance,
                    result: discovery::discover(&profile),
                    profile: Box::new(profile),
                }
            });
    }

    pub(super) fn initialize_background(&mut self) {
        if self.client_tab() != ClientTab::Claude
            || self.background.sync_running
            || self.background.proxy_running
            || self.background.queued_sync.is_some()
            || self
                .background
                .active
                .values()
                .any(|kind| matches!(kind, TaskKind::Inspect))
        {
            return;
        }
        let paths = self.paths.clone();
        self.background.spawn(TaskKind::Inspect, move || {
            let status =
                claude_config::settings_path().and_then(|path| sync::inspect(&paths, &path));
            Completion::Inspect(status, proxy::status(&paths).ok())
        });
    }

    pub(super) fn start_discovery(&mut self, form: Option<uuid::Uuid>) {
        let Some(id) = self.selected_profile_id() else {
            self.set_error("Select a provider before fetching models");
            return;
        };
        if self.background.requests.contains_key(&id) {
            self.status = "A model request is already running for this provider".into();
            return;
        }
        let profile = self.config.profiles[&id].clone();
        if !profile.enabled {
            self.set_error("Enable this provider before fetching models");
            return;
        }
        self.background.sequence += 1;
        let request = self.background.sequence;
        self.background.requests.insert(id.clone(), request);
        self.status_error = false;
        self.status = format!("Fetching models from {}…", profile.name);
        self.background
            .spawn(TaskKind::Discover(id.clone(), request), move || {
                let result = discovery::discover(&profile);
                Completion::Discover {
                    request,
                    id,
                    profile: Box::new(profile),
                    form,
                    result,
                }
            });
    }

    pub(super) fn queue_sync(&mut self, explicit: bool, preferred: Option<String>) {
        if self.client_tab() != ClientTab::Claude && self.settings_menu.is_none() {
            return;
        }
        if !explicit
            && !self.background.connected
            && !claude_config::settings_path()
                .and_then(|settings| sync::inspect(&self.paths, &settings))
                .is_ok_and(|status| matches!(status, sync::Status::Synced | sync::Status::Pending))
        {
            return;
        }
        if let Some(queued) = &mut self.background.queued_sync {
            if explicit {
                queued.explicit = true;
                queued.preferred = preferred;
            }
        } else {
            self.background.queued_sync = Some(SyncRequest {
                explicit,
                preferred,
            });
        }
        self.background.due =
            Instant::now() + Duration::from_millis(if explicit { 0 } else { 200 });
        self.background
            .active
            .retain(|_, kind| !matches!(kind, TaskKind::Inspect));
        self.background.status = sync::Status::Pending;
    }

    pub(super) fn start_proxy_action(&mut self, control: ProxyControl) {
        if self.background.proxy_running || self.background.sync_running {
            self.set_error("A proxy operation is in progress; retry when it finishes");
            return;
        }
        let Some(Modal::Proxy(manager)) = &mut self.modal else {
            return;
        };
        manager.message = "Working…".into();
        manager.error = false;
        let mut manager = manager.clone();
        let paths = self.paths.clone();
        self.background.proxy_running = true;
        self.background.spawn(TaskKind::Proxy, move || {
            manager.activate(&paths, control);
            Completion::Proxy(Box::new(manager))
        });
    }

    pub(super) fn poll_background(&mut self) -> bool {
        let mut changed = false;
        while let Ok(completion) = self.background.receiver.try_recv() {
            let completion = match completion {
                Completion::Finished(id, result) => {
                    if self.background.active.remove(&id).is_none() {
                        continue;
                    }
                    *result
                }
                result => result,
            };
            changed = true;
            match completion {
                Completion::ConnectionTest(instance, result) => {
                    self.background.model_test_running = false;
                    match result {
                        Ok((code, ms)) => {
                            self.status_error = false;
                            let detail = match code {
                                200..=299 => "HTTP reachable; use model Test to check inference",
                                300..=399 => "redirect received (not followed)",
                                401 | 403 => "server reachable; authentication rejected",
                                404 | 405 => "server reachable; base path has no GET endpoint",
                                _ => "server reachable; HTTP error returned",
                            };
                            self.status = format!("Base URL · HTTP {code} · {ms} ms · {detail}");
                        }
                        Err(error) => self.set_error(format!("Base URL unreachable: {error}")),
                    }
                    self.record_profile_test_message(Some(instance));
                }

                Completion::ModelTest {
                    instance,
                    name,
                    result,
                } => {
                    self.background.model_test_running = false;
                    match result {
                        Ok(ms) => {
                            self.status_error = false;
                            self.status = format!(
                                "Basic text test passed · {name} · {ms} ms · tools/streaming not tested"
                            );
                        }
                        Err(error) => {
                            self.set_error(format!("Model test failed · {name}: {error:#}"))
                        }
                    }
                    self.record_profile_test_message(instance);
                }

                Completion::ProfileDiscover {
                    instance,
                    profile,
                    result,
                } => {
                    if let Some(Modal::Profile(form)) = &mut self.modal
                        && form.instance == instance
                        && form.fetching_profile.as_deref() == Some(profile.as_ref())
                    {
                        form.fetching_profile = None;
                        if form.discovery_profile().ok().as_ref() == Some(profile.as_ref()) {
                            match result {
                                Ok(models) => {
                                    form.fetched_profile = Some(profile);
                                    form.fetched_models = models.clone();
                                    if let Some(picker) = &mut form.picker {
                                        picker.api_status = format!(
                                            "{} models · Enter selects · Esc returns",
                                            models.len()
                                        );
                                        picker.api_models = models;
                                    }
                                }
                                Err(error) => {
                                    if let Some(picker) = &mut form.picker {
                                        picker.api_status =
                                            format!("{error:#} · Ctrl+R retry · Esc returns")
                                    }
                                }
                            }
                        }
                    }
                }
                Completion::Inspect(status, proxy) => {
                    // A slow inspection describes the state before an action;
                    // it must not overwrite that action's newer pending state.
                    if self.background.sync_running
                        || self.background.proxy_running
                        || self.background.queued_sync.is_some()
                    {
                        continue;
                    }
                    self.proxy_status = proxy;
                    match status {
                        Ok(status) => {
                            self.background.connected =
                                matches!(status, sync::Status::Pending | sync::Status::Synced);
                            if !self.background.sync_running
                                && self.background.queued_sync.is_none()
                            {
                                self.background.status = status;
                                if status == sync::Status::Pending {
                                    self.queue_sync(false, None);
                                }
                            }
                        }
                        Err(error) => {
                            self.background.status = sync::Status::Failed;
                            self.set_error(format!("Could not inspect sync state: {error:#}"));
                        }
                    }
                }
                Completion::Discover {
                    request,
                    id,
                    profile,
                    form,
                    result,
                } => {
                    if self.background.requests.get(&id) != Some(&request) {
                        continue;
                    }
                    self.background.requests.remove(&id);
                    if self.config.profiles.get(&id) != Some(profile.as_ref())
                        || self
                            .load_client_config()
                            .ok()
                            .and_then(|config| config.profiles.get(&id).cloned())
                            .as_ref()
                            != Some(profile.as_ref())
                    {
                        continue;
                    }
                    // A result belongs to the form that requested it, never its replacement.
                    let target_form = form.is_some_and(|instance| matches!(&self.modal, Some(Modal::Model(current)) if current.instance == instance));
                    match result {
                        Ok(models) => {
                            let cached = CachedModels {
                                fetched_at: now_epoch(),
                                models: models.clone(),
                            };
                            let persisted = discovery::update_cache(
                                &self.client_cache_path(self.config_client()),
                                |cache| {
                                    cache.profiles.insert(id.clone(), cached.clone());
                                },
                            );
                            self.cache.profiles.insert(id.clone(), cached);
                            if target_form && let Some(Modal::Model(current)) = &mut self.modal {
                                let selected = current
                                    .filtered_api_models()
                                    .get(current.api_selected)
                                    .map(|model| model.id.clone());
                                current.api_models = models.clone();
                                current.api_selected = selected
                                    .and_then(|id| {
                                        current
                                            .filtered_api_models()
                                            .iter()
                                            .position(|model| model.id == id)
                                    })
                                    .unwrap_or(0);
                                current.api_scroll = current.api_selected;
                                current.api_status = format!("{} models available", models.len());
                            }
                            if self.selected_profile_id().as_deref() == Some(&id) {
                                self.refresh_editor_preserving_selection();
                                self.status_error = false;
                                self.status = format!(
                                    "Connection verified · {} models discovered",
                                    models.len()
                                );
                            }
                            if let Err(error) = persisted {
                                let message = format!(
                                    "Models available, but cache could not be saved: {error:#}"
                                );
                                if target_form && let Some(Modal::Model(current)) = &mut self.modal
                                {
                                    current.api_status = message.clone();
                                }
                                self.set_error(message);
                            }
                        }
                        Err(error) => {
                            let message = format!("Model request failed: {error:#}");
                            if target_form && let Some(Modal::Model(current)) = &mut self.modal {
                                current.api_status = message.clone();
                            }
                            if self.selected_profile_id().as_deref() == Some(&id) {
                                self.set_error(message);
                            }
                        }
                    }
                }
                Completion::Sync(result, status, proxy) => {
                    self.background.sync_running = false;
                    self.proxy_status = proxy;
                    match result {
                        Ok(result) => {
                            self.background.connected = true;
                            self.background.status = status.unwrap_or(sync::Status::Pending);
                            self.status_error = false;
                            self.status = format!(
                                "Synced {} models and client settings to Claude · restart for startup settings",
                                result.model_count
                            );
                            if self.background.status == sync::Status::Pending {
                                if result.model_count > 0 {
                                    self.queue_sync(false, None);
                                } else {
                                    self.status = "Client settings pending · enable a model and sync to apply".into();
                                }
                            }
                        }
                        Err(error) => {
                            if matches!(
                                status,
                                Ok(sync::Status::Paused | sync::Status::NotConnected)
                            ) {
                                self.background.connected = false;
                            }
                            self.background.status = if matches!(status, Ok(sync::Status::Paused)) {
                                sync::Status::Paused
                            } else {
                                sync::Status::Failed
                            };
                            self.set_error(format!(
                                "Changes saved · sync failed: {error:#} · press p to retry"
                            ));
                        }
                    }
                }
                Completion::Proxy(manager) => {
                    self.background.proxy_running = false;
                    if manager.port_changed {
                        if self.background.connected {
                            self.background.status = sync::Status::Pending;
                        }
                        self.status_error = false;
                        self.status = manager.message.clone();
                    }
                    self.proxy_status = manager.runtime.clone();
                    if let Some(Modal::Proxy(current)) = &mut self.modal
                        && current.instance == manager.instance
                    {
                        let selected = current.selected;
                        *current = *manager;
                        current.selected = selected;
                    }
                }
                Completion::Panicked(kind) => {
                    match kind {
                        TaskKind::Test => self.background.model_test_running = false,
                        TaskKind::Sync => {
                            self.background.sync_running = false;
                            self.background.status = sync::Status::Failed;
                        }
                        TaskKind::Proxy => self.background.proxy_running = false,
                        TaskKind::Discover(id, request) => {
                            if self.background.requests.get(&id) == Some(&request) {
                                self.background.requests.remove(&id);
                            }
                        }
                        TaskKind::ProfileDiscover(instance) => {
                            if let Some(Modal::Profile(form)) = &mut self.modal
                                && form.instance == instance
                            {
                                form.fetching_profile = None;
                                form.test_message =
                                    Some(("Model discovery failed; retry fetching".into(), true));
                            }
                        }
                        TaskKind::Inspect => {}
                    }
                    self.set_error("Background task failed; saved changes are preserved. Retry the failed operation.");
                }
                Completion::Finished(_, _) => unreachable!("completion already unwrapped"),
            }
        }
        if !self.background.sync_running
            && !self.background.proxy_running
            && Instant::now() >= self.background.due
            && let Some(request) = self.background.queued_sync.take()
        {
            changed = true;
            self.background.sync_running = true;
            self.background.status = sync::Status::Syncing;
            let paths = self.paths.clone();
            self.background.spawn(TaskKind::Sync, move || {
                let result = claude_config::settings_path().and_then(|settings| {
                    sync::apply(
                        &paths,
                        &settings,
                        request.preferred.as_deref(),
                        request.explicit,
                    )
                });
                let status = claude_config::settings_path()
                    .and_then(|settings| sync::inspect(&paths, &settings));
                Completion::Sync(result, status, proxy::status(&paths).ok())
            });
        }
        changed
    }

    pub(super) fn refresh_editor_preserving_selection(&mut self) {
        let previous = self.provider_editor.as_ref().map(|editor| {
            (
                editor.query.clone(),
                editor.search_active,
                editor.selected_model().map(|model| model.id.clone()),
            )
        });
        self.init_provider_editor();
        if let (Some((query, search, selected)), Some(editor)) =
            (previous, &mut self.provider_editor)
        {
            editor.query = query;
            editor.search_active = search;
            editor.selected = selected
                .and_then(|id| {
                    editor
                        .filtered_indices()
                        .iter()
                        .position(|index| editor.catalog[*index].id == id)
                })
                .unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::tests::persisted_app;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[test]
    fn provider_form_reuses_one_fetch_until_explicit_refresh() {
        let (_temp, mut app) = persisted_app();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while seen.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut request = [0; 2048];
                        assert!(stream.read(&mut request).unwrap() > 0);
                        seen.fetch_add(1, Ordering::SeqCst);
                        let body = r#"{"data":[{"id":"first"},{"id":"second"}]}"#;
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("mock discovery failed: {error}"),
                }
            }
        });
        let mut form = ProfileForm::new();
        form.fields[3].value = format!("http://{address}");
        form.fields[4].value = "none".into();
        app.modal = Some(Modal::Profile(Box::new(form)));
        app.fetch_profile_models(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(&app.modal, Some(Modal::Profile(form)) if form.fetched_models.is_empty())
            && Instant::now() < deadline
        {
            app.poll_background();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(&app.modal, Some(Modal::Profile(form)) if form.fetched_models.len() == 2));
        if let Some(Modal::Profile(form)) = &mut app.modal {
            form.selected = 6;
            form.fill_selected_model("first");
            form.picker = None;
        }
        app.fetch_profile_models(false);
        assert!(
            matches!(&app.modal, Some(Modal::Profile(form)) if form.picker.as_ref().is_some_and(|picker| picker.api_models.len() == 2) && form.fetching_profile.is_none())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        app.fetch_profile_models(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(&app.modal, Some(Modal::Profile(form)) if form.fetching_profile.is_some())
            && Instant::now() < deadline
        {
            app.poll_background();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        if let Some(Modal::Profile(form)) = &mut app.modal {
            form.fields[13].value = format!("http://{address}/custom/catalog");
            form.picker = None;
        }
        app.fetch_profile_models(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(&app.modal, Some(Modal::Profile(form)) if form.fetching_profile.is_some())
            && Instant::now() < deadline
        {
            app.poll_background();
            thread::sleep(Duration::from_millis(2));
        }
        server.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        if let Some(Modal::Profile(form)) = &mut app.modal {
            form.fields[0].value = "new-provider".into();
            form.fields[1].value = "New provider".into();
            form.picker = None;
        }
        app.handle_modal(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.modal.is_none());
        assert_eq!(app.cache.profiles["new-provider"].models.len(), 2);
        app.edit_profile();
        assert!(matches!(&app.modal, Some(Modal::Profile(form)) if form.fetched_models.len() == 2));
        app.fetch_profile_models(false);
        assert!(
            matches!(&app.modal, Some(Modal::Profile(form)) if form.picker.as_ref().is_some_and(|picker| picker.api_models.len() == 2) && form.fetching_profile.is_none())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn slow_discovery_keeps_navigation_responsive() {
        let (_temp, mut app) = persisted_app();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        app.config = config::update(&app.paths.config, |config| {
            config.profiles.get_mut("one").unwrap().base_url = format!("http://{address}");
            Ok(())
        })
        .unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 2048];
            assert!(stream.read(&mut bytes).unwrap() > 0);
            accepted_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let body = r#"{"data":[{"id":"fresh-model"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        app.enter_provider_view();
        app.start_discovery(None);
        accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.view_mode, ViewMode::Home);
        assert!(app.cache.profiles.is_empty());
        release_tx.send(()).unwrap();
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !app.background.requests.is_empty() && Instant::now() < deadline {
            app.poll_background();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.cache.profiles["one"].models[0].id, "fresh-model");
    }

    #[test]
    fn completion_does_not_fill_a_replacement_form_or_apply_stale_provider_results() {
        let (_temp, mut app) = persisted_app();
        app.enter_provider_view();
        app.open_add_model_modal();
        let Some(Modal::Model(old)) = &app.modal else {
            unreachable!()
        };
        let old_instance = old.instance;
        app.open_add_model_modal();
        let profile = app.config.profiles["one"].clone();
        app.background.requests.insert("one".into(), 1);
        app.background
            .sender
            .send(Completion::Discover {
                request: 1,
                id: "one".into(),
                profile: Box::new(profile.clone()),
                form: Some(old_instance),
                result: Ok(vec![ModelEntry {
                    max_output_tokens: None,
                    context_window: None,
                    reasoning_max: None,
                    id: "stale-form-model".into(),
                    label: None,
                    description: None,
                }]),
            })
            .unwrap();
        app.poll_background();
        assert!(matches!(&app.modal, Some(Modal::Model(form)) if form.api_models.is_empty()));
        config::update(&app.paths.config, |config| {
            config.profiles.get_mut("one").unwrap().base_url = "https://changed.invalid".into();
            Ok(())
        })
        .unwrap();
        app.background.requests.insert("one".into(), 2);
        app.background
            .sender
            .send(Completion::Discover {
                request: 2,
                id: "one".into(),
                profile: Box::new(profile),
                form: None,
                result: Ok(vec![ModelEntry {
                    max_output_tokens: None,
                    context_window: None,
                    reasoning_max: None,
                    id: "wrong-endpoint-model".into(),
                    label: None,
                    description: None,
                }]),
            })
            .unwrap();
        app.poll_background();
        assert_eq!(app.cache.profiles["one"].models[0].id, "stale-form-model");
    }

    #[test]
    fn edits_after_connection_coalesce_and_preserve_explicit_sync_preference() {
        let (_temp, mut app) = persisted_app();
        app.background
            .sender
            .send(Completion::Inspect(Ok(sync::Status::Synced), None))
            .unwrap();
        app.poll_background();
        app.enter_provider_view();
        for _ in 0..3 {
            app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE))
                .unwrap();
        }
        assert!(app.background.queued_sync.is_some());
        assert!(!app.background.sync_running);
        assert_eq!(app.background.status, sync::Status::Pending);
        app.queue_sync(true, Some("two".into()));
        app.queue_sync(false, None);
        let request = app.background.queued_sync.as_ref().unwrap();
        assert!(request.explicit);
        assert_eq!(request.preferred.as_deref(), Some("two"));
        assert_eq!(config::load(&app.paths.config).unwrap(), app.config);
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn inspection_is_coalesced_and_older_results_cannot_replace_completed_sync() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        let id = uuid::Uuid::new_v4();
        app.background.active.insert(id, TaskKind::Inspect);
        app.initialize_background();
        assert_eq!(app.background.active.len(), 1);
        app.queue_sync(true, None);
        assert!(!app.background.active.contains_key(&id));
        // Simulate the newer sync finishing before the old inspection returns.
        app.background.queued_sync = None;
        app.background.status = sync::Status::Synced;
        app.background
            .sender
            .send(Completion::Finished(
                id,
                Box::new(Completion::Inspect(
                    Err(anyhow::anyhow!("stale inspection")),
                    None,
                )),
            ))
            .unwrap();
        assert!(!app.poll_background());
        assert_eq!(app.background.status, sync::Status::Synced);
    }
    #[test]
    fn inspection_errors_do_not_overwrite_an_active_proxy_operation() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        app.background.proxy_running = true;
        app.background.status = sync::Status::Synced;
        app.background
            .sender
            .send(Completion::Inspect(
                Err(anyhow::anyhow!("older status error")),
                None,
            ))
            .unwrap();
        app.poll_background();
        assert!(app.background.proxy_running);
        assert_eq!(app.background.status, sync::Status::Synced);
    }
    #[test]
    fn panic_only_clears_its_own_task_and_model_tests_can_retry() {
        let (_temp, mut app) = crate::tui::tests::persisted_app();
        app.background.model_test_running = true;
        app.background.sync_running = true;
        app.background.proxy_running = true;
        app.background.requests.insert("other".into(), 7);
        app.background
            .spawn(TaskKind::Test, || panic!("fixture failure"));
        let event = app
            .background
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        app.background.sender.send(event).unwrap();
        app.poll_background();
        assert!(!app.background.model_test_running);
        assert!(app.background.sync_running && app.background.proxy_running);
        assert_eq!(app.background.requests.get("other"), Some(&7));
        assert!(app.background.active.is_empty());
        app.background
            .sender
            .send(Completion::Finished(
                uuid::Uuid::new_v4(),
                Box::new(Completion::Panicked(TaskKind::Sync)),
            ))
            .unwrap();
        app.poll_background();
        assert!(app.background.sync_running);
    }
}
