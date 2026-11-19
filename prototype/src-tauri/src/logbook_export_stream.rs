//! Chunked Logbook ADIF export: the destination is chosen first, the prepared
//! text is streamed into a private temporary file beside it in bounded pieces,
//! and only `finish` publishes it with one atomic replace. No step holds the
//! whole document, so a log larger than the single-call 32 MiB save exports.
//!
//! One export may be open at a time (it holds the shared export lease). It ends
//! by `finish`, `abort`, an error, application shutdown, or after
//! `IDLE_LIFETIME` with no append; every ending removes its temporary files and
//! leaves the destination untouched unless `finish` succeeded.
use crate::contest_analytics::{check_destination, choose_export, validate_logbook_target, ExportLease};
use serde_json::{json, Value};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Largest single append; the UI batches converted pages up to about 1 MiB.
pub const MAX_APPEND_BYTES: usize = 2 * 1024 * 1024;
/// Sanity ceiling for one export, far above any real logbook.
pub const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// ADX header (with its USERDEF declarations) or footer.
pub const MAX_ENVELOPE_BYTES: usize = 256 * 1024;
/// An export with no append for this long is abandoned and removed. The
/// longest legitimate gap is before the first append, while the worker takes
/// the logbook snapshot under the host's 300 s "begin" budget; the lifetime
/// must outlast that (1.6.67 used 300 s here and expired a 69,938-contact
/// export before its first write).
pub const IDLE_LIFETIME: Duration = Duration::from_secs(600);

struct OpenExport {
    handle: String,
    path: PathBuf,
    body: tempfile::NamedTempFile,
    written: u64,
    last_use: Instant,
    _guard: Option<Box<dyn Send>>,
}

pub struct Slot(Mutex<Option<OpenExport>>);

impl Slot {
    pub const fn new() -> Self { Self(Mutex::new(None)) }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<OpenExport>> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Opens a stream to `path` (None: the chooser was cancelled). `guard` is
    /// held for the stream's whole life and dropped when it ends.
    pub fn open(&self, path: Option<PathBuf>, extension: &str, guard: Option<Box<dyn Send>>, now: Instant) -> Result<Value, String> {
        let Some(path) = path else { return Ok(json!({"cancelled":true})); };
        check_destination(&path, extension)?;
        let parent = path.parent().filter(|parent| parent.is_dir()).ok_or("Choose an existing export folder.")?;
        let body = tempfile::Builder::new().prefix(".xlog-export-").tempfile_in(parent)
            .map_err(|_| "The export could not be started in that folder. Choose another destination.".to_string())?;
        let mut slot = self.lock();
        if slot.is_some() { return Err("An export is already open.".into()); }
        let handle = uuid::Uuid::new_v4().simple().to_string();
        *slot = Some(OpenExport { handle: handle.clone(), path, body, written: 0, last_use: now, _guard: guard });
        Ok(json!({"cancelled":false,"handle":handle}))
    }

