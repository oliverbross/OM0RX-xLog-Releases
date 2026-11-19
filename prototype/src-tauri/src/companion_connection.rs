//! Automatic same-user discovery for the standalone xNexus application.
//! Credentials stay in the user's private directory and never enter a WebView.
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
static FILE_IO: Mutex<()> = Mutex::new(());

pub fn directory() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        .ok_or("Windows local application data is unavailable")?;
    #[cfg(not(windows))]
    let base = std::env::temp_dir();
    Ok(base.join("OM0RX").join("xLog").join("companion"))
}

pub fn publish(value: &Value) -> Result<PathBuf, String> {
    // Disposable acceptance must never replace production discovery metadata.
    let dir = if let Some(root) = crate::runtime_profile::offline_root() {
        root.join("native").join("companion")
    } else { directory()? };
    publish_in(&dir, value)
}

fn publish_in(dir: &std::path::Path, value: &Value) -> Result<PathBuf, String> {
    use std::io::Write;
    let _io = FILE_IO.lock().map_err(|_| "Connection metadata is unavailable")?;
    std::fs::create_dir_all(dir.parent().ok_or("Companion directory is unavailable")?)
        .map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        extern "C" {
            fn xlog_create_private_windows_directory(path: *const u16) -> i32;
            fn xlog_private_windows_path(path: *const u16, directory: i32) -> i32;
        }
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        if !dir.exists() && unsafe { xlog_create_private_windows_directory(wide.as_ptr()) } != 1 {
            return Err("Could not create the private xLog connection directory".into());
        }
        if unsafe { xlog_private_windows_path(wide.as_ptr(), 1) } != 1 {
            return Err("The xLog connection directory is not private to this Windows user".into());
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.exists() { std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| e.to_string())?; }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    let path = dir.join("connection.json");
    file.persist(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// A retiring instance must not delete a newer connection descriptor.
pub fn remove_matching(path: &std::path::Path, endpoint: &str, token: &str) {
    let Ok(_io) = FILE_IO.lock() else { return; };
    let current = std::fs::read(path).ok().and_then(|v| serde_json::from_slice::<Value>(&v).ok());
    if current.is_some_and(|v| v["endpoint"] == endpoint && v["token"] == token) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_republication_and_old_shutdown_preserve_latest_connection() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("companion");
        let old = serde_json::json!({"endpoint":"http://127.0.0.1:1234", "token":"old"});
        let new = serde_json::json!({"endpoint":"http://127.0.0.1:1235", "token":"new"});
        let path = publish_in(&dir, &old).unwrap();
        assert_eq!(publish_in(&dir, &new).unwrap(), path);
        remove_matching(&path, "http://127.0.0.1:1234", "old");
        assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(), new);
        remove_matching(&path, "http://127.0.0.1:1235", "new");
        assert!(!path.exists());
    }
}
