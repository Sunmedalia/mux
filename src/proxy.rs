mod lifecycle;
mod protocol;
mod routing;
mod service;
mod tokens;
#[cfg(test)]
use lifecycle::health_matches_build;
pub use lifecycle::{set_port, start, status, stop};
pub use protocol::models_endpoint;
use protocol::*;
pub(crate) use protocol::{api_endpoint, completion_endpoint};
use routing::*;
#[cfg(target_os = "macos")]
pub(crate) use service::xml_escape;
pub use service::{install, service_status, uninstall};
use tokens::estimate_tokens;
mod limits;
mod metering;
mod responses;
mod transport;
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use transport::*;

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use fs2::FileExt;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tempfile::NamedTempFile;
use url::Url;
use uuid::Uuid;

use crate::config::{
    self, ApiFormat, AppPaths, Config, Credential, ModelEntry, Profile, set_private,
};

const DEFAULT_LISTEN: &str = "127.0.0.1:17321";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RouteTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_profile_id: Option<String>,
    #[serde(default)]
    codex: bool,
    #[serde(default)]
    grok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pi_home: Option<PathBuf>,
    config_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    models: BTreeMap<String, AggregateModelTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AggregateModelTarget {
    profile_id: String,
    model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Registry {
    listen: String,
    local_token: String,
    #[serde(default)]
    resources: config::ProxyResources,
    #[serde(default)]
    resource_config: Option<PathBuf>,
    #[serde(default)]
    routes: BTreeMap<String, RouteTarget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub listen: String,
    pub routes: usize,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ProxyServiceStatus {
    pub installed: bool,
    pub loaded: Option<bool>,
    pub manager: &'static str,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
struct ProxyPaths {
    registry: PathBuf,
    registry_lock: PathBuf,
    daemon_lock: PathBuf,
    pid: PathBuf,
    log: PathBuf,
}

impl ProxyPaths {
    fn from_app(paths: &AppPaths) -> Result<Self> {
        let directory = &paths.state_dir;
        Ok(Self {
            registry: directory.join("proxy.json"),
            registry_lock: directory.join("proxy.json.lock"),
            daemon_lock: directory.join("proxy.daemon.lock"),
            pid: directory.join("proxy.pid"),
            log: directory.join("proxy.log"),
        })
    }
}

pub fn aggregate_model_id(profile_id: &str, model_id: &str) -> String {
    let suffix = if model_id.to_ascii_lowercase().ends_with("[1m]") {
        "[1m]"
    } else {
        ""
    };
    format!("{profile_id}::{}{suffix}", strip_1m(model_id))
}

fn resolve_aggregate_model_id(
    targets: &BTreeMap<String, AggregateModelTarget>,
    profile_id: &str,
    model_id: &str,
) -> Option<String> {
    let exact = aggregate_model_id(profile_id, model_id);
    if targets.contains_key(&exact) {
        return Some(exact);
    }
    let canonical = config::canonical_model_id(model_id);
    targets.iter().find_map(|(exposed, target)| {
        (target.profile_id == profile_id
            && config::canonical_model_id(&target.model_id) == canonical)
            .then(|| exposed.clone())
    })
}

pub fn aggregate_profile(
    paths: &AppPaths,
    config: &Config,
    models_by_profile: &BTreeMap<String, Vec<ModelEntry>>,
    default_profile_id: &str,
) -> Result<(Profile, Vec<ModelEntry>)> {
    let default_profile = config
        .profiles
        .get(default_profile_id)
        .with_context(|| format!("profile '{default_profile_id}' does not exist"))?;
    if !default_profile.enabled {
        bail!("default profile '{default_profile_id}' is disabled");
    }
    let mut targets = BTreeMap::new();
    let mut models = Vec::new();
    for (profile_id, profile) in &config.profiles {
        let active = models_by_profile
            .get(profile_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for model in active {
            let exposed = aggregate_model_id(profile_id, &model.id);
            targets.insert(
                exposed.clone(),
                AggregateModelTarget {
                    profile_id: profile_id.clone(),
                    model_id: model.id.clone(),
                },
            );
            models.push(ModelEntry {
                max_output_tokens: None,
                context_window: None,
                reasoning_max: None,
                id: exposed,
                label: Some(format!("{} · {}", profile.name, model.label())),
                description: Some(format!(
                    "{} · {} · {}",
                    profile.api_format.label(),
                    profile_id,
                    model.id
                )),
            });
        }
    }
    if targets.is_empty() {
        bail!("enable at least one model before syncing to Claude");
    }
    let default_model =
        resolve_aggregate_model_id(&targets, default_profile_id, &default_profile.default_model)
            .with_context(|| {
                format!(
                    "default model '{}' is not enabled for profile '{default_profile_id}'",
                    default_profile.default_model
                )
            })?;

    let proxy_paths = ProxyPaths::from_app(paths)?;
    let route_id = update_registry(&proxy_paths, None, |registry| {
        if let Some((id, target)) = registry.routes.iter_mut().find(|(_, target)| {
            !target.codex
                && !target.grok
                && target.config_path == paths.config
                && target.profile_id.is_none()
        }) {
            target.models = targets.clone();
            target.default_profile_id = Some(default_profile_id.into());
            return id.clone();
        }
        let id = Uuid::new_v4().simple().to_string();
        registry.routes.insert(
            id.clone(),
            RouteTarget {
                default_profile_id: Some(default_profile_id.into()),
                codex: false,
                grok: false,
                pi_home: None,
                config_path: paths.config.clone(),
                profile_id: None,
                models: targets.clone(),
            },
        );
        id
    })?;
    start(paths, None)?;
    let registry = load_registry(&proxy_paths)?;
    let expose = |model: &str| resolve_aggregate_model_id(&targets, default_profile_id, model);
    let mut routed = default_profile.clone();
    routed.name = "Mux · all providers".into();
    routed.api_format = ApiFormat::Anthropic;
    routed.base_url = format!("http://{}/r/{route_id}", registry.listen);
    routed.credential = Credential::Bearer {
        value: registry.local_token,
    };
    routed.default_model = default_model;
    routed.aliases = config::RoleModels {
        opus: Some("mux-role::opus".into()),
        sonnet: Some("mux-role::sonnet".into()),
        haiku: Some("mux-role::haiku".into()),
        fable: Some("mux-role::fable".into()),
    };
    routed.subagent_model = routed.subagent_model.as_deref().and_then(expose);
    routed.fallback_models = routed
        .fallback_models
        .iter()
        .filter_map(|model| expose(model))
        .collect();
    routed.enabled_models = models.iter().map(|model| model.id.clone()).collect();
    routed.models = models.clone();
    Ok((routed, models))
}

pub struct AggregateCheckpoint(Vec<(String, RouteTarget)>);

pub fn aggregate_checkpoint(paths: &AppPaths) -> Result<AggregateCheckpoint> {
    let registry = load_or_default_registry(&ProxyPaths::from_app(paths)?, None)?;
    Ok(AggregateCheckpoint(
        registry
            .routes
            .into_iter()
            .filter(|(_, route)| {
                !route.codex
                    && !route.grok
                    && route.config_path == paths.config
                    && route.profile_id.is_none()
            })
            .collect(),
    ))
}

pub fn restore_aggregate(paths: &AppPaths, checkpoint: AggregateCheckpoint) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        registry.routes.retain(|_, route| {
            route.codex
                || route.grok
                || route.config_path != paths.config
                || route.profile_id.is_some()
        });
        registry.routes.extend(checkpoint.0);
    })
}

pub fn owns_settings(paths: &AppPaths, value: &Value) -> Result<bool> {
    let registry = load_or_default_registry(&ProxyPaths::from_app(paths)?, None)?;
    let endpoint = value["env"]["ANTHROPIC_BASE_URL"].as_str();
    let token = value["env"]["ANTHROPIC_AUTH_TOKEN"].as_str();
    Ok(token == Some(registry.local_token.as_str())
        && registry.routes.iter().any(|(id, route)| {
            !route.codex
                && !route.grok
                && route.profile_id.is_none()
                && route.config_path == paths.config
                && endpoint == Some(format!("http://{}/r/{id}", registry.listen).as_str())
        }))
}

pub fn clear_aggregate_models(paths: &AppPaths) -> Result<()> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    update_registry(&proxy_paths, None, |registry| {
        for target in registry.routes.values_mut().filter(|target| {
            !target.codex
                && !target.grok
                && target.config_path == paths.config
                && target.profile_id.is_none()
        }) {
            target.models.clear();
        }
    })?;
    Ok(())
}

