#[tokio::test]
async fn grok_gateway_routes_and_records_upstream_usage() {
    use super::*;
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (sent, received) = tokio::sync::oneshot::channel();
    let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(sent)));
    let upstream_app = Router::new().route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let sender = sender.clone();
                async move {
                    if let Some(sender) = sender.lock().unwrap().take() {
                        sender.send((headers, body)).unwrap();
                    }
                    Json(json!({"id":"test","model":"upstream-model","choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":13,"completion_tokens":7}}))
                }
            }),
        );
    let upstream_task = tokio::spawn(async move {
        axum::serve(upstream, upstream_app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config: temp.path().join("config.toml"),
        state_dir: temp.path().join("state"),
        cache: temp.path().join("cache.json"),
    };
    config::update_client(&paths.config, config::Client::Grok, |config| {
            let mut profile: Profile = toml::from_str("name='Gateway provider'\nbase_url='http://localhost/v1'\ndefault_model='upstream-model'\n[[models]]\nid='upstream-model'\n")?;
            profile.base_url = format!("http://{upstream_address}/v1");
            profile.api_format = ApiFormat::OpenaiChat;
            profile.credential = Credential::Bearer {
                value: "upstream-secret".into(),
            };
            config.profiles.insert("provider".into(), profile);
            config.grok.active_mode = Some(crate::grok::Mode::Api);
            Ok(())
        })
        .unwrap();
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let proxy_paths = ProxyPaths::from_app(&paths).unwrap();
    update_registry(&proxy_paths, Some(&proxy_address.to_string()), |_| ()).unwrap();
    let token = load_registry(&proxy_paths).unwrap().local_token;
    let server = tokio::spawn(async move {
        serve_with_listener(proxy_paths.registry, Some(proxy_listener)).await
    });
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    for _ in 0..100 {
        if client
            .get(format!("http://{proxy_address}/health"))
            .bearer_auth(&token)
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let grok_home = temp.path().join("grok");
    fs::create_dir_all(&grok_home).unwrap();
    let sync_paths = paths.clone();
    let sync_home = grok_home.clone();
    tokio::task::spawn_blocking(move || {
        let config = config::load_client(&sync_paths.config, config::Client::Grok).unwrap();
        crate::grok::apply(&sync_paths, &sync_home, &config, None, false).unwrap();
    })
    .await
    .unwrap();
    let native: toml::Value = fs::read_to_string(grok_home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let model = &native["model"]["mux::provider::upstream-model"];
    assert_eq!(
        model["model"].as_str(),
        Some("mux::provider::upstream-model")
    );
    assert_eq!(model["api_backend"].as_str(), Some("messages"));
    let url = format!("{}/messages", model["base_url"].as_str().unwrap());
    assert_eq!(model["api_key"].as_str(), Some(token.as_str()));
    assert_eq!(
        load_registry(&ProxyPaths::from_app(&paths).unwrap())
            .unwrap()
            .routes
            .len(),
        1
    );
    let response = client
            .post(url)
            .header("x-api-key", &token)
            .json(&json!({"model":"mux::provider::upstream-model","max_tokens":16,"messages":[{"role":"user","content":"hello"}]}))
            .send()
            .await
            .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["usage"]["output_tokens"], 7);
    let (headers, forwarded) = received.await.unwrap();
    assert_eq!(forwarded["model"], "upstream-model");
    assert_eq!(headers[header::AUTHORIZATION], "Bearer upstream-secret");
    let usage = crate::usage::tests::settled_for(
        &paths.state_dir.join(crate::usage::FILE),
        &paths.config,
        1,
    )
    .await;
    let totals = usage.total(Some("Grok"), Some("provider"), None, "generation");
    assert_eq!((totals.calls, totals.input, totals.output), (1, 13, 7));
    config::update_client(&paths.config, config::Client::Grok, |config| {
        config
            .grok
            .use_account(&mut config.profiles, "grok-build".into());
        Ok(())
    })
    .unwrap();
    let sync_paths = paths.clone();
    let sync_home = grok_home.clone();
    tokio::task::spawn_blocking(move || {
        let config = config::load_client(&sync_paths.config, config::Client::Grok).unwrap();
        crate::grok::apply(&sync_paths, &sync_home, &config, None, false).unwrap();
    })
    .await
    .unwrap();
    let native: toml::Value = fs::read_to_string(grok_home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(native["models"]["default"].as_str(), Some("grok-build"));
    assert_eq!(
        native["model"]["mux::provider::upstream-model"]["base_url"].as_str(),
        model["base_url"].as_str()
    );
    assert_eq!(
        load_registry(&ProxyPaths::from_app(&paths).unwrap())
            .unwrap()
            .routes
            .len(),
        1
    );
    let response = client
            .post(format!(
                "{}/messages",
                native["model"]["mux::provider::upstream-model"]["base_url"]
                    .as_str()
                    .unwrap()
            ))
            .header("x-api-key", &token)
            .json(&json!({"model":"mux::provider::upstream-model","max_tokens":16,"messages":[{"role":"user","content":"account default with API model"}]}))
            .send()
            .await
            .unwrap();
    assert!(response.status().is_success());
    let usage =
        crate::usage::snapshot(&paths.state_dir.join(crate::usage::FILE), &paths.config).unwrap();
    assert_eq!(
        usage
            .total(Some("Grok"), Some("provider"), None, "generation")
            .calls,
        2
    );
    config::update_client(&paths.config, config::Client::Grok, |config| {
        config.grok.use_api(&mut config.profiles, None);
        Ok(())
    })
    .unwrap();
    let sync_paths = paths.clone();
    let sync_home = grok_home.clone();
    tokio::task::spawn_blocking(move || {
        let config = config::load_client(&sync_paths.config, config::Client::Grok).unwrap();
        crate::grok::apply(&sync_paths, &sync_home, &config, None, false).unwrap();
    })
    .await
    .unwrap();
    let native: toml::Value = fs::read_to_string(grok_home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        native["models"]["default"].as_str(),
        Some("mux::provider::upstream-model")
    );
    assert_eq!(
        native["model"]["mux::provider::upstream-model"]["api_backend"].as_str(),
        Some("messages")
    );
    assert_eq!(
        load_registry(&ProxyPaths::from_app(&paths).unwrap())
            .unwrap()
            .routes
            .len(),
        1
    );
    server.abort();
    upstream_task.abort();
}

#[test]
fn aggregate_operations_and_recovery_are_client_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config: temp.path().join("config.toml"),
        state_dir: temp.path().join("state"),
        cache: temp.path().join("cache.json"),
    };
    let proxy_paths = ProxyPaths::from_app(&paths).unwrap();
    let claude = RouteTarget {
        default_profile_id: Some("local".into()),
        codex: false,
        grok: false,
        pi_home: None,
        config_path: paths.config.clone(),
        profile_id: None,
        models: BTreeMap::from([(
            "local::old".into(),
            AggregateModelTarget {
                profile_id: "local".into(),
                model_id: "old".into(),
            },
        )]),
    };
    let codex = RouteTarget {
        codex: true,
        ..claude.clone()
    };
    let grok = RouteTarget {
        grok: true,
        ..claude.clone()
    };
    update_registry(&proxy_paths, None, |registry| {
        registry.routes.insert("claude".into(), claude.clone());
        registry.routes.insert("codex".into(), codex.clone());
        registry.routes.insert("grok".into(), grok.clone());
    })
    .unwrap();
    let checkpoint = aggregate_checkpoint(&paths).unwrap();
    clear_aggregate_models(&paths).unwrap();
    let cleared = load_registry(&proxy_paths).unwrap();
    assert!(cleared.routes["claude"].models.is_empty());
    assert_eq!(cleared.routes["codex"], codex);
    assert_eq!(cleared.routes["grok"], grok);

    let changed = RouteTarget {
        models: BTreeMap::new(),
        ..codex.clone()
    };
    let plan = CodexRoutePlan {
        id: "codex".into(),
        before: Some(codex.clone()),
        after: changed.clone(),
    };
    apply_codex_route(&paths, &plan).unwrap();
    restore_aggregate(&paths, checkpoint).unwrap();
    let restored = load_registry(&proxy_paths).unwrap();
    assert_eq!(restored.routes["claude"], claude);
    assert_eq!(restored.routes["codex"], changed);
    assert_eq!(restored.routes["grok"], grok);
    let settings = json!({"env":{
        "ANTHROPIC_BASE_URL":format!("http://{}/r/codex", restored.listen),
        "ANTHROPIC_AUTH_TOKEN":restored.local_token,
    }});
    assert!(!owns_settings(&paths, &settings).unwrap());
    restore_codex_route(&paths, &plan).unwrap();
    assert_eq!(load_registry(&proxy_paths).unwrap().routes["codex"], codex);
    restore_codex_route(&paths, &plan).unwrap(); // Recovery can safely retry.
}

#[test]
fn proxy_health_requires_current_config_and_binary_versions() {
    let current = serde_json::json!({"name":"mux-proxy", "config_version":crate::config::CONFIG_VERSION, "version":env!("CARGO_PKG_VERSION"), "grok_gateway":true, "pi_proxy":true});
    assert!(super::health_matches_build(&current));
    let mut without_grok = current.clone();
    without_grok.as_object_mut().unwrap().remove("grok_gateway");
    assert!(!super::health_matches_build(&without_grok));
    let mut without_pi = current.clone();
    without_pi.as_object_mut().unwrap().remove("pi_proxy");
    assert!(!super::health_matches_build(&without_pi));
    assert!(!super::health_matches_build(
        &serde_json::json!({"name":"mux-proxy"})
    ));
    let mut old = current.clone();
    old["config_version"] = serde_json::json!(4);
    assert!(!super::health_matches_build(&old));
    old = current;
    old["version"] = serde_json::json!("older");
    assert!(!super::health_matches_build(&old));
}

#[tokio::test]
async fn roles_follow_models_per_session_and_never_cross_routes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    config::update(&path, |config| {
            for id in ["a", "b"] {
                let profile: Profile = toml::from_str(&format!("name='{id}'\nbase_url='https://example.invalid'\ndefault_model='x'\n[aliases]\nsonnet='{id}-sonnet'\nopus='{id}-opus'\n"))?;
                config.profiles.insert(id.into(), profile);
            }
            Ok(())
        }).unwrap();
    let config = config::load(&path).unwrap();
    let mut target = RouteTarget {
        default_profile_id: Some("a".into()),
        codex: false,
        grok: false,
        pi_home: None,
        config_path: path.clone(),
        profile_id: None,
        models: BTreeMap::new(),
    };
    for (id, profile) in &config.profiles {
        for model in crate::discovery::active_models(profile, &[]) {
            target.models.insert(
                format!("{id}::{}", model.id),
                AggregateModelTarget {
                    profile_id: id.clone(),
                    model_id: model.id,
                },
            );
        }
    }
    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    let body = |session: &str, model: &str| json!({"model":model,"metadata":{"user_id":json!({"session_id":session}).to_string()}});
    let mut sessions = SessionProviders::default();
    sessions
        .resolve("route", &target, &config, &body(&first, "b::x"), true)
        .unwrap();
    assert_eq!(
        sessions
            .resolve(
                "route",
                &target,
                &config,
                &body(&first, "mux-role::sonnet"),
                true
            )
            .unwrap()
            .1,
        "b-sonnet"
    );
    assert_eq!(
        sessions
            .resolve(
                "route",
                &target,
                &config,
                &body(&second, "mux-role::opus"),
                true
            )
            .unwrap()
            .1,
        "a-opus"
    );
    assert_eq!(
        sessions
            .resolve("other", &target, &config, &body(&first, "sonnet5"), true)
            .unwrap()
            .1,
        "a-sonnet"
    );
    sessions
        .resolve("route", &target, &config, &body(&first, "a::x"), false)
        .unwrap();
    assert_eq!(
        sessions
            .resolve("route", &target, &config, &body(&first, "opus"), true)
            .unwrap()
            .1,
        "b-opus"
    );
    sessions
        .resolve("route", &target, &config, &body(&first, "a::x"), true)
        .unwrap();
    assert_eq!(
        sessions
            .resolve("route", &target, &config, &body(&first, "opus"), true)
            .unwrap()
            .1,
        "a-opus"
    );
    assert_eq!(
        sessions
            .resolve("route", &target, &config, &json!({"model":"sonnet"}), true)
            .unwrap()
            .1,
        "a-sonnet"
    );
    assert_eq!(
        request_session(
            &json!({"metadata":{"user_id":format!("user_test_account_test_session_{first}")}})
        ),
        Some(first.clone())
    );
    assert!(request_session(&json!({"metadata":{"user_id":"shared-user"}})).is_none());
    let state = ServerState {
        limits: limits::Limits::new(Default::default()),
        sessions: std::sync::Arc::new(std::sync::Mutex::new(sessions)),
        shutdown: Default::default(),
        registry: temp.path().join("proxy.json"),
        client: Client::new(),
        usage: crate::usage::Writer::new(temp.path().join(crate::usage::FILE)),
    };
    assert_eq!(
        resolve_request(&state, "route", &target, &body(&first, "opus"), true)
            .await
            .unwrap()
            .1,
        "a-opus"
    );
    config::update(&path, |config| {
        config.profiles.get_mut("a").unwrap().enabled = false;
        Ok(())
    })
    .unwrap();
    assert!(
        resolve_request(&state, "route", &target, &body(&first, "opus"), true)
            .await
            .is_err()
    );
}

