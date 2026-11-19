//! Read-only, context-bound station identity for companion displays.
//! This is saved normal-logging identity, not authorization to transmit.
use serde_json::{json, Value};

const MAX_SAFE: u64 = 9_007_199_254_740_991;

pub fn project(retained: &Value, worker: u64, workspace: &str, module: Option<&str>) -> Value {
    let mut result = json!({
        "schema_version":1, "source":"saved_station_settings", "state":"unavailable",
        "mycall":null, "mygrid":null, "binding":null,
        "transmit_authority":false
    });
    let value = &retained["snapshot"];
    let valid_revision = |v: &Value| v.as_u64().is_some_and(|n| n <= MAX_SAFE);
    let valid_uuid = |v: &str| uuid::Uuid::parse_str(v).is_ok_and(|id| !id.is_nil());
    if module != Some("normal_logging") || retained["available"] != true
        || worker == 0 || worker > MAX_SAFE || !valid_uuid(workspace)
        || value["worker_generation"].as_u64() != Some(worker)
        || value["workspace_generation"].as_str() != Some(workspace)
        || !valid_revision(&value["settings_revision"])
        || !valid_revision(&retained["revision"])
        || !retained["epoch"].as_str().is_some_and(valid_uuid)
    {
        return result;
    }
    // Never substitute operator_call or derive an identity from an application name.
    result["mycall"] = value["station_call"].as_str().filter(|call| {
        !call.is_empty() && call.len() <= 64
            && call.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'/')
            && call.bytes().any(|c| c.is_ascii_alphabetic())
            && call.bytes().any(|c| c.is_ascii_digit())
            && !call.starts_with('/') && !call.ends_with('/') && !call.contains("//")
    }).map(|call| json!(call.to_ascii_uppercase())).unwrap_or(Value::Null);
    result["mygrid"] = value["grid"].as_str().filter(|grid| valid_grid(grid))
        .map(|grid| json!(grid.to_ascii_uppercase())).unwrap_or(Value::Null);
    result["state"] = json!(if result["mycall"].is_null() || result["mygrid"].is_null() { "incomplete" } else { "current" });
    result["binding"] = json!({"workspace_generation":workspace,"worker_generation":worker,
        "settings_revision":value["settings_revision"],"identity_epoch":retained["epoch"],
        "identity_revision":retained["revision"],"operating_module":"normal_logging"});
    result
}