fn load_or_default_registry(paths: &ProxyPaths, listen: Option<&str>) -> Result<Registry> {
    if paths.registry.exists() {
        return load_registry(paths);
    }
    Ok(Registry {
        resources: Default::default(),
        resource_config: None,
        listen: listen.unwrap_or(DEFAULT_LISTEN).to_owned(),
        local_token: format!("mux-{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        routes: BTreeMap::new(),
    })
}

fn load_registry(paths: &ProxyPaths) -> Result<Registry> {
    serde_json::from_slice(&fs::read(&paths.registry)?)
        .with_context(|| format!("failed to read {}", paths.registry.display()))
}

fn update_registry<T>(
    paths: &ProxyPaths,
    listen: Option<&str>,
    edit: impl FnOnce(&mut Registry) -> T,
) -> Result<T> {
    let parent = paths.registry.parent().context("registry has no parent")?;
    fs::create_dir_all(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&paths.registry_lock)?;
    lock.lock_exclusive()?;
    let mut registry = load_or_default_registry(paths, listen)?;
    if let Some(listen) = listen {
        registry.listen = listen.to_owned();
    }
    let result = edit(&mut registry);
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(&serde_json::to_vec_pretty(&registry)?)?;
    temp.as_file().sync_all()?;
    set_private(temp.path())?;
    temp.persist(&paths.registry).map_err(|error| error.error)?;
    set_private(&paths.registry)?;
    FileExt::unlock(&lock).ok();
    Ok(result)
}

#[derive(Clone)]
struct ServerState {
    limits: limits::Limits,
    sessions: std::sync::Arc<std::sync::Mutex<SessionProviders>>,
    shutdown: std::sync::Arc<tokio::sync::Notify>,
    registry: PathBuf,
    client: Client,
    usage: crate::usage::Writer,
}

pub async fn serve(registry_path: PathBuf) -> Result<()> {
    serve_with_listener(registry_path, None).await
}

async fn serve_with_listener(
    registry_path: PathBuf,
    prebound_listener: Option<tokio::net::TcpListener>,
) -> Result<()> {
    let proxy_paths = ProxyPaths {
        registry: registry_path.clone(),
        registry_lock: registry_path.with_extension("json.lock"),
        daemon_lock: registry_path.with_file_name("proxy.daemon.lock"),
        pid: registry_path.with_file_name("proxy.pid"),
        log: registry_path.with_file_name("proxy.log"),
    };
    let singleton = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&proxy_paths.daemon_lock)?;
    singleton
        .try_lock_exclusive()
        .context("another Mux proxy is already running")?;
    let usage = crate::usage::Writer::new(registry_path.with_file_name(crate::usage::FILE));
    let recovery = usage.clone();
    if tokio::task::spawn_blocking(move || recovery.recover())
        .await?
        .is_err()
    {
        eprintln!("Mux usage: database unavailable; request statistics may be incomplete");
    }
    let mut registry = load_registry(&proxy_paths)?;
    if let Some(path) = &registry.resource_config {
        registry.resources = config::load(path)?.proxy;
    }
    registry.resources.validate()?;
    let address: SocketAddr = registry.listen.parse()?;
    if !address.ip().is_loopback() {
        bail!("Mux proxy refuses to bind a non-loopback address");
    }
    fs::write(&proxy_paths.pid, std::process::id().to_string())?;
    set_private(&proxy_paths.pid)?;
    let listener = if let Some(listener) = prebound_listener {
        anyhow::ensure!(
            listener.local_addr()? == address,
            "proxy listener address changed"
        );
        listener
    } else {
        tokio::net::TcpListener::bind(address)
            .await
            .with_context(|| format!("failed to bind {address}"))?
    };
    let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    let state = ServerState {
        limits: limits::Limits::new(registry.resources),
        sessions: Default::default(),
        shutdown: shutdown.clone(),
        registry: registry_path,
        usage,
        client: Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?,
    };
    let app = business_router(state.clone())
        .route("/health", get(health))
        .route("/internal/shutdown", post(shutdown_request))
        .with_state(state);
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! { _ = shutdown.notified() => {}, _ = shutdown_signal() => {} }
        })
        .await;
    fs::remove_file(&proxy_paths.pid).ok();
    result.context("proxy server failed")
}

