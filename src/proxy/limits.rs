//! Authenticate and bound work before JSON extraction; retain capacity through streaming.
use super::*;
use axum::{extract::Request, middleware::Next};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub(super) struct Limits {
    pub resources: config::ProxyResources,
    inflight: Arc<Semaphore>,
    pub token_tasks: Arc<Semaphore>,
}
impl Limits {
    pub fn new(resources: config::ProxyResources) -> Self {
        Self {
            resources,
            inflight: Arc::new(Semaphore::new(resources.max_inflight)),
            token_tasks: Arc::new(Semaphore::new(resources.max_token_tasks)),
        }
    }
}

pub(super) fn busy(responses: bool) -> Response {
    let mut response = if responses {
        responses::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Local proxy capacity reached; retry shortly",
        )
    } else {
        anthropic_error(
            StatusCode::SERVICE_UNAVAILABLE,
            anyhow::anyhow!("Local proxy capacity reached; retry shortly"),
        )
    };
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
    response
}

// Own permits before the generator is first polled, including an unpolled body.
fn retain(response: Response, permit: OwnedSemaphorePermit) -> Response {
    let (parts, body) = response.into_parts();
    let mut stream = body.into_data_stream();
    let output = async_stream::stream! {
        let _permit = permit;
        while let Some(chunk) = stream.next().await { yield chunk; }
    };
    Response::from_parts(parts, Body::from_stream(output))
}

pub(super) async fn admit(
    State(state): State<ServerState>,
    AxumPath(route): AxumPath<String>,
    request: Request,
    next: Next,
) -> Response {
    let responses = request.uri().path().contains("/responses");
    if authenticated_target(&state, &route, request.headers())
        .await
        .is_err()
    {
        return if responses {
            responses::error(
                StatusCode::UNAUTHORIZED,
                "Invalid local proxy authentication",
            )
        } else {
            anthropic_error(
                StatusCode::UNAUTHORIZED,
                anyhow::anyhow!("Invalid local proxy authentication"),
            )
        };
    }
    let permit = match state.limits.inflight.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return busy(responses),
    };
    retain(next.run(request).await, permit)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn capacity_is_retained_until_body_is_consumed_or_dropped() {
        let semaphore = Arc::new(Semaphore::new(1));
        let response = retain(
            "hello".into_response(),
            semaphore.clone().try_acquire_owned().unwrap(),
        );
        assert!(semaphore.clone().try_acquire_owned().is_err());
        drop(response);
        assert_eq!(semaphore.available_permits(), 1);
        let response = retain(
            "hello".into_response(),
            semaphore.clone().try_acquire_owned().unwrap(),
        );
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 100)
                .await
                .unwrap(),
            "hello"
        );
        assert_eq!(semaphore.available_permits(), 1);
    }
}

#[cfg(test)]
mod ingress_tests {
    use super::*;
    #[tokio::test]
    async fn authentication_precedes_json_and_overload_preserves_control_endpoints() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        let registry = temp.path().join("proxy.json");
        fs::write(&config, "version=6\n[profiles.test]\nname='Test'\nbase_url='http://localhost'\ndefault_model='m'\n").unwrap();
        fs::write(
            &registry,
            serde_json::to_vec(&Registry {
                resources: Default::default(),
                resource_config: None,
                listen: "127.0.0.1:1".into(),
                local_token: "local-fixture".into(),
                routes: BTreeMap::from([(
                    "route".into(),
                    RouteTarget {
                        config_path: config,
                        profile_id: None,
                        default_profile_id: Some("test".into()),
                        codex: false,
                        grok: false,
                        pi_home: None,
                        models: BTreeMap::from([(
                            "test::m".into(),
                            AggregateModelTarget {
                                profile_id: "test".into(),
                                model_id: "m".into(),
                            },
                        )]),
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap();
        let state = ServerState {
            limits: Limits::new(config::ProxyResources {
                max_body_mib: 1,
                ..Default::default()
            }),
            sessions: Default::default(),
            shutdown: Default::default(),
            registry,
            client: Client::new(),
            usage: crate::usage::Writer::new(temp.path().join(crate::usage::FILE)),
        };
        let released = Arc::new(tokio::sync::Notify::new());
        let release = released.clone();
        let app = business_router(state.clone())
            .route("/r/{route}/v1/body", post(|Json(_): Json<Value>| async { StatusCode::OK }))
            .route("/r/{route}/v1/held", get(move || { let release = release.clone(); async move {
                let body = async_stream::stream! { yield Ok::<_,std::convert::Infallible>(Bytes::from_static(b"hello")); release.notified().await; };
                Response::new(Body::from_stream(body))
            }}).route_layer(axum::middleware::from_fn_with_state(state.clone(), admit)))
            .route("/health", get(health)).route("/internal/shutdown", post(shutdown_request)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let url = |suffix: &str| format!("http://{address}{suffix}");
        let denied = client
            .post(url("/r/route/v1/messages"))
            .header("content-type", "application/json")
            .body("invalid JSON")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        for (size, expected) in [
            (512 * 1024, StatusCode::OK),
            (2 * 1024 * 1024, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let response = client
                .post(url("/r/route/v1/body"))
                .bearer_auth("local-fixture")
                .json(&json!({"input": "x".repeat(size)}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            response.bytes().await.unwrap();
        }
        let mut held = Vec::new();
        for _ in 0..16 {
            let response = client
                .get(url("/r/route/v1/held"))
                .bearer_auth("local-fixture")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            held.push(response);
        }
        assert_eq!(state.limits.inflight.available_permits(), 0);
        for endpoint in ["/r/route/v1/messages", "/r/route/v1/responses/compact"] {
            let response = client
                .post(url(endpoint))
                .bearer_auth("local-fixture")
                .header("content-type", "application/json")
                .body("invalid JSON")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[header::RETRY_AFTER], "1");
            response.bytes().await.unwrap();
        }
        assert_eq!(
            client
                .get(url("/health"))
                .bearer_auth("local-fixture")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        released.notify_waiters();
        for response in held {
            response.bytes().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.limits.inflight.available_permits() != 16 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let _token_one = state
            .limits
            .token_tasks
            .clone()
            .try_acquire_owned()
            .unwrap();
        let _token_two = state
            .limits
            .token_tasks
            .clone()
            .try_acquire_owned()
            .unwrap();
        let response = client
            .post(url("/r/route/v1/messages/count_tokens"))
            .bearer_auth("local-fixture")
            .json(&json!({"model":"test::m","messages":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
        response.bytes().await.unwrap();
        assert!(!temp.path().join(crate::usage::FILE).exists());
        assert_eq!(
            client
                .post(url("/internal/shutdown"))
                .bearer_auth("local-fixture")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        task.abort();
    }
}
