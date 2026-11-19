use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
#[derive(Default)]
pub struct NeedsFence {
    operations: std::collections::HashMap<String, (u64, bool, bool, Option<u64>)>,
    overflow: bool,
    workspace: Option<String>,
    stamp: u64,
}
impl NeedsFence {
    fn advance(&mut self) {
        if let Some(next) = self.stamp.checked_add(1) { self.stamp = next; }
        else { self.overflow = true; }
    }
    pub fn stamp(&self) -> u64 { self.stamp }
    pub fn uncertain(&self) -> bool { self.overflow || self.operations.values().any(|v| v.2) }
    pub fn bind_workspace(&mut self, workspace: &str) {
        if self.workspace.as_deref() != Some(workspace) { self.advance(); }
        if self
            .workspace
            .as_deref()
            .is_some_and(|previous| previous != workspace)
        {
            self.operations.clear();
            self.overflow = false;
        }
        self.workspace = Some(workspace.into());
    }
    pub fn begin(&mut self, key: String, classification_revision: u64) {
        self.advance();
        if self.operations.len() >= 4096 && !self.operations.contains_key(&key) {
            self.overflow = true;
            return;
        }
        self.operations
            .insert(key, (classification_revision, true, false, None));
    }
    pub fn complete(&mut self, key: &str, response: &Result<Value, String>) {
        self.advance();
        let Some(operation) = self.operations.get_mut(key) else {
            return;
        };
        // These owning handlers return the listed errors only before their
        // atomic database mutation. Do not generalize this to arbitrary errors
        // such as a failed saved-record lookup after a successful commit.
        let proven_no_effect = response
            .as_ref()
            .ok()
            .filter(|r| r["ok"] == false)
            .is_some_and(|r| {
                let code = r["error"]["code"].as_str();
                (key.starts_with("request:logbook_contact_apply:")
                    && matches!(code, Some("invalid_params" | "contact_operation_reused")))
                    || (key.starts_with("request:logbook_import_adif:")
                        && matches!(
                            code,
                            Some("invalid_params" | "preview_expired" | "workspace_changed")
                        ))
                    || (key.starts_with("qso:") && code == Some("invalid_params"))
            });
        let result = response
            .as_ref()
            .ok()
            .filter(|r| r["ok"] == true)
            .map(|r| &r["result"]);
        if proven_no_effect
            || result.is_some_and(|r| {
                matches!(r["status"].as_str(), Some("rejected" | "workspace_changed"))
                    || (key.starts_with("request:logbook_contact_apply:")
                        && r["status"] == "conflict")
            })
        {
            self.operations.remove(key);
            return;
        }
        operation.1 = false;
        operation.2 = result.is_none_or(|r| r["status"] == "uncertain");
        operation.3 = result.and_then(|r| r["logbook_revision"].as_u64());
    }
    pub fn invalid(
        &mut self,
        classification_revision: Option<u64>,
        logbook_revision: Option<u64>,
    ) -> bool {
        self.operations
            .retain(|_, (baseline, pending, uncertain, target)| {
                *pending
                    || *uncertain
                    || match target {
                        Some(target) => logbook_revision.is_none_or(|revision| revision < *target),
                        None => {
                            classification_revision.is_none_or(|revision| revision <= *baseline)
                        }
                    }
            });
        self.overflow || !self.operations.is_empty()
    }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis() as u64)
        .unwrap_or(0)
}
pub fn fresh(value: &Value, age: u64) -> bool {
    value.as_u64().is_some_and(|time| {
        time <= now().saturating_add(1_000) && now().saturating_sub(time) <= age
    })
}
pub fn invalidates_needs(request: &Value) -> bool {
    let method = request["method"].as_str().unwrap_or("");
    if method == "sync_mutation" {
        return matches!(
            request["params"]["method"].as_str(),
            Some("sync_run" | "sync_resolve")
        );
    }
    if method == "logbook_operator_draft_apply" {
        return request["params"]["action"] == "save";
    }
    if method == "logbook_import_adif"
        && matches!(
            request["params"]["action"].as_str(),
            Some("preview" | "retire")
        )
    {
        return false;
    }
    method.starts_with("logbook_")
        && (method.contains("apply")
            || method.contains("save")
            || method.contains("import")
            || method.contains("delete")
            || method.ends_with("_undo"))
        || matches!(
            method,
            "contest_log_transfer_commit" | "fldigi_accept_completed"
        )
        || matches!(method, "sync_run" | "sync_resolve")
        || method.starts_with("wsjtx_") && method.contains("import")
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite())
}
fn bounded(value: &Value, limit: usize) -> String {
    let mut result = String::new();
    for c in value
        .as_str()
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
    {
        if result.len() + c.len_utf8() > limit {
            break;
        }
        result.push(c);
    }
    result
}
fn optional(value: &Value, limit: usize) -> Value {
    let text = bounded(value, limit);
    if text.is_empty() {
        Value::Null
    } else {
        json!(text)
    }
}