fn business_router(state: ServerState) -> Router<ServerState> {
    Router::new()
        .route("/r/{route}/v1/messages", post(messages))
        .route("/r/{route}/v1/messages/count_tokens", post(count_tokens))
        .route("/r/{route}/v1/models", get(models))
        .route("/r/{route}/v1/responses", post(responses::handle))
        .route("/r/{route}/v1/responses/compact", post(responses::compact))
        .layer(DefaultBodyLimit::max(state.limits.resources.body_bytes()))
        .route_layer(axum::middleware::from_fn_with_state(state, limits::admit))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = terminate.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
    }
    #[cfg(not(unix))]
    if tokio::signal::ctrl_c().await.is_err() {
        // A detached Windows daemon has no console; wait for authenticated shutdown.
        std::future::pending::<()>().await;
    }
}

async fn health(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let registry = match read_registry(&state.registry).await {
        Ok(registry) => registry,
        Err(error) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let expected = format!("Bearer {}", registry.local_token);
    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if actual != Some(expected.as_str()) {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            anyhow::anyhow!("invalid local proxy credential"),
        );
    }
    Json(json!({"name":"mux-proxy","status":"ok", "config_version": config::CONFIG_VERSION, "version": env!("CARGO_PKG_VERSION"), "grok_gateway": true, "pi_proxy": true, "resources": state.limits.resources})).into_response()
}

async fn shutdown_request(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let response = health(State(state.clone()), headers).await;
    if response.status().is_success() {
        state.shutdown.notify_one();
    }
    response
}

fn registry_from_path(path: &Path) -> Result<Registry> {
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("failed to load proxy registry {}", path.display()))
}

async fn read_registry(path: &Path) -> Result<Registry> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || registry_from_path(&path)).await?
}

async fn models(
    State(state): State<ServerState>,
    AxumPath(route): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    match authenticated_target(&state, &route, &headers).await {
        Ok((target, _)) => {
            let ids = match visible_route_models(&target).await {
                Ok(ids) => ids,
                Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, error),
            };
            Json(json!({
                "data": ids.into_iter().map(|id| json!({"id":id, "object":"model"})).collect::<Vec<_>>()
            }))
            .into_response()
        }
        Err(error) => anthropic_error(StatusCode::UNAUTHORIZED, error),
    }
}

async fn count_tokens(
    State(state): State<ServerState>,
    AxumPath(route): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let target = match authenticated_target(&state, &route, &headers).await {
        Ok((target, _)) => target,
        Err(error) => return anthropic_error(StatusCode::UNAUTHORIZED, error),
    };
    if target.profile_id.is_none()
        && let Err(error) = resolve_request(&state, &route, &target, &body, false).await
    {
        return anthropic_error(StatusCode::BAD_REQUEST, error);
    }
    let permit = match state.limits.token_tasks.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return limits::busy(false),
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        estimate_tokens(&body)
    })
    .await;
    match result.unwrap_or_else(|_| Err(anyhow::anyhow!("Token estimation failed"))) {
        Ok(tokens) => Json(json!({"input_tokens": tokens})).into_response(),
        Err(error) => anthropic_error(StatusCode::BAD_REQUEST, error),
    }
}