#[test]
fn remembered_roles_recover_when_provider_is_disabled_removed_or_unsynced() {
    let config: Config = toml::from_str("version=6\n[profiles.a]\nname='A'\nbase_url='https://example.invalid'\ndefault_model='a-sonnet'\n[profiles.a.aliases]\nsonnet='a-sonnet'\n[profiles.b]\nname='B'\nbase_url='https://example.invalid'\ndefault_model='b-sonnet'\n[profiles.b.aliases]\nsonnet='b-sonnet'\n").unwrap();
    let target = RouteTarget {
        config_path: PathBuf::from("fixture.toml"),
        profile_id: None,
        default_profile_id: Some("a".into()),
        codex: false,
        grok: false,
        pi_home: None,
        models: ["a", "b"]
            .into_iter()
            .map(|id| {
                (
                    format!("{id}::{id}-sonnet"),
                    AggregateModelTarget {
                        profile_id: id.into(),
                        model_id: format!("{id}-sonnet"),
                    },
                )
            })
            .collect(),
    };
    let session = Uuid::new_v4().to_string();
    let body = |model: &str| json!({"model": model, "metadata": {"user_id": json!({"session_id":session}).to_string()}});
    for change in ["disable", "remove", "unsync"] {
        let mut sessions = SessionProviders::default();
        sessions
            .resolve("route", &target, &config, &body("b::b-sonnet"), true)
            .unwrap();
        let mut latest = config.clone();
        let mut route = target.clone();
        match change {
            "disable" => latest.profiles.get_mut("b").unwrap().enabled = false,
            "remove" => {
                latest.profiles.remove("b");
            }
            _ => {
                route.models.retain(|_, model| model.profile_id != "b");
            }
        }
        assert_eq!(
            sessions
                .resolve("route", &route, &latest, &body("sonnet"), true)
                .unwrap()
                .1,
            "a-sonnet",
            "{change}"
        );
        assert!(sessions.entries.is_empty());
        assert!(
            sessions
                .resolve("route", &route, &latest, &body("b::b-sonnet"), true)
                .is_err(),
            "explicit selection must fail: {change}"
        );
    }
}

