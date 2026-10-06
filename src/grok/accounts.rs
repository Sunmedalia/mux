//! Private Grok OAuth snapshots; only public account metadata enters Mux config.
use super::auth;
use crate::{
    codex::atomic_write,
    config::{self, AppPaths},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub email: Option<String>,
}
fn private_dir(path: &Path) -> Result<()> {
    if path.is_symlink() {
        bail!("Refusing symlink Grok account directory");
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn lock(paths: &AppPaths) -> Result<fs::File> {
    private_dir(&paths.state_dir)?;
    let path = paths.state_dir.join("grok-accounts.lock");
    if path.is_symlink() {
        bail!("Refusing symlink Grok account lock");
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    config::set_private(&path)?;
    file.try_lock_exclusive()
        .context("A Grok account operation is running; retry when it finishes")?;
    Ok(file)
}
fn directory(paths: &AppPaths, id: &str) -> Result<PathBuf> {
    if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid Grok account ID");
    }
    let root = paths.state_dir.join("grok-accounts");
    let target = root.join(id);
    if root.is_symlink() || target.is_symlink() {
        bail!("Refusing symlink Grok account directory");
    }
    Ok(target)
}
fn identity(entry: &Value) -> Result<String> {
    let claims = entry["key"]
        .as_str()
        .or_else(|| entry["access_token"].as_str())
        .and_then(|token| token.split('.').nth(1))
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or(Value::Null);
    let subject = entry["user_id"]
        .as_str()
        .or_else(|| entry["sub"].as_str())
        .or_else(|| claims["sub"].as_str())
        .or_else(|| entry["email"].as_str())
        .filter(|s| !s.trim().is_empty())
        .context(
            "Grok login has no stable account identity; cannot save it as a separate account",
        )?;
    Ok(format!(
        "{:x}",
        Sha256::digest(format!(
            "{}\0{}\0{}",
            entry["oidc_issuer"]
                .as_str()
                .or_else(|| claims["iss"].as_str())
                .unwrap_or(""),
            subject,
            entry["principal_id"].as_str().unwrap_or("")
        ))
    ))
}
pub fn current_id(home: &Path) -> Result<Option<String>> {
    auth::saved_entry(home)?.as_ref().map(identity).transpose()
}
fn capture_locked(paths: &AppPaths, home: &Path) -> Result<Option<String>> {
    let Some(document) = auth::read_auth(home)? else {
        return Ok(None);
    };
    let Some(entry) = auth::saved_entry_from(&document) else {
        return Ok(None);
    };
    let id = identity(&entry)?;
    let directory = directory(paths, &id)?;
    private_dir(&paths.state_dir.join("grok-accounts"))?;
    private_dir(&directory)?;
    let snapshot = directory.join("auth.json");
    // A re-login may have saved fresher credentials without activating them.
    // Do not replace those with the untouched, older native login on reopen.
    if snapshot.exists() {
        let saved = auth::saved_entry(&directory)?.context("Saved Grok login is missing")?;
        if identity(&saved)? != id {
            bail!("Saved Grok account identity mismatch");
        }
        if fs::metadata(&snapshot)?.modified()?
            > fs::metadata(home.join("auth.json"))?.modified()?
            && config::load(&paths.config)?.grok.accounts.contains_key(&id)
        {
            return Ok(Some(id));
        }
    }
    atomic_write(
        &directory.join("auth.json"),
        &serde_json::to_vec(&document)?,
    )?;
    let email = auth::entry_status(&entry).email;
    config::update(&paths.config, |config| {
        let account = config
            .grok
            .accounts
            .entry(id.clone())
            .or_insert_with(|| Account {
                name: email
                    .clone()
                    .unwrap_or_else(|| format!("Grok {}", &id[..8])),
                email: email.clone(),
            });
        account.email = email.clone();
        Ok(())
    })?;
    Ok(Some(id))
}
pub fn capture(paths: &AppPaths, home: &Path) -> Result<Option<String>> {
    let _guard = lock(paths)?;
    capture_locked(paths, home)
}
pub fn login(
    paths: &AppPaths,
    home: &Path,
    action: auth::Action,
    cancel: &AtomicBool,
    notify: impl FnMut(String),
) -> Result<String> {
    login_with(paths, home, action, |isolated| {
        auth::run(action, isolated, cancel, notify).map(|_| ())
    })
}
fn login_with(
    paths: &AppPaths,
    home: &Path,
    action: auth::Action,
    run: impl FnOnce(&Path) -> Result<()>,
) -> Result<String> {
    if action == auth::Action::Logout {
        bail!("Use native sign out for the active account")
    }
    let _guard = lock(paths)?;
    capture_locked(paths, home)?;
    let temp = tempfile::tempdir_in(&paths.state_dir)?;
    private_dir(temp.path())?;
    // Preserve native issuer/client options while isolating the new credentials.
    let config = home.join("config.toml");
    if config.exists() {
        if fs::symlink_metadata(&config)?.file_type().is_symlink() {
            bail!("Refusing symlink Grok config")
        }
        atomic_write(&temp.path().join("config.toml"), &fs::read(config)?)?;
    }
    run(temp.path())?;
    capture_locked(paths, temp.path())?.context("Grok did not save an OAuth account")
}
pub fn activate(paths: &AppPaths, home: &Path, id: &str) -> Result<()> {
    let _guard = lock(paths)?;
    if !config::load(&paths.config)?.grok.accounts.contains_key(id) {
        bail!("Saved Grok account no longer exists")
    }
    let saved = directory(paths, id)?;
    let entry = auth::saved_entry(&saved)?.context("Saved Grok login is missing; sign in again")?;
    if identity(&entry)? != id {
        bail!("Saved Grok account identity mismatch")
    }
    // Capture token refreshes made by the native CLI before changing accounts.
    if current_id(home)?.as_deref() != Some(id) {
        capture_locked(paths, home)?;
    }
    let mut document = auth::read_auth(&saved)?.context("Saved Grok login disappeared")?;
    if document.get("auth_mode").is_none() {
        // Native keyed auth stores may also contain unrelated API credentials.
        // Keep those from the live store, replacing only its OAuth entries.
        if let Some(current) = auth::read_auth(home)?
            && current.get("auth_mode").is_none()
            && let (Some(target), Some(live)) = (document.as_object_mut(), current.as_object())
        {
            target.retain(|_, entry| auth::entry_status(entry).saved);
            for (key, entry) in live {
                if !auth::entry_status(entry).saved && !target.contains_key(key) {
                    target.insert(key.clone(), entry.clone());
                }
            }
        }
    }
    if home.join("auth.json").is_symlink() {
        bail!("Refusing symlink Grok auth.json")
    }
    private_dir(home)?;
    atomic_write(&home.join("auth.json"), &serde_json::to_vec(&document)?)
}
pub fn remove(paths: &AppPaths, home: &Path, id: &str) -> Result<()> {
    let _guard = lock(paths)?;
    let directory = directory(paths, id)?;
    if current_id(home)?.as_deref() == Some(id) {
        bail!("Switch accounts or sign out before deleting the active Grok account")
    }
    if directory.exists() {
        fs::remove_dir_all(directory)?;
    }
    config::update(&paths.config, |config| {
        config.grok.accounts.remove(id);
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (tempfile::TempDir, AppPaths, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: temp.path().join("config.toml"),
            state_dir: temp.path().join("state"),
            cache: temp.path().join("cache"),
        };
        let home = temp.path().join("grok");
        fs::create_dir_all(&home).unwrap();
        config::update(&paths.config, |_| Ok(())).unwrap();
        (temp, paths, home)
    }
    fn write(home: &Path, user: &str, token: &str) {
        fs::write(home.join("auth.json"), serde_json::to_vec(&json!({"auth_mode":"oidc","user_id":user,"email":format!("{user}@example.com"),"key":token})).unwrap()).unwrap();
    }
    #[test]
    fn first_login_is_saved_without_replacing_native_login() {
        let (_temp, paths, home) = fixture();
        let id = login_with(&paths, &home, auth::Action::Browser, |isolated| {
            write(isolated, "first", "FIRST");
            Ok(())
        })
        .unwrap();
        assert_eq!(current_id(&home).unwrap(), None);
        assert!(!home.join("auth.json").exists());
        assert_eq!(config::load(&paths.config).unwrap().grok.accounts.len(), 1);
        activate(&paths, &home, &id).unwrap();
        assert_eq!(current_id(&home).unwrap(), Some(id));
    }
    #[test]
    fn isolated_login_switch_refresh_and_delete_preserve_other_accounts() {
        let (_temp, paths, home) = fixture();
        write(&home, "one", "TOKEN_ONE");
        let first = capture(&paths, &home).unwrap().unwrap();
        fs::write(home.join("config.toml"), "[models]\ndefault='grok-build'\n").unwrap();
        let original = fs::read(home.join("auth.json")).unwrap();
        let second = login_with(&paths, &home, auth::Action::Browser, |isolated| {
            assert_ne!(isolated, home);
            assert!(!isolated.join("auth.json").exists());
            assert_eq!(
                fs::read(isolated.join("config.toml"))?,
                fs::read(home.join("config.toml"))?
            );
            write(isolated, "two", "TOKEN_TWO");
            Ok(())
        })
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), original);
        assert_eq!(config::load(&paths.config).unwrap().grok.accounts.len(), 2);
        let metadata = fs::read_to_string(&paths.config).unwrap();
        assert!(!metadata.contains("TOKEN_ONE") && !metadata.contains("TOKEN_TWO"));
        write(&home, "one", "REFRESHED_ONE");
        activate(&paths, &home, &second).unwrap();
        assert_eq!(current_id(&home).unwrap().as_deref(), Some(second.as_str()));
        activate(&paths, &home, &first).unwrap();
        assert_eq!(
            auth::read_auth(&home).unwrap().unwrap()["key"],
            "REFRESHED_ONE"
        );
        assert!(remove(&paths, &home, &first).is_err());
        remove(&paths, &home, &second).unwrap();
        assert!(!directory(&paths, &second).unwrap().exists());
        assert_eq!(config::load(&paths.config).unwrap().grok.accounts.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(directory(&paths, &first).unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(home.join("auth.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn failed_login_preserves_live_credentials_and_relogin_updates_same_account() {
        let (_temp, paths, home) = fixture();
        write(&home, "one", "OLD");
        let first = capture(&paths, &home).unwrap().unwrap();
        let before = fs::read(home.join("auth.json")).unwrap();
        assert!(login_with(&paths, &home, auth::Action::Device, |_| bail!("cancelled")).is_err());
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), before);
        let again = login_with(&paths, &home, auth::Action::Device, |isolated| {
            write(isolated, "one", "NEW");
            Ok(())
        })
        .unwrap();
        assert_eq!(again, first);
        assert_eq!(config::load(&paths.config).unwrap().grok.accounts.len(), 1);
        capture(&paths, &home).unwrap(); // reopening the page must keep the new login
        activate(&paths, &home, &again).unwrap();
        assert_eq!(auth::read_auth(&home).unwrap().unwrap()["key"], "NEW");
    }
    #[test]
    fn keyed_accounts_keep_live_api_credentials_and_reject_tampering() {
        let (_temp, paths, home) = fixture();
        fs::write(home.join("auth.json"), json!({"oauth":{"auth_mode":"oidc","user_id":"one","key":"OLD"},"api":{"auth_mode":"api_key","key":"API_OLD"}}).to_string()).unwrap();
        let first = capture(&paths, &home).unwrap().unwrap();
        fs::write(home.join("auth.json"), json!({"oauth":{"auth_mode":"oidc","user_id":"two","key":"TWO"},"api":{"auth_mode":"api_key","key":"API_NEW"}}).to_string()).unwrap();
        activate(&paths, &home, &first).unwrap();
        assert_eq!(
            auth::read_auth(&home).unwrap().unwrap()["api"]["key"],
            "API_NEW"
        );
        write(&directory(&paths, &first).unwrap(), "other", "BAD");
        let before = fs::read(home.join("auth.json")).unwrap();
        assert!(activate(&paths, &home, &first).is_err());
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), before);
        assert!(remove(&paths, &home, "../invalid").is_err());
    }
}