async fn messages(
    State(state): State<ServerState>,
    AxumPath(route): AxumPath<String>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let (target, registry) = match authenticated_target(&state, &route, &headers).await {
        Ok(value) => value,
        Err(error) => return anthropic_error(StatusCode::UNAUTHORIZED, error),
    };
    let (profile, upstream_model, profile_id) =
        match resolve_request(&state, &route, &target, &body, true).await {
            Ok(value) => value,
            Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, error),
        };
    if target.profile_id.is_none() {
        body["model"] = Value::String(strip_1m(&upstream_model).to_owned());
    }
    if let Err(error) = apply_model_limits(&profile, &upstream_model, &mut body) {
        return anthropic_error(StatusCode::BAD_REQUEST, error);
    }
    let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let anthropic = profile.api_format == ApiFormat::Anthropic;
    let upstream_body = match translate_request_with_effort(
        &body,
        profile.api_format,
        profile
            .models
            .iter()
            .find(|m| {
                config::canonical_model_id(&m.id) == config::canonical_model_id(&upstream_model)
            })
            .and_then(|m| m.reasoning_max.as_deref())
            .unwrap_or("high"),
    ) {
        Ok(body) => body,
        Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, error),
    };
    let endpoint = match if anthropic {
        api_endpoint(&profile.base_url, "messages")
    } else {
        completion_endpoint(&profile.base_url, profile.api_format)
    } {
        Ok(endpoint) => endpoint,
        Err(error) => return anthropic_error(StatusCode::BAD_REQUEST, error),
    };
    let mut request = state.client.post(endpoint).json(&upstream_body);
    if anthropic {
        request = request.header(
            "anthropic-version",
            headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("2023-06-01"),
        );
        if let Some(beta) = headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
        {
            request = request.header("anthropic-beta", beta);
        }
    }
    request = match &profile.credential {
        Credential::Bearer { value } => request.bearer_auth(value),
        Credential::XApiKey { value } => request.header("x-api-key", value),
        Credential::ApiKey { value } => request.header("api-key", value),
        Credential::None => request,
    };
    let mut ticket = metering::begin(
        &state,
        &target,
        &profile_id,
        &profile,
        &upstream_model,
        "generation",
    )
    .await;
    let response = match tokio::time::timeout(HEADER_TIMEOUT, request.send()).await {
        Ok(Ok(response)) => response,
        _ => {
            return anthropic_error(
                StatusCode::BAD_GATEWAY,
                anyhow::anyhow!("upstream connection failed or response headers timed out"),
            );
        }
    };
    let response = if stream && response.status().is_success() {
        metering::observe(
            redact_stream(
                response,
                profile.credential.clone(),
                registry.local_token.clone(),
            ),
            ticket.take(),
            true,
        )
    } else {
        response
    };
    let status = response.status();
    let upstream_headers = forwarding_response_headers(response.headers());
    // Native responses keep the upstream body and status, including errors.
    if anthropic && (!stream || !status.is_success()) {
        return match tokio::time::timeout_at(
            deadline,
            metering::read_observed_body(
                response,
                ticket.take(),
                if status.is_success() {
                    state.limits.resources.body_bytes()
                } else {
                    ERROR_LIMIT
                },
            ),
        )
        .await
        {
            Ok(Ok((bytes, _value))) => {
                let bytes = if status.is_success() {
                    bytes
                } else {
                    let mut value = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| json!({"type":"error","error":{"type":"api_error","message":"Provider returned an invalid error response"}}));
                    crate::diagnostics::redact(
                        &mut value,
                        &profile.credential,
                        &registry.local_token,
                    );
                    serde_json::to_vec(&value).expect("JSON error response")
                };
                let mut result = (status, bytes).into_response();
                result.headers_mut().extend(upstream_headers);
                if !status.is_success() {
                    result.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("application/json"),
                    );
                }
                result
            }
            _ => anthropic_error(
                StatusCode::BAD_GATEWAY,
                anyhow::anyhow!("upstream body failed, exceeded limit or timed out"),
            ),
        };
    }
    if !status.is_success() {
        let bytes = tokio::time::timeout_at(deadline, read_body(response, ERROR_LIMIT, true)).await;
        let text = bytes
            .ok()
            .and_then(Result::ok)
            .as_deref()
            .and_then(|bytes| {
                crate::diagnostics::detail(bytes, &profile.credential, &registry.local_token)
            })
            .unwrap_or_else(|| {
                "check provider credentials, model and request compatibility".into()
            });
        let mut result = anthropic_error(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            anyhow::anyhow!("upstream returned HTTP {status}: {text}"),
        );
        result.headers_mut().extend(upstream_headers);
        result.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        return result;
    }
    if stream {
        if anthropic {
            return passthrough_response(response);
        }
        let mut result = stream_response(response, profile.api_format);
        result.headers_mut().extend(upstream_headers);
        result.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream"),
        );
        result
    } else {
        let value = match tokio::time::timeout_at(
            deadline,
            metering::read_observed_body(
                response,
                ticket.take(),
                state.limits.resources.body_bytes(),
            ),
        )
        .await
        {
            Ok(Ok((_, Some(value)))) => value,
            Ok(Ok((_, None))) => {
                return anthropic_error(
                    StatusCode::BAD_GATEWAY,
                    anyhow::anyhow!("Invalid upstream JSON"),
                );
            }
            _ => {
                return anthropic_error(
                    StatusCode::BAD_GATEWAY,
                    anyhow::anyhow!("upstream body failed, exceeded limit or timed out"),
                );
            }
        };
        if anthropic {
            return Json(value).into_response();
        }
        match translate_response(&value, profile.api_format) {
            Ok(value) => {
                let mut result = Json(value).into_response();
                result.headers_mut().extend(upstream_headers);
                result.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
                result
            }
            Err(error) => anthropic_error(StatusCode::BAD_GATEWAY, error),
        }
    }
}

fn passthrough_response(response: reqwest::Response) -> Response {
    passthrough_with_idle(response, IDLE_TIMEOUT)
}

fn forwarding_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers {
        let key = name.as_str();
        let connection_specific = headers.get_all(header::CONNECTION).iter().any(|v| {
            v.to_str().is_ok_and(|v| {
                v.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case(key))
            })
        });
        if !connection_specific
            && (matches!(
                key,
                "content-type"
                    | "retry-after"
                    | "request-id"
                    | "x-request-id"
                    | "cache-control"
                    | "x-codex-turn-state"
                    | "openai-processing-ms"
            ) || key.starts_with("x-ratelimit-")
                || key.starts_with("anthropic-ratelimit-"))
        {
            forwarded.append(name.clone(), value.clone());
        }
    }
    forwarded
}
fn passthrough_with_idle(response: reqwest::Response, idle: Duration) -> Response {
    let status = response.status();
    let forwarded = forwarding_response_headers(response.headers());
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| header::HeaderValue::from_static("application/json"));
    let mut stream = response.bytes_stream();
    let output = async_stream::stream! {
        let mut decoder = Decoder::default();
        let mut completed = false;
        loop {
            match tokio::time::timeout(idle, stream.next()).await {
                Ok(Some(Ok(bytes))) => {
                    match decoder.push(&bytes) {
                        Ok(frames) => {
                            completed |= frames.iter().any(|frame| serde_json::from_str::<Value>(frame).is_ok_and(|v| v["type"] == "message_stop" || v["type"] == "error"));
                            yield Ok::<Bytes, std::io::Error>(bytes);
                            if completed { return; }
                        },
                        Err(error) => { yield Err(std::io::Error::other(error.to_string())); return; }
                    }
                },
                Ok(None) => { if !completed { yield Err(std::io::Error::other("upstream stream ended without completion")); } break; },
                _ => { yield Err(std::io::Error::other("upstream stream failed or idle timeout")); break; }
            }
        }
    };
    let mut result = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from_stream(output))
        .expect("valid upstream response");
    result.headers_mut().extend(forwarded);
    result
}