pub fn project(e: &Value) -> Value {
    let p = &e["radio"];
    let s = &p["snapshot"];
    let r = &s["radio"];
    let radio_fresh = p["available"] == true
        && fresh(&p["received_at_ms"], 5_000)
        && s["station"]["radio_stale"] != true;
    let mut station = json!({"connected":false});
    if p["radio_id"].is_string() {
        station["radio_id"] = p["radio_id"].clone();
    }
    if radio_fresh
        && r["connected"] == true
        && r["observed"] == true
        && r["frequency_hz"]
            .as_u64()
            .is_some_and(|v| (1..=1_000_000_000_000).contains(&v))
    {
        station = json!({"radio_id":p["radio_id"],"connected":true,"frequency_hz":r["frequency_hz"],"mode":optional(&r["mode"],32),"transmitting":r["transmitting"]});
    } else if radio_fresh
        && p["provider"] == "hamlib"
        && fresh(&s["observed_at_ms"], 5_000)
        && fresh(&s["field_observed_at_ms"]["frequency"], 5_000)
        && fresh(&s["field_observed_at_ms"]["mode"], 5_000)
    {
        let hz = number(&s["fields"]["frequency"]["Frequency"]);
        if let Some(hz) = hz.filter(|hz| (1.0..=1_000_000_000_000.0).contains(hz)) {
            let transmitting = if fresh(&s["field_observed_at_ms"]["ptt"], 1_000) {
                match s["fields"]["ptt"]["PTT"].as_str() {
                    Some("0") => json!(false),
                    Some("1" | "2" | "3") => json!(true),
                    _ => Value::Null,
                }
            } else {
                Value::Null
            };
            station = json!({"radio_id":p["radio_id"],"connected":true,"frequency_hz":hz as u64,"mode":optional(&s["fields"]["mode"]["Mode"],32),"transmitting":transmitting});
        }
    }
    let p = &e["rotor"];
    let s = &p["snapshot"];
    let owner = s["owner"].as_str();
    let mut rotor = json!({"connected":false});
    if owner.is_some_and(|v| matches!(v, "native_dcu1" | "hamlib")) {
        if let Some(route) = s["companion_route_id"].as_str().filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)) {
            rotor["rotor_id"] = json!(route);
        }
    }
    let azimuth = number(&s["backend"]["azimuth"])
        .or_else(|| number(&s["backend"]["fields"]["position"]["Azimuth"]));
    if p["available"] == true
        && rotor["rotor_id"].is_string()
        && fresh(&p["received_at_ms"], 5_000)
        && s["state"] == "active"
        && s["backend_available"] != false
        && (owner != Some("native_dcu1") || s["backend"]["connected"] == true)
    {
        if let Some(azimuth) = azimuth.filter(|v| (0.0..=360.0).contains(v)) {
            rotor["connected"] = json!(true);
            rotor["azimuth_deg"] = json!(azimuth % 360.0);
        }
    }
    let cluster = &e["cluster"];
    let data = &cluster["response"]["snapshot"];
    let needs_ok = cluster["available"] == true
        && fresh(&cluster["received_at_ms"], 60_000)
        && data["logbookDataLoaded"] == true
        && cluster["response"]["contact_refresh_state"] == "current"
        && e["needs_invalid"] == false;
    let unavailable_reason = if e["needs_invalid"] == true {
        "xLog logbook changed; waiting for reclassification.".to_owned()
    } else if cluster["available"] != true {
        "xLog cluster owner is unavailable.".to_owned()
    } else if !fresh(&cluster["received_at_ms"], 60_000) {
        "The xLog cluster observation is stale.".to_owned()
    } else if data["logbookDataLoaded"] != true {
        "xLog log history is not loaded.".to_owned()
    } else {
        format!(
            "xLog classification {}: {}",
            bounded(&cluster["response"]["contact_refresh_state"], 32),
            bounded(&cluster["response"]["contact_refresh_error"], 128)
        )
    };
    let mut seen = std::collections::HashSet::new();
    let spots:Vec<Value>=data["spots"].as_array().into_iter().flatten().take(1024).filter_map(|s| {
        let hz=s["frequencyKHz"].as_f64().filter(|v|v.is_finite() && *v>=0.001 && *v<=1_000_000_000.0)?*1_000.0;
        let call=s["callsign"].as_str()?; if call.is_empty()||call.len()>32||!call.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'/'||b==b'-'){return None;}
        let id=bounded(&s["id"],128);let mode=bounded(&s["mode"],32);let source=bounded(&s["source"],256);
        if id.is_empty()||mode.is_empty()||source.is_empty()||!seen.insert(id.clone()){return None;}
        let call_text=bounded(&s["callStatus"],64);let dxcc_text=bounded(&s["dxccStatus"],64);let call_status=call_text.as_str();let dxcc=dxcc_text.as_str();
        let needed=matches!(call_status,"New Call"|"New Band"|"New Mode"|"New Band+Mode") || matches!(dxcc,"ATNO"|"C/New Band"|"C/New Mode"|"C/New Band+Mode"|"W/New Band"|"W/New Mode"|"W/New Band+Mode");
        let status=if !needs_ok {"unknown"} else if needed {"needed"} else if matches!(call_status,"Worked"|"Confirmed") {"worked"} else {"unknown"};
        let reasons:Vec<String>=if needs_ok { s["scoreReasons"].as_array().into_iter().flatten().take(30).map(|v|bounded(v,256)).chain([call_status.to_owned(),dxcc.to_owned()]).filter(|v|!v.is_empty()).collect() } else {vec![unavailable_reason.clone()]};
        // Swift JSONEncoder's default Date epoch is 2001-01-01.
        let timestamp=s["receivedAt"].as_f64().filter(|v|v.is_finite()).map(|v|((v+978_307_200.0)*1_000.0).max(0.0) as u64).unwrap_or(0);
        if timestamp>now().saturating_add(2_000){return None;}
        Some(json!({"id":id,"callsign":call,"frequency_hz":hz.round() as u64,"mode":mode,"observed_at_unix_ms":timestamp,"source":source,"country":optional(&s["country"],128),"adif":s["adif"].as_u64(),"latitude":number(&s["latitude"]).filter(|v|(-90.0..=90.0).contains(v)),"longitude":number(&s["longitude"]).filter(|v|(-180.0..=180.0).contains(v)),"band":optional(&s["band"],32),"call_status":optional(&s["callStatus"],64),"dxcc_status":optional(&s["dxccStatus"],64),"need":{"status":status,"score":if needs_ok{s["score"].as_i64().unwrap_or(0).clamp(i32::MIN as i64,i32::MAX as i64)}else{0},"reasons":reasons}}))
    }).collect();
    json!({"generated_at_unix_ms":now(),"max_age_ms":3_000,"station":station,"rotor":rotor,"spots":spots})
}