#[test]
fn deepseek_catalog_uses_root_without_changing_messages_endpoint() {
    for suffix in [
        "",
        "/",
        "/anthropic",
        "/anthropic/",
        "/anthropic/v1",
        "/anthropic/v1/messages",
    ] {
        let base = format!("https://api.deepseek.com{suffix}");
        assert_eq!(
            models_endpoint(&base, ApiFormat::Anthropic)
                .unwrap()
                .as_str(),
            "https://api.deepseek.com/models"
        );
    }
    assert_eq!(
        api_endpoint("https://api.deepseek.com/anthropic", "messages")
            .unwrap()
            .as_str(),
        "https://api.deepseek.com/anthropic/v1/messages"
    );
    assert_eq!(
        models_endpoint("https://gateway.example/anthropic", ApiFormat::Anthropic)
            .unwrap()
            .as_str(),
        "https://gateway.example/anthropic/v1/models"
    );
    assert_eq!(
        models_endpoint(
            "https://api.deepseek.com.evil.example/anthropic",
            ApiFormat::Anthropic
        )
        .unwrap()
        .host_str(),
        Some("api.deepseek.com.evil.example")
    );
}

#[test]
fn builtin_role_patterns_are_bounded() {
    for model in [
        "sonnet",
        "claude-sonnet",
        "sonnet5",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-3-5-sonnet-20241022",
        "SONNET[1m]",
    ] {
        assert_eq!(super::requested_role(model), Some("sonnet"), "{model}");
    }
    assert_eq!(super::requested_role("claude-opus-4-6"), Some("opus"));
    assert_eq!(
        super::requested_role("claude-3-haiku-20240307"),
        Some("haiku")
    );
    for model in [
        "other::sonnet5",
        "my-sonnet",
        "sonnetish",
        "sonnet5-custom",
        "gpt-5",
        "",
    ] {
        assert_eq!(super::requested_role(model), None, "{model}");
    }
}