fn anthropic_error(status: StatusCode, error: anyhow::Error) -> Response {
    let error_type = match status.as_u16() {
        400 => "invalid_request_error",
        401 | 403 => "authentication_error",
        429 => "rate_limit_error",
        500..=599 => "api_error",
        _ => "invalid_request_error",
    };
    (
        status,
        Json(json!({
            "type":"error",
            "error":{"type":error_type,"message":format!("{error:#}")}
        })),
    )
        .into_response()
}

fn strip_1m(model: &str) -> String {
    model.strip_suffix("[1m]").unwrap_or(model).to_owned()
}

/// Stop only the authenticated endpoint; never trust a PID during full uninstall.
pub fn shutdown_authenticated(paths: &AppPaths) -> Result<()> {
    let registry = load_registry(&ProxyPaths::from_app(paths)?)?;
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?
        .post(format!("http://{}/internal/shutdown", registry.listen))
        .bearer_auth(&registry.local_token)
        .send()?
        .error_for_status()?;
    Ok(())
}

/// The journal stores only route mappings, never upstream credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodexRoutePlan {
    id: String,
    before: Option<RouteTarget>,
    after: RouteTarget,
}

#[derive(Debug, Clone)]
pub(crate) struct GrokRoutePlan {
    id: String,
    before: Option<RouteTarget>,
    after: RouteTarget,
}

pub(crate) fn prepare_grok_route(
    paths: &AppPaths,
    config: &Config,
) -> Result<(GrokRoutePlan, String, String)> {
    let mut models = BTreeMap::new();
    for (profile_id, profile) in &config.profiles {
        if !profile.enabled {
            continue;
        }
        for model in crate::discovery::active_models(profile, &[]) {
            models.insert(
                crate::grok::model_key(&config.grok, profile_id, &model.id),
                AggregateModelTarget {
                    profile_id: profile_id.clone(),
                    model_id: model.id,
                },
            );
        }
    }
    if models.is_empty() {
        bail!("Enable at least one Grok model before using Mux Gateway");
    }
    start(paths, None)?;
    let registry = load_registry(&ProxyPaths::from_app(paths)?)?;
    let existing = registry.routes.iter().find(|(_, target)| {
        target.grok && target.config_path == paths.config && target.profile_id.is_none()
    });
    let plan = GrokRoutePlan {
        id: existing
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
        before: existing.map(|(_, target)| target.clone()),
        after: RouteTarget {
            default_profile_id: None,
            codex: false,
            grok: true,
            pi_home: None,
            config_path: paths.config.clone(),
            profile_id: None,
            models,
        },
    };
    let url = format!("http://{}/r/{}/v1", registry.listen, plan.id);
    Ok((plan, url, registry.local_token))
}

pub(crate) fn apply_grok_route(paths: &AppPaths, plan: &GrokRoutePlan) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        if registry.routes.get(&plan.id) != plan.before.as_ref() {
            bail!("Grok Gateway route changed during preparation; retry");
        }
        registry.routes.insert(plan.id.clone(), plan.after.clone());
        Ok(())
    })?
}

pub(crate) fn rollback_grok_route(paths: &AppPaths, plan: &GrokRoutePlan) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        if registry.routes.get(&plan.id) != Some(&plan.after) {
            bail!("Grok Gateway route changed during rollback");
        }
        if let Some(before) = &plan.before {
            registry.routes.insert(plan.id.clone(), before.clone());
        } else {
            registry.routes.remove(&plan.id);
        }
        Ok(())
    })?
}

pub(crate) fn remove_grok_route(paths: &AppPaths) -> Result<()> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    if !proxy_paths.registry.exists() {
        return Ok(());
    }
    update_registry(&proxy_paths, None, |registry| {
        registry.routes.retain(|_, target| {
            !(target.grok && target.config_path == paths.config && target.profile_id.is_none())
        });
    })
}

pub(crate) fn codex_model_id(profile_id: &str, model_id: &str) -> String {
    format!("{profile_id}::{}", config::canonical_model_id(model_id))
}

/// Build the picker and proxy mapping from the same client-scoped snapshot.
/// Do not install the route until the Codex transaction has been journaled.
pub(crate) fn prepare_codex_route(
    paths: &AppPaths,
    config: &Config,
) -> Result<(CodexRoutePlan, Vec<ModelEntry>, String, String)> {
    let mut models = Vec::new();
    let mut targets = BTreeMap::new();
    for (profile_id, profile) in &config.profiles {
        for mut model in crate::discovery::active_models(profile, &[]) {
            let exposed = codex_model_id(profile_id, &model.id);
            targets.insert(
                exposed.clone(),
                AggregateModelTarget {
                    profile_id: profile_id.clone(),
                    model_id: model.id.clone(),
                },
            );
            model.context_window = Some(model.context_window.unwrap_or(
                if model.id.to_ascii_lowercase().ends_with("[1m]") {
                    1_000_000
                } else {
                    128_000
                },
            ));
            model.label = Some(format!("{} · {}", profile.name, model.label()));
            model.id = exposed;
            models.push(model);
        }
    }
    if models.is_empty() {
        bail!("Enable at least one Codex model before applying");
    }
    start(paths, None)?;
    let registry = load_registry(&ProxyPaths::from_app(paths)?)?;
    let existing = registry.routes.iter().find(|(_, target)| {
        target.codex && target.config_path == paths.config && target.profile_id.is_none()
    });
    let plan = CodexRoutePlan {
        id: existing
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
        before: existing.map(|(_, target)| target.clone()),
        after: RouteTarget {
            default_profile_id: None,
            codex: true,
            grok: false,
            pi_home: None,
            config_path: paths.config.clone(),
            profile_id: None,
            models: targets,
        },
    };
    let url = format!("http://{}/r/{}/v1", registry.listen, plan.id);
    Ok((plan, models, url, registry.local_token))
}

