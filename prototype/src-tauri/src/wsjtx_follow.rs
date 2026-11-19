//! Follows the station a WSJT-X-compatible engine (Nexus, WSJT-X) is working.
//!
//! The engine's Status message names the station it is calling or answering.
//! Once a second this owner reads the WSJT-X listener's passive snapshot; when
//! the worked station changes, its callsign (and grid, when sent) becomes the
//! Active QSO call, and the automatic rotor is asked to follow. It fills an
//! empty call, or replaces only a call it filled itself: a callsign the
//! operator entered is never replaced. When the engine logs that contact, the
//! draft this owner filled is cleared. It never tunes the radio or keys PTT.
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

const POLL: Duration = Duration::from_secs(1);
const DRAFT_LABELS: [&str; 5] = ["main", "log-qso", "callsign-intelligence", "previous-qsos", "monitor"];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Follow { call: String, grid: String, replacing: String },
    Clear { call: String },
}

/// The pure decision core: what to do with one listener snapshot.
#[derive(Default)]
pub(crate) struct Follower {
    last_seen: String,
    followed: Option<(String, u64)>,
}

impl Follower {
    pub(crate) fn decide(&mut self, snapshot: &Value) -> Action {
        // The operator can switch the follow off in the WSJT-X listener settings.
        if snapshot["running"] != true || snapshot["configuration"]["followActiveQSO"] == false {
            self.last_seen.clear(); return Action::None;
        }
        if let (Some((call, at)), Some(logged)) = (&self.followed, snapshot["last_logged"].as_object()) {
            if logged.get("call").and_then(Value::as_str) == Some(call.as_str())
                && logged.get("at_ms").and_then(Value::as_u64).is_some_and(|logged_at| logged_at >= *at) {
                return Action::Clear { call: call.clone() };
            }
        }
        let working = &snapshot["activity"]["working"];
        let call = working["call"].as_str().unwrap_or("").to_owned();
        if call.is_empty() { self.last_seen.clear(); return Action::None; }
        if call == self.last_seen { return Action::None; }
        self.last_seen = call.clone();
        let replacing = self.followed.as_ref().map(|(call, _)| call.clone()).unwrap_or_default();
        if replacing == call { return Action::None; }
        Action::Follow { call, grid: working["grid"].as_str().unwrap_or("").to_owned(), replacing }
    }

    pub(crate) fn followed(&mut self, call: &str, at: u64) { self.followed = Some((call.to_owned(), at)); }
    pub(crate) fn cleared(&mut self) { self.followed = None; }
}

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|t| t.as_millis() as u64).unwrap_or(0) }

fn call(worker: u64, method: &str, params: Value) -> Result<Value, String> {
    let request = json!({"id": format!("native-wsjtx-follow-{}", uuid::Uuid::new_v4()), "method": method, "params": params});
    let response = crate::worker::request_existing_generation(request.clone(), worker)?;
    if response["id"] != request["id"] || response["ok"] != true {
        return Err(response["error"]["message"].as_str().unwrap_or("WSJT-X follow request failed").to_owned());
    }
    Ok(response["result"].clone())
}

/// (generation, draft id, revision, current call) of the shared draft.
fn draft(worker: u64) -> Result<(String, String, u64, String), String> {
    let draft = call(worker, "logbook_operator_draft", json!({}))?["draft"].clone();
    let generation = draft["generation"].as_str().ok_or("Draft generation unavailable")?.to_owned();
    let id = draft["qso"]["id"].as_str().ok_or("Draft identity unavailable")?.to_owned();
    let revision = draft["revision"].as_u64().ok_or("Draft revision unavailable")?;
    let current = draft["qso"]["call"].as_str().unwrap_or("").trim().to_ascii_uppercase();
    Ok((generation, id, revision, current))
}

fn announce(app: &tauri::AppHandle, rotor: bool) {
    for label in DRAFT_LABELS { let _ = app.emit_to(label, "xlog:operator-draft-changed", ()); }
    if rotor { crate::auto_rotor_reconcile::request(app); }
}