#[tokio::test]
async fn role_routes_respect_exact_matches_and_live_model_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    config::update(&path, |config| {
            let profile: Profile = toml::from_str("name='One'\nbase_url='https://example.invalid'\ndefault_model='a'\nenabled_models=['b']\n[aliases]\nsonnet='a'\nopus='b'\n")?;
            config.profiles.insert("one".into(), profile);
            Ok(())
        }).unwrap();
    let mut target = RouteTarget {
        default_profile_id: Some("one".into()),
        codex: false,
        grok: false,
        pi_home: None,
        config_path: path.clone(),
        profile_id: None,
        models: ["a", "b"]
            .into_iter()
            .map(|id| {
                (
                    format!("one::{id}"),
                    AggregateModelTarget {
                        profile_id: "one".into(),
                        model_id: id.into(),
                    },
                )
            })
            .collect(),
    };
    assert_eq!(
        resolve_profile(&target, Some("sonnet5")).await.unwrap().1,
        "a"
    );
    assert_eq!(
        resolve_profile(&target, Some("claude-opus-4-6"))
            .await
            .unwrap()
            .1,
        "b"
    );
    assert!(resolve_profile(&target, Some("haiku")).await.is_err());
    assert!(
        resolve_profile(&target, Some("other::sonnet5"))
            .await
            .is_err()
    );
    target.models.insert(
        "sonnet5".into(),
        AggregateModelTarget {
            profile_id: "one".into(),
            model_id: "b".into(),
        },
    );
    assert_eq!(
        resolve_profile(&target, Some("sonnet5")).await.unwrap().1,
        "b"
    );
    config::update(&path, |config| {
        config
            .profiles
            .get_mut("one")
            .unwrap()
            .disabled_models
            .push("a".into());
        Ok(())
    })
    .unwrap();
    assert!(resolve_profile(&target, Some("sonnet")).await.is_err());
    target.default_profile_id = None;
    assert!(resolve_profile(&target, Some("opus")).await.is_err());
    target.default_profile_id = Some("one".into());
    target.models.clear();
    assert!(resolve_profile(&target, Some("opus")).await.is_err());
}

#[test]
fn explicit_endpoints_preserve_gateway_prefix_and_query() {
    for endpoint in ["messages", "responses", "chat/completions", "models"] {
        let base = format!("https://example.com/gateway/{endpoint}?api-version=test");
        assert_eq!(
            super::api_endpoint(&base, "models").unwrap().as_str(),
            "https://example.com/gateway/models?api-version=test"
        );
    }
}

#[test]
fn preserves_retry_metadata_without_hop_headers_or_cookies() {
    let mut headers = super::HeaderMap::new();
    for (key, value) in [
        ("retry-after", "15"),
        ("x-request-id", "req-123"),
        ("anthropic-ratelimit-tokens-remaining", "0"),
        ("set-cookie", "secret"),
        ("content-length", "100"),
        ("connection", "x-ratelimit-private"),
        ("x-ratelimit-private", "hidden"),
    ] {
        headers.insert(
            super::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    let output = super::forwarding_response_headers(&headers);
    assert_eq!(output["retry-after"], "15");
    assert_eq!(output["x-request-id"], "req-123");
    assert_eq!(output["anthropic-ratelimit-tokens-remaining"], "0");
    assert_eq!(output.len(), 3);
}

use super::*;
use crate::config::{Config, RoleModels};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
};

#[tokio::test]
async fn stalled_streams_timeout_and_passthrough_rejects_truncation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/",
        get(|| async {
            Body::from_stream(
                futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(Bytes::from_static(b": keepalive\n\n"))
                })
                .chain(futures_util::stream::pending()),
            )
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = Client::builder().no_proxy().build().unwrap();
    let response = client.get(format!("http://{addr}/")).send().await.unwrap();
    let text = axum::body::to_bytes(
        stream_with_idle(response, ApiFormat::OpenaiChat, Duration::from_millis(20)).into_body(),
        BODY_LIMIT,
    )
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&text).contains("idle timeout"));
    let response = client.get(format!("http://{addr}/")).send().await.unwrap();
    assert!(
        axum::body::to_bytes(
            passthrough_with_idle(response, Duration::from_millis(20)).into_body(),
            BODY_LIMIT
        )
        .await
        .is_err()
    );
    server.abort();
    let (response, server) = mock_stream(vec![Bytes::from_static(
        b"data: {\"type\":\"message_start\"}\n\n",
    )])
    .await;
    assert!(
        axum::body::to_bytes(passthrough_response(response).into_body(), BODY_LIMIT)
            .await
            .is_err()
    );
    server.abort();
}

