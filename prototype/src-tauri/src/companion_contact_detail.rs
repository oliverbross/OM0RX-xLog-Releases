//! Fixed selected-contact wire validation. No storage, provider or effects access.
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_DETAIL: usize = 120 * 1024;
pub const MAX_ENVELOPE: usize = 128 * 1024;
pub const LEASE_MS: i64 = 5000;
const SAFE: i64 = 9_007_199_254_740_990;
type Check = Result<(), String>;
fn bad() -> String { "contact_detail_invalid".into() }
pub fn canonical_id(raw: &str) -> bool {
    uuid::Uuid::parse_str(raw).is_ok_and(|id| raw == id.to_string())
}
fn keys(value: &Value, names: &str) -> Check {
    let object = value.as_object().ok_or_else(bad)?;
    let names: Vec<_> = names.split_whitespace().collect();
    if object.len() != names.len() || names.iter().any(|n| !object.contains_key(*n)) { return Err(bad()); }
    Ok(())
}
fn integer(value: &Value, min: i64, max: i64) -> bool {
    value.as_i64().is_some_and(|v| (min..=max).contains(&v))
}
fn text(value: &Value, cap: usize, nonempty: bool) -> bool {
    value.as_str().is_some_and(|v| v.len() <= cap && (!nonempty || !v.is_empty()))
}
fn reason(value: &Value) -> bool {
    value.as_str().is_some_and(|v| !v.is_empty() && v.len() <= 128
        && v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}
#[derive(Clone, Copy)]
enum Kind { Text, Notes, Integer, Number, Bool }
fn field(value: &Value, kind: Kind) -> Check {
    keys(value, "state value reason")?;
    match value["state"].as_str() {
        Some("empty") if value["value"].is_null() && value["reason"].is_null() => Ok(()),
        Some("unavailable") if value["value"].is_null() && reason(&value["reason"]) => Ok(()),
        Some("value") if value["reason"].is_null() => {
            let v = &value["value"];
            let valid = match kind {
                Kind::Text => text(v, 4096, true), Kind::Notes => text(v, 16384, true),
                Kind::Integer => integer(v, -SAFE, SAFE),
                Kind::Number => v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0 && n <= SAFE as f64),
                Kind::Bool => v.is_boolean(),
            };
            if valid { Ok(()) } else { Err(bad()) }
        }
        _ => Err(bad()),
    }
}
fn group(value: &Value, texts: &str, ints: &str, nums: &str, bools: &str) -> Check {
    keys(value, &format!("{texts} {ints} {nums} {bools}"))?;
    for (names, kind) in [(texts, Kind::Text), (ints, Kind::Integer), (nums, Kind::Number), (bools, Kind::Bool)] {
        for name in names.split_whitespace() { field(&value[name], if name == "notes" { Kind::Notes } else { kind })?; }
    }
    Ok(())
}
fn fields(value: &Value) -> Check {
    keys(value, "contact radio station own_station references confirmations awards uploads contest")?;
    let contact = &value["contact"];
    group(contact, "callsign band mode submode stored_time_unit original_start_precision original_end_precision raw_qso_date raw_time_on raw_qso_date_off raw_time_off rst_sent rst_received grid comment notes",
        "started_at_unix_ms ended_at_unix_ms", "", "")?;
    group(&value["radio"], "receive_band tx_power_raw propagation_mode antenna_path satellite_name",
        "frequency_hz receive_frequency_hz", "tx_power_w", "")?;
    group(&value["station"], "name qth state county country continent cq_zone itu_zone", "dxcc", "", "")?;
    group(&value["own_station"], "operator_call callsign grid recorded_rig radio_label station_location", "station_profile_id", "", "")?;
    keys(&value["references"], "their my dok")?;
    field(&value["references"]["dok"], Kind::Text)?;
    for side in ["their", "my"] { group(&value["references"][side], "pota sota wwff iota sig sig_info", "", "", "")?; }
    keys(&value["confirmations"], "paper lotw eqsl qrz")?;
    for source in ["paper", "lotw", "eqsl", "qrz"] {
        let v = &value["confirmations"][source];
        group(v, "source evidence_kind sent_status sent_date sent_via received_status received_date received_via", "", "match_age_seconds", "")?;
        if v["source"]["value"] != source { return Err(bad()); }
    }
    group(&value["awards"], "credit_granted credit_submitted authority", "", "", "")?;
    keys(&value["uploads"], "lotw eqsl qrz clublog")?;
    for source in ["lotw", "eqsl", "qrz", "clublog"] {
        let v = &value["uploads"][source];
        group(v, "source state", "receipt_revision last_attempt_at_unix_ms", "", "is_current_revision")?;
        if v["source"]["value"] != source { return Err(bad()); }
    }
    group(&value["contest"], "contest_id stx srx stx_string srx_string", "", "", "")
}

