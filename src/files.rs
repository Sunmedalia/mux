//! Local filesystem operations. Paths stay native; display strings are never used as identifiers.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub path: PathBuf,
    pub directory: bool,
    pub link: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

pub(crate) fn list(path: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(path).with_context(|| format!("Read {}", path.display()))? {
        let item = item?;
        let metadata = match fs::symlink_metadata(item.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        entries.push(Entry {
            directory: item.path().is_dir(),
            link: metadata.is_symlink(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
            path: item.path(),
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
    });
    Ok(entries)
}

#[derive(Clone, Debug)]
pub(crate) enum Operation {
    File(PathBuf),
    Directory(PathBuf),
    Rename(PathBuf, PathBuf),
    Copy(PathBuf, PathBuf),
    Move(PathBuf, PathBuf),
    Delete(PathBuf),
}

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("Target already exists: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn check_destination(source: &Path, target: &Path) -> Result<()> {
    absent(target)?;
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        let root = fs::canonicalize(source)?;
        let parent = fs::canonicalize(
            target
                .parent()
                .context("Destination needs a parent directory")?,
        )?;
        ensure!(
            !parent.starts_with(&root),
            "Cannot place a directory inside itself"
        );
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(source)?, target)?;
        #[cfg(windows)]
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(fs::read_link(source)?, target)?;
        } else {
            std::os::windows::fs::symlink_file(fs::read_link(source)?, target)?;
        }
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        let result = (|| -> Result<()> {
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                copy(&entry.path(), &target.join(entry.file_name()))?;
            }
            fs::set_permissions(target, metadata.permissions())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(target);
        }
        result?;
    } else if metadata.is_file() {
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let result = std::io::copy(&mut input, &mut output)
            .and_then(|_| output.set_permissions(metadata.permissions()));
        if result.is_err() {
            let _ = fs::remove_file(target);
        }
        result?;
    } else {
        bail!("Unsupported file type: {}", source.display());
    }
    Ok(())
}

fn delete(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn execute(operation: &Operation) -> Result<()> {
    match operation {
        Operation::File(path) => {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
        }
        Operation::Directory(path) => fs::create_dir(path)?,
        Operation::Copy(source, target) => {
            check_destination(source, target)?;
            copy(source, target)?;
        }
        Operation::Rename(source, target) | Operation::Move(source, target) => {
            check_destination(source, target)?;
            // Use an atomic no-replace rename, including when a target appears concurrently.
            rename_noreplace(source, target)?;
        }
        Operation::Delete(path) => delete(path)?,
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source_c = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    let result =
        unsafe { libc::renamex_np(source_c.as_ptr(), target_c.as_ptr(), libc::RENAME_EXCL) };
    finish_rename(source, target, result)
}
#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source_c = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let target_c = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            target_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    finish_rename(source, target, result)
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn finish_rename(source: &Path, target: &Path, result: i32) -> Result<()> {
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EXDEV) {
        copy(source, target)?;
        delete(source).context("Copied destination, but could not remove source")?;
        Ok(())
    } else {
        Err(error.into())
    }
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    absent(target)?;
    fs::rename(source, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_and_no_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let source = root.join("中文 ' $() file");
        execute(&Operation::File(source.clone())).unwrap();
        fs::write(&source, b"content").unwrap();
        let target = root.join("copy");
        execute(&Operation::Copy(source.clone(), target.clone())).unwrap();
        assert!(execute(&Operation::Copy(source.clone(), target.clone())).is_err());
        assert!(execute(&Operation::Rename(source.clone(), target.clone())).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"content");
        let moved = root.join("moved");
        execute(&Operation::Move(target.clone(), moved.clone())).unwrap();
        assert!(!target.exists());
        execute(&Operation::Delete(moved.clone())).unwrap();
        assert!(!moved.exists());
        let renamed = root.join("renamed");
        execute(&Operation::Rename(source, renamed.clone())).unwrap();
        assert_eq!(fs::read(renamed).unwrap(), b"content");
    }
    #[test]
    fn directories_and_self_copy() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("dir");
        execute(&Operation::Directory(source.clone())).unwrap();
        fs::write(source.join("file"), b"nested").unwrap();
        let target = temp.path().join("copy");
        execute(&Operation::Copy(source.clone(), target.clone())).unwrap();
        assert_eq!(fs::read(target.join("file")).unwrap(), b"nested");
        assert!(execute(&Operation::Copy(source.clone(), source.join("child"))).is_err());
        assert!(execute(&Operation::Move(source.clone(), source.join("child"))).is_err());
        execute(&Operation::Delete(target.clone())).unwrap();
        assert!(!target.exists());
        let entries = list(temp.path()).unwrap();
        assert!(entries[0].directory);
        assert!(list(&temp.path().join("missing")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn links_are_preserved_and_never_recursed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("dir")).unwrap();
        fs::write(root.join("dir/file"), b"keep").unwrap();
        std::os::unix::fs::symlink(root.join("dir"), root.join("link")).unwrap();
        execute(&Operation::Copy(root.join("link"), root.join("copy"))).unwrap();
        assert!(
            fs::symlink_metadata(root.join("copy"))
                .unwrap()
                .is_symlink()
        );
        execute(&Operation::Delete(root.join("link"))).unwrap();
        assert!(root.join("dir/file").exists());
        std::os::unix::fs::symlink("missing", root.join("broken")).unwrap();
        assert!(execute(&Operation::File(root.join("broken"))).is_err());
        assert!(execute(&Operation::Copy(root.join("copy"), root.join("broken"))).is_err());
        execute(&Operation::Delete(root.join("broken"))).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn permission_errors_and_missing_sources_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        assert!(execute(&Operation::Delete(temp.path().join("missing"))).is_err());
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let blocked = temp.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let listing = list(&blocked);
        let creation = execute(&Operation::File(blocked.join("file")));
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            listing
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            creation
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
