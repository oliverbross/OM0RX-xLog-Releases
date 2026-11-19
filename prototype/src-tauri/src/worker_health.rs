//! Worker health the operator can see without a terminal: every worker restart
//! with its cause, and an opt-in diagnostics log.
//!
//! Restarts are always recorded (in memory, the last 50). Diagnostics are off
//! by default; the switch in Settings writes a flag file so the choice survives
//! restarts, and the log rotates at `MAX_LOG_BYTES` (two files at most).
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::Manager;

const MAX_RESTARTS: usize = 50;
const MAX_LOG_BYTES: u64 = 20 * 1024 * 1024;

static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static RESTARTS: Mutex<VecDeque<Value>> = Mutex::new(VecDeque::new());
static LOG: Mutex<()> = Mutex::new(());

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_millis() as u64).unwrap_or(0)
}

pub fn install(app: &tauri::AppHandle) {
    if let Ok(directory) = app.path().app_data_dir().map(|path| path.join("diagnostics")) {
        let _ = std::fs::create_dir_all(&directory);
        let flag = directory.join("enabled");
        let _ = DIRECTORY.set(directory);
        ENABLED.store(flag.exists() || std::env::var("XLOG_WORKER_DIAGNOSTICS").as_deref() == Ok("1"), Ordering::Release);
    } else if std::env::var("XLOG_WORKER_DIAGNOSTICS").as_deref() == Ok("1") {
        ENABLED.store(true, Ordering::Release);
    }
}

pub fn diagnostics_enabled() -> bool { ENABLED.load(Ordering::Acquire) }

fn log_path() -> Option<PathBuf> { DIRECTORY.get().map(|directory| directory.join("worker.log")) }

/// Append one diagnostic line, rotating the file once it passes the cap.
pub fn log_line(line: &str) {
    let Some(path) = log_path() else { return; };
    let Ok(_guard) = LOG.lock() else { return; };
    if std::fs::metadata(&path).map(|meta| meta.len() >= MAX_LOG_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Record a worker restart and the request that triggered it.
pub fn record_restart(reason: &str, method: Option<&str>, phase: Option<&str>) {
    if let Ok(mut restarts) = RESTARTS.lock() {
        restarts.push_back(json!({"at_ms": now_ms(), "reason": reason, "method": method, "phase": phase}));
        while restarts.len() > MAX_RESTARTS { restarts.pop_front(); }
    }
    if diagnostics_enabled() {
        log_line(&json!({"event": "restart_recorded", "at_ms": now_ms(), "reason": reason, "method": method}).to_string());
    }
}

fn snapshot() -> Value {
    let restarts: Vec<Value> = RESTARTS.lock().map(|restarts| restarts.iter().cloned().collect()).unwrap_or_default();
    json!({"restarts": restarts, "diagnostics_enabled": diagnostics_enabled(),
        "log_path": log_path().map(|path| path.display().to_string())})
}

fn owner(window: &tauri::WebviewWindow) -> Result<(), String> {
    if matches!(window.label(), "main" | "settings") { Ok(()) } else { Err("Service health belongs to Settings.".into()) }
}

#[tauri::command]
pub fn worker_health(window: tauri::WebviewWindow) -> Result<Value, String> {
    owner(&window)?;
    Ok(snapshot())
}

#[tauri::command]
pub fn worker_diagnostics_set(window: tauri::WebviewWindow, enabled: bool) -> Result<Value, String> {
    owner(&window)?;
    let directory = DIRECTORY.get().ok_or("The diagnostics folder is unavailable.")?;
    let flag = directory.join("enabled");
    if enabled { std::fs::write(&flag, b"1").map_err(|_| "Diagnostics could not be switched on.")?; }
    else if flag.exists() { std::fs::remove_file(&flag).map_err(|_| "Diagnostics could not be switched off.")?; }
    ENABLED.store(enabled, Ordering::Release);
    Ok(snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_are_kept_with_their_cause_and_bounded() {
        for index in 0..(MAX_RESTARTS + 5) {
            record_restart("Station service request failed; control outcome unknown", Some("logbook_catalog"), Some(if index % 2 == 0 { "reply_timeout" } else { "reply_failure" }));
        }
        let value = snapshot();
        let restarts = value["restarts"].as_array().unwrap();
        assert_eq!(restarts.len(), MAX_RESTARTS);
        assert_eq!(restarts.last().unwrap()["method"], "logbook_catalog");
        assert!(restarts.last().unwrap()["at_ms"].as_u64().unwrap() > 0);
    }
}