#[test]
fn model_limits_clamp_all_protocols_and_validate_thinking() {
    let profile: Profile = toml::from_str("name='Local'\nbase_url='https://example.invalid'\ndefault_model='m'\n[[models]]\nid='m'\nmax_output_tokens=8192\ncontext_window=32768\n").unwrap();
    for format in [
        ApiFormat::Anthropic,
        ApiFormat::OpenaiChat,
        ApiFormat::OpenaiResponses,
    ] {
        for requested in [None, Some(4096), Some(16384)] {
            let mut request = json!({"model":"m", "messages":[{"role":"user","content":"Hi"}]});
            if let Some(n) = requested {
                request["max_tokens"] = json!(n);
            }
            apply_model_limits(&profile, "m[1m]", &mut request).unwrap();
            let translated = translate_request(&request, format).unwrap();
            let key = if format == ApiFormat::OpenaiResponses {
                "max_output_tokens"
            } else {
                "max_tokens"
            };
            assert_eq!(translated[key], requested.unwrap_or(8192).min(8192));
        }
    }
    let mut request =
        json!({"max_tokens":16384,"thinking":{"type":"enabled","budget_tokens":8192}});
    assert!(apply_model_limits(&profile, "m", &mut request).is_err());
    request = json!({"max_tokens":16384});
    apply_model_limits(&profile, "other", &mut request).unwrap();
    assert_eq!(request["max_tokens"], 16384);
}

async fn mock_stream(chunks: Vec<Bytes>) -> (reqwest::Response, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/",
        get(move || {
            let chunks = chunks.clone();
            async move {
                Body::from_stream(futures_util::stream::iter(
                    chunks.into_iter().map(Ok::<_, std::io::Error>),
                ))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let response = Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap();
    (response, server)
}

#[tokio::test]
async fn streaming_completion_is_single_and_abrupt_eof_is_error() {
    let frame = "data: {\"id\":\"x\",\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"中文😀\"}}]}\r\n\r\ndata: [DONE]\n\ndata: [DONE]\n\n";
    let (response, server) = mock_stream(
        frame
            .as_bytes()
            .iter()
            .map(|b| Bytes::copy_from_slice(&[*b]))
            .collect(),
    )
    .await;
    let result = stream_response(response, ApiFormat::OpenaiChat);
    let body = axum::body::to_bytes(result.into_body(), BODY_LIMIT)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("中文😀"));
    assert_eq!(body.matches("event: message_stop").count(), 1);
    server.abort();
    let partial = frame.split("data: [DONE]").next().unwrap();
    let (response, server) = mock_stream(vec![Bytes::copy_from_slice(partial.as_bytes())]).await;
    let body = axum::body::to_bytes(
        stream_response(response, ApiFormat::OpenaiChat).into_body(),
        BODY_LIMIT,
    )
    .await
    .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("ended without completion"));
    assert!(!text.contains("event: message_stop"));
    server.abort();
}

#[tokio::test]
async fn body_limits_reject_large_success_and_bound_error_reads() {
    let (response, server) = mock_stream(vec![Bytes::from(vec![b'x'; 100])]).await;
    assert!(read_body(response, 50, false).await.is_err());
    server.abort();
    let (response, server) = mock_stream(vec![Bytes::from(vec![b'x'; 100])]).await;
    assert_eq!(read_body(response, 50, true).await.unwrap().len(), 50);
    server.abort();
}

#[test]
fn aggregate_model_resolution_falls_back_to_the_configured_context_variant() {
    let targets = BTreeMap::from([(
        "route::model-a[1m]".into(),
        AggregateModelTarget {
            profile_id: "route".into(),
            model_id: "model-a[1m]".into(),
        },
    )]);

    assert_eq!(
        resolve_aggregate_model_id(&targets, "route", "model-a").as_deref(),
        Some("route::model-a[1m]")
    );
    assert_eq!(
        resolve_aggregate_model_id(&targets, "other", "model-a"),
        None
    );
}

#[test]
fn clearing_aggregate_models_removes_stale_disabled_routes() {
    let temp = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config: temp.path().join("config.toml"),
        state_dir: temp.path().join("state"),
        cache: temp.path().join("cache/models.json"),
    };
    let proxy_paths = ProxyPaths::from_app(&paths).unwrap();
    update_registry(&proxy_paths, None, |registry| {
        registry.routes.insert(
            "aggregate".into(),
            RouteTarget {
                default_profile_id: None,
                codex: false,
                grok: false,
                pi_home: None,
                config_path: paths.config.clone(),
                profile_id: None,
                models: BTreeMap::from([(
                    "route::model-a".into(),
                    AggregateModelTarget {
                        profile_id: "route".into(),
                        model_id: "model-a".into(),
                    },
                )]),
            },
        );
    })
    .unwrap();

    clear_aggregate_models(&paths).unwrap();
    let registry = load_registry(&proxy_paths).unwrap();
    assert!(registry.routes["aggregate"].models.is_empty());
}

#[test]
fn normalizes_root_v1_and_complete_urls() {
    assert_eq!(
        completion_endpoint("https://api.example", ApiFormat::OpenaiChat)
            .unwrap()
            .as_str(),
        "https://api.example/v1/chat/completions"
    );
    assert_eq!(
        completion_endpoint("https://api.example/v1", ApiFormat::OpenaiResponses)
            .unwrap()
            .as_str(),
        "https://api.example/v1/responses"
    );
    assert_eq!(
        completion_endpoint(
            "https://api.example/v1/chat/completions?api-version=1",
            ApiFormat::OpenaiChat
        )
        .unwrap()
        .as_str(),
        "https://api.example/v1/chat/completions?api-version=1"
    );
}

