//! Live client-scoped model resolution and session routing.
use super::*;

pub(super) async fn authenticated_target(
    state: &ServerState,
    route: &str,
    headers: &HeaderMap,
) -> Result<(RouteTarget, Registry)> {
    let registry = read_registry(&state.registry).await?;
    let target = registry
        .routes
        .get(route)
        .cloned()
        .with_context(|| format!("unknown Mux route {route}"))?;
    let expected = format!("Bearer {}", registry.local_token);
    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let grok_api_key = target.grok
        && headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            == Some(registry.local_token.as_str());
    if actual != Some(expected.as_str()) && !grok_api_key {
        bail!("invalid local proxy credential");
    }
    Ok((target, registry))
}

// Only recognize role aliases and Claude family IDs, never arbitrary names
// containing a role (or another provider's namespaced route).
pub(super) fn requested_role(model: &str) -> Option<&'static str> {
    let model = model.strip_prefix("mux-role::").unwrap_or(model);
    let normalized = model.to_ascii_lowercase();
    let normalized = strip_1m(&normalized);
    let family = normalized.strip_prefix("claude-").unwrap_or(&normalized);
    for role in ["opus", "sonnet", "haiku", "fable"] {
        if let Some(suffix) = family.strip_prefix(role)
            && (suffix.is_empty()
                || suffix == "-latest"
                || suffix
                    .trim_start_matches('-')
                    .starts_with(|c: char| c.is_ascii_digit()))
            && suffix.trim_start_matches('-').split('-').all(|part| {
                part.is_empty()
                    || part == "latest"
                    || part.chars().all(|c| c.is_ascii_digit() || c == '.')
            })
        {
            return Some(role);
        }
        // Older Claude IDs put the version before the family: claude-3-5-sonnet-…
        if normalized.starts_with("claude-") {
            let parts: Vec<_> = family.split('-').collect();
            if let Some(index) = parts.iter().position(|part| *part == role)
                && index > 0
                && parts[..index].iter().all(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_digit() || c == '.')
                })
                && parts[index + 1..].iter().all(|part| {
                    *part == "latest"
                        || (!part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
                })
            {
                return Some(role);
            }
        }
    }
    None
}

pub(super) fn role_target<'a>(
    target: &'a RouteTarget,
    config: &Config,
    requested: &str,
) -> Option<&'a AggregateModelTarget> {
    if target.codex || target.grok {
        return None;
    }
    let role = requested_role(requested)?;
    let profile_id = target.default_profile_id.as_ref()?;
    let profile = config.profiles.get(profile_id)?;
    let (_, model_id) = profile.aliases.iter().find(|(name, _)| *name == role)?;
    target.models.values().find(|mapped| {
        mapped.profile_id == *profile_id
            && config::canonical_model_id(&mapped.model_id) == config::canonical_model_id(model_id)
    })
}

pub(super) async fn route_config(target: &RouteTarget) -> Result<config::Config> {
    let path = target.config_path.clone();
    let codex = target.codex;
    let grok = target.grok;
    let pi_home = target.pi_home.clone();
    tokio::task::spawn_blocking(move || {
        if let Some(home) = pi_home {
            return crate::pi::native::load(&home);
        }
        config::load_client(
            &path,
            if grok {
                config::Client::Grok
            } else if codex {
                config::Client::Codex
            } else {
                config::Client::Claude
            },
        )
    })
    .await?
}

pub(super) async fn resolve_profile(
    target: &RouteTarget,
    requested_model: Option<&str>,
) -> Result<(Profile, String, String)> {
    let config = route_config(target).await?;
    resolve_profile_from_config(target, &config, requested_model)
}

pub(super) fn resolve_profile_from_config(
    target: &RouteTarget,
    config: &config::Config,
    requested_model: Option<&str>,
) -> Result<(Profile, String, String)> {
    let (profile_id, model_id) = if let Some(profile_id) = &target.profile_id {
        (profile_id.as_str(), requested_model.unwrap_or_default())
    } else {
        let requested = requested_model.context("request is missing model")?;
        let mapped = target
            .models
            .get(requested)
            .or_else(|| {
                let canonical = strip_1m(requested);
                target
                    .models
                    .iter()
                    .find(|(exposed, _)| strip_1m(exposed) == canonical)
                    .map(|(_, mapped)| mapped)
            })
            .or_else(|| role_target(target, config, requested))
            .with_context(|| if target.grok {
                format!("model '{requested}' is not synced by Mux; press p to reconnect Grok")
            } else if target.codex {
                format!("model '{requested}' is not synced by Mux; press p to sync, then restart Codex to reload /model")
            } else {
                format!("model '{requested}' is not synced by Mux; configure its role on the default provider and sync with p")
            })?;
        (mapped.profile_id.as_str(), mapped.model_id.as_str())
    };
    let profile = config
        .profiles
        .get(profile_id)
        .cloned()
        .with_context(|| format!("profile '{profile_id}' no longer exists"))?;
    if !profile.enabled {
        bail!("profile '{profile_id}' is disabled");
    }
    if target.profile_id.is_some()
        && !target.codex
        && target.pi_home.is_none()
        && !profile.api_format.is_openai()
    {
        bail!("profile '{profile_id}' is not an OpenAI route");
    }
    let active = crate::discovery::active_models(&profile, &[]);
    let effective = active.into_iter().find(|model| {
        config::canonical_model_id(&model.id) == config::canonical_model_id(model_id)
    });
    if !model_id.is_empty() && effective.is_none() {
        bail!("model '{model_id}' is disabled or no longer configured");
    }
    let effective = effective.map(|model| model.id).unwrap_or_default();
    Ok((profile, effective, profile_id.to_owned()))
}