fn execute(app: &tauri::AppHandle, follower: &mut Follower, action: Action) -> Result<(), String> {
    let worker = crate::worker::existing_generation()?;
    match action {
        Action::None => Ok(()),
        Action::Follow { call: callsign, grid, replacing } => {
            let (generation, id, revision, _) = draft(worker)?;
            let mut params = json!({"operationID": uuid::Uuid::new_v4().to_string(), "draftID": id,
                "expectedRevision": revision, "generation": generation, "callsign": callsign});
            if !grid.is_empty() { params["grid"] = json!(grid); }
            if !replacing.is_empty() { params["replacing"] = json!(replacing); }
            match call(worker, "logbook_operator_draft_follow_call", params) {
                Ok(result) if result["status"] == "applied" => {
                    follower.followed(&callsign, now_ms());
                    announce(app, true);
                    Ok(())
                }
                // A callsign the operator entered, or a draft that changed
                // meanwhile, wins; the next station change tries again.
                Ok(_) => Ok(()),
                Err(error) if error.contains("callsign is retained") => Ok(()),
                Err(error) => Err(error),
            }
        }
        Action::Clear { call: callsign } => {
            let (generation, id, revision, current) = draft(worker)?;
            follower.cleared();
            if current != callsign { return Ok(()); }
            let result = call(worker, "logbook_operator_draft_apply", json!({"operationID": uuid::Uuid::new_v4().to_string(),
                "draftID": id, "expectedRevision": revision, "action": "clear", "generation": generation}))?;
            if result["status"] == "applied" { announce(app, false); }
            Ok(())
        }
    }
}

pub fn install(app: tauri::AppHandle) {
    std::thread::Builder::new().name("xlog-wsjtx-follow".into()).spawn(move || {
        let mut follower = Follower::default();
        while !crate::APP_EXITING.load(Ordering::Acquire) {
            std::thread::sleep(POLL);
            if crate::worker::generation() == 0 { continue; }
            let request = json!({"id": format!("native-wsjtx-follow-{}", uuid::Uuid::new_v4()), "method": "wsjtx_snapshot", "params": {}});
            let Ok(response) = crate::worker::request_existing(request) else { continue; };
            if response["ok"] != true { continue; }
            let action = follower.decide(&response["result"]);
            if let Err(error) = execute(&app, &mut follower, action) {
                eprintln!("WSJT-X follow unavailable: {error}");
            }
        }
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(call: &str, logged: Option<(&str, u64)>) -> Value {
        json!({"running": true,
            "activity": {"working": if call.is_empty() { Value::Null } else { json!({"call": call, "grid": "KN95"}) }},
            "last_logged": logged.map(|(call, at)| json!({"call": call, "at_ms": at})).unwrap_or(Value::Null)})
    }

    #[test]
    fn a_new_worked_station_is_followed_once_and_a_repeat_is_ignored() {
        let mut f = Follower::default();
        assert_eq!(f.decide(&snapshot("SV1DKR", None)),
            Action::Follow { call: "SV1DKR".into(), grid: "KN95".into(), replacing: "".into() });
        f.followed("SV1DKR", 1_000);
        assert_eq!(f.decide(&snapshot("SV1DKR", None)), Action::None, "the same station is not re-applied every second");
        assert_eq!(f.decide(&snapshot("AD2NU", None)),
            Action::Follow { call: "AD2NU".into(), grid: "KN95".into(), replacing: "SV1DKR".into() },
            "a new station replaces only the call this owner filled");
    }

    #[test]
    fn the_logged_contact_clears_the_draft_this_owner_filled() {
        let mut f = Follower::default();
        let _ = f.decide(&snapshot("SV1DKR", None));
        f.followed("SV1DKR", 5_000);
        assert_eq!(f.decide(&snapshot("SV1DKR", Some(("SV1DKR", 4_000)))), Action::None, "an older log of the same call is not this contact");
        assert_eq!(f.decide(&snapshot("SV1DKR", Some(("SV1DKR", 6_000)))), Action::Clear { call: "SV1DKR".into() });
        f.cleared();
        assert_eq!(f.decide(&snapshot("SV1DKR", Some(("SV1DKR", 6_000)))), Action::None);
    }

    #[test]
    fn the_operator_can_switch_the_follow_off() {
        let mut f = Follower::default();
        let mut off = snapshot("SV1DKR", None);
        off["configuration"] = json!({"followActiveQSO": false});
        assert_eq!(f.decide(&off), Action::None);
        off["configuration"]["followActiveQSO"] = json!(true);
        assert!(matches!(f.decide(&off), Action::Follow { .. }));
    }

    #[test]
    fn nothing_happens_while_the_listener_is_off_or_no_one_is_worked() {
        let mut f = Follower::default();
        assert_eq!(f.decide(&json!({"running": false})), Action::None);
        assert_eq!(f.decide(&snapshot("", None)), Action::None);
        // After a gap with no worked station, the same call is followed again.
        assert!(matches!(f.decide(&snapshot("SV1DKR", None)), Action::Follow { .. }));
        assert_eq!(f.decide(&snapshot("", None)), Action::None);
        assert!(matches!(f.decide(&snapshot("SV1DKR", None)), Action::Follow { .. }));
    }
}