#[test]
fn translates_chat_tools_and_strips_1m() {
    let request = json!({
        "model":"gpt-test[1m]","max_tokens":100,
        "system":[{"type":"text","text":"code"}],
        "messages":[{"role":"assistant","content":[{"type":"tool_use","id":"call-1","name":"read","input":{"path":"a"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"ok"}]}],
        "tools":[{"name":"read","description":"read","input_schema":{"type":"object"}}]
    });
    let value = translate_request(&request, ApiFormat::OpenaiChat).unwrap();
    assert_eq!(value["model"], "gpt-test");
    assert_eq!(
        value["messages"][1]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(value["messages"][2]["role"], "tool");
}

#[test]
fn rejects_unknown_content_blocks() {
    let request = json!({"model":"x","messages":[{"role":"user","content":[{"type":"document"}]}]});
    assert!(translate_request(&request, ApiFormat::OpenaiChat).is_err());
}

#[test]
fn translates_mid_conversation_system_messages() {
    let request = json!({
        "model":"x",
        "messages":[
            {"role":"system","content":[{"type":"text","text":"title the session"}]},
            {"role":"user","content":"hello"}
        ]
    });
    let chat = translate_request(&request, ApiFormat::OpenaiChat).unwrap();
    assert_eq!(chat["messages"][0]["role"], "system");
    assert_eq!(chat["messages"][0]["content"], "title the session");
    let responses = translate_request(&request, ApiFormat::OpenaiResponses).unwrap();
    assert_eq!(responses["input"][0]["role"], "system");
    assert_eq!(responses["input"][0]["content"][0]["type"], "input_text");
}

#[test]
fn translates_non_streaming_chat_tool_response() {
    let value = chat_response(&json!({
            "id":"chat-1","model":"gpt","choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"call-1","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":3}
        })).unwrap();
    assert_eq!(value["stop_reason"], "tool_use");
    assert_eq!(value["content"][0]["input"]["path"], "a");
}

#[test]
fn translates_responses_request_and_function_output() {
    let request = json!({
        "model":"gpt-response[1m]","max_tokens":50,"system":"code",
        "messages":[
            {"role":"assistant","content":[{"type":"tool_use","id":"call-7","name":"shell","input":{"cmd":"pwd"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call-7","content":"/tmp"}]}
        ]
    });
    let value = translate_request(&request, ApiFormat::OpenaiResponses).unwrap();
    assert_eq!(value["model"], "gpt-response");
    assert_eq!(value["store"], false);
    assert_eq!(value["input"][0]["type"], "function_call");
    assert_eq!(value["input"][1]["type"], "function_call_output");
}

#[test]
fn translates_non_streaming_responses_output() {
    let value = responses_response(&json!({
        "id":"resp-1","model":"gpt","status":"completed",
        "output":[
            {"type":"message","content":[{"type":"output_text","text":"done"}]},
            {"type":"function_call","call_id":"call-2","name":"read","arguments":"{\"path\":\"b\"}"}
        ],
        "usage":{"input_tokens":9,"output_tokens":4}
    }))
    .unwrap();
    assert_eq!(value["stop_reason"], "tool_use");
    assert_eq!(value["content"][0]["text"], "done");
    assert_eq!(value["content"][1]["input"]["path"], "b");
}

#[test]
fn chat_stream_accumulates_fragmented_tool_arguments() {
    let mut state = StreamState::default();
    let first = chat_stream_event(
        &json!({"id":"chat","model":"gpt","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read","arguments":"{\"pa"}}]}}]}),
        &mut state,
    );
    let second = chat_stream_event(
        &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]},"finish_reason":"tool_calls"}]}),
        &mut state,
    );
    let completed = finalize_stream(&mut state);
    let all = first
        .into_iter()
        .chain(second)
        .chain(completed)
        .collect::<String>();
    assert!(all.contains("input_json_delta"));
    assert!(all.contains("{\\\"pa"));
    assert!(all.contains("th\\\":\\\"a\\\"}"));
    assert!(all.contains("tool_use"));
    assert!(all.contains("message_stop"));
}