pub(crate) fn apply_codex_route(paths: &AppPaths, plan: &CodexRoutePlan) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        if registry.routes.get(&plan.id) != plan.before.as_ref() {
            bail!("Codex proxy route changed during preparation; retry");
        }
        registry.routes.insert(plan.id.clone(), plan.after.clone());
        Ok(())
    })?
}

pub(crate) fn restore_codex_route(paths: &AppPaths, plan: &CodexRoutePlan) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        let current = registry.routes.get(&plan.id);
        if current == plan.before.as_ref() {
            return Ok(());
        }
        if current != Some(&plan.after) {
            bail!("Codex proxy route changed during recovery; preserve the recovery journal");
        }
        if let Some(before) = &plan.before {
            registry.routes.insert(plan.id.clone(), before.clone());
        } else {
            registry.routes.remove(&plan.id);
        }
        Ok(())
    })?
}

pub(crate) struct PiRoutePlan {
    id: String,
    before: Option<RouteTarget>,
    after: RouteTarget,
}

pub(crate) fn prepare_pi_route(
    paths: &AppPaths,
    home: &Path,
    profile_id: &str,
) -> Result<(PiRoutePlan, String, String)> {
    let config = crate::pi::native::load(home)?;
    let profile = config
        .profiles
        .get(profile_id)
        .with_context(|| format!("Pi provider '{profile_id}' no longer exists"))?;
    if crate::discovery::active_models(profile, &[]).is_empty() {
        bail!("Add at least one model before enabling the Pi proxy API");
    }
    start(paths, None)?;
    let registry = load_registry(&ProxyPaths::from_app(paths)?)?;
    let existing = registry.routes.iter().find(|(_, target)| {
        target.pi_home.as_deref() == Some(home)
            && target.config_path == paths.config
            && target.profile_id.as_deref() == Some(profile_id)
    });
    let plan = PiRoutePlan {
        id: existing
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string()),
        before: existing.map(|(_, target)| target.clone()),
        after: RouteTarget {
            default_profile_id: None,
            codex: false,
            grok: false,
            pi_home: Some(home.to_owned()),
            config_path: paths.config.clone(),
            profile_id: Some(profile_id.to_owned()),
            models: BTreeMap::new(),
        },
    };
    // Pi's anthropic-messages client appends /v1/messages itself.
    let url = format!("http://{}/r/{}", registry.listen, plan.id);
    Ok((plan, url, registry.local_token))
}

pub(crate) fn apply_pi_route(paths: &AppPaths, plan: &PiRoutePlan) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        if registry.routes.get(&plan.id) != plan.before.as_ref() {
            bail!("Pi proxy route changed during preparation; retry");
        }
        registry.routes.insert(plan.id.clone(), plan.after.clone());
        Ok(())
    })?
}

pub(crate) fn restore_pi_route(paths: &AppPaths, plan: &PiRoutePlan) -> Result<()> {
    update_registry(&ProxyPaths::from_app(paths)?, None, |registry| {
        if registry.routes.get(&plan.id) != Some(&plan.after) {
            bail!("Pi proxy route changed during rollback");
        }
        if let Some(before) = &plan.before {
            registry.routes.insert(plan.id.clone(), before.clone());
        } else {
            registry.routes.remove(&plan.id);
        }
        Ok(())
    })?
}

pub(crate) fn remove_pi_route(paths: &AppPaths, home: &Path, profile_id: &str) -> Result<()> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    if !proxy_paths.registry.exists() {
        return Ok(());
    }
    update_registry(&proxy_paths, None, |registry| {
        registry.routes.retain(|_, target| {
            target.pi_home.as_deref() != Some(home)
                || target.config_path != paths.config
                || target.profile_id.as_deref() != Some(profile_id)
        });
    })
}