#[derive(Default)]
pub(super) struct SessionProviders {
    pub(super) entries: BTreeMap<(String, String), (String, std::time::Instant)>,
}

pub(super) fn request_session(body: &Value) -> Option<String> {
    let user = body.get("metadata")?.get("user_id")?.as_str()?;
    let session = serde_json::from_str::<Value>(user)
        .ok()
        .and_then(|value| value.get("session_id")?.as_str().map(str::to_owned))
        .or_else(|| {
            user.rsplit_once("_session_")
                .map(|(_, session)| session.to_owned())
        })?;
    uuid::Uuid::parse_str(&session)
        .ok()
        .map(|id| id.to_string())
}

impl SessionProviders {
    pub(super) fn resolve(
        &mut self,
        route: &str,
        target: &RouteTarget,
        config: &config::Config,
        body: &Value,
        remember: bool,
    ) -> Result<(Profile, String, String)> {
        let now = std::time::Instant::now();
        self.entries
            .retain(|_, (_, used)| now.duration_since(*used) < Duration::from_secs(24 * 60 * 60));
        let session = request_session(body).map(|session| (route.to_owned(), session));
        let mut effective = target.clone();
        if let Some(key) = &session {
            let usable = self.entries.get(key).is_some_and(|(provider, _)| {
                config
                    .profiles
                    .get(provider)
                    .is_some_and(|profile| profile.enabled)
                    && target
                        .models
                        .values()
                        .any(|mapped| mapped.profile_id == *provider)
            });
            if usable {
                let (provider, used) = self.entries.get_mut(key).expect("checked session");
                effective.default_profile_id = Some(provider.clone());
                *used = now;
            } else {
                // A remembered provider must not keep roles pinned to a route
                // which was disabled, removed or excluded by the latest sync.
                self.entries.remove(key);
            }
        }
        let requested = body.get("model").and_then(Value::as_str);
        let resolved = resolve_profile_from_config(&effective, config, requested)?;
        if remember
            && !target.codex
            && !target.grok
            && target.profile_id.is_none()
            && let Some(key) = session
            && let Some(mapped) = requested.and_then(|requested| {
                target
                    .models
                    .iter()
                    .find(|(id, _)| strip_1m(id) == strip_1m(requested))
                    .map(|(_, mapped)| mapped)
            })
        {
            if self.entries.len() >= 4096
                && !self.entries.contains_key(&key)
                && let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
            self.entries.insert(key, (mapped.profile_id.clone(), now));
        }
        Ok(resolved)
    }
}

// Read a fresh configuration before taking the session lock. Only in-memory
// selection and session updates are serialized; disk IO cannot block sessions.
pub(super) async fn resolve_request(
    state: &ServerState,
    route: &str,
    target: &RouteTarget,
    body: &Value,
    remember: bool,
) -> Result<(Profile, String, String)> {
    let config = route_config(target).await?;
    state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(route, target, &config, body, remember)
}

pub(super) async fn visible_route_models(target: &RouteTarget) -> Result<Vec<String>> {
    let config = route_config(target).await?;
    let active: BTreeMap<_, std::collections::BTreeSet<String>> = config
        .profiles
        .iter()
        .map(|(id, profile)| {
            (
                id.as_str(),
                crate::discovery::active_models(profile, &[])
                    .into_iter()
                    .map(|model| config::canonical_model_id(&model.id).to_owned())
                    .collect(),
            )
        })
        .collect();
    if let Some(id) = &target.profile_id {
        return Ok(active
            .get(id.as_str())
            .map(|models| models.iter().cloned().collect())
            .unwrap_or_default());
    }
    Ok(target
        .models
        .iter()
        .filter(|(_, model)| {
            active
                .get(model.profile_id.as_str())
                .is_some_and(|models| models.contains(config::canonical_model_id(&model.model_id)))
        })
        .map(|(id, _)| id.clone())
        .collect())
}
