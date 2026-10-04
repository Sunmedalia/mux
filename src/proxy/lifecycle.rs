//! Daemon lifecycle, socket selection and authenticated control.
use super::*;

pub(super) fn lifecycle_lock(paths: &AppPaths) -> Result<fs::File> {
    fs::create_dir_all(&paths.state_dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.state_dir.join("proxy.lifecycle.lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

pub(super) fn available_listener(address: SocketAddr) -> Result<std::net::TcpListener> {
    if !address.ip().is_loopback() || address.port() == 0 {
        bail!("use a loopback address and a port between 1 and 65535");
    }
    std::net::TcpListener::bind(address).with_context(|| format!("cannot listen on {address}; the port may be in use by another user or process. Choose another port in Proxy > Port (e)"))
}

/// Change this user's saved listen port without touching another user's process.
/// A running daemon retains its bound socket, so it must be stopped first.
pub fn set_port(paths: &AppPaths, port: u16) -> Result<ProxyStatus> {
    let _lifecycle = lifecycle_lock(paths)?;
    let proxy_paths = ProxyPaths::from_app(paths)?;
    let daemon = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&proxy_paths.daemon_lock)?;
    daemon
        .try_lock_exclusive()
        .context("stop this user's proxy before changing its port (Stop / x)")?;
    let registry = load_or_default_registry(&proxy_paths, None)?;
    let mut address: SocketAddr = registry
        .listen
        .parse()
        .context("saved proxy listen address is invalid")?;
    address.set_port(port);
    let _probe = available_listener(address)?;
    let listen = address.to_string();
    update_registry(&proxy_paths, Some(&listen), |_| ())?;
    Ok(ProxyStatus {
        running: false,
        listen,
        routes: registry.routes.len(),
        pid: None,
    })
}

pub fn start(paths: &AppPaths, listen: Option<&str>) -> Result<ProxyStatus> {
    let _lifecycle = lifecycle_lock(paths)?;
    start_locked(paths, listen)
}

pub(super) fn start_locked(paths: &AppPaths, listen: Option<&str>) -> Result<ProxyStatus> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    if let Ok(status) = status(paths)
        && status.running
    {
        if let Some(wanted) = listen
            && wanted != status.listen
        {
            bail!(
                "Mux proxy already runs at {}; stop it before changing the address",
                status.listen
            );
        }
        let health = health_document(paths)?;
        if health_matches_build(&health) {
            return Ok(status);
        }
        // An old daemon may still be running after the executable/config upgrade.
        // Replace it through the authenticated shutdown path under the same lock.
        stop_locked(paths)?;
    }
    let saved = load_or_default_registry(&proxy_paths, None)?;
    let address = listen.unwrap_or(&saved.listen);
    let socket: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid proxy listen address {address}"))?;
    if !socket.ip().is_loopback() {
        bail!("Mux proxy only accepts loopback listen addresses");
    }
    let probe = available_listener(socket)?;
    let resources = config::load(&paths.config)?.proxy;
    update_registry(&proxy_paths, Some(address), |registry| {
        registry.resources = resources;
        registry.resource_config = Some(paths.config.clone());
    })?;
    let parent = proxy_paths
        .registry
        .parent()
        .context("proxy registry has no parent")?;
    fs::create_dir_all(parent)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&proxy_paths.log)?;
    set_private(&proxy_paths.log)?;
    let stderr = log.try_clone()?;
    let executable = std::env::current_exe().context("cannot resolve mux executable")?;
    let mut command = Command::new(executable);
    command
        .args(["internal", "proxy-serve", "--registry"])
        .arg(&proxy_paths.registry)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    for name in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
    ] {
        command.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid has no memory-safety preconditions and is called in the child.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    drop(probe);
    #[cfg(windows)]
    crate::windows::spawn_background(&mut command).context("failed to start Mux proxy")?;
    #[cfg(not(windows))]
    command.spawn().context("failed to start Mux proxy")?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(50));
        let current = status(paths)?;
        if current.running && health_matches_build(&health_document(paths)?) {
            return Ok(current);
        }
    }
    bail!(
        "Mux proxy did not become ready; inspect {}",
        proxy_paths.log.display()
    )
}

pub(super) fn health_matches_build(value: &Value) -> bool {
    value["name"] == "mux-proxy"
        && value["config_version"].as_u64() == Some(u64::from(config::CONFIG_VERSION))
        && value["version"].as_str() == Some(env!("CARGO_PKG_VERSION"))
        && value["grok_gateway"] == true
        && value["pi_proxy"] == true
}

pub(super) fn health_document(paths: &AppPaths) -> Result<Value> {
    let registry = load_or_default_registry(&ProxyPaths::from_app(paths)?, None)?;
    Ok(reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(format!("http://{}/health", registry.listen))
        .bearer_auth(&registry.local_token)
        .send()?
        .error_for_status()?
        .json()?)
}

pub fn status(paths: &AppPaths) -> Result<ProxyStatus> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    let registry = load_or_default_registry(&proxy_paths, None)?;
    let url = format!("http://{}/health", registry.listen);
    let running = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(url)
        .bearer_auth(&registry.local_token)
        .send()
        .and_then(|response| response.json::<Value>())
        .is_ok_and(|value| value.get("name").and_then(Value::as_str) == Some("mux-proxy"));
    let pid = fs::read_to_string(&proxy_paths.pid)
        .ok()
        .and_then(|value| value.trim().parse().ok());
    Ok(ProxyStatus {
        running,
        listen: registry.listen,
        routes: registry.routes.len(),
        pid,
    })
}

pub fn stop(paths: &AppPaths) -> Result<()> {
    let _lifecycle = lifecycle_lock(paths)?;
    stop_locked(paths)
}

pub(super) fn stop_locked(paths: &AppPaths) -> Result<()> {
    let proxy_paths = ProxyPaths::from_app(paths)?;
    if !status(paths)?.running {
        fs::remove_file(&proxy_paths.pid).ok();
        bail!("Mux proxy is not running");
    }
    shutdown_authenticated(paths).context("proxy does not support authenticated shutdown; stop the older daemon with its original Mux version")?;
    let daemon = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&proxy_paths.daemon_lock)?;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(50));
        if FileExt::try_lock_exclusive(&daemon).is_ok() && !status(paths)?.running {
            fs::remove_file(&proxy_paths.pid).ok();
            return Ok(());
        }
    }
    bail!("Mux proxy did not stop")
}
