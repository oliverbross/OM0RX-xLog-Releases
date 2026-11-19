//! Fixed passive-bearing validation; no storage, provider, or hardware access.
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_BEARING: usize = 16 * 1024;
pub const MAX_ENVELOPE: usize = 20 * 1024;
pub const LEASE_MS: i64 = 5000;
const MAX_CLOCK_SKEW_MS: i64 = 250;
const SAFE: i64 = 9_007_199_254_740_990;
type Check = Result<(), String>;
fn bad() -> String { "bearing_invalid".into() }
fn keys(value: &Value, names: &str) -> Check {
    let object = value.as_object().ok_or_else(bad)?;
    let names: Vec<_> = names.split_whitespace().collect();
    if object.len() != names.len() || names.iter().any(|n| !object.contains_key(*n)) { return Err(bad()); }
    Ok(())
}
fn integer(value: &Value, min: i64, max: i64) -> bool {
    value.as_i64().is_some_and(|n| (min..=max).contains(&n))
}
fn text(value: &Value, limit: usize, nonempty: bool) -> bool {
    value.as_str().is_some_and(|s| s.len() <= limit && (!nonempty || !s.is_empty()))
}
pub fn normalized_call(value: &str) -> Option<String> {
    ((3..=32).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'/')
        && value.bytes().any(|b| b.is_ascii_alphabetic())
        && value.bytes().any(|b| b.is_ascii_digit())
        && !value.starts_with('/') && !value.ends_with('/') && !value.contains("//"))
        .then(|| value.to_ascii_uppercase())
}
fn grid(value: &Value) -> bool {
    value.as_str().is_some_and(|s| matches!(s.len(), 4 | 6 | 8)
        && s.bytes().enumerate().all(|(i, b)| match i {
            0 | 1 => (b'A'..=b'R').contains(&b),
            4 | 5 => (b'A'..=b'X').contains(&b),
            _ => b.is_ascii_digit(),
        }))
}
fn uuid(value: &Value) -> bool {
    value.as_str().is_some_and(|s| uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s))
}
fn non_nil_uuid(value: &Value) -> bool {
    uuid(value) && value.as_str().and_then(|s| uuid::Uuid::parse_str(s).ok()).is_some_and(|id| !id.is_nil())
}
fn point(value: &Value) -> bool {
    value["latitude"].as_f64().is_some_and(|x| x.is_finite() && (-90.0..=90.0).contains(&x))
        && value["longitude"].as_f64().is_some_and(|x| x.is_finite() && (-180.0..=180.0).contains(&x))
}
fn unavailable_reason(value: &Value) -> bool {
    matches!(value.as_str(), Some("origin_unavailable" | "record_too_large" | "invalid_record" |
        "logbook_unavailable" | "reference_unavailable" | "country_ambiguous" | "country_known_no_entity" |
        "country_invalid" | "country_missing_date" | "country_unknown" | "target_coordinates_unavailable" |
        "reference_metadata_invalid" | "coincident" | "antipodal" | "invalid_geometry"))
}
pub fn validate_bearing(value: &Value, requested: &str, origin_grid: Option<&str>) -> Check {
    let normalized = normalized_call(requested).ok_or_else(bad)?;
    if serde_json::to_vec(value).map_err(|_| bad())?.len() > MAX_BEARING { return Err(bad()); }
    keys(value, "schema_version requested_callsign normalized_callsign status reason origin target geometry logbook_revision reference_revision motion_authority transmit_authority")?;
    if value["schema_version"] != 1 || value["requested_callsign"] != requested
        || value["normalized_callsign"] != normalized || value["motion_authority"] != false
        || value["transmit_authority"] != false || !integer(&value["logbook_revision"], 0, SAFE) { return Err(bad()); }
    let origin = &value["origin"];
    if origin.is_null() {
        if origin_grid.is_some() || value["reason"] != "origin_unavailable" { return Err(bad()); }
    } else {
        keys(origin, "source grid latitude longitude")?;
        if origin["source"] != "saved_station_settings" || !grid(&origin["grid"]) || !point(origin)
            || origin_grid.is_some_and(|expected| origin["grid"] != expected) { return Err(bad()); }
    }
    let target = &value["target"];
    if target.is_null() {
        if !value["reference_revision"].is_null() { return Err(bad()); }
    } else {
        keys(target, "source grid latitude longitude precision contact_id contact_revision contact_started_at_unix_ms reference_revision matched_reference_revision matched_rule country_status")?;
        if !point(target) || value["reference_revision"] != target["reference_revision"] { return Err(bad()); }
        match target["source"].as_str() {
            Some("exact_logbook_grid") => {
                if target["precision"] != "historical_grid_cell_center" || !grid(&target["grid"])
                    || !uuid(&target["contact_id"])
                    || !integer(&target["contact_revision"], 0, value["logbook_revision"].as_i64().unwrap())
                    || !integer(&target["contact_started_at_unix_ms"], -SAFE, SAFE)
                    || ["reference_revision", "matched_reference_revision", "matched_rule", "country_status"]
                        .iter().any(|key| !target[*key].is_null()) { return Err(bad()); }
            }
            Some("cty_entity_approximation") => {
                if target["precision"] != "entity_approximation" || target["country_status"] != "resolved"
                    || !text(&target["reference_revision"], 2048, true)
                    || (!target["matched_reference_revision"].is_null() && !text(&target["matched_reference_revision"], 2048, false))
                    || (!target["matched_rule"].is_null() && !text(&target["matched_rule"], 128, false))
                    || ["grid", "contact_id", "contact_revision", "contact_started_at_unix_ms"]
                        .iter().any(|key| !target[*key].is_null()) { return Err(bad()); }
            }
            _ => return Err(bad()),
        }
    }
    match value["status"].as_str() {
        Some("available") => {
            let geometry = &value["geometry"];
            keys(geometry, "short_path_deg long_path_deg distance_km")?;
            if origin.is_null() || target.is_null() || !value["reason"].is_null()
                || ["short_path_deg", "long_path_deg"].iter().any(|key| !geometry[*key].as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..360.0).contains(&n)))
                || !geometry["distance_km"].as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) { return Err(bad()); }
        }
        Some("unavailable") => {
            if !unavailable_reason(&value["reason"]) || !value["geometry"].is_null() { return Err(bad()); }
            let geometry_failure = matches!(value["reason"].as_str(), Some("coincident" | "antipodal" | "invalid_geometry"));
            if geometry_failure == target.is_null() { return Err(bad()); }
        }
        _ => return Err(bad()),
    }
    Ok(())
}
pub fn validate_binding(value: &Value, workspace: &str, worker: u64) -> Check {
    keys(value, "workspace_generation worker_generation settings_revision identity_epoch identity_revision operating_module")?;
    if value["workspace_generation"] != workspace || value["worker_generation"].as_u64() != Some(worker)
        || !integer(&value["worker_generation"], 1, SAFE) || value["operating_module"] != "normal_logging"
        || !integer(&value["settings_revision"], 0, SAFE) || !integer(&value["identity_revision"], 0, SAFE)
        || !non_nil_uuid(&value["identity_epoch"]) || !non_nil_uuid(&value["workspace_generation"]) {
        return Err("bearing_identity_changed".into());
    }
    Ok(())
}
pub fn identity_origin(value: &Value, workspace: &str, worker: u64) -> Result<String, String> {
    keys(value, "schema_version source state mycall mygrid binding transmit_authority")?;
    if value["schema_version"] != 1 || value["source"] != "saved_station_settings"
        || !matches!(value["state"].as_str(),Some("current" | "incomplete"))
        || value["transmit_authority"] != false || !grid(&value["mygrid"]) {
        return Err("bearing_identity_unavailable".into());
    }
    validate_binding(&value["binding"], workspace, worker)?;
    Ok(value["mygrid"].as_str().unwrap().to_owned())
}
pub fn validate_identity_pair(before: &Value, after: &Value, workspace: &str, worker: u64) -> Check {
    identity_origin(before, workspace, worker)?;
    identity_origin(after, workspace, worker)?;
    if before != after { return Err("bearing_identity_changed".into()); }
    Ok(())
}
pub fn now_ms() -> Result<i64, String> {
    let value = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "bearing_clock")?.as_millis();
    if value > SAFE as u128 { return Err("bearing_clock".into()); }
    Ok(value as i64)
}
pub fn validate_owner(value: &Value, requested: &str, workspace: &str, origin_grid: Option<&str>,
    started: i64, now: i64, elapsed: Duration) -> Check {
    keys(value, "generation generated_at_unix_ms max_age_ms bearing")?;
    if value["generation"] != workspace || workspace.is_empty() || workspace.len() > 128 { return Err("bearing_workspace_changed".into()); }
    if value["max_age_ms"] != LEASE_MS || !integer(&value["generated_at_unix_ms"], 0, SAFE)
        || !(0..=SAFE).contains(&started) || !(0..=SAFE).contains(&now) { return Err(bad()); }
    let stamp = value["generated_at_unix_ms"].as_i64().unwrap();
    if now < started || stamp < started || stamp > now || now - stamp >= LEASE_MS || now - started >= LEASE_MS
        || elapsed >= Duration::from_millis(LEASE_MS as u64)
        || ((now - started) - elapsed.as_millis() as i64).abs() > MAX_CLOCK_SKEW_MS {
        return Err("bearing_stale".into());
    }
    validate_bearing(&value["bearing"], requested, origin_grid)
}
pub fn validate_provider(value: &Value, requested: &str, workspace: &str, started: i64, now: i64, elapsed: Duration) -> Check {
    keys(value, "generation generated_at_unix_ms max_age_ms bearing worker_generation station_identity_binding")?;
    if serde_json::to_vec(value).map_err(|_| bad())?.len() > MAX_ENVELOPE
        || !integer(&value["worker_generation"], 1, SAFE) { return Err(bad()); }
    validate_binding(&value["station_identity_binding"], workspace, value["worker_generation"].as_u64().unwrap())?;
    let mut owner = value.clone();
    owner.as_object_mut().unwrap().remove("worker_generation");
    owner.as_object_mut().unwrap().remove("station_identity_binding");
    validate_owner(&owner, requested, workspace, None, started, now, elapsed)
}

