//! Read-only projection of the existing station owner. No worker/device calls.
use crate::companion_tx_contract::{Identity, OutputRoute};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commission {
    pub schema_version: u32,
    pub device_id: String,
    pub radio_id: String,
    pub receiver_id: String,
    pub selected_channels: Vec<u16>,
    pub route_epoch: String,
    pub confirmed_call: String,
    pub confirmed_grid: String,
    pub operator_confirmed: bool,
}
impl Commission {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || !self.operator_confirmed
            || self.device_id.is_empty()
            || self.device_id.len() > 1024
            || self.device_id.chars().any(char::is_control)
            || !matches!(self.radio_id.as_str(), "radio1" | "radio2")
            || self.receiver_id.is_empty()
            || self.receiver_id.len() > 256
            || !crate::companion_tx_prepare::canonical_uuid(&self.route_epoch)
            || self.selected_channels.is_empty()
            || self.selected_channels.len() > 2
            || self.selected_channels.iter().any(|c| *c > 1)
            || self.selected_channels.len() == 2
                && self.selected_channels[0] == self.selected_channels[1]
            || self.confirmed_call.is_empty()
            || self.confirmed_call.len() > 64
            || !matches!(self.confirmed_grid.len(), 4 | 6 | 8)
        {
            return Err(
                "Confirm the exact DAX TX stereo channel mapping and saved station identity".into(),
            );
        }
        Ok(())
    }
}
// Worker scheduling can age the envelope without changing the radio route.
// Liveness comes from the bounded actual FLEX observation/heartbeat below.
//
// Callers pass `now_ms` truncated from microseconds, so the true current time
// lies in [now_ms, now_ms + 1). A fractional timestamp taken within that same
// millisecond is not in the future and must not be rejected.
fn not_future(n: f64, now_ms: u64) -> bool {
    n < now_ms as f64 + 1.0
}
fn valid_timestamp(v: &Value, now_ms: u64, seconds: bool) -> bool {
    let n = v.as_f64().map(|n| if seconds { n * 1000.0 } else { n });
    n.is_some_and(|n| n.is_finite() && n >= 0.0 && not_future(n, now_ms))
}
fn fresh_within(v: &Value, now_ms: u64, seconds: bool, max_age_ms: u64) -> bool {
    let n = v.as_f64().map(|n| if seconds { n * 1000.0 } else { n });
    n.is_some_and(|n| {
        n.is_finite()
            && n >= 0.0
            && not_future(n, now_ms)
            && now_ms as f64 - n <= max_age_ms as f64
    })
}
/// The current TX flag may be true for our retained operation. It is not a new
/// acquisition grant: PTT owner preflight independently requires RX before ON.
pub fn route(radio: &Value, c: &Commission, now_ms: u64) -> Option<OutputRoute> {
    route_checked(radio, c, now_ms).ok()
}
pub fn route_checked(
    radio: &Value,
    c: &Commission,
    now_ms: u64,
) -> Result<OutputRoute, &'static str> {
    route_checked_for(radio, c, now_ms, "A")
}
pub fn route_checked_for(
    radio: &Value,
    c: &Commission,
    now_ms: u64,
    letter: &str,
) -> Result<OutputRoute, &'static str> {
    if !matches!(letter, "A" | "B") {
        return Err("Unsupported configured FLEX slice");
    }
    c.validate().map_err(|_| "TX output binding is invalid")?;
    let s = &radio["snapshot"];
    let r = &s["radio"];
    for (failed, reason) in [
        (
            radio["provider"] != "direct_flex_lan",
            "Active radio is not direct FLEX",
        ),
        (
            radio["radio_id"] != c.radio_id,
            "Active radio selection changed",
        ),
        (
            radio["available"] != true,
            "Active radio telemetry is unavailable",
        ),
        (
            !valid_timestamp(&radio["received_at_ms"], now_ms, false),
            "Active radio telemetry timestamp is invalid",
        ),
        (
            !valid_timestamp(&s["generated_at"], now_ms, true),
            "Worker radio snapshot timestamp is invalid",
        ),
        (
            // FLEX waits one second before each ping, then awaits its reply
            // (worker timeout: five seconds). Allow cadence/reply jitter here,
            // still below that timeout; transport and PTT leases stay at 1s.
            !(fresh_within(&r["observed_at"], now_ms, true, 2500)
                || fresh_within(&s["heartbeat_at"], now_ms, true, 2500)),
            "FLEX observation and heartbeat are stale",
        ),
        (
            s["digital_ptt_lease"]["supported"] != true
                || s["digital_ptt_lease"]["max_age_ms"].as_u64() != Some(1000),
            "Worker does not support the required bounded PTT lease",
        ),
        (
            r["connected"] != true || r["observed"] != true,
            "FLEX is not connected with an observed radio state",
        ),
        (
            r["transmit_vfo"] != letter,
            "FLEX transmit configured slice is not selected",
        ),
        (!r["transmitting"].is_boolean(), "FLEX PTT state is unknown"),
    ] {
        if failed {
            return Err(reason);
        }
    }
    let mut candidates = r["vfos"]
        .as_array()
        .ok_or("FLEX slices are unavailable")?
        .iter()
        .filter(|v| v["letter"] == letter);
    let a = candidates
        .next()
        .ok_or("FLEX configured slice is unavailable")?;
    if candidates.next().is_some() || a["transmit"] != true {
        return Err("FLEX configured slice must uniquely own transmit");
    }
    let selection = radio["selection_id"]
        .as_str()
        .ok_or("Active radio selection identity is unavailable")?;
    if !crate::companion_tx_prepare::canonical_uuid(selection) {
        return Err("Active radio selection identity is invalid");
    }
    let client = a["flex_client_handle"]
        .as_str()
        .ok_or("FLEX configured slice client handle is unavailable")?;
    if client.is_empty()
        || client.len() > 32
        || !client.bytes().all(|b| b.is_ascii_hexdigit() || b == b'x')
    {
        return Err("FLEX configured slice client handle is invalid");
    }
    let slice = a["flex_slice_id"]
        .as_u64()
        .filter(|v| *v <= 255)
        .ok_or("FLEX configured slice identifier is unavailable")?;
    let receiver_id = format!("{selection}/{client}/{slice}");
    if receiver_id != c.receiver_id {
        return Err("FLEX receiver identity changed; refresh the current route");
    }
    let frequency = a["frequency_hz"]
        .as_u64()
        .filter(|v| (1..=1_000_000_000_000).contains(v))
        .ok_or("FLEX configured slice frequency is unavailable")?;
    if r["transmit_frequency_hz"].as_u64() != Some(frequency) {
        return Err("FLEX transmit frequency does not match configured slice");
    }
    let mode = a["mode"]
        .as_str()
        .filter(|m| matches!(*m, "DIGU" | "USB"))
        .ok_or("FLEX configured slice must use DIGU or USB")?;
    Ok(OutputRoute {
        radio_id: c.radio_id.clone(),
        receiver_id,
        transmit_slice: letter.into(),
        device_id: c.device_id.clone(),
        route_epoch: c.route_epoch.clone(),
        sample_rate_hz: 48_000,
        channels: 2,
        selected_channels: c.selected_channels.clone(),
        frequency_hz: frequency,
        radio_mode: mode.into(),
    })
}
/// Caller supplies only compatible output metadata; zero or multiple endpoints
/// never fall back to the Windows default output.
pub fn unique_output(ids: Vec<String>) -> Result<String, &'static str> {
    match ids.len() {
        0 => Err("No DAX TX output with 48000 Hz stereo float format is available"),
        1 => Ok(ids.into_iter().next().unwrap()),
        _ => Err("Multiple compatible DAX TX outputs are present; resolve the duplicate endpoints"),
    }
}
pub fn automatic(
    radio: &Value,
    projected: &Value,
    device_id: String,
    epoch: &str,
    now_ms: u64,
) -> Result<(OutputRoute, Identity), &'static str> {
    automatic_for(radio, projected, device_id, epoch, now_ms, "A")
}
pub fn automatic_for(
    radio: &Value,
    projected: &Value,
    device_id: String,
    epoch: &str,
    now_ms: u64,
    letter: &str,
) -> Result<(OutputRoute, Identity), &'static str> {
    if projected["state"] != "current" || projected["source"] != "saved_station_settings" {
        return Err("Current saved station callsign and grid are unavailable");
    }
    let a = radio["snapshot"]["radio"]["vfos"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["letter"] == letter));
    let receiver = a.and_then(|a| {
        Some(format!(
            "{}/{}/{}",
            radio["selection_id"].as_str()?,
            a["flex_client_handle"].as_str()?,
            a["flex_slice_id"].as_u64()?
        ))
    });
    // Placeholder is only a validation input, never a published route: the
    // checked projection below diagnoses missing live identity before equality.
    let c = Commission {
        schema_version: 1,
        device_id,
        radio_id: radio["radio_id"]
            .as_str()
            .ok_or("Active radio identity is unavailable")?
            .into(),
        receiver_id: receiver.unwrap_or_else(|| "unavailable".into()),
        selected_channels: vec![0, 1],
        route_epoch: epoch.into(),
        confirmed_call: projected["mycall"]
            .as_str()
            .ok_or("Saved station callsign is unavailable")?
            .into(),
        confirmed_grid: projected["mygrid"]
            .as_str()
            .ok_or("Saved station grid is unavailable")?
            .into(),
        operator_confirmed: true,
    };
    let route = route_checked_for(radio, &c, now_ms, letter)?;
    let identity =
        identity(projected, &c).ok_or("Saved station identity binding is unavailable")?;
    Ok((route, identity))
}
pub fn identity(projected: &Value, c: &Commission) -> Option<Identity> {
    if projected["state"] != "current"
        || projected["source"] != "saved_station_settings"
        || projected["mycall"] != c.confirmed_call
        || projected["mygrid"] != c.confirmed_grid
    {
        return None;
    }
    let b = &projected["binding"];
    Some(Identity {
        mycall: projected["mycall"].as_str()?.into(),
        mygrid: projected["mygrid"].as_str()?.into(),
        worker_generation: b["worker_generation"].as_u64()?,
        settings_revision: b["settings_revision"].as_u64()?,
        identity_epoch: b["identity_epoch"].as_str()?.into(),
        identity_revision: b["identity_revision"].as_u64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (Value, Commission) {
        let c = Commission {
            schema_version: 1,
            device_id: "Windows:synthetic".into(),
            radio_id: "radio1".into(),
            receiver_id: "00000000-0000-4000-8000-000000000001/0x12/0".into(),
            selected_channels: vec![0, 1],
            route_epoch: "00000000-0000-4000-8000-000000000002".into(),
            confirmed_call: "K1ABC".into(),
            confirmed_grid: "FN42".into(),
            operator_confirmed: true,
        };
        let p = json!({"provider":"direct_flex_lan","radio_id":"radio1","selection_id":"00000000-0000-4000-8000-000000000001",
            "available":true,"received_at_ms":10000,"snapshot":{"allow_transmit":true,"digital_ptt_lease":{"supported":true,"max_age_ms":1000},"generated_at":10.0,
            "radio":{"connected":true,"observed":true,"observed_at":10.0,"transmitting":false,"receive_vfo":"A","transmit_vfo":"A","transmit_frequency_hz":14074000,
            "vfos":[{"letter":"A","receive":true,"transmit":true,"flex_client_handle":"0x12","flex_slice_id":0,"frequency_hz":14074000,"mode":"DIGU"}]}}});
        (p, c)
    }
    #[test]
    fn saved_audio_configured_slice_does_not_require_gui_active_receive_but_requires_real_tx() {
        let (mut p, c) = fixture();
        p["snapshot"]["allow_transmit"] = json!(false);
        p["snapshot"]["radio"]["receive_vfo"] = Value::Null;
        p["snapshot"]["radio"]["vfos"][0]["receive"] = json!(false);
        assert!(route_checked_for(&p, &c, 10000, "A").is_ok());
        p["snapshot"]["radio"]["transmit_vfo"] = json!("B");
        p["snapshot"]["radio"]["vfos"][0]["letter"] = json!("B");
        let route = route_checked_for(&p, &c, 10000, "B").unwrap();
        assert_eq!(route.transmit_slice, "B");
        assert!(route_checked_for(&p, &c, 10000, "A").is_err());
        p["snapshot"]["radio"]["vfos"][0]["transmit"] = json!(false);
        assert!(route_checked_for(&p, &c, 10000, "B").is_err());
    }
    #[test]
    fn timestamps_within_the_truncated_current_millisecond_are_not_future() {
        let (mut p, c) = fixture();
        // Generated at 10000.7 ms, evaluated at 10000.9 ms (now_ms = 10000).
        p["snapshot"]["generated_at"] = json!(10.0007);
        p["snapshot"]["radio"]["observed_at"] = json!(10.0007);
        assert!(route_checked_for(&p, &c, 10000, "A").is_ok());
        // A timestamp beyond the current millisecond is still refused.
        p["snapshot"]["generated_at"] = json!(10.002);
        assert!(route_checked_for(&p, &c, 10000, "A").is_err());
    }
    #[test]
    fn exact_flex_a_route_survives_own_ptt_but_not_stale_or_changed_selection() {
        let (mut p, c) = fixture();
        let expected = route(&p, &c, 10000).unwrap();
        p["snapshot"]["radio"]["transmitting"] = json!(true);
        assert_eq!(route(&p, &c, 10000), Some(expected));
        assert!(route(&p, &c, 12501).is_none());
        p["snapshot"]["radio"]["transmit_vfo"] = json!("B");
        assert!(route(&p, &c, 10000).is_none());
    }
    #[test]
    fn no_substitute_endpoint_channel_mode_or_identity() {
        let (mut p, mut c) = fixture();
        c.selected_channels = vec![1, 1];
        assert!(route(&p, &c, 10000).is_none());
        c.selected_channels = vec![0];
        p["snapshot"]["radio"]["vfos"][0]["mode"] = json!("LSB");
        assert!(route(&p, &c, 10000).is_none());
        assert!(identity(&json!({"state":"current","source":"saved_station_settings","mycall":"K2XYZ","mygrid":"FN42"}),&c).is_none());
    }
    fn saved_identity() -> Value {
        json!({"state":"current","source":"saved_station_settings","mycall":"K1ABC","mygrid":"FN42",
            "binding":{"worker_generation":1,"settings_revision":2,"identity_epoch":"00000000-0000-4000-8000-000000000003","identity_revision":3}})
    }
    #[test]
    fn automatic_route_uses_current_receiver_without_saved_commission() {
        let (mut p, c) = fixture();
        let (first, identity) = automatic(
            &p,
            &saved_identity(),
            "exact-output".into(),
            &c.route_epoch,
            10000,
        )
        .unwrap();
        assert_eq!(first.selected_channels, vec![0, 1]);
        assert_eq!(identity.mycall, "K1ABC");
        p["snapshot"]["radio"]["vfos"][0]["flex_client_handle"] = json!("0x34");
        let (next, _) = automatic(
            &p,
            &saved_identity(),
            "exact-output".into(),
            &c.route_epoch,
            10000,
        )
        .unwrap();
        assert_ne!(first.receiver_id, next.receiver_id);
        assert!(next.receiver_id.ends_with("/0x34/0"));
        assert!(
            route(&p, &c, 10000).is_none(),
            "old explicit handle is not reused"
        );
    }
    #[test]
    fn automatic_output_never_selects_a_default_or_ambiguous_device() {
        assert!(unique_output(vec![]).unwrap_err().starts_with("No DAX TX"));
        assert_eq!(
            unique_output(vec!["exact-output".into()]).unwrap(),
            "exact-output"
        );
        assert!(unique_output(vec!["one".into(), "two".into()])
            .unwrap_err()
            .starts_with("Multiple"));
    }
    #[test]
    fn aged_worker_envelope_preserves_route_while_actual_flex_heartbeat_is_live() {
        let (mut p, c) = fixture();
        let expected = route_checked(&p, &c, 10000).unwrap();
        // The retained worker envelope is 1.2 s old, while its last confirmed
        // FLEX ping is 1.5 s old. Worker scheduling is not radio liveness.
        p["received_at_ms"] = json!(8800);
        p["snapshot"]["generated_at"] = json!(8.8);
        p["snapshot"]["radio"]["observed_at"] = json!(7.0);
        p["snapshot"]["heartbeat_at"] = json!(8.5);
        assert_eq!(
            route_checked(&p, &c, 10000).expect(
                "live FLEX heartbeat must preserve the exact TX route despite worker envelope age"
            ),
            expected
        );
        assert_eq!(
            route_checked(&p, &c, 11001).unwrap_err(),
            "FLEX observation and heartbeat are stale"
        );
    }
    #[test]
    fn heartbeat_cadence_allows_jitter_without_extending_transport_or_ptt_leases() {
        let (mut p, c) = fixture();
        p["snapshot"]["radio"]["observed_at"] = json!(7.0);
        p["snapshot"]["heartbeat_at"] = json!(8.5);
        assert!(route_checked(&p, &c, 10000).is_ok());
        p["snapshot"]["heartbeat_at"] = json!(7.499);
        assert_eq!(
            route_checked(&p, &c, 10000).unwrap_err(),
            "FLEX observation and heartbeat are stale"
        );
        p["snapshot"]["heartbeat_at"] = json!(8.5);
        p["received_at_ms"] = json!(8999);
        assert!(route_checked(&p, &c, 10000).is_ok());
        p["received_at_ms"] = json!(10000);
        p["snapshot"]["generated_at"] = json!(8.999);
        assert!(route_checked(&p, &c, 10000).is_ok());
        p["snapshot"]["generated_at"] = json!(10.0);
        p["snapshot"]["digital_ptt_lease"]["max_age_ms"] = json!(2500);
        assert_eq!(
            route_checked(&p, &c, 10000).unwrap_err(),
            "Worker does not support the required bounded PTT lease"
        );
        p["snapshot"]["digital_ptt_lease"]["max_age_ms"] = json!(1000);
        p["snapshot"]["radio"]["connected"] = json!(false);
        assert_eq!(
            route_checked(&p, &c, 10000).unwrap_err(),
            "FLEX is not connected with an observed radio state"
        );
    }
    #[test]
    fn invalid_worker_envelope_timestamps_still_refuse_a_live_radio() {
        let (p, c) = fixture();
        for (pointer, future, reason) in [
            (
                "/received_at_ms",
                json!(10001),
                "Active radio telemetry timestamp is invalid",
            ),
            (
                "/snapshot/generated_at",
                json!(10.001),
                "Worker radio snapshot timestamp is invalid",
            ),
        ] {
            for value in [Value::Null, json!("10000"), json!(-1), future] {
                let mut changed = p.clone();
                *changed.pointer_mut(pointer).unwrap() = value;
                assert_eq!(route_checked(&changed, &c, 10000).unwrap_err(), reason);
            }
        }
    }
    #[test]
    fn automatic_route_reports_real_rejections_without_relaxing_admission() {
        let (p, c) = fixture();
        for (pointer, value, reason) in [
            (
                "/snapshot/digital_ptt_lease/supported",
                json!(false),
                "Worker does not support the required bounded PTT lease",
            ),
            (
                "/snapshot/radio/transmit_vfo",
                json!("B"),
                "FLEX transmit configured slice is not selected",
            ),
            (
                "/snapshot/radio/vfos/0/flex_client_handle",
                Value::Null,
                "FLEX configured slice client handle is unavailable",
            ),
            (
                "/received_at_ms",
                json!(10001),
                "Active radio telemetry timestamp is invalid",
            ),
        ] {
            let mut changed = p.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                automatic(
                    &changed,
                    &saved_identity(),
                    "exact-output".into(),
                    &c.route_epoch,
                    10000
                )
                .unwrap_err(),
                reason
            );
        }
        assert!(automatic(
            &p,
            &Value::Null,
            "exact-output".into(),
            &c.route_epoch,
            10000
        )
        .is_err());
    }
}