#[cfg(test)]
mod tests {
    #[test]
    fn mutation_fences_recover_rejections_without_releasing_other_operations() {
        let mut fence = super::NeedsFence::default();
        fence.begin("pending".into(), 4);
        fence.bind_workspace("workspace-a");
        fence.bind_workspace("workspace-a");
        fence.begin("rejected".into(), 4);
        fence.complete(
            "rejected",
            &Ok(serde_json::json!({"ok":true,"result":{"status":"rejected"}})),
        );
        assert!(
            fence.invalid(Some(99), Some(99)),
            "pending cannot be cleared by another publication"
        );
        fence.complete(
            "pending",
            &Ok(
                serde_json::json!({"ok":true,"result":{"status":"applied","logbook_revision":101}}),
            ),
        );
        assert!(fence.invalid(Some(100), Some(100)));
        assert!(!fence.invalid(Some(100), Some(101)));
        fence.begin("noop".into(), 100);
        fence.complete(
            "noop",
            &Ok(serde_json::json!({"ok":true,"result":{"status":"rejected"}})),
        );
        assert!(
            !fence.invalid(Some(100), Some(101)),
            "rejection needs no new publication"
        );
        fence.begin("lost-reply".into(), 100);
        fence.complete("lost-reply", &Err("lost response".into()));
        assert!(
            fence.invalid(Some(200), Some(200)),
            "uncertain write remains fenced"
        );
        fence.complete(
            "lost-reply",
            &Ok(serde_json::json!({"ok":true,"result":{"receipt":{},"logbook_revision":201}})),
        );
        assert!(fence.invalid(Some(200), Some(200)));
        assert!(
            !fence.invalid(Some(201), Some(201)),
            "durable receipt and actual owner publication resolve uncertainty"
        );
        fence.begin("old-workspace".into(), 201);
        fence.bind_workspace("workspace-b");
        assert!(!fence.invalid(None, None));
    }
    #[test]
    fn native_edit_and_import_validation_errors_release_only_proven_no_effect_fences() {
        for (key, code) in [
            ("request:logbook_contact_apply:a", "invalid_params"),
            (
                "request:logbook_contact_apply:b",
                "contact_operation_reused",
            ),
            ("request:logbook_import_adif:c", "preview_expired"),
            ("request:logbook_import_adif:d", "invalid_params"),
            ("qso:e", "invalid_params"),
        ] {
            let mut fence = super::NeedsFence::default();
            fence.begin(key.into(), 10);
            fence.complete(
                key,
                &Ok(serde_json::json!({"ok":false,"error":{"code":code}})),
            );
            assert!(!fence.invalid(Some(10), None), "{key} {code}");
        }
        let key = "request:logbook_contact_apply:a";
        let mut fence = super::NeedsFence::default();
        fence.begin(key.into(), 10);
        fence.complete(
            key,
            &Ok(serde_json::json!({"ok":true,"result":{"status":"conflict"}})),
        );
        assert!(!fence.invalid(Some(10), None));
        fence.begin(key.into(), 10);
        fence.complete(
            key,
            &Ok(serde_json::json!({"ok":false,"error":{"code":"database_failure"}})),
        );
        assert!(
            fence.invalid(Some(99), Some(99)),
            "unknown post-dispatch errors remain uncertain"
        );
    }
    #[test]
    fn hamlib_ptt_is_unknown_without_a_fresh_exact_observation() {
        let now = super::now();
        let mut evidence = serde_json::json!({"radio":{"available":true,"provider":"hamlib","radio_id":"r1","received_at_ms":now,"snapshot":{"observed_at_ms":now,"field_observed_at_ms":{"frequency":now,"mode":now,"ptt":now},"fields":{"frequency":{"Frequency":"14025000"},"mode":{"Mode":"CW"},"ptt":{"PTT":"0"}}}}});
        assert_eq!(super::project(&evidence)["station"]["transmitting"], false);
        evidence["radio"]["snapshot"]["field_observed_at_ms"]["ptt"] =
            serde_json::json!(now - 2_000);
        assert!(super::project(&evidence)["station"]["transmitting"].is_null());
        evidence["radio"]["snapshot"]["field_observed_at_ms"]["ptt"] = serde_json::Value::Null;
        assert!(super::project(&evidence)["station"]["transmitting"].is_null());
        evidence["radio"]["snapshot"]["field_observed_at_ms"]["ptt"] = serde_json::json!(now);
        evidence["radio"]["snapshot"]["fields"]["ptt"]["PTT"] = serde_json::json!("unknown");
        assert!(super::project(&evidence)["station"]["transmitting"].is_null());
        evidence["radio"]["snapshot"]["fields"]["ptt"]["PTT"] = serde_json::json!("1");
        assert_eq!(super::project(&evidence)["station"]["transmitting"], true);
    }
    use super::*;
    fn evidence(call: &str) -> Value {
        json!({"needs_invalid":false,"cluster":{"available":true,"received_at_ms":now(),"response":{"contact_refresh_state":"current","snapshot":{"logbookDataLoaded":true,"spots":[{"id":"spot-1","callsign":"W1AW","frequencyKHz":14025.0,"mode":"CW","source":"DX cluster","receivedAt":now() as f64/1000.0-978307200.0,"callStatus":call,"dxccStatus":"","score":10,"scoreReasons":["xLog classification"]}]}}}})
    }
    #[test]
    fn worked_entity_new_band_and_mode_priorities_preserve_owner_scoring() {
        // Source contract: SpotLogic.dxccStatus/score and the native
        // ClusterClassificationTests confirmation-prefix cases. "W" means
        // worked but unconfirmed; it does not make a new band/mode unneeded.
        for (status, score) in [
            ("W/New Band+Mode", 62),
            ("W/New Band", 52),
            ("W/New Mode", 42),
            ("C/New Band+Mode", 68),
            ("C/New Band", 58),
            ("C/New Mode", 48),
        ] {
            let mut e = evidence("Worked");
            let spot = &mut e["cluster"]["response"]["snapshot"]["spots"][0];
            spot["dxccStatus"] = json!(status);
            spot["score"] = json!(score);
            spot["scoreReasons"] = json!([status]);
            let projected = project(&e);
            let need = &projected["spots"][0]["need"];
            assert_eq!(need["status"], "needed", "{status}");
            assert_eq!(need["score"], score);
            assert_eq!(need["reasons"][0], status);
            assert_eq!(projected["spots"][0]["dxcc_status"], status);
        }
    }
    #[test]
    fn needed_worked_confirmed_and_unknown_are_owned_by_xlog() {
        for (classification, expected) in [
            ("New Call", "needed"),
            ("New Band", "needed"),
            ("Worked", "worked"),
            ("Confirmed", "worked"),
            ("Unknown", "unknown"),
        ] {
            let p = project(&evidence(classification));
            assert_eq!(p["spots"][0]["need"]["status"], expected);
        }
    }
    #[test]
    fn edits_imports_and_missing_or_stale_history_never_fabricate_needs() {
        let mut e = evidence("New Call");
        e["needs_invalid"] = json!(true);
        assert_eq!(project(&e)["spots"][0]["need"]["status"], "unknown");
        e["needs_invalid"] = json!(false);
        e["cluster"]["response"]["snapshot"]["logbookDataLoaded"] = json!(false);
        assert_eq!(project(&e)["spots"][0]["need"]["status"], "unknown");
        e = evidence("New Band");
        e["cluster"]["response"]["contact_refresh_state"] = json!("stale");
        assert_eq!(project(&e)["spots"][0]["need"]["status"], "unknown");
        e = evidence("New Band");
        e["cluster"]["received_at_ms"] = json!(now() - 60_001);
        assert_eq!(project(&e)["spots"][0]["need"]["score"], 0);
    }
    #[test]
    fn malformed_spots_do_not_poison_valid_projection_and_north_is_zero() {
        let mut e = evidence("Confirmed");
        let mut invalid = e["cluster"]["response"]["snapshot"]["spots"][0].clone();
        invalid["callsign"] = json!("bad call");
        e["cluster"]["response"]["snapshot"]["spots"]
            .as_array_mut()
            .unwrap()
            .push(invalid);
        e["rotor"] = json!({"available":true,"received_at_ms":now(),"snapshot":{"owner":"native_dcu1","companion_route_id":"native_dcu1:connection-a","state":"active","backend":{"connected":true,"azimuth":360.0}}});
        let projected = project(&e);
        assert_eq!(projected["spots"].as_array().unwrap().len(), 1);
        assert!(projected["spots"][0]["country"].is_null());
        assert!(projected["spots"][0]["dxcc_status"].is_null());
        assert_eq!(projected["rotor"]["azimuth_deg"], 0.0);
        assert_eq!(projected["rotor"]["rotor_id"], "native_dcu1:connection-a");
        e["rotor"]["snapshot"].as_object_mut().unwrap().remove("companion_route_id");
        assert_eq!(project(&e)["rotor"]["connected"], false);
        assert!(project(&e)["rotor"]["rotor_id"].is_null());
    }
    #[test]
    fn stale_retained_station_is_disconnected_even_when_bridge_poll_is_new() {
        let mut e = evidence("Worked");
        e["radio"] = json!({"radio_id":"radio1","available":true,"received_at_ms":now()-5001,"snapshot":{"radio":{"connected":true,"observed":true,"frequency_hz":14025000,"mode":"CW"}}});
        assert_eq!(project(&e)["station"]["connected"], false);
    }
    #[test]
    fn typing_and_preview_are_not_logbook_mutations() {
        for action in ["replace", "clear"] {
            assert!(!invalidates_needs(
                &json!({"method":"logbook_operator_draft_apply","params":{"action":action}})
            ));
        }
        assert!(invalidates_needs(
            &json!({"method":"logbook_operator_draft_apply","params":{"action":"save"}})
        ));
        for action in ["preview", "retire"] {
            assert!(!invalidates_needs(
                &json!({"method":"logbook_import_adif","params":{"action":action}})
            ));
        }
        for method in [
            "logbook_contact_apply",
            "logbook_companion_apply",
            "logbook_save",
        ] {
            assert!(invalidates_needs(&json!({"method":method,"params":{}})));
        }
        assert!(!invalidates_needs(
            &json!({"method":"sync_summary","params":{}})
        ));
        for method in ["sync_configure", "sync_retry"] {
            assert!(!invalidates_needs(
                &json!({"method":"sync_mutation","params":{"method":method}})
            ));
            assert!(!invalidates_needs(&json!({"method":method})));
        }
        for method in ["sync_run", "sync_resolve"] {
            assert!(invalidates_needs(
                &json!({"method":"sync_mutation","params":{"method":method}})
            ));
        }
    }
}