    /// Takes the open export if `handle` names it and it has not gone idle.
    fn current<'a>(slot: &'a mut Option<OpenExport>, handle: &str, now: Instant) -> Result<&'a mut OpenExport, String> {
        let expired = slot.as_ref().is_some_and(|open| now.saturating_duration_since(open.last_use) >= IDLE_LIFETIME);
        if expired { *slot = None; }
        match slot.as_mut() {
            Some(open) if open.handle == handle => Ok(open),
            _ => Err("The export is no longer open. Start the export again.".into()),
        }
    }

    pub fn append(&self, handle: &str, text: &str, now: Instant) -> Result<Value, String> {
        if text.is_empty() || text.len() > MAX_APPEND_BYTES || text.contains('\0') {
            return Err("Each export piece must contain between 1 byte and 2 MiB of text.".into());
        }
        let mut slot = self.lock();
        let open = Self::current(&mut slot, handle, now)?;
        let total = open.written.saturating_add(text.len() as u64);
        if total > MAX_TOTAL_BYTES {
            *slot = None;
            return Err("The export exceeds the 1 GiB limit. Nothing was saved.".into());
        }
        if open.body.write_all(text.as_bytes()).is_err() {
            *slot = None;
            return Err("The export could not be written. Nothing was saved; choose another destination and retry.".into());
        }
        open.written = total; open.last_use = now;
        Ok(json!({"written":total}))
    }

    /// Publishes header + body + footer at the destination with one atomic
    /// replace. The stream ends whether or not this succeeds.
    pub fn finish(&self, handle: &str, header: &str, footer: &str, now: Instant) -> Result<Value, String> {
        let open = {
            let mut slot = self.lock();
            Self::current(&mut slot, handle, now)?;
            slot.take().expect("current export")
        };
        for part in [header, footer] {
            if part.len() > MAX_ENVELOPE_BYTES || part.contains('\0') { return Err("The export header is invalid. Nothing was saved.".into()); }
        }
        if open.written == 0 { return Err("The export is empty. Nothing was saved.".into()); }
        let OpenExport { path, mut body, written, .. } = open;
        let failed = |_| "The export could not be saved. Nothing was replaced; choose another destination and retry.".to_string();
        let output = if header.is_empty() && footer.is_empty() {
            body.flush().map_err(failed)?;
            body
        } else {
            let parent = path.parent().ok_or("Missing export folder.")?;
            let mut output = tempfile::Builder::new().prefix(".xlog-export-").tempfile_in(parent).map_err(failed)?;
            output.write_all(header.as_bytes()).map_err(failed)?;
            body.flush().map_err(failed)?;
            body.as_file_mut().seek(SeekFrom::Start(0)).map_err(failed)?;
            let copied = std::io::copy(body.as_file_mut(), &mut output).map_err(failed)?;
            if copied != written { return Err("The export could not be verified. Nothing was replaced.".into()); }
            output.write_all(footer.as_bytes()).map_err(failed)?;
            output
        };
        output.as_file().sync_all().map_err(failed)?;
        output.persist(&path).map_err(|_| "The export could not be saved. Nothing was replaced; choose another destination and retry.".to_string())?;
        Ok(json!({"saved":true,"cancelled":false,"bytes":written + (header.len() + footer.len()) as u64}))
    }

    pub fn abort(&self, handle: &str) -> Value {
        let mut slot = self.lock();
        let matches = slot.as_ref().is_some_and(|open| open.handle == handle);
        if matches { *slot = None; }
        json!({"aborted":matches})
    }

    /// Removes an export left idle; true when one was removed.
    pub fn expire_idle(&self, now: Instant) -> bool {
        let mut slot = self.lock();
        let expired = slot.as_ref().is_some_and(|open| now.saturating_duration_since(open.last_use) >= IDLE_LIFETIME);
        if expired { *slot = None; }
        expired
    }

    fn holds(&self, handle: &str) -> bool { self.lock().as_ref().is_some_and(|open| open.handle == handle) }

    pub fn clear(&self) { *self.lock() = None; }
}

static STREAM: Slot = Slot::new();

/// Application exit: remove any unfinished export's temporary files.
pub fn shutdown() { STREAM.clear(); }

fn owner(window: &tauri::WebviewWindow) -> Result<(), String> {
    if !matches!(window.label(), "main" | "logbook") { return Err("Only the integrated Logbook can export ADIF files.".into()); }
    if crate::APP_EXITING.load(Ordering::Acquire) { return Err("Logbook export is unavailable during shutdown.".into()); }
    Ok(())
}

fn watch_idle(handle: String) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(15));
        STREAM.expire_idle(Instant::now());
        if !STREAM.holds(&handle) { break; }
    });
}