fn valid_grid(grid: &str) -> bool {
    let bytes = grid.as_bytes();
    matches!(bytes.len(), 4 | 6 | 8) && bytes.iter().enumerate().all(|(i, b)| match i {
        0 | 1 => (b'A'..=b'R').contains(&b.to_ascii_uppercase()),
        4 | 5 => (b'A'..=b'X').contains(&b.to_ascii_uppercase()),
        _ => b.is_ascii_digit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const WORKSPACE: &str = "00000000-0000-4000-8000-000000000001";
    fn retained() -> Value {
        json!({"available":true,"epoch":"00000000-0000-4000-8000-000000000002","revision":3,
            "snapshot":{"workspace_generation":WORKSPACE,"worker_generation":7,"settings_revision":11,
                "station_call":"VK8RX/P","operator_call":"OM0RX","grid":"PH57ab"}})
    }
    fn read(value: &Value) -> Value { project(value, 7, WORKSPACE, Some("normal_logging")) }
    #[test]
    fn station_identity_has_exact_provenance_and_no_operator_fallback() {
        let mut value = retained();
        let result = read(&value);
        assert_eq!(result["mycall"], "VK8RX/P");
        assert_eq!(result["mygrid"], "PH57AB");
        assert_eq!(result["state"], "current");
        assert_eq!(result["binding"]["settings_revision"], 11);
        assert_eq!(result["transmit_authority"], false);
        assert!(!result.to_string().contains("OM0RX"));
        value["snapshot"]["station_call"] = json!("");
        assert!(read(&value)["mycall"].is_null());
        assert_eq!(read(&value)["state"], "incomplete");
    }
    #[test]
    fn invalidated_or_foreign_context_never_exposes_retained_identity() {
        let original = retained();
        for (key, replacement) in [("available", json!(false)), ("epoch", json!("")), ("revision", json!(MAX_SAFE + 1))] {
            let mut value = original.clone(); value[key] = replacement;
            let result = read(&value);
            assert_eq!(result["state"], "unavailable"); assert!(result["mycall"].is_null()); assert!(result["binding"].is_null());
        }
        for result in [project(&original, 8, WORKSPACE, Some("normal_logging")),
            project(&original, 7, "00000000-0000-4000-8000-000000000003", Some("normal_logging")),
            project(&original, 7, WORKSPACE, Some("contesting")), project(&original, 7, WORKSPACE, None)] {
            assert_eq!(result["state"], "unavailable"); assert!(result["mycall"].is_null()); assert!(result["mygrid"].is_null());
        }
    }
    #[test]
    fn malformed_fields_are_unknown_not_rewritten_into_valid_identity() {
        for call in ["VK8RX\n", " VK8RX", "VK8RX/P;", "/VK8RX", "VK8RX//P", "UNKNOWN"] {
            let mut value = retained(); value["snapshot"]["station_call"] = json!(call);
            assert!(read(&value)["mycall"].is_null(), "{call}");
        }
        for grid in ["", "AA00YZ", "SS00", "JN88T", "JN88TQ123", " JN88", "JN88\n"] {
            let mut value = retained(); value["snapshot"]["grid"] = json!(grid);
            assert!(read(&value)["mygrid"].is_null(), "{grid}");
        }
        for grid in ["AA00", "RR99XX", "jn88tq12"] { assert!(valid_grid(grid)); }
    }
    #[test]
    fn revision_changes_remain_visible_and_bad_saved_binding_is_unavailable() {
        let original = retained();
        let mut changed = original.clone();
        changed["revision"] = json!(4);
        changed["snapshot"]["settings_revision"] = json!(12);
        changed["snapshot"]["station_call"] = json!("VK8RX");
        assert_ne!(read(&original)["binding"], read(&changed)["binding"]);
        assert_eq!(read(&changed)["mycall"], "VK8RX");
        for (key, replacement) in [("settings_revision", json!(MAX_SAFE + 1)),
            ("worker_generation", json!(8)), ("workspace_generation", json!("invalid"))] {
            let mut value = original.clone(); value["snapshot"][key] = replacement;
            assert_eq!(read(&value)["state"], "unavailable");
        }
    }
    #[test]
    fn actual_projector_fixture_corpus() {
        let original = retained();
        let mut incomplete = original.clone(); incomplete["snapshot"]["station_call"] = json!("");
        let mut invalidated = original.clone(); invalidated["available"] = json!(false); invalidated["revision"] = json!(4);
        let cases = vec![
            ("current", read(&original), "current"),
            ("incomplete", read(&incomplete), "incomplete"),
            ("unavailable", read(&Value::Null), "unavailable"),
            ("write_invalidated", read(&invalidated), "unavailable"),
            ("workspace_mismatch", project(&original, 7, "00000000-0000-4000-8000-000000000003", Some("normal_logging")), "unavailable"),
            ("contesting", project(&original, 7, WORKSPACE, Some("contesting")), "unavailable"),
        ];
        for (_, value, state) in &cases { assert_eq!(value["state"], *state); }
        if let Some(path) = std::env::var_os("XLOG_IDENTITY_FIXTURE_OUTPUT") {
            let corpus = json!({"fixture_schema":1,"synthetic":true,"workspace_id":WORKSPACE,
                "producer":"companion_identity::project",
                "cases":cases.into_iter().map(|(name, identity, _)| json!({"name":name,"station_identity":identity})).collect::<Vec<_>>()});
            let file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).expect("new fixture file");
            serde_json::to_writer_pretty(file, &corpus).expect("write projector fixtures");
        }
    }
}
