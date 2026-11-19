//! One remembered owner audio route. No second radio owner or default endpoint fallback.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub radio_id: String,
    pub slice: String,
    pub rx_device_id: String,
    pub rx_channel: u16,
    pub tx_device_id: String,
}
impl Config {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.radio_id.as_str(), "radio1" | "radio2")
            || !matches!(self.slice.as_str(), "A" | "B")
            || self.rx_channel >= 16
            || [&self.rx_device_id, &self.tx_device_id]
                .iter()
                .any(|s| s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
        {
            return Err(
                "Choose an observed FLEX A/B slice and exact DAX input, input channel and output."
                    .into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u32,
    config: Config,
    station: String,
}
#[derive(Default)]
struct State {
    loaded: bool,
    saved: Option<Saved>,
    message: Option<String>,
    aligned: Option<String>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static CONTROL: Mutex<()> = Mutex::new(());
static INSTALLED: AtomicBool = AtomicBool::new(false);
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("companion-audio-config.json"))
}
fn load(app: &tauri::AppHandle) -> Result<(), String> {
    let mut s = state().lock().map_err(|_| "Audio settings unavailable")?;
    if s.loaded {
        return Ok(());
    }
    let path = path(app)?;
    if path.exists() {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.len() > 8192 {
            return Err("Saved audio settings exceed their bound".into());
        }
        let saved: Saved = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Saved audio settings are invalid: {e}"))?;
        saved.config.validate()?;
        if saved.version != 1 || saved.station.is_empty() || saved.station.len() > 256 {
            return Err("Saved audio station is invalid".into());
        }
        s.saved = Some(saved);
    }
    s.loaded = true;
    Ok(())
}
pub fn selected() -> Option<Config> {
    state()
        .lock()
        .ok()?
        .saved
        .as_ref()
        .map(|s| s.config.clone())
}
fn projection(saved: &Saved, snapshots: &[Value]) -> Result<Value, String> {
    projection_at(saved, snapshots, now())
}
fn projection_at(saved: &Saved, snapshots: &[Value], at: u64) -> Result<Value, String> {
    let mut found = snapshots.iter().filter(|p| {
        p["radio_id"] == saved.config.radio_id
            && p["provider"] == "direct_flex_lan"
            && p["snapshot"]["selected_station"] == saved.station
    });
    let p = found
        .next()
        .ok_or("Waiting for the saved FLEX station and radio slot")?;
    if found.next().is_some() {
        return Err("Saved radio route is ambiguous".into());
    }
    crate::companion_rx_buffer::route_for(p, at, &saved.config.slice)
        .ok_or("Waiting for fresh observed FLEX slice/DAX telemetry")?;
    Ok(p.clone())
}
pub fn selected_projection() -> Result<Value, String> {
    let saved = state()
        .lock()
        .map_err(|_| "Audio settings unavailable")?
        .saved
        .clone()
        .ok_or("Choose Slice, RX DAX and TX DAX in Audio settings")?;
    projection(&saved, &crate::station_telemetry::retained_snapshots())
}
pub fn valid_tx_device(id: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use cpal::traits::DeviceTrait;
        let d = crate::contest_recorder_audio::device(id)?;
        let description = d.description().map_err(|e| e.to_string())?;
        if !matches!(
            description.name(),
            "DAX Audio TX" | "DAX TX" | "DAX TX (FlexRadio DAX)"
        ) {
            return Err("The saved output is not a DAX TX endpoint".into());
        }
        let f = d.default_output_config().map_err(|e| e.to_string())?;
        if f.channels() != 2
            || f.sample_rate() != 48_000
            || f.sample_format() != cpal::SampleFormat::F32
        {
            return Err("DAX TX requires its 48000 Hz stereo float endpoint format".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        Err("DAX audio requires Windows".into())
    }
}
fn input(
    c: &Config,
    route: &crate::companion_rx_buffer::Route,
) -> Result<crate::companion_rx_buffer::Binding, String> {
    #[cfg(windows)]
    {
        use cpal::traits::DeviceTrait;
        let d = crate::contest_recorder_audio::device(&c.rx_device_id)?;
        if crate::companion_rx_buffer::device_name_channel(
            d.description().map_err(|e| e.to_string())?.name(),
        ) != Some(route.dax_channel)
        {
            return Err(
                "Selected RX DAX endpoint does not match the configured slice's DAX channel".into(),
            );
        }
        let f = d.default_input_config().map_err(|e| e.to_string())?;
        let b = crate::companion_rx_buffer::Binding {
            device_id: c.rx_device_id.clone(),
            radio_id: c.radio_id.clone(),
            receiver_id: route.receiver_id.clone(),
            dax_channel: route.dax_channel,
            sample_rate: f.sample_rate(),
            source_channels: f.channels(),
            selected_channel: c.rx_channel,
            operator_confirmed: true,
        };
        b.validate()?;
        Ok(b)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, route);
        Err("DAX audio requires Windows".into())
    }
}
fn inventory() -> Result<(Vec<Value>, Vec<Value>, Vec<Value>), String> {
    let mut slices = vec![];
    for p in crate::station_telemetry::retained_snapshots() {
        for letter in ["A", "B"] {
            if crate::companion_rx_buffer::route_for(&p, now(), letter).is_some() {
                slices.push(json!({"radio_id":p["radio_id"],"slice":letter,"label":format!("{} · {} · Slice {}",p["radio_id"].as_str().unwrap_or(""),p["snapshot"]["selected_station"].as_str().unwrap_or(""),letter)}));
            }
        }
    }
    let mut rx = vec![];
    let mut tx = vec![];
    #[cfg(windows)]
    {
        use cpal::traits::{DeviceTrait, HostTrait};
        for d in cpal::default_host().devices().map_err(|e| e.to_string())? {
            let (Ok(id), Ok(description)) = (d.id(), d.description()) else {
                continue;
            };
            let id = id.to_string();
            let name = description.name();
            if crate::companion_rx_buffer::device_name_channel(name).is_some() {
                if let Ok(f) = d.default_input_config() {
                    rx.push(json!({"id":id,"name":name,"channels":f.channels()}));
                }
            } else if matches!(name, "DAX Audio TX" | "DAX TX" | "DAX TX (FlexRadio DAX)") {
                if let Ok(f) = d.default_output_config() {
                    if f.channels() == 2
                        && f.sample_rate() == 48000
                        && f.sample_format() == cpal::SampleFormat::F32
                    {
                        tx.push(json!({"id":id,"name":name}));
                    }
                }
            }
        }
    }
    Ok((slices, rx, tx))
}
fn align_tx(app: &tauri::AppHandle, saved: &Saved, p: &Value) -> Result<(), String> {
    let r = &p["snapshot"]["radio"];
    let key = serde_json::to_string(&(
        saved,
        &p["selection_id"],
        &p["selection_generation"],
        &p["worker_generation"],
        &p["snapshot"]["generation"],
    ))
    .map_err(|e| e.to_string())?;
    if r["transmit_vfo"] == saved.config.slice
        && r["vfos"].as_array().is_some_and(|v| {
            v.iter()
                .filter(|v| v["letter"] == saved.config.slice && v["transmit"] == true)
                .count()
                == 1
        })
    {
        state()
            .lock()
            .map_err(|_| "Audio settings unavailable")?
            .aligned = Some(key);
        return Ok(());
    }
    if r["transmitting"] != false {
        return Err("Waiting for idle FLEX before selecting the configured TX slice".into());
    }
    crate::ptt_session::require_idle_for(app, &saved.config.radio_id)?;
    let _output = crate::output_authority::shared().reserve_ordinary()?;
    {
        let s = state().lock().map_err(|_| "Audio settings unavailable")?;
        if s.aligned.as_ref() == Some(&key) {
            return Err("Waiting for confirmed configured FLEX TX slice; no repeated selector command was sent".into());
        }
    }
    let selection = crate::station_selection::owner_for(app, &saved.config.radio_id)?;
    let current = selection.snapshot()?;
    let worker = p["worker_generation"]
        .as_u64()
        .ok_or("Worker generation unavailable")?;
    let q = json!({"id":uuid::Uuid::new_v4().to_string(),"method":"station_radio_control","params":{"provider":"direct_flex_lan","control":"set_split","enabled":saved.config.slice=="B","expected_generation":p["snapshot"]["generation"],"radio_id":saved.config.radio_id}});
    let reply = selection.receive_control_with_dispatch(
        q,
        p["selection_generation"]
            .as_u64()
            .ok_or("Selection generation unavailable")?,
        p["selection_id"]
            .as_str()
            .ok_or("Selection identity unavailable")?,
        current["control_revision"]
            .as_u64()
            .ok_or("Control revision unavailable")?,
        || {
            crate::ptt_session::require_idle_for(app, &saved.config.radio_id)?;
            let fresh = selected_projection()?;
            if fresh["selection_id"] != p["selection_id"]
                || fresh["snapshot"]["generation"] != p["snapshot"]["generation"]
                || fresh["snapshot"]["radio"]["transmitting"] != false
            {
                return Err("FLEX changed before TX slice selection".into());
            }
            crate::station_telemetry::owner_for(app, &saved.config.radio_id)?.control()
        },
        |q| {
            state()
                .lock()
                .map_err(|_| "Audio settings unavailable")?
                .aligned = Some(key);
            crate::worker::request_existing_generation(q, worker)
        },
    )?;
    if reply["ok"] != true {
        return Err("FLEX did not confirm the configured TX slice".into());
    }
    Err("Waiting for fresh readback of the configured TX slice".into())
}
fn reconcile(app: &tauri::AppHandle) -> Result<(), String> {
    let _control = CONTROL.lock().map_err(|_| "Audio settings busy")?;
    load(app)?;
    let saved = state()
        .lock()
        .map_err(|_| "Audio settings unavailable")?
        .saved
        .clone()
        .ok_or("Choose Slice, RX DAX and TX DAX")?;
    let lease = crate::companion_host::active_lease()
        .filter(|v| v.load(Ordering::Acquire))
        .ok_or("Waiting for the companion connection")?;
    if crate::operating_module::selected_worker_value().ok() != Some("normal_logging") {
        return Err("Audio sharing requires normal logging".into());
    }
    let p = projection(&saved, &crate::station_telemetry::retained_snapshots())?;
    let route = crate::companion_rx_buffer::route_for(&p, now(), &saved.config.slice)
        .ok_or("Configured slice is unavailable")?;
    let tx_ready = align_tx(app, &saved, &p);
    if crate::companion_rx::current_route().as_ref() != Some(&route) {
        if crate::companion_rx::current_route().is_some() {
            crate::companion_rx::invalidate_route("Rebinding the saved FLEX audio route");
        }
        if crate::companion_rx::native_running() {
            return Err("Waiting for the previous capture to retire".into());
        }
        let binding = input(&saved.config, &route)?;
        crate::companion_rx::request(
            app,
            "start_saved",
            serde_json::to_value(binding).map_err(|e| e.to_string())?,
            lease,
        )?;
    } else if !crate::companion_rx::native_running() {
        crate::companion_rx::invalidate_route("Restarting the saved FLEX audio route");
        return Err("Waiting for the saved input to restart".into());
    }
    tx_ready?;
    valid_tx_device(&saved.config.tx_device_id)?;
    if crate::companion_rx::status()["running"] != true {
        return Err("Waiting for valid DAX capture timestamps".into());
    }
    Ok(())
}
pub fn tune_confirmed(descriptor: &Value, response: &Value) -> bool {
    let Some(slice) = descriptor["companion_slice"]
        .as_str()
        .filter(|v| matches!(*v, "A" | "B"))
    else {
        return false;
    };
    let Some(hz) = descriptor["frequency_hz"].as_u64() else {
        return false;
    };
    let Some(mode) = descriptor["mode"].as_str() else {
        return false;
    };
    let mode = match mode {
        "FT8" | "FT4" | "FT2" | "RTTY" | "JTTY" | "PSK" | "DIGI" => "DIGU",
        "SSB" if hz < 10_000_000 => "LSB",
        "SSB" => "USB",
        other => other,
    };
    let result = &response["result"];
    let state = &result["state"];
    let radio = &state["radio"];
    response["ok"] == true
        && descriptor["provider"] == "direct_flex_lan"
        && result["confirmed"] == true
        && result["control"] == "digital_tune"
        && result["slice"] == slice
        && state["source"] == "direct_flex_lan"
        && state["generation"] == descriptor["connection_generation"]
        && radio["connected"] == true
        && radio["observed"] == true
        && radio["transmitting"] == false
        && radio["vfos"].as_array().is_some_and(|v| {
            v.iter().filter(|v| v["letter"] == slice).count() == 1
                && v.iter()
                    .any(|v| v["letter"] == slice && v["frequency_hz"] == hz && v["mode"] == mode)
        })
}
/// Used only inside the existing durable companion StationSession receipt.
/// Original operation ID is preserved; no generic manual tune behavior changes.
pub fn execute_tune(
    app: &tauri::AppHandle,
    mut request: Value,
    expected: &Value,
    guard: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    let _control = CONTROL.lock().map_err(|_| "Audio settings busy")?;
    let config = selected().ok_or("Saved audio route unavailable")?;
    let p = selected_projection()?;
    for field in [
        "radio_id",
        "selection_id",
        "selection_generation",
        "worker_generation",
    ] {
        if p[field] != expected[field] {
            return Err("Configured radio changed before tune".into());
        }
    }
    if p["snapshot"]["generation"] != expected["snapshot"]["generation"] {
        return Err("Configured FLEX connection changed before tune".into());
    }
    let expected_route = crate::companion_rx_buffer::route_for(expected, now(), &config.slice)
        .ok_or("Original configured slice evidence expired")?;
    if crate::companion_rx_buffer::route_for(&p, now(), &config.slice).as_ref()
        != Some(&expected_route)
    {
        return Err("Configured slice changed before tune".into());
    }
    let hz = request["params"]["frequency_hz"]
        .as_u64()
        .filter(|v| (100_000..=1_000_000_000).contains(v))
        .ok_or("Invalid tune frequency")?;
    let mode = request["params"]["mode"]
        .as_str()
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 12
                && v.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
        .ok_or("Invalid tune mode")?
        .to_owned();
    let worker = p["worker_generation"]
        .as_u64()
        .ok_or("Worker generation unavailable")?;
    request["method"] = json!("station_radio_control");
    request["params"] = json!({"provider":"direct_flex_lan","control":"digital_tune","slice":config.slice,"frequency_hz":hz,"mode":mode,"expected_generation":p["snapshot"]["generation"],"radio_id":config.radio_id});
    let selected = crate::station_selection::owner_for(app, &config.radio_id)?;
    let current = selected.snapshot()?;
    let _output = crate::output_authority::shared().reserve_ordinary()?;
    selected.receive_control_with_dispatch(
        request,
        p["selection_generation"]
            .as_u64()
            .ok_or("Selection generation unavailable")?,
        p["selection_id"]
            .as_str()
            .ok_or("Selection identity unavailable")?,
        current["control_revision"]
            .as_u64()
            .ok_or("Control revision unavailable")?,
        || {
            guard()?;
            crate::ptt_session::require_idle_for(app, &config.radio_id)?;
            let fresh = selected_projection()?;
            if crate::companion_rx_buffer::route_for(&fresh, now(), &config.slice).as_ref()
                != Some(&expected_route)
                || fresh["snapshot"]["radio"]["transmitting"] != false
            {
                return Err("Configured slice changed or is transmitting".into());
            }
            crate::station_telemetry::owner_for(app, &config.radio_id)?.control()
        },
        |q| {
            guard()?;
            crate::worker::request_existing_generation(q, worker)
        },
    )
}
pub fn install(app: tauri::AppHandle) {
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("xlog-companion-audio".into())
        .spawn(move || loop {
            let result = reconcile(&app);
            if let Ok(mut s) = state().lock() {
                s.message = result.err();
            }
            std::thread::sleep(Duration::from_secs(2));
        });
}
pub fn request(app: &tauri::AppHandle, action: &str, params: Value) -> Result<Value, String> {
    load(app)?;
    if action == "save" {
        let _control = CONTROL.lock().map_err(|_| "Audio settings busy")?;
        let config: Config = serde_json::from_value(params).map_err(|e| e.to_string())?;
        config.validate()?;
        let p = crate::station_telemetry::retained_snapshots()
            .into_iter()
            .find(|p| {
                p["radio_id"] == config.radio_id
                    && crate::companion_rx_buffer::route_for(p, now(), &config.slice).is_some()
            })
            .ok_or("Choose a currently observed FLEX slice")?;
        let station = p["snapshot"]["selected_station"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or("Current FLEX station identity unavailable")?
            .to_owned();
        let route = crate::companion_rx_buffer::route_for(&p, now(), &config.slice)
            .ok_or("Slice route changed")?;
        input(&config, &route)?;
        valid_tx_device(&config.tx_device_id)?;
        let saved = Saved {
            version: 1,
            config,
            station,
        };
        if state()
            .lock()
            .map_err(|_| "Audio settings unavailable")?
            .saved
            .as_ref()
            != Some(&saved)
        {
            let path = path(app)?;
            let parent = path.parent().ok_or("Audio settings path unavailable")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let mut f = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            f.write_all(&serde_json::to_vec_pretty(&saved).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            f.as_file().sync_all().map_err(|e| e.to_string())?;
            f.persist(path).map_err(|e| e.to_string())?;
            crate::companion_tx::stop_all();
            crate::companion_rx::invalidate_route("Saved audio selection changed");
            let mut s = state().lock().map_err(|_| "Audio settings unavailable")?;
            s.saved = Some(saved);
            s.aligned = None;
            s.message = Some("Applying saved audio selection".into());
        }
    } else if !matches!(action, "status" | "devices") {
        return Err("Unknown audio settings action".into());
    }
    let (slices, rx_devices, tx_devices) = inventory()?;
    let s = state().lock().map_err(|_| "Audio settings unavailable")?;
    let configured = s.saved.is_some();
    let ready = configured
        && s.message.is_none()
        && crate::companion_host::active_lease().is_some_and(|v| v.load(Ordering::Acquire))
        && crate::companion_rx::status()["running"] == true;
    Ok(
        json!({"config":s.saved.as_ref().map(|s|&s.config),"slices":slices,"rx_devices":rx_devices,"tx_devices":tx_devices,"state":if !configured{"unconfigured"}else if ready{"ready"}else{"waiting"},"message":s.message}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn saved() -> Saved {
        Saved {
            version: 1,
            station: "Maestro".into(),
            config: Config {
                radio_id: "radio2".into(),
                slice: "B".into(),
                rx_device_id: "exact-rx".into(),
                rx_channel: 1,
                tx_device_id: "exact-tx".into(),
            },
        }
    }
    fn snapshot() -> Value {
        json!({"provider":"direct_flex_lan","radio_id":"radio2","available":true,"received_at_ms":10000,"selection_id":"00000000-0000-4000-8000-000000000001","selection_generation":1,"worker_generation":1,"snapshot":{"selected_station":"Maestro","generation":1,"generated_at":10.0,"heartbeat_at":10.0,"radio":{"connected":true,"observed":true,"transmitting":false,"receive_vfo":null,"vfos":[{"letter":"B","flex_slice_id":7,"flex_client_handle":"0x12","dax_channel":2,"frequency_hz":14074000,"mode":"DIGU"}]}}})
    }
    #[test]
    fn saved_audio_companion_tune_receipt_confirms_only_exact_chosen_slice_not_global_rx() {
        let d = json!({"provider":"direct_flex_lan","companion_slice":"B","frequency_hz":10136000,"mode":"FT8","connection_generation":7});
        let r = json!({"ok":true,"result":{"confirmed":true,"control":"digital_tune","slice":"B","state":{"source":"direct_flex_lan","generation":7,"radio":{"connected":true,"observed":true,"transmitting":false,"receive_vfo":"A","vfos":[{"letter":"B","frequency_hz":10136000,"mode":"DIGU"}]}}}});
        assert!(tune_confirmed(&d, &r));
        for (path, value) in [
            ("/result/slice", json!("A")),
            ("/result/state/generation", json!(8)),
            ("/result/state/radio/transmitting", json!(true)),
            ("/result/state/radio/vfos/0/frequency_hz", json!(7074000)),
            ("/result/state/radio/vfos/0/mode", json!("USB")),
        ] {
            let mut bad = r.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(!tune_confirmed(&d, &bad));
        }
    }
    #[test]
    fn saved_audio_saved_binding_contains_no_ephemeral_receiver_and_rematches_reconnect() {
        let saved = saved();
        let wire = serde_json::to_value(&saved).unwrap();
        assert_eq!(wire["config"].as_object().unwrap().len(), 5);
        assert!(wire.get("receiver_id").is_none());
        let mut p = snapshot();
        assert!(projection_at(&saved, &[p.clone()], 10000).is_ok());
        p["selection_id"] = json!(uuid::Uuid::new_v4().to_string());
        p["snapshot"]["generation"] = json!(2);
        p["snapshot"]["radio"]["vfos"][0]["flex_client_handle"] = json!("0x34");
        assert_eq!(projection_at(&saved, &[p.clone()], 10000).unwrap(), p);
        p["snapshot"]["selected_station"] = json!("Other station");
        assert!(projection_at(&saved, &[p], 10000).is_err());
    }
    #[test]
    fn saved_audio_explicit_slice_never_falls_back_to_active_radio_or_other_slice() {
        let saved = saved();
        let mut p = snapshot();
        p["radio_id"] = json!("radio1");
        assert!(projection_at(&saved, &[p], 10000).is_err());
        let mut p = snapshot();
        p["snapshot"]["radio"]["vfos"][0]["letter"] = json!("A");
        assert!(projection_at(&saved, &[p], 10000).is_err());
        assert!(projection_at(&saved, &[snapshot()], 15001).is_err());
    }
    #[test]
    fn saved_audio_saved_config_rejects_unknown_fields_and_unsupported_slices() {
        let mut c = saved().config;
        assert!(c.validate().is_ok());
        c.slice = "C".into();
        assert!(c.validate().is_err());
        c.slice = "A".into();
        c.rx_channel = 16;
        assert!(c.validate().is_err());
        let mut v = serde_json::to_value(saved().config).unwrap();
        v["receiver_id"] = json!("stale");
        assert!(serde_json::from_value::<Config>(v).is_err());
    }
}