#[tauri::command]
pub async fn logbook_export_open(window: tauri::WebviewWindow, filename: String) -> Result<Value, String> {
    let extension = validate_logbook_target(window.label(), &filename)?;
    owner(&window)?;
    let lease = ExportLease::acquire()?;
    let path = choose_export(window, filename, extension, "Save the ADIF copy. The integrated logbook remains unchanged.").await?;
    let opened = tauri::async_runtime::spawn_blocking(move || {
        if crate::APP_EXITING.load(Ordering::Acquire) { return Err("Logbook export is unavailable during shutdown.".to_string()); }
        STREAM.open(path, extension, Some(Box::new(lease)), Instant::now())
    }).await.map_err(|_| "Logbook export task failed.".to_string())??;
    if let Some(handle) = opened["handle"].as_str() { watch_idle(handle.to_owned()); }
    Ok(opened)
}

#[tauri::command]
pub async fn logbook_export_append(window: tauri::WebviewWindow, handle: String, text: String) -> Result<Value, String> {
    owner(&window)?;
    tauri::async_runtime::spawn_blocking(move || STREAM.append(&handle, &text, Instant::now()))
        .await.map_err(|_| "Logbook export task failed.".to_string())?
}

#[tauri::command]
pub async fn logbook_export_finish(window: tauri::WebviewWindow, handle: String, header: String, footer: String) -> Result<Value, String> {
    owner(&window)?;
    tauri::async_runtime::spawn_blocking(move || STREAM.finish(&handle, &header, &footer, Instant::now()))
        .await.map_err(|_| "Logbook export task failed.".to_string())?
}

