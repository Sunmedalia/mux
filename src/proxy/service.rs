//! Platform login-service configuration, liveness and rollback.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::lifecycle::stop_locked;
use super::lifecycle::{lifecycle_lock, start_locked};
use super::*;

pub fn service_status() -> Result<ProxyServiceStatus> {
    #[cfg(not(windows))]
    let home = crate::platform::home()?;
    #[cfg(target_os = "macos")]
    let (manager, path) = (
        "launchd",
        home.join("Library/LaunchAgents/com.mux.proxy.plist"),
    );
    #[cfg(target_os = "linux")]
    let (manager, path) = (
        "systemd user",
        home.join(".config/systemd/user/mux-proxy.service"),
    );
    #[cfg(windows)]
    let (manager, path) = ("Windows Startup", crate::windows::startup_path()?);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let (manager, path) = ("unsupported", home.join(".mux-proxy-service"));
    Ok(ProxyServiceStatus {
        installed: path.exists(),
        #[cfg(target_os = "macos")]
        loaded: Some(macos_service_loaded()?),
        #[cfg(not(target_os = "macos"))]
        loaded: None,
        manager,
        path,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn macos_service_target() -> String {
    format!("gui/{}/com.mux.proxy", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
pub(super) fn macos_service_loaded() -> Result<bool> {
    Ok(Command::new("launchctl")
        .args(["print", &macos_service_target()])
        .output()
        .context("could not inspect Mux login service")?
        .status
        .success())
}

#[cfg(target_os = "macos")]
pub(super) fn macos_launchctl(action: &str, targets: &[&str]) -> Result<()> {
    let output = Command::new("launchctl")
        .arg(action)
        .args(targets)
        .output()
        .with_context(|| format!("could not run launchctl {action}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("launchctl {action} failed: {}", detail.trim());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn wait_service_ready(paths: &AppPaths) -> Result<()> {
    for _ in 0..50 {
        if status(paths)?.running {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("Login service is loaded but proxy did not become ready; inspect service logs")
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn install_macos_plist(
    path: &Path,
    plist: &[u8],
    domain: &str,
    target: &str,
    loaded: bool,
    mut launch: impl FnMut(&str, &[&str]) -> Result<()>,
) -> Result<()> {
    let previous = if path.exists() {
        Some(fs::read(path)?)
    } else {
        None
    };
    if loaded {
        launch("bootout", &[target])?;
    }
    let installation = crate::codex::atomic_write(path, plist)
        .and_then(|()| launch("bootstrap", &[domain, path.to_string_lossy().as_ref()]));
    if let Err(error) = installation {
        let recovery = if let Some(previous) = previous {
            crate::codex::atomic_write(path, &previous).and_then(|()| {
                if loaded {
                    launch("bootstrap", &[domain, path.to_string_lossy().as_ref()])
                } else {
                    Ok(())
                }
            })
        } else {
            fs::remove_file(path).map_err(Into::into)
        };
        if recovery.is_err() {
            return Err(error).context("Login service installation failed; rollback also failed; previous file retained where possible");
        }
        return Err(error)
            .context("Login service installation failed; previous configuration restored");
    }
    Ok(())
}

pub fn install(paths: &AppPaths) -> Result<PathBuf> {
    let _lifecycle = lifecycle_lock(paths)?;
    let proxy_paths = ProxyPaths::from_app(paths)?;
    let executable = std::env::current_exe()?;
    #[cfg(not(windows))]
    let home = crate::platform::home()?;
    #[cfg(target_os = "macos")]
    {
        let directory = home.join("Library/LaunchAgents");
        fs::create_dir_all(&directory)?;
        let path = directory.join("com.mux.proxy.plist");
        let label = "com.mux.proxy";
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{}</string><string>internal</string><string>proxy-serve</string><string>--registry</string><string>{}</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#,
            xml_escape(&executable.to_string_lossy()),
            xml_escape(&proxy_paths.registry.to_string_lossy())
        );
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let loaded = macos_service_loaded()?;
        let previous = if path.exists() {
            Some(fs::read(&path)?)
        } else {
            None
        };
        if loaded && previous.as_deref() == Some(plist.as_bytes()) {
            if !status(paths)?.running {
                let resources = config::load(&paths.config)?.proxy;
                update_registry(&proxy_paths, None, |registry| {
                    registry.resources = resources;
                    registry.resource_config = Some(paths.config.clone());
                })?;
                macos_launchctl("kickstart", &[&macos_service_target()])?;
                wait_service_ready(paths)?;
            }
            return Ok(path);
        }
        start_locked(paths, None)?;
        stop_locked(paths)?;
        install_macos_plist(
            &path,
            plist.as_bytes(),
            &domain,
            &macos_service_target(),
            loaded,
            macos_launchctl,
        )?;
        wait_service_ready(paths)?;
        Ok(path)
    }
    #[cfg(target_os = "linux")]
    {
        start_locked(paths, None)?;
        stop_locked(paths)?;
        let directory = home.join(".config/systemd/user");
        fs::create_dir_all(&directory)?;
        let path = directory.join("mux-proxy.service");
        fs::write(
            &path,
            format!(
                "[Unit]\nDescription=Mux protocol proxy\n\n[Service]\nExecStart={} internal proxy-serve --registry {}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
                executable.display(),
                proxy_paths.registry.display()
            ),
        )?;
        let status = Command::new("systemctl")
            .args(["--user", "enable", "--now", "mux-proxy.service"])
            .output()?
            .status;
        if !status.success() {
            bail!("systemctl could not install {}", path.display());
        }
        Ok(path)
    }
    #[cfg(windows)]
    {
        let path = crate::windows::install(&executable, &proxy_paths.registry)?;
        start_locked(paths, None)?;
        Ok(path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    bail!("proxy service installation is supported on macOS and Linux");
}

#[allow(clippy::needless_return)]
pub fn uninstall() -> Result<Option<PathBuf>> {
    #[cfg(windows)]
    return crate::windows::uninstall();
    #[cfg(not(windows))]
    let home = crate::platform::home()?;
    #[cfg(target_os = "macos")]
    {
        let path = home.join("Library/LaunchAgents/com.mux.proxy.plist");
        let loaded = macos_service_loaded()?;
        if !path.exists() && !loaded {
            return Ok(None);
        }
        if loaded {
            macos_launchctl("bootout", &[&macos_service_target()])?;
        }
        if path.exists() {
            fs::remove_file(&path)?;
        }
        return Ok(Some(path));
    }
    #[cfg(target_os = "linux")]
    {
        let path = home.join(".config/systemd/user/mux-proxy.service");
        if !path.exists() {
            return Ok(None);
        }
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "mux-proxy.service"])
            .status();
        fs::remove_file(&path)?;
        return Ok(Some(path));
    }
    #[allow(unreachable_code)]
    Ok(None)
}

#[cfg(target_os = "macos")]
pub(crate) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_failure_restores_previous_file_and_loaded_service() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.plist");
        fs::write(&path, b"previous").unwrap();
        let mut commands = Vec::new();
        let mut bootstraps = 0;
        let result = install_macos_plist(
            &path,
            b"edited",
            "gui/fixture",
            "gui/fixture/unique-fixture",
            true,
            |action, _| {
                commands.push(action.to_owned());
                if action == "bootstrap" {
                    bootstraps += 1;
                    if bootstraps == 1 {
                        bail!("fixture bootstrap failure");
                    }
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(commands, ["bootout", "bootstrap", "bootstrap"]);
    }
    #[test]
    fn failed_first_install_removes_staged_file_and_bootout_failure_preserves_original() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.plist");
        assert!(
            install_macos_plist(&path, b"edited", "fixture", "fixture", false, |_, _| bail!(
                "fixture failure"
            ))
            .is_err()
        );
        assert!(!path.exists());
        fs::write(&path, b"previous").unwrap();
        assert!(
            install_macos_plist(&path, b"edited", "fixture", "fixture", true, |_, _| bail!(
                "fixture failure"
            ))
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"previous");
    }
}