pub fn validate_detail(value: &Value, requested: &str) -> Check {
    if !canonical_id(requested) || serde_json::to_vec(value).map_err(|_| bad())?.len() > MAX_DETAIL { return Err(bad()); }
    keys(value, "schema_version contact_id status reason logbook_revision contact_revision fields editability locate")?;
    if value["schema_version"] != 1 || value["contact_id"] != requested || !integer(&value["logbook_revision"], 0, SAFE) { return Err(bad()); }
    if !value["contact_revision"].is_null() && !integer(&value["contact_revision"], 0, value["logbook_revision"].as_i64().unwrap()) { return Err(bad()); }
    match value["status"].as_str() {
        Some("found") if !value["contact_revision"].is_null() && value["reason"].is_null() => fields(&value["fields"])? ,
        Some("deleted") if !value["contact_revision"].is_null() && value["fields"].is_null() && value["reason"].is_null() => (),
        Some("not_found") if value["contact_revision"].is_null() && value["fields"].is_null() && value["reason"].is_null() => (),
        Some("unavailable") if value["fields"].is_null() && reason(&value["reason"]) => (),
        _ => return Err(bad()),
    }
    field(&value["locate"], Kind::Text)?;
    if value["locate"]["state"] != "unavailable" || value["locate"]["reason"] != "paged_logbook_not_supported" { return Err(bad()); }
    let edit = &value["editability"];
    keys(edit, "allowed reason allowed_fields base")?;
    if !reason(&edit["reason"]) { return Err(bad()); }
    if edit["allowed"] == false {
        if !edit["base"].is_null() || edit["allowed_fields"] != json!([]) { return Err(bad()); }
        return Ok(());
    }
    if edit["allowed"] != true || value["status"] != "found" || !integer(&value["contact_revision"], 1, SAFE)
        || edit["reason"] != "complete_core" || edit["allowed_fields"] != json!(["started_at_unix_ms","frequency_hz","rst_sent","rst_received","grid","comment"]) { return Err(bad()); }
    let base = &edit["base"];
    keys(base, "contact_id callsign band mode started_at_unix_ms frequency_hz rst_sent rst_received grid comment")?;
    if base["contact_id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok()) != uuid::Uuid::parse_str(requested).ok()
        || !integer(&base["started_at_unix_ms"], 0, 4_102_444_800_000) { return Err(bad()); }
    validate_base(base)?;
    for name in ["callsign", "band", "mode", "rst_sent", "rst_received", "grid", "comment"] {
        let leaf = &value["fields"]["contact"][name];
        let expected = match leaf["state"].as_str() {
            Some("value") => leaf["value"].clone(), Some("empty") => json!(""), _ => return Err(bad()),
        };
        if base[name] != expected || !base[name].is_string() { return Err(bad()); }
    }
    if base["started_at_unix_ms"] != value["fields"]["contact"]["started_at_unix_ms"]["value"] { return Err(bad()); }
    let frequency = &value["fields"]["radio"]["frequency_hz"];
    if frequency["state"] == "unavailable" || base["frequency_hz"] != frequency["value"] { return Err(bad()); }
    Ok(())
}

fn validate_base(base: &Value) -> Check {
    let ascii = |key: &str, min: usize, max: usize, allowed: fn(u8) -> bool| {
        base[key].as_str().is_some_and(|s| (min..=max).contains(&s.len()) && s.bytes().all(allowed))
    };
    if !ascii("callsign",3,32,|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'/' || b == b'-')
        || !ascii("band",1,16,|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.')
        || !ascii("mode",1,16,|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        || (!base["frequency_hz"].is_null() && !integer(&base["frequency_hz"],1,1_000_000_000_000)) { return Err(bad()); }
    for key in ["rst_sent","rst_received"] {
        if !base[key].as_str().is_some_and(|s| s.len() <= 16 && !s.chars().any(char::is_control)) { return Err(bad()); }
    }
    if !base["comment"].as_str().is_some_and(|s| s.len() <= 4096 && s.chars().all(|c| !c.is_control() || matches!(c,'\t'|'\n'|'\r'))) { return Err(bad()); }
    let grid = base["grid"].as_str().ok_or_else(bad)?.as_bytes();
    if !grid.is_empty() && (!(grid.len()==4 || grid.len()==6 || grid.len()==8)
        || !grid[0..2].iter().all(|b| (b'A'..=b'R').contains(b))
        || !grid[2..4].iter().all(u8::is_ascii_digit)
        || (grid.len()>=6 && !grid[4..6].iter().all(|b| (b'A'..=b'X').contains(b)))
        || (grid.len()==8 && !grid[6..8].iter().all(u8::is_ascii_digit))) { return Err(bad()); }
    Ok(())
}

pub fn now_ms() -> Result<i64, String> {
    let value = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "contact_detail_clock")?.as_millis();
    if value > SAFE as u128 { return Err("contact_detail_clock".into()); }
    Ok(value as i64)
}
pub fn validate_owner(value: &Value, requested: &str, workspace: &str, started_ms: i64, now: i64, elapsed: Duration) -> Check {
    keys(value, "generation generated_at_unix_ms max_age_ms detail")?;
    if value["generation"] != workspace || workspace.is_empty() || workspace.len() > 128 { return Err("contact_detail_workspace_changed".into()); }
    if value["max_age_ms"] != LEASE_MS || !integer(&value["generated_at_unix_ms"], 0, SAFE) { return Err(bad()); }
    let stamp = value["generated_at_unix_ms"].as_i64().unwrap();
    if elapsed >= Duration::from_millis(LEASE_MS as u64) || now < started_ms || stamp < started_ms || stamp > now || now - stamp >= LEASE_MS {
        return Err("contact_detail_stale".into());
    }
    validate_detail(&value["detail"], requested)
}
pub fn validate_provider(value: &Value, requested: &str, workspace: &str, started_ms: i64, now: i64, elapsed: Duration) -> Check {
    keys(value, "generation generated_at_unix_ms max_age_ms detail worker_generation")?;
    if !integer(&value["worker_generation"], 1, SAFE) { return Err(bad()); }
    let mut owner = value.clone(); owner.as_object_mut().unwrap().remove("worker_generation");
    validate_owner(&owner, requested, workspace, started_ms, now, elapsed)
}

#[cfg(test)]
pub(crate) fn fixtures() -> Value {
    serde_json::from_str(include_str!("../../../scripts/companion-bridge-tests/fixtures/contact-detail-swift-v1.json")).unwrap()
}
#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "abcdef12-3456-4abc-8def-123456abcdef";
    #[test]
    fn actual_swift_found_deleted_missing_preserve_exact_base_and_nulls() {
        let fixtures = fixtures();
        for name in ["found", "deleted", "not_found"] {
            let value = &fixtures[name];
            validate_detail(value, value["contact_id"].as_str().unwrap()).unwrap();
        }
        let found = &fixtures["found"];
        assert_eq!(found["editability"]["base"]["frequency_hz"], Value::Null);
        assert_eq!(found["editability"]["base"]["comment"], "line one\nline two");
        assert_eq!(found["editability"]["base"]["mode"], "MFSK");
        assert_eq!(found["fields"]["contact"]["submode"]["value"], "FT8");
        assert_eq!(found["fields"]["confirmations"]["paper"]["sent_via"]["value"], "D");
    }
    #[test]
    fn fixed_shape_rejects_unknown_missing_wrong_types_and_wrong_identity() {
        let found = fixtures()["found"].clone();
        let mut mutations = Vec::new();
        let mut v = found.clone(); v["extra"] = json!(true); mutations.push(v);
        let mut v = found.clone(); v["fields"]["station"]["secret"] = json!("hidden"); mutations.push(v);
        let mut v = found.clone(); v["editability"]["base"].as_object_mut().unwrap().remove("frequency_hz"); mutations.push(v);
        let mut v = found.clone(); v["fields"]["contact"]["grid"]["value"] = json!(""); mutations.push(v);
        let mut v = found.clone(); v["fields"]["contact"]["grid"]["state"] = json!("value"); mutations.push(v);
        let mut v = found.clone(); v["contact_revision"] = json!(2); mutations.push(v);
        let mut v = found.clone(); v["logbook_revision"] = json!(9_007_199_254_740_992_u64); mutations.push(v);
        let mut v = found.clone(); v["fields"]["contact"]["started_at_unix_ms"]["value"] = json!(1.5); mutations.push(v);
        let mut v = found.clone(); v["editability"]["base"]["mode"] = json!("FT8"); mutations.push(v);
        let mut v = found.clone(); v["editability"]["base"]["contact_id"] = json!("00000000-0000-4000-8000-000000000999"); mutations.push(v);
        let mut v = found.clone(); v["status"] = json!("deleted"); mutations.push(v);
        for v in mutations { assert!(validate_detail(&v, ID).is_err()); }
        for invalid in [ID.to_uppercase(), format!("{ID}?x=1"), format!("{ID}/"), ID.replace('-', "")] {
            assert!(!canonical_id(&invalid));
        }
    }
    #[test]
    fn leaf_and_total_budgets_do_not_truncate() {
        let mut value = fixtures()["found"].clone();
        value["fields"]["contact"]["notes"] = json!({"state":"value","value":"N".repeat(16384),"reason":null});
        validate_detail(&value, ID).unwrap();
        value["fields"]["contact"]["notes"]["value"] = json!("N".repeat(16385));
        assert!(validate_detail(&value, ID).is_err());
        value["fields"]["contact"]["notes"]["value"] = json!("N".repeat(MAX_DETAIL));
        assert!(validate_detail(&value, ID).is_err());
        let mut unavailable = fixtures()["deleted"].clone();
        unavailable["status"] = json!("unavailable"); unavailable["reason"] = json!("detail_too_large");
        unavailable["editability"]["reason"] = json!("detail_too_large");
        validate_detail(&unavailable, ID).unwrap();
    }
    #[test]
    fn invalid_retained_core_is_readable_but_never_editable() {
        for (key, invalid) in [("callsign",json!("old call?")),("band",json!("OLD-BAND")),("mode",json!("legacy mode")),
            ("grid",json!("bad grid")),("rst_sent",json!("599\u{0001}")),("comment",json!("note\u{0001}")),("frequency_hz",json!(-1))] {
            let mut v=fixtures()["found"].clone();
            let group=if key=="frequency_hz" {"radio"} else {"contact"};
            v["fields"][group][key]=json!({"state":"value","value":invalid,"reason":null});
            v["editability"]["base"][key]=invalid.clone();
            assert!(validate_detail(&v,ID).is_err(),"{key} editable");
            v["editability"]=json!({"allowed":false,"reason":"core_not_lossless","allowed_fields":[],"base":null});
            validate_detail(&v,ID).unwrap();
        }
    }
    #[test]
    fn maximum_escaped_comment_roundtrips_without_truncation() {
        let comment="\t\n".repeat(2048);
        let mut value=fixtures()["found"].clone();
        value["fields"]["contact"]["comment"]["value"]=json!(comment);
        value["editability"]["base"]["comment"]=json!(comment);
        validate_detail(&value,ID).unwrap();
        assert_eq!(value["editability"]["base"]["comment"].as_str().unwrap().len(),4096);
        value["fields"]["contact"]["comment"]["value"]=json!(format!("{comment}x"));
        value["editability"]["base"]["comment"]=json!(format!("{comment}x"));
        assert!(validate_detail(&value,ID).is_err());
    }
    #[test]
    fn owner_lease_rejects_future_stale_clock_jump_and_foreign_binding() {
        let owner = json!({"generation":"workspace","generated_at_unix_ms":1000,"max_age_ms":5000,"detail":fixtures()["found"]});
        validate_owner(&owner, ID, "workspace", 1000, 1001, Duration::from_millis(1)).unwrap();
        for (start, now, elapsed) in [(1000,999,1),(1001,1002,1),(1000,6000,1),(1000,1001,5000)] {
            assert!(validate_owner(&owner, ID, "workspace", start, now, Duration::from_millis(elapsed)).is_err());
        }
        assert!(validate_owner(&owner, ID, "changed", 1000,1001,Duration::ZERO).is_err());
        let mut v = owner; v["worker_generation"] = json!(0);
        assert!(validate_provider(&v, ID, "workspace",1000,1001,Duration::ZERO).is_err());
        v["worker_generation"] = json!(1);
        validate_provider(&v,ID,"workspace",1000,1001,Duration::ZERO).unwrap();
    }
}