pub(crate) fn prune_pi_routes(paths: &AppPaths, home: &Path) -> Result<()> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    if !proxy_paths.registry.exists() {
        return Ok(());
    }
    let config = crate::pi::native::load(home)?;
    let mut active = std::collections::BTreeSet::new();
    for id in config.profiles.keys() {
        if crate::pi::native::proxy_endpoint(home, id)?.is_some() {
            active.insert(id.clone());
        }
    }
    update_registry(&proxy_paths, None, |registry| {
        registry.routes.retain(|_, target| {
            target.pi_home.as_deref() != Some(home)
                || target.config_path != paths.config
                || target
                    .profile_id
                    .as_ref()
                    .is_some_and(|id| active.contains(id))
        });
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod client_preference_tests {
    use super::*;
    #[test]
    fn client_search_roundtrip_preserves_calls_and_resolves_references() {
        let request = json!({"model":"test","max_tokens":100,
            "tools":[{"name":"ToolSearch","input_schema":{"type":"object"}},
                {"name":"lookup","defer_loading":true,"input_schema":{"type":"object","properties":{"id":{"type":"string"}}}}],
            "messages":[{"role":"assistant","content":[{"type":"tool_use","id":"search-1","name":"ToolSearch","input":{"query":"lookup"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"search-1","content":[{"type":"text","text":"Found"},{"type":"tool_reference","tool_name":"lookup"}]}]}]});
        for format in [ApiFormat::OpenaiChat, ApiFormat::OpenaiResponses] {
            let converted = translate_request(&request, format).unwrap();
            assert_eq!(converted["tools"].as_array().unwrap().len(), 2);
            assert!(converted.to_string().contains("Available tool: lookup"));
            assert!(converted.to_string().contains("search-1"));
            assert!(!converted.to_string().contains("tool_reference"));
            assert!(!converted.to_string().contains("defer_loading"));
            let mut bad = request.clone();
            bad["messages"][1]["content"][0]["content"][1]["tool_name"] = json!("missing");
            assert!(
                translate_request(&bad, format)
                    .unwrap_err()
                    .to_string()
                    .contains("missing")
            );
            let mut server = request.clone();
            server["tools"][0]["type"] = json!("tool_search_tool_regex_20251119");
            assert!(translate_request(&server, format).is_err());
        }
        assert_eq!(
            translate_request(&request, ApiFormat::Anthropic).unwrap(),
            request
        );
    }
    #[test]
    fn effort_caps_are_model_specific_and_explicit_disable_stays_disabled() {
        let mut request = json!({"model":"test","messages":[],"output_config":{"effort":"max"}});
        for (maximum, expected) in [("low", "low"), ("high", "high"), ("xhigh", "xhigh")] {
            let chat =
                translate_request_with_effort(&request, ApiFormat::OpenaiChat, maximum).unwrap();
            assert_eq!(chat["reasoning_effort"], expected);
            let responses =
                translate_request_with_effort(&request, ApiFormat::OpenaiResponses, maximum)
                    .unwrap();
            assert_eq!(responses["reasoning"]["effort"], expected);
        }
        assert!(
            translate_request_with_effort(&request, ApiFormat::OpenaiChat, "off")
                .unwrap()
                .get("reasoning_effort")
                .is_none()
        );
        request["output_config"]["effort"] = json!("auto");
        assert!(mapped_effort(&request, "high").unwrap().is_none());
        request.as_object_mut().unwrap().remove("output_config");
        request["thinking"] = json!({"type":"disabled"});
        assert!(mapped_effort(&request, "high").unwrap().is_none());
        request["thinking"] = json!({"type":"adaptive"});
        assert_eq!(
            mapped_effort(&request, "medium").unwrap().as_deref(),
            Some("medium")
        );
        request["output_config"] = json!({"effort":"low"});
        assert_eq!(
            mapped_effort(&request, "xhigh").unwrap().as_deref(),
            Some("low")
        );
    }
}

#[cfg(test)]
mod client_search_integration {
    use super::*;
    #[tokio::test]
    async fn proxy_search_and_tool_call_work_for_chat_responses_and_streaming() {
        for format in [ApiFormat::OpenaiChat, ApiFormat::OpenaiResponses] {
            for streaming in [false, true] {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = upstream.local_addr().unwrap();
                let handler = move |Json(request): Json<Value>| {
                    let tx = tx.clone();
                    async move {
                        let found = request.to_string().contains("Available tool: lookup");
                        tx.send(request).unwrap();
                        let name = if found { "lookup" } else { "ToolSearch" };
                        let args = if found {
                            r#"{"id":"42"}"#
                        } else {
                            r#"{"query":"lookup"}"#
                        };
                        let response = match format {
                            ApiFormat::OpenaiChat => {
                                json!({"id":"r1","model":"test","choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"call1","type":"function","function":{"name":name,"arguments":args}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":5}})
                            }
                            _ => {
                                json!({"id":"r1","model":"test","status":"completed","output":[{"id":"fc1","type":"function_call","call_id":"call1","name":name,"arguments":args}],"usage":{"input_tokens":10,"output_tokens":5}})
                            }
                        };
                        if !streaming {
                            return Json(response).into_response();
                        }
                        let body = match format {
                            ApiFormat::OpenaiChat => format!(
                                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                                json!({"id":"r1","model":"test","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":name,"arguments":args}}]},"finish_reason":null}]}),
                                json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})
                            ),
                            _ => format!(
                                "event: response.output_item.added\ndata: {}\n\nevent: response.function_call_arguments.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                                json!({"type":"response.output_item.added","output_index":0,"item":{"id":"fc1","type":"function_call","call_id":"call1","name":name,"arguments":""}}),
                                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":args}),
                                json!({"type":"response.completed","response":response})
                            ),
                        };
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            body,
                        )
                            .into_response()
                    }
                };
                let upstream_app = axum::Router::new()
                    .route("/v1/chat/completions", axum::routing::post(handler.clone()))
                    .route("/v1/responses", axum::routing::post(handler));
                let upstream_task = tokio::spawn(async move {
                    axum::serve(upstream, upstream_app).await.unwrap();
                });
                let temp = tempfile::tempdir().unwrap();
                let paths = AppPaths {
                    config: temp.path().join("config.toml"),
                    state_dir: temp.path().join("state"),
                    cache: temp.path().join("cache.json"),
                };
                let mut profile: Profile = toml::from_str("name='Test'\nbase_url='http://localhost'\ndefault_model='test'\n[[models]]\nid='test'\nreasoning_max='xhigh'\n").unwrap();
                profile.api_format = format;
                profile.base_url = format!("http://{address}");
                config::update(&paths.config, |c| {
                    c.profiles.insert("test".into(), profile);
                    Ok(())
                })
                .unwrap();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let proxy_address = listener.local_addr().unwrap();
                let proxy_paths = ProxyPaths::from_app(&paths).unwrap();
                update_registry(&proxy_paths, Some(&proxy_address.to_string()), |r| {
                    r.routes.insert(
                        "search".into(),
                        RouteTarget {
                            config_path: paths.config.clone(),
                            profile_id: Some("test".into()),
                            codex: false,
                            grok: false,
                            pi_home: None,
                            default_profile_id: None,
                            models: BTreeMap::new(),
                        },
                    );
                })
                .unwrap();
                let registry = load_registry(&proxy_paths).unwrap();
                let server = tokio::spawn(async move {
                    serve_with_listener(proxy_paths.registry, Some(listener)).await
                });
                let client = Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap();
                let url = format!("http://{proxy_address}/r/search/v1/messages");
                let mut ready = false;
                for _ in 0..500 {
                    if client
                        .get(format!("http://{proxy_address}/health"))
                        .bearer_auth(&registry.local_token)
                        .send()
                        .await
                        .is_ok_and(|response| response.status().is_success())
                    {
                        ready = true;
                        break;
                    }
                    assert!(!server.is_finished(), "proxy exited before becoming ready");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert!(ready, "proxy did not become ready");
                let mut request = json!({"model":"test","stream":streaming,"max_tokens":100,"output_config":{"effort":"max"},
                    "tools":[{"name":"ToolSearch","input_schema":{"type":"object"}},{"name":"lookup","defer_loading":true,"input_schema":{"type":"object"}}],
                    "messages":[{"role":"user","content":"Find and call lookup"}]});
                for name in ["ToolSearch", "lookup"] {
                    let response = client
                        .post(&url)
                        .bearer_auth(&registry.local_token)
                        .json(&request)
                        .send()
                        .await
                        .unwrap();
                    assert!(response.status().is_success());
                    let text = response.text().await.unwrap();
                    assert!(
                        text.contains(name),
                        "{format:?} streaming={streaming}: {text}"
                    );
                    assert!(text.contains("tool_use"));
                    if streaming {
                        assert!(text.contains("message_stop"));
                    }
                    let captured = rx.recv().await.unwrap();
                    let effort = if format == ApiFormat::OpenaiChat {
                        &captured["reasoning_effort"]
                    } else {
                        &captured["reasoning"]["effort"]
                    };
                    assert_eq!(effort, "xhigh");
                    assert!(!captured.to_string().contains("defer_loading"));
                    if name == "lookup" {
                        assert!(captured.to_string().contains("Available tool: lookup"));
                    }
                    request["messages"].as_array_mut().unwrap().extend([
                        json!({"role":"assistant","content":[{"type":"tool_use","id":"call1","name":"ToolSearch","input":{"query":"lookup"}}]}),
                        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call1","content":[{"type":"tool_reference","tool_name":"lookup"}]}]}),
                    ]);
                }
                server.abort();
                upstream_task.abort();
            }
        }
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;
    #[tokio::test]
    async fn upstream_errors_are_redacted_and_keep_retry_headers_across_ingress_protocols() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = Router::new().fallback(|| async {
            (StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "17"),("x-request-id", "fixture-request"),("connection", "x-ratelimit-private"),("x-ratelimit-private", "must-not-forward")],
                Json(json!({"type":"error","error":{"message":"upstream-secret local-secret\n","headers":{"Authorization":"debug-secret"}}})))
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        for format in [
            ApiFormat::Anthropic,
            ApiFormat::OpenaiChat,
            ApiFormat::OpenaiResponses,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.toml");
            let mut profile: Profile =
                toml::from_str("name='Test'\nbase_url='http://localhost'\ndefault_model='m'\n")
                    .unwrap();
            profile.base_url = format!("http://{address}");
            profile.api_format = format;
            profile.credential = Credential::Bearer {
                value: "upstream-secret".into(),
            };
            config::update(&path, |config| {
                config.profiles.insert("test".into(), profile.clone());
                config.codex.profiles.insert("test".into(), profile);
                Ok(())
            })
            .unwrap();
            let target = RouteTarget {
                default_profile_id: Some("test".into()),
                codex: false,
                grok: false,
                pi_home: None,
                config_path: path,
                profile_id: None,
                models: BTreeMap::from([(
                    "test::m".into(),
                    AggregateModelTarget {
                        profile_id: "test".into(),
                        model_id: "m".into(),
                    },
                )]),
            };
            let mut codex_target = target.clone();
            codex_target.codex = true;
            let registry = temp.path().join("proxy.json");
            fs::write(
                &registry,
                serde_json::to_vec(&Registry {
                    listen: "127.0.0.1:1".into(),
                    local_token: "local-secret".into(),
                    resources: Default::default(),
                    resource_config: None,
                    routes: BTreeMap::from([
                        ("messages".into(), target),
                        ("responses".into(), codex_target),
                    ]),
                })
                .unwrap(),
            )
            .unwrap();
            let state = ServerState {
                limits: limits::Limits::new(Default::default()),
                sessions: Default::default(),
                shutdown: Default::default(),
                registry,
                client: Client::new(),
                usage: crate::usage::Writer::new(temp.path().join(crate::usage::FILE)),
            };
            let headers = HeaderMap::from_iter([(
                header::AUTHORIZATION,
                header::HeaderValue::from_static("Bearer local-secret"),
            )]);
            let message = messages(State(state.clone()),AxumPath("messages".into()),headers.clone(),Json(json!({"model":"test::m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))).await;
            let response = responses::handle(
                State(state),
                AxumPath("responses".into()),
                headers,
                Json(json!({"model":"test::m","input":"hi"})),
            )
            .await;
            for result in [message, response] {
                assert_eq!(result.status(), StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(result.headers()[header::RETRY_AFTER], "17");
                assert_eq!(result.headers()["x-request-id"], "fixture-request");
                assert!(!result.headers().contains_key("x-ratelimit-private"));
                let bytes = axum::body::to_bytes(result.into_body(), BODY_LIMIT)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert!(!value.to_string().contains("secret"));
            }
        }
        task.abort();
    }
    #[test]
    #[ignore = "manual local tokenizer benchmark; no upstream requests"]
    fn tokenizer_benchmark() {
        fn median(mut times: Vec<u128>) -> u128 {
            times.sort();
            times[times.len() / 2]
        }
        for (label, body) in [
            (
                "short",
                json!({"messages":[{"role":"user","content":"hello"}]}),
            ),
            (
                "long",
                json!({"messages":[{"role":"user","content":"hello world ".repeat(10_000)}]}),
            ),
        ] {
            estimate_tokens(&body).unwrap();
            let encoded = serde_json::to_string(&body).unwrap();
            let mut old = Vec::new();
            let mut cached = Vec::new();
            for _ in 0..7 {
                let start = std::time::Instant::now();
                let bpe = tiktoken_rs::cl100k_base().unwrap();
                let before = bpe.encode_with_special_tokens(&encoded).len();
                old.push(start.elapsed().as_micros());
                let start = std::time::Instant::now();
                let after = estimate_tokens(&body).unwrap();
                cached.push(start.elapsed().as_micros());
                assert_eq!(before, after);
            }
            let (old, cached) = (median(old), median(cached));
            eprintln!(
                "{label}: uncached p50={old} us; cached p50={cached} us; speedup={:.1}x",
                old as f64 / cached.max(1) as f64
            );
        }
    }
}