#[tokio::test]
async fn aggregate_proxy_forwards_namespaced_model_to_chat_completions() {
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::channel();
    let upstream_task = thread::spawn(move || {
        let (mut stream, _) = upstream.accept().unwrap();
        let mut request = vec![0_u8; 65536];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]).into_owned();
        request_tx.send(request).unwrap();
        let body = r#"{"id":"chat-1","model":"gpt-test","choices":[{"finish_reason":"stop","message":{"content":"hello"}}],"usage":{"prompt_tokens":4,"completion_tokens":1}}"#;
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
    });

    let temp = tempfile::tempdir().unwrap();
    let app_paths = AppPaths {
        config: temp.path().join("config.toml"),
        state_dir: temp.path().join("state"),
        cache: temp.path().join("cache/models.json"),
    };
    let profile = Profile {
        name: "OpenAI".into(),
        enabled: true,
        base_url: format!("http://{upstream_address}"),
        models_url: None,
        api_format: ApiFormat::OpenaiChat,
        credential: Credential::Bearer {
            value: "upstream-secret".into(),
        },
        default_model: "gpt-test[1m]".into(),
        aliases: RoleModels::default(),
        subagent_model: None,
        fallback_models: vec![],
        enabled_models: vec![],
        disabled_models: vec![],
        models: vec![],
    };
    config::update(&app_paths.config, |config: &mut Config| {
        config.profiles.insert("openai".into(), profile);
        Ok(())
    })
    .unwrap();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);
    let proxy_paths = ProxyPaths::from_app(&app_paths).unwrap();
    update_registry(&proxy_paths, Some(&proxy_address.to_string()), |registry| {
        registry.routes.insert(
            "route-test".into(),
            RouteTarget {
                default_profile_id: None,
                codex: false,
                grok: false,
                pi_home: None,
                config_path: app_paths.config.clone(),
                profile_id: None,
                models: BTreeMap::from([(
                    "openai::gpt-test[1m]".into(),
                    AggregateModelTarget {
                        profile_id: "openai".into(),
                        model_id: "gpt-test[1m]".into(),
                    },
                )]),
            },
        );
    })
    .unwrap();
    let registry = load_registry(&proxy_paths).unwrap();
    let registry_path = proxy_paths.registry.clone();
    let server = tokio::spawn(async move { serve(registry_path).await });
    let client = Client::new();
    let url = format!("http://{proxy_address}/r/route-test/v1/messages");
    let mut response = None;
    for _ in 0..30 {
        if let Ok(result) = client
            .post(&url)
            .bearer_auth(&registry.local_token)
            .json(&json!({
                // Claude strips its [1m] context hint before sending the request.
                "model":"openai::gpt-test",
                "max_tokens":20,
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
        {
            response = Some(result);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let value: Value = response.unwrap().json().await.unwrap();
    assert_eq!(value["type"], "message");
    assert_eq!(value["content"][0]["text"], "hello");
    let upstream_request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(upstream_request.starts_with("POST /v1/chat/completions "));
    assert!(upstream_request.contains("authorization: Bearer upstream-secret"));
    assert!(upstream_request.contains("\"model\":\"gpt-test\""));
    assert!(!upstream_request.contains(&registry.local_token));
    let usage = crate::usage::tests::settled_for(
        &app_paths.state_dir.join(crate::usage::FILE),
        &app_paths.config,
        1,
    )
    .await;
    let totals = usage.total(Some("Claude"), Some("openai"), None, "generation");
    assert_eq!(
        (totals.calls, totals.success, totals.input, totals.output),
        (1, 1, 4, 1)
    );
    server.abort();
    upstream_task.join().unwrap();
}

#[tokio::test]
async fn pi_proxy_uses_native_upstream_and_records_pi_usage() {
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::channel();
    let upstream_task = thread::spawn(move || {
        let (mut stream, _) = upstream.accept().unwrap();
        let mut request = vec![0_u8; 65536];
        let size = stream.read(&mut request).unwrap();
        request_tx
            .send(String::from_utf8_lossy(&request[..size]).into_owned())
            .unwrap();
        let body = r#"{"id":"chat-1","model":"pi-model","choices":[{"finish_reason":"stop","message":{"content":"from upstream"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#;
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("pi");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("models.json"),
        serde_json::to_vec(&json!({"providers":{"direct":{
            "name":"Direct Pi", "baseUrl":format!("http://{upstream_address}/v1"),
            "api":"openai-completions", "models":[{"id":"pi-model"}]
        }}}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        home.join("auth.json"),
        r#"{"direct":{"type":"api_key","key":"upstream-secret"}}"#,
    )
    .unwrap();
    let paths = AppPaths {
        config: temp.path().join("config.toml"),
        state_dir: temp.path().join("state"),
        cache: temp.path().join("cache.json"),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let proxy_paths = ProxyPaths::from_app(&paths).unwrap();
    update_registry(&proxy_paths, Some(&address.to_string()), |registry| {
        registry.routes.insert(
            "pi-route".into(),
            RouteTarget {
                default_profile_id: None,
                codex: false,
                grok: false,
                pi_home: Some(home.clone()),
                config_path: paths.config.clone(),
                profile_id: Some("direct".into()),
                models: BTreeMap::new(),
            },
        );
    })
    .unwrap();
    let registry = load_registry(&proxy_paths).unwrap();
    crate::pi::native::set_proxy(
        &home,
        "direct",
        "pi-model",
        Some((
            &format!("http://{address}/r/pi-route"),
            &registry.local_token,
        )),
    )
    .unwrap();
    let base = crate::pi::native::proxy_endpoint(&home, "direct")
        .unwrap()
        .unwrap();
    assert_eq!(
        api_endpoint(&base, "messages").unwrap().path(),
        "/r/pi-route/v1/messages"
    );
    let registry_path = proxy_paths.registry.clone();
    let server =
        tokio::spawn(async move { serve_with_listener(registry_path, Some(listener)).await });
    let response = Client::new()
        .post(format!("http://{address}/r/pi-route/v1/messages"))
        .bearer_auth(&registry.local_token)
        .json(&json!({"model":"pi-model", "max_tokens":20,
                "messages":[{"role":"user","content":"hello"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["content"][0]["text"], "from upstream");
    let request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(request.starts_with("POST /v1/chat/completions "));
    assert!(request.contains("authorization: Bearer upstream-secret"));
    assert!(!request.contains(&registry.local_token));
    let usage = crate::usage::tests::settled_for(
        &paths.state_dir.join(crate::usage::FILE),
        &paths.config,
        1,
    )
    .await;
    let totals = usage.total(Some("Pi"), Some("direct"), None, "generation");
    assert_eq!(
        (totals.calls, totals.success, totals.input, totals.output),
        (1, 1, 5, 2)
    );
    server.abort();
    upstream_task.join().unwrap();
}

#[tokio::test]
async fn aggregate_proxy_passes_namespaced_model_to_anthropic_provider() {
    check_anthropic_model_forwarding("anthropic::claude-test").await;
}

#[tokio::test]
async fn aggregate_proxy_maps_builtin_sonnet_to_upstream_model() {
    check_anthropic_model_forwarding("sonnet5").await;
}

async fn check_anthropic_model_forwarding(requested: &str) {
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::channel();
    let upstream_task = thread::spawn(move || {
        let (mut stream, _) = upstream.accept().unwrap();
        let mut request = vec![0_u8; 65536];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]).into_owned();
        request_tx.send(request).unwrap();
        let body = r#"{"id":"msg-upstream","type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":4,"output_tokens":1}}"#;
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
    });

    let temp = tempfile::tempdir().unwrap();
    let app_paths = AppPaths {
        config: temp.path().join("config.toml"),
        state_dir: temp.path().join("state"),
        cache: temp.path().join("cache/models.json"),
    };
    let profile = Profile {
        name: "Anthropic compatible".into(),
        enabled: true,
        base_url: format!("http://{upstream_address}"),
        models_url: None,
        api_format: ApiFormat::Anthropic,
        credential: Credential::XApiKey {
            value: "anthropic-secret".into(),
        },
        default_model: "claude-test[1m]".into(),
        aliases: RoleModels {
            sonnet: Some("claude-test[1m]".into()),
            ..Default::default()
        },
        subagent_model: None,
        fallback_models: vec![],
        enabled_models: vec![],
        disabled_models: vec![],
        models: vec![],
    };
    config::update(&app_paths.config, |config: &mut Config| {
        config.profiles.insert("anthropic".into(), profile);
        Ok(())
    })
    .unwrap();

    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);
    let proxy_paths = ProxyPaths::from_app(&app_paths).unwrap();
    update_registry(&proxy_paths, Some(&proxy_address.to_string()), |registry| {
        registry.routes.insert(
            "aggregate-test".into(),
            RouteTarget {
                default_profile_id: Some("anthropic".into()),
                codex: false,
                grok: false,
                pi_home: None,
                config_path: app_paths.config.clone(),
                profile_id: None,
                models: BTreeMap::from([(
                    "anthropic::claude-test[1m]".into(),
                    AggregateModelTarget {
                        profile_id: "anthropic".into(),
                        model_id: "claude-test[1m]".into(),
                    },
                )]),
            },
        );
    })
    .unwrap();
    let registry = load_registry(&proxy_paths).unwrap();
    let registry_path = proxy_paths.registry.clone();
    let server = tokio::spawn(async move { serve(registry_path).await });
    let client = Client::new();
    let url = format!("http://{proxy_address}/r/aggregate-test/v1/messages");
    let mut response = None;
    for _ in 0..30 {
        if let Ok(result) = client
            .post(&url)
            .bearer_auth(&registry.local_token)
            .json(&json!({
                // Claude strips its [1m] context hint before sending the request.
                "model":requested,
                "max_tokens":20,
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
        {
            response = Some(result);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let value: Value = response.unwrap().json().await.unwrap();
    assert_eq!(value["type"], "message");
    assert_eq!(value["content"][0]["text"], "hello");
    let upstream_request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(upstream_request.starts_with("POST /v1/messages "));
    assert!(upstream_request.contains("x-api-key: anthropic-secret"));
    assert!(upstream_request.contains("\"model\":\"claude-test\""));
    assert!(!upstream_request.contains(&registry.local_token));
    server.abort();
    upstream_task.join().unwrap();
}
#[tokio::test]
async fn stale_registry_cannot_expose_or_resolve_disabled_models() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    fs::write(
        &path,
        r#"version = 2
[profiles.one]
name = "One"
base_url = "https://example.invalid"
default_model = "a"
enabled_models = ["b"]
"#,
    )
    .unwrap();
    let target = RouteTarget {
        default_profile_id: None,
        codex: false,
        grok: false,
        pi_home: None,
        config_path: path.clone(),
        profile_id: None,
        models: ["a", "b"]
            .into_iter()
            .map(|id| {
                (
                    format!("one::{id}"),
                    AggregateModelTarget {
                        profile_id: "one".into(),
                        model_id: id.into(),
                    },
                )
            })
            .collect(),
    };
    assert_eq!(visible_route_models(&target).await.unwrap().len(), 2);
    config::update(&path, |config| {
        let profile = config.profiles.get_mut("one").unwrap();
        profile.enabled_models.clear();
        profile.disabled_models.push("b".into());
        Ok(())
    })
    .unwrap();
    assert!(resolve_profile(&target, Some("one::b")).await.is_err());
    assert_eq!(visible_route_models(&target).await.unwrap(), ["one::a"]);
    config::update(&path, |config| {
        config.profiles.get_mut("one").unwrap().enabled = false;
        Ok(())
    })
    .unwrap();
    assert!(resolve_profile(&target, Some("one::a")).await.is_err());
    assert!(visible_route_models(&target).await.unwrap().is_empty());
}
#[test]
fn port_configuration_checks_availability_and_preserves_registry_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config: temp.path().join("config.toml"),
        cache: temp.path().join("cache.json"),
        state_dir: temp.path().join("state"),
    };
    let available = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = available.local_addr().unwrap().port();
    drop(available);
    assert_eq!(
        set_port(&paths, port).unwrap().listen,
        format!("127.0.0.1:{port}")
    );
    let registry = paths.state_dir.join("proxy.json");
    let before = fs::read(&registry).unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let error = set_port(&paths, busy.local_addr().unwrap().port()).unwrap_err();
    assert!(format!("{error:#}").contains("another user or process"));
    assert!(set_port(&paths, 0).is_err());
    assert_eq!(fs::read(&registry).unwrap(), before);
    let daemon = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.state_dir.join("proxy.daemon.lock"))
        .unwrap();
    daemon.lock_exclusive().unwrap();
    assert!(
        set_port(&paths, port)
            .unwrap_err()
            .to_string()
            .contains("stop this user's proxy")
    );
    assert_eq!(fs::read(&registry).unwrap(), before);
}