#[tauri::command]
pub async fn logbook_export_abort(window: tauri::WebviewWindow, handle: String) -> Result<Value, String> {
    if !matches!(window.label(), "main" | "logbook") { return Err("Only the integrated Logbook can export ADIF files.".into()); }
    Ok(STREAM.abort(&handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }

    fn temp_entries(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".xlog-export-")).collect()
    }

    fn open(slot: &Slot, path: PathBuf, extension: &str, now: Instant) -> (String, Arc<AtomicBool>) {
        let released = Arc::new(AtomicBool::new(false));
        let opened = slot.open(Some(path), extension, Some(Box::new(Flag(released.clone()))), now).unwrap();
        (opened["handle"].as_str().unwrap().to_owned(), released)
    }

    #[test]
    fn streamed_adi_is_published_whole_only_at_finish_and_larger_than_the_single_call_limit() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("log.adi");
        std::fs::write(&target, "previous").unwrap();
        let slot = Slot::new();
        let now = Instant::now();
        let (handle, released) = open(&slot, target.clone(), "adi", now);
        let piece = format!("<CALL:5>K1ABC<EOR>\r\n{}", "x".repeat(MAX_APPEND_BYTES - 20));
        let mut expected = String::new();
        for _ in 0..20 { slot.append(&handle, &piece, now).unwrap(); expected.push_str(&piece); }
        assert!(expected.len() > 32 * 1024 * 1024);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "previous", "destination untouched until finish");
        assert!(!released.load(Ordering::SeqCst));
        let saved = slot.finish(&handle, "", "", now).unwrap();
        assert_eq!(saved["saved"], true);
        assert_eq!(saved["bytes"], expected.len() as u64);
        assert!(std::fs::read_to_string(&target).unwrap() == expected);
        assert!(released.load(Ordering::SeqCst), "lease released when the stream ends");
        assert!(temp_entries(root.path()).is_empty());
        assert!(slot.append(&handle, "more", now).is_err(), "a finished handle is retired");
    }

    #[test]
    fn adx_header_and_footer_wrap_the_streamed_body() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("log.adx");
        let slot = Slot::new();
        let now = Instant::now();
        let (handle, _) = open(&slot, target.clone(), "adx", now);
        slot.append(&handle, "<RECORD><CALL>K1ABC</CALL></RECORD>\r\n", now).unwrap();
        slot.append(&handle, "<RECORD><CALL>K2DEF</CALL></RECORD>\r\n", now).unwrap();
        slot.finish(&handle, "<ADX><HEADER><USERDEF FIELDID=\"1\" TYPE=\"G\">X</USERDEF></HEADER><RECORDS>\r\n", "</RECORDS></ADX>\r\n", now).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(),
            "<ADX><HEADER><USERDEF FIELDID=\"1\" TYPE=\"G\">X</USERDEF></HEADER><RECORDS>\r\n<RECORD><CALL>K1ABC</CALL></RECORD>\r\n<RECORD><CALL>K2DEF</CALL></RECORD>\r\n</RECORDS></ADX>\r\n");
        assert!(temp_entries(root.path()).is_empty());
    }

    #[test]
    fn abort_cancel_and_wrong_handles_never_touch_the_destination() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("log.adi");
        std::fs::write(&target, "previous").unwrap();
        let slot = Slot::new();
        let now = Instant::now();
        assert_eq!(slot.open(None, "adi", None, now).unwrap()["cancelled"], true);
        let (handle, released) = open(&slot, target.clone(), "adi", now);
        assert!(slot.open(Some(root.path().join("other.adi")), "adi", None, now).is_err(), "one export at a time");
        slot.append(&handle, "<CALL:5>K1ABC<EOR>\r\n", now).unwrap();
        assert!(slot.append("not-the-handle", "x", now).is_err());
        assert!(slot.finish("not-the-handle", "", "", now).is_err());
        assert_eq!(slot.abort("not-the-handle")["aborted"], false);
        assert_eq!(slot.abort(&handle)["aborted"], true);
        assert!(released.load(Ordering::SeqCst));
        assert!(temp_entries(root.path()).is_empty());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "previous");
        assert!(slot.finish(&handle, "", "", now).is_err());
    }

    #[test]
    fn bounds_and_destination_shape_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let slot = Slot::new();
        let now = Instant::now();
        assert!(slot.open(Some(root.path().join("log.adx")), "adi", None, now).is_err(), "extension must match");
        std::fs::create_dir(root.path().join("folder.adi")).unwrap();
        assert!(slot.open(Some(root.path().join("folder.adi")), "adi", None, now).is_err(), "never a folder");
        let (handle, _) = open(&slot, root.path().join("log.adi"), "adi", now);
        assert!(slot.append(&handle, "", now).is_err());
        assert!(slot.append(&handle, "a\0b", now).is_err());
        assert!(slot.append(&handle, &"x".repeat(MAX_APPEND_BYTES + 1), now).is_err());
        assert!(slot.finish(&handle, "", "", now).is_err(), "an empty export is never published");
        assert!(!root.path().join("log.adi").exists());
        assert!(temp_entries(root.path()).is_empty());
    }

    #[test]
    fn the_idle_lifetime_outlasts_the_worker_snapshot_budget_before_the_first_write() {
        // The worker's export "begin" may take up to 300 s; the stream must survive it.
        assert!(IDLE_LIFETIME > Duration::from_secs(300));
        let root = tempfile::tempdir().unwrap();
        let slot = Slot::new();
        let start = Instant::now();
        let (handle, _) = open(&slot, root.path().join("log.adi"), "adi", start);
        let after_snapshot = start + Duration::from_secs(330);
        assert!(!slot.expire_idle(after_snapshot));
        slot.append(&handle, "<CALL:5>K1ABC<EOR>
", after_snapshot).unwrap();
        assert_eq!(slot.finish(&handle, "", "", after_snapshot).unwrap()["saved"], true);
    }

    #[test]
    fn an_idle_export_expires_and_an_active_one_does_not() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("log.adi");
        let slot = Slot::new();
        let start = Instant::now();
        let (handle, released) = open(&slot, target.clone(), "adi", start);
        let mut now = start;
        for _ in 0..4 {
            now += Duration::from_secs(200);
            slot.append(&handle, "<CALL:5>K1ABC<EOR>\r\n", now).unwrap();
        }
        assert!(!slot.expire_idle(now + Duration::from_secs(599)));
        assert!(slot.expire_idle(now + IDLE_LIFETIME));
        assert!(released.load(Ordering::SeqCst));
        assert!(slot.append(&handle, "x", now + IDLE_LIFETIME).is_err());
        assert!(!target.exists());
        assert!(temp_entries(root.path()).is_empty());
    }
}