#[cfg(test)]
pub(crate) fn fixtures() -> Value {
    serde_json::from_str(include_str!("../../../scripts/companion-bridge-tests/fixtures/bearing-swift-v1.json")).unwrap()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const WORKSPACE: &str = "00000000-0000-4000-8000-000000000001";
    fn original() -> Value { fixtures()["historical_grid"].clone() }
    fn owner() -> Value {
        json!({"generation":WORKSPACE,"generated_at_unix_ms":1000,"max_age_ms":5000,"bearing":original()})
    }
    fn binding() -> Value {
        json!({"workspace_generation":WORKSPACE,"worker_generation":7,"settings_revision":11,
            "identity_epoch":"00000000-0000-4000-8000-000000000002","identity_revision":3,"operating_module":"normal_logging"})
    }
    fn provider() -> Value {
        let mut result=owner(); result["worker_generation"]=json!(7); result["station_identity_binding"]=binding(); result
    }
    #[test]
    fn actual_swift_cases_preserve_historical_vs_approximate_evidence() {
        for name in ["historical_grid","cty_approximation","coincident","reference_unavailable"] {
            let fixture=fixtures()[name].clone();
            validate_bearing(&fixture,fixture["requested_callsign"].as_str().unwrap(),None).unwrap();
        }
        assert_eq!(original()["target"]["contact_started_at_unix_ms"],1_700_000_000_000_i64);
        assert_eq!(original()["target"]["precision"],"historical_grid_cell_center");
        assert_eq!(fixtures()["cty_approximation"]["target"]["contact_started_at_unix_ms"],Value::Null);
        assert_eq!(fixtures()["coincident"]["geometry"],Value::Null);
        assert_ne!(fixtures()["coincident"]["target"],Value::Null);
    }
    #[test]
    fn strict_calls_do_not_repair_or_substitute_operating_locations() {
        assert_eq!(normalized_call("vk8rx/p").as_deref(),Some("VK8RX/P"));
        for raw in [" VK8RX","VK8RX ","VK8RX//P","VK8RX;","/VK8RX","VK8RX/","UNKNOWN","1234","VK8RX\n","VＫ8RX"] {
            assert!(normalized_call(raw).is_none(),"{raw}");
        }
        assert!(validate_bearing(&original(),"VK8RX",None).is_err());
        assert!(validate_bearing(&original(),"VK8RX/P",Some("PH57")).is_err());
    }
    #[test]
    fn fixed_fields_numbers_and_evidence_variants_are_checked() {
        let mut bad_values=Vec::new();
        let mut v=original();v["extra"]=json!(true);bad_values.push(v);
        let mut v=original();v.as_object_mut().unwrap().remove("reference_revision");bad_values.push(v);
        let mut v=original();v["target"]["secret"]=json!("hidden");bad_values.push(v);
        let mut v=original();v["target"]["contact_revision"]=json!(8);bad_values.push(v);
        let mut v=original();v["target"]["contact_started_at_unix_ms"]=json!(SAFE+1);bad_values.push(v);
        let mut v=original();v["logbook_revision"]=json!(1.5);bad_values.push(v);
        let mut v=original();v["geometry"]["short_path_deg"]=json!(360);bad_values.push(v);
        let mut v=original();v["geometry"]["distance_km"]=json!(-1);bad_values.push(v);
        let mut v=original();v["geometry"]["long_path_deg"]=Value::Null;bad_values.push(v);
        let mut v=original();v["origin"]["latitude"]=json!(91);bad_values.push(v);
        let mut v=original();v["target"]["longitude"]=json!(181);bad_values.push(v);
        let mut v=original();v["target"]["grid"]=json!("PH57YZ");bad_values.push(v);
        let mut v=original();v["motion_authority"]=json!(true);bad_values.push(v);
        let mut v=original();v["transmit_authority"]=json!(true);bad_values.push(v);
        let mut v=original();v["status"]=json!("unavailable");bad_values.push(v);
        let mut v=original();v["target"]["source"]=json!("live_position");bad_values.push(v);
        let mut v=original();v["target"]["reference_revision"]=json!("mixed");bad_values.push(v);
        for v in bad_values {assert!(validate_bearing(&v,"VK8RX/P",Some("JN88TQ12")).is_err());}
        let mut v=fixtures()["cty_approximation"].clone();v["target"]["contact_id"]=original()["target"]["contact_id"].clone();
        assert!(validate_bearing(&v,"VK8RX",None).is_err());
        let mut v=fixtures()["coincident"].clone();v["target"]=Value::Null;
        assert!(validate_bearing(&v,"VK8RX/P",None).is_err());
    }
    #[test]
    fn leaf_and_total_budgets_reject_without_truncation() {
        let mut v=fixtures()["cty_approximation"].clone();
        v["target"]["reference_revision"]=json!("r".repeat(2049));v["reference_revision"]=v["target"]["reference_revision"].clone();
        assert!(validate_bearing(&v,"VK8RX",None).is_err());
        let mut v=original();v["target"]["matched_rule"]=json!("r".repeat(MAX_BEARING));
        assert!(validate_bearing(&v,"VK8RX/P",None).is_err());
    }
    #[test]
    fn five_second_read_start_never_rebases_historical_time_or_expired_evidence() {
        validate_owner(&owner(),"VK8RX/P",WORKSPACE,Some("JN88TQ12"),1000,1001,Duration::from_millis(1)).unwrap();
        for (start,now,elapsed) in [(1001,1001,1),(1000,999,1),(1000,6000,4999),(1000,1001,5000),
            (1000,1500,1),(1000,1500,1000)] {
            assert!(validate_owner(&owner(),"VK8RX/P",WORKSPACE,Some("JN88TQ12"),start,now,Duration::from_millis(elapsed)).is_err());
        }
        let mut future=owner();future["generated_at_unix_ms"]=json!(1002);
        assert!(validate_owner(&future,"VK8RX/P",WORKSPACE,None,1000,1001,Duration::from_millis(1)).is_err());
        let mut jumped=owner();jumped["generated_at_unix_ms"]=json!(10000);
        assert!(validate_owner(&jumped,"VK8RX/P",WORKSPACE,None,1000,10001,Duration::from_millis(1)).is_err());
        assert!(validate_owner(&owner(),"VK8RX/P","foreign",None,1000,1001,Duration::from_millis(1)).is_err());
    }
    #[test]
    fn provider_identity_binding_is_exact_and_read_only() {
        validate_provider(&provider(),"VK8RX/P",WORKSPACE,1000,1001,Duration::from_millis(1)).unwrap();
        for (key,value) in [("worker_generation",json!(8)),("workspace_generation",json!("foreign")),
            ("operating_module",json!("contesting")),("settings_revision",json!(-1)),("identity_revision",json!(SAFE+1)),
            ("identity_epoch",json!("00000000-0000-0000-0000-000000000000"))] {
            let mut v=provider();v["station_identity_binding"][key]=value;
            assert!(validate_provider(&v,"VK8RX/P",WORKSPACE,1000,1001,Duration::from_millis(1)).is_err());
        }
        let mut v=provider();v["station_identity_binding"]["secret"]=json!(true);
        assert!(validate_provider(&v,"VK8RX/P",WORKSPACE,1000,1001,Duration::from_millis(1)).is_err());
    }
    #[test]
    fn real_identity_projection_changes_retire_the_origin() {
        let original=json!({"available":true,"epoch":"00000000-0000-4000-8000-000000000002","revision":3,
            "snapshot":{"workspace_generation":WORKSPACE,"worker_generation":7,"settings_revision":11,
                "station_call":"OM0RX","operator_call":"OTHER","grid":"JN88TQ12"}});
        let project=|retained:&Value,module| crate::companion_identity::project(retained,7,WORKSPACE,module);
        let before=project(&original,Some("normal_logging"));
        assert_eq!(identity_origin(&before,WORKSPACE,7).unwrap(),"JN88TQ12");
        validate_identity_pair(&before,&before,WORKSPACE,7).unwrap();
        for change in 0..6 {
            let mut retained=original.clone();
            match change {
                0=>retained["snapshot"]["grid"]=json!("PH57"),
                1=>retained["snapshot"]["settings_revision"]=json!(12),
                2=>retained["revision"]=json!(4),
                3=>retained["epoch"]=json!("00000000-0000-4000-8000-000000000003"),
                4=>retained["available"]=json!(false),
                _=>retained["snapshot"]["workspace_generation"]=json!("00000000-0000-4000-8000-000000000004"),
            }
            assert!(validate_identity_pair(&before,&project(&retained,Some("normal_logging")),WORKSPACE,7).is_err());
        }
        assert!(identity_origin(&project(&original,Some("contesting")),WORKSPACE,7).is_err());
        let mut missing=original.clone();missing["snapshot"]["grid"]=Value::Null;
        assert!(identity_origin(&project(&missing,Some("normal_logging")),WORKSPACE,7).is_err());
        let mut no_call=original.clone();no_call["snapshot"]["station_call"]=json!("");
        assert_eq!(identity_origin(&project(&no_call,Some("normal_logging")),WORKSPACE,7).unwrap(),"JN88TQ12");
    }
}
