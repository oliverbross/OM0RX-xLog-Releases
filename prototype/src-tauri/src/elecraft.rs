use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serialport::SerialPort;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::Manager;

pub const BANDS: [&str; 11] = [
    "160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m",
];
const RATES: [u32; 6] = [4800, 9600, 19200, 38400, 57600, 115200];

fn valid_serial_port(port: &str) -> bool {
    if cfg!(target_os = "macos") {
        return ["/dev/cu.", "/dev/tty."].iter().any(|prefix| port.strip_prefix(prefix)
            .is_some_and(|name| !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))));
    }
    port.strip_prefix("COM").and_then(|n| n.parse::<u16>().ok())
        .is_some_and(|n| n > 0 && port == format!("COM{n}"))
}

fn remote_agent_meters(projection: &Value, now: u64) -> Result<Value, String> {
    let fresh = projection["received_at_ms"].as_u64()
        .is_some_and(|at| at <= now && now - at <= 5000);
    let snapshot = &projection["snapshot"];
    if !fresh || projection["available"] != true || projection["provider"] != "direct_agent"
        || snapshot["source"] != "direct_agent" || snapshot["available"] != true {
        return Err("Remote Station Agent telemetry is unavailable or stale.".into());
    }
    snapshot["elecraft"].as_object().map(|value| json!(value))
        .ok_or("Remote Station Agent instrument telemetry is unavailable.".into())
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    #[default]
    Serial,
    Agent,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    kpa_port: String,
    kat_port: String,
    kpa_baud: u32,
    kat_baud: u32,
    names: [String; 3],
    #[serde(default)]
    source: Source,
    #[serde(default = "enabled_by_default")]
    kpa_auto_connect: bool,
    #[serde(default = "enabled_by_default")]
    kat_auto_connect: bool,
    #[serde(default)]
    power_policy: PowerPolicy,
    /// Set the KPA500 and KAT500 band whenever the radio moves to another band.
    #[serde(default = "enabled_by_default")]
    follow_band: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct BandPower { pa_w: u8, barefoot_w: u8, digital_w: u8, pa_allowed: bool, digital_pa_allowed: bool }
#[derive(Clone, Serialize, Deserialize)]
struct PowerPolicy { enabled: bool, bands: BTreeMap<String, BandPower> }
impl Default for PowerPolicy {
    fn default() -> Self { Self { enabled: false, bands: BANDS.iter().map(|band| ((*band).into(), BandPower {
        pa_w: 25, barefoot_w: 100, digital_w: 10, pa_allowed: true, digital_pa_allowed: false,
    })).collect() } }
}
impl PowerPolicy {
    fn validate(&self) -> Result<(), String> {
        if self.bands.len() != BANDS.len() || BANDS.iter().any(|band| !self.bands.contains_key(*band)) ||
            self.bands.values().any(|p| [p.pa_w, p.barefoot_w, p.digital_w].iter().any(|w| !(1..=100).contains(w))) {
            return Err("Set radio drive to 1–100 W for every band.".into());
        }
        Ok(())
    }
}
fn enabled_by_default() -> bool { true }
impl Config {
    fn initial() -> Self {
        let mut config = Self {
            source: if cfg!(target_os = "macos") { Source::Agent } else { Source::Serial },
            kpa_auto_connect: true,
            kat_auto_connect: true,
            follow_band: true,
            power_policy: PowerPolicy::default(),
            kpa_port: String::new(),
            kat_port: String::new(),
            kpa_baud: 38400,
            kat_baud: 38400,
            names: ["Antenna 1".into(), "Antenna 2".into(), "Antenna 3".into()],
        };
        if let Some(agent) = std::env::var_os("APPDATA")
            .and_then(|base| {
                std::fs::read(PathBuf::from(base).join("OM0RX Remote Station/agent.json")).ok()
            })
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        {
            for (key, port, rate) in [
                ("kpa500", &mut config.kpa_port, &mut config.kpa_baud),
                ("kat500", &mut config.kat_port, &mut config.kat_baud),
            ] {
                if let Some(p) = agent[key]["port"].as_str() {
                    *port = p.into();
                }
                if let Some(r) = agent[key]["baudrate"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|r| RATES.contains(r))
                {
                    *rate = r;
                }
            }
            for (i, name) in config.names.iter_mut().enumerate() {
                if let Some(n) = agent["station"]["kat_antenna_names"][(i + 1).to_string()].as_str()
                {
                    *name = n.into();
                }
            }
        }
        config
    }
    fn validate(&self) -> Result<(), String> {
        self.power_policy.validate()?;
        for port in [&self.kpa_port, &self.kat_port] {
            if !port.is_empty() && !valid_serial_port(port) {
                return Err("Choose a serial device for this computer.".into());
            }
        }
        if !self.kpa_port.is_empty() && self.kpa_port == self.kat_port {
            return Err("Choose a different port for each instrument.".into());
        }
        if !RATES.contains(&self.kpa_baud)
            || !RATES.contains(&self.kat_baud)
            || self
                .names
                .iter()
                .any(|s| s.trim().is_empty() || s.chars().count() > 48)
        {
            return Err("Check the baud rates and antenna names.".into());
        }
        Ok(())
    }
}

struct Device {
    port: Box<dyn SerialPort>,
    path: String,
    baud: u32,
    readings: Option<(Instant, BTreeMap<String, String>)>,
}
#[derive(Default)]
struct Pair {
    kpa: Option<Device>,
    kat: Option<Device>,
    touched: [bool; 2],
}
static DEVICES: Mutex<Pair> = Mutex::new(Pair {
    kpa: None,
    kat: None,
    touched: [false; 2],
});
trait Wire: Read + Write {}
impl<T: Read + Write + ?Sized> Wire for T {}
fn send<W: Wire + ?Sized>(wire: &mut W, cmd: &str) -> Result<(), String> {
    wire.write_all(cmd.as_bytes())
        .and_then(|_| wire.flush())
        .map_err(|_| "Serial write failed. Check the cable and reconnect.".into())
}
fn query<W: Wire + ?Sized>(wire: &mut W, key: &str) -> Result<String, String> {
    send(wire, &format!("{key};"))?;
    let until = Instant::now() + Duration::from_millis(600);
    let mut frame = Vec::new();
    let mut byte = [0; 1];
    while Instant::now() < until && frame.len() < 256 {
        match wire.read(&mut byte) {
            Ok(1) if byte[0] == b';' => {
                let value = String::from_utf8_lossy(&frame);
                if let Some(reply) = value.trim().strip_prefix(key) {
                    return Ok(reply.trim().into());
                }
                frame.clear();
            }
            Ok(1) => frame.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            _ => break,
        }
    }
    Err(format!(
        "No {key} readback. Check the port, baud rate and device power."
    ))
}
fn observed<W: Wire + ?Sized>(wire: &mut W, kpa: bool) -> Result<BTreeMap<String, String>, String> {
    let keys: &[&str] = if kpa {
        &[
            "^ON", "^OS", "^BN", "^WS", "^TM", "^VI", "^FL", "^FC", "^SP",
        ]
    } else {
        &[
            "PS", "MD", "AN", "BN", "FC", "VSWR", "VSWRB", "VFWD", "VRFL", "FLT", "TP", "AMPI",
            "ATTN", "BYP",
        ]
    };
    let mut result = BTreeMap::new();
    let power = query(wire, keys[0])?;
    result.insert(keys[0].into(), power.clone());
    if power != "0" {
        for key in &keys[1..] {
            if let Ok(v) = query(wire, key) {
                result.insert((*key).into(), v);
            }
        }
    }
    Ok(result)
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Route {
    enabled: [bool; 3],
    preferred: u8,
}
fn band_code(band: &str) -> Result<String, String> {
    BANDS
        .iter()
        .position(|&b| b == band)
        .map(|n| format!("{n:02}"))
        .ok_or_else(|| "Unknown band.".into())
}
fn routes<W: Wire + ?Sized>(wire: &mut W, band: &str) -> Result<Route, String> {
    let code = band_code(band)?;
    let mut enabled = [false; 3];
    for (i, flag) in enabled.iter_mut().enumerate() {
        *flag = match query(wire, &format!("AE{code}{}", i + 1))?.as_str() {
            "0" => false,
            "1" => true,
            _ => return Err("Invalid antenna enable readback.".into()),
        };
    }
    let preferred = query(wire, &format!("AP{code}"))?
        .parse::<u8>()
        .ok()
        .filter(|n| *n <= 3)
        .ok_or("Invalid preferred antenna readback.")?;
    Ok(Route { enabled, preferred })
}
fn save_route<W: Wire + ?Sized>(wire: &mut W, band: &str, target: &Route) -> Result<Route, String> {
    let code = band_code(band)?;
    if !target.enabled.contains(&true)
        || target.preferred > 3
        || (target.preferred > 0 && !target.enabled[(target.preferred - 1) as usize])
    {
        return Err("Enable at least one antenna and choose an enabled preferred antenna.".into());
    }
    let old = routes(wire, band)?;
    for i in 0..3 {
        if target.enabled[i] && !old.enabled[i] {
            write_checked(wire, &format!("AE{code}{}", i + 1), "1")?;
        }
    }
    if target.preferred != old.preferred {
        write_checked(wire, &format!("AP{code}"), &target.preferred.to_string())?;
    }
    for i in 0..3 {
        if !target.enabled[i] && old.enabled[i] {
            write_checked(wire, &format!("AE{code}{}", i + 1), "0")?;
        }
    }
    let read = routes(wire, band)?;
    if read != *target {
        return Err("Band assignment differs from the tuner. Read the band again.".into());
    }
    Ok(read)
}
fn write_checked<W: Wire + ?Sized>(wire: &mut W, key: &str, value: &str) -> Result<(), String> {
    send(wire, &format!("{key}{value};"))?;
    let mut readback = query(wire, key)?;
    // A band change takes a moment to settle, and the reply may be unpadded
    // ("9" for "09"): compare band numbers and read again briefly before
    // reporting that the change was not confirmed.
    if matches!(key, "^BN" | "BN") {
        let same = |reply: &str| reply.parse::<u8>().ok().is_some_and(|n| value.parse::<u8>() == Ok(n));
        for _ in 0..4 {
            if same(&readback) { return Ok(()); }
            std::thread::sleep(Duration::from_millis(250));
            readback = query(wire, key)?;
        }
        if same(&readback) { return Ok(()); }
    }
    // KPA500 firmware can return a zero-padded fan minimum (e.g. ^FC01;).
    let matches = if key == "^FC" {
        value
            .parse::<u8>()
            .ok()
            .is_some_and(|n| readback.parse::<u8>() == Ok(n))
    } else {
        readback == value
    };
    if !matches {
        return Err(format!(
            "{key} change was not confirmed. Read the device before retrying."
        ));
    }
    Ok(())
}
fn control<W: Wire + ?Sized>(
    wire: &mut W,
    kpa: bool,
    key: &str,
    value: &str,
) -> Result<String, String> {
    let valid = match (kpa, key) {
        (true, "^FC") => ["0", "1", "2", "3", "4", "5", "6"].contains(&value),
        (true, "^SP" | "^OS" | "^ON") | (false, "PS" | "ATTN" | "AMPI") => {
            ["0", "1"].contains(&value)
        }
        (true, "^BN") | (false, "BN") => BANDS
            .iter()
            .any(|band| band_code(band).as_deref() == Ok(value)),
        (false, "AN") => ["1", "2", "3"].contains(&value),
        (false, "MD") => ["A", "M", "B"].contains(&value),
        (false, "BYP") => ["B", "N"].contains(&value),
        _ => false,
    };
    match (kpa, key, value) {
        (true, "^ON", "1") => {
            send(wire, "P")?;
            std::thread::sleep(Duration::from_millis(500));
            if query(wire, "^ON")? != "1" {
                return Err("Power on was not confirmed.".into());
            }
        }
        (true, "^ON", "0") => {
            send(wire, "^ON0;")?;
            return Ok("Power off requested".into());
        }
        (true, "clear", "") => {
            send(wire, "^FLC;")?;
            if query(wire, "^FL")? != "00" {
                return Err("The amplifier still reports a fault.".into());
            }
        }
        (false, "clear", "") => {
            send(wire, "FLTC;")?;
            if query(wire, "FLT")?.parse::<u32>() != Ok(0) {
                return Err("The tuner still reports a fault.".into());
            }
        }
        (false, "tune" | "cancel", "") => {
            send(wire, if key == "tune" { "FTNS;" } else { "CT;" })?;
            return Ok(format!("{} requested", key));
        }
        _ if valid => write_checked(wire, key, value)?,
        _ => return Err("Unknown device setting or value.".into()),
    }
    Ok("Confirmed by device".into())
}
fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map_err(|_| "Saved Elecraft settings could not be read.".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::initial()),
        Err(_) => Err("Cannot read Elecraft settings.".into()),
    }
}
fn run(path: &Path, action: &str, device: &str, params: Value) -> Result<Value, String> {
    run_scoped(path, action, device, params, false)
}
fn run_scoped(path: &Path, action: &str, device: &str, params: Value, startup: bool) -> Result<Value, String> {
    if startup && crate::APP_EXITING.load(std::sync::atomic::Ordering::Acquire) { return Ok(Value::Null); }
    let mut pair = DEVICES
        .lock()
        .map_err(|_| "Serial controller unavailable.")?;
    if startup {
        let config = load(path)?;
        let index = usize::from(device == "kat");
        if !path.is_file() || pair.touched[index] || config.source != Source::Serial
            || if index == 0 { !config.kpa_auto_connect || config.kpa_port.is_empty() || pair.kpa.is_some() }
               else { !config.kat_auto_connect || config.kat_port.is_empty() || pair.kat.is_some() } {
            return Ok(Value::Null);
        }
    } else if action == "save" {
        pair.touched = [true; 2];
    } else if matches!(action, "connect" | "disconnect" | "control" | "save_route") {
        if let Some(index) = ["kpa", "kat"].iter().position(|value| *value == device) { pair.touched[index] = true; }
    }
    if action == "status" {
        return Ok(
            json!({"config": load(path)?, "kpa": pair.kpa.as_ref().map(|d| &d.path),
            "kat": pair.kat.as_ref().map(|d| &d.path)}),
        );
    }
    if action == "settings" {
        return Ok(
            json!({"config":load(path)?,"ports":serialport::available_ports().unwrap_or_default().iter().map(|p|&p.port_name).collect::<Vec<_>>(),"kpa":pair.kpa.as_ref().map(|d|&d.path),"kat":pair.kat.as_ref().map(|d|&d.path)}),
        );
    }
    if action == "save" {
        let config: Config =
            serde_json::from_value(params).map_err(|_| "Invalid serial settings.")?;
        config.validate()?;
        let dir = path.parent().ok_or("Missing settings directory.")?;
        std::fs::create_dir_all(dir).map_err(|_| "Cannot create settings directory.")?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|_| "Cannot stage settings.")?;
        serde_json::to_writer(tmp.as_file_mut(), &config).map_err(|_| "Cannot write settings.")?;
        tmp.as_file()
            .sync_all()
            .map_err(|_| "Cannot flush settings.")?;
        tmp.persist(path).map_err(|_| "Cannot replace settings.")?;
        // Port handles belong to the station configuration, across window lifetimes.
        let release = |slot: &mut Option<Device>, port: &str, baud| {
            if slot
                .as_ref()
                .is_some_and(|d| config.source == Source::Agent || d.path != port || d.baud != baud)
            {
                *slot = None;
            }
        };
        release(&mut pair.kpa, &config.kpa_port, config.kpa_baud);
        release(&mut pair.kat, &config.kat_port, config.kat_baud);
        return Ok(json!(config));
    }
    let kpa = device == "kpa";
    let slot = match device {
        "kpa" => &mut pair.kpa,
        "kat" => &mut pair.kat,
        _ => return Err("Unknown instrument.".into()),
    };
    if action == "disconnect" {
        *slot = None;
        return Ok(Value::Null);
    }
    if action == "connect" {
        let config = load(path)?;
        config.validate()?;
        if config.source != Source::Serial {
            return Err("Select Direct serial ports in Settings before connecting.".into());
        }
        let (port, rate) = if kpa {
            (config.kpa_port, config.kpa_baud)
        } else {
            (config.kat_port, config.kat_baud)
        };
        if port.is_empty() {
            return Err("Save a serial port for this device first.".into());
        }
        *slot = None;
        let serial=serialport::new(&port,rate).timeout(Duration::from_millis(120)).open().map_err(|e|format!("{port} could not be opened ({e}). Check whether the Station Agent or Elecraft Remote owns it."))?;
        serial
            .clear(serialport::ClearBuffer::Input)
            .map_err(|_| "Cannot clear serial input.")?;
        *slot = Some(Device {
            port: serial,
            path: port.clone(),
            baud: rate,
            readings: None,
        });
        return Ok(json!(port));
    }
    if action == "policy_standby" {
        let config = load(path)?;
        if device != "kpa" || config.source != Source::Serial || !config.power_policy.enabled ||
            serde_json::to_value(&config).map_err(|_| "Cannot verify power policy.")? != params["config"] ||
            !slot.as_ref().is_some_and(|d| d.path == config.kpa_port && d.baud == config.kpa_baud) {
            return Err("Power policy changed; standby was not sent.".into());
        }
        let instrument = slot.as_mut().ok_or("Amplifier disconnected.")?;
        let watts = query(instrument.port.as_mut(), "^WS")?;
        if watts.split_whitespace().next().and_then(|v| v.parse::<u32>().ok()) != Some(0) {
            return Err("Amplifier is producing RF; policy waits for receive.".into());
        }
        instrument.readings = None;
        return Ok(json!(control(instrument.port.as_mut(), true, "^OS", "0")?));
    }
    let device = slot.as_mut().ok_or("Connect the instrument first.")?;
    if action == "read" {
        return Ok(json!(shared_readings(device.port.as_mut(), &mut device.readings, kpa, Instant::now())?));
    }
    if matches!(action, "control" | "save_route") { device.readings = None; }
    let serial = device.port.as_mut();
    let field = |k: &str| params[k].as_str().unwrap_or("");
    match action {
        "control" => Ok(json!(control(serial, kpa, field("key"), field("value"))?)),
        "route" if !kpa => Ok(json!(routes(serial, field("band"))?)),
        "save_route" if !kpa => Ok(json!(save_route(
            serial,
            field("band"),
            &serde_json::from_value(params["route"].clone())
                .map_err(|_| "Invalid band assignment.")?
        )?)),
        _ => Err("Unknown instrument operation.".into()),
    }
}

fn shared_readings<W: Wire + ?Sized>(wire: &mut W, cache: &mut Option<(Instant, BTreeMap<String, String>)>,
    kpa: bool, now: Instant) -> Result<BTreeMap<String, String>, String> {
    if let Some((at, readings)) = cache.as_ref() {
        if now.saturating_duration_since(*at) < Duration::from_secs(1) { return Ok(readings.clone()); }
    }
    *cache = None;
    let readings = observed(wire, kpa)?;
    *cache = Some((now, readings.clone()));
    Ok(readings)
}

/// The band an amateur frequency falls in, as the KPA500/KAT500 band list names it.
fn band_for_hz(hz: u64) -> Option<&'static str> {
    const EDGES: [(u64, u64, &str); 11] = [
        (1_800_000, 2_000_000, "160m"), (3_500_000, 4_000_000, "80m"), (5_250_000, 5_450_000, "60m"),
        (7_000_000, 7_300_000, "40m"), (10_100_000, 10_150_000, "30m"), (14_000_000, 14_350_000, "20m"),
        (18_068_000, 18_168_000, "17m"), (21_000_000, 21_450_000, "15m"), (24_890_000, 24_990_000, "12m"),
        (28_000_000, 29_700_000, "10m"), (50_000_000, 54_000_000, "6m"),
    ];
    EDGES.iter().find(|(low, high, _)| (*low..=*high).contains(&hz)).map(|(_, _, band)| *band)
}

static CONFIG_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static FOLLOWED_BAND: Mutex<Option<&'static str>> = Mutex::new(None);
static FOLLOWING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a band change should send: (device, command key, band code) for each
/// connected instrument, or nothing while transmitting or off the bands.
fn band_commands(band: &str, kpa_connected: bool, kat_connected: bool) -> Vec<(&'static str, &'static str, String)> {
    let Ok(code) = band_code(band) else { return Vec::new(); };
    let mut commands = Vec::new();
    if kpa_connected { commands.push(("kpa", "^BN", code.clone())); }
    if kat_connected { commands.push(("kat", "BN", code)); }
    commands
}

/// Radio telemetry: when the radio moves to another band (and is not
/// transmitting), set the band on each connected KPA500/KAT500 so they are
/// ready before the next over. The rotor follows separately from the Active QSO.
pub fn observe(projection: &Value) {
    if crate::APP_EXITING.load(std::sync::atomic::Ordering::Acquire) { return; }
    let Some(path) = CONFIG_PATH.get().cloned() else { return; };
    if projection["snapshot"]["radio"]["transmitting"] == true { return; }
    let Ok(selection) = crate::station_selection::retained_snapshot() else { return; };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_millis() as u64).unwrap_or(0);
    let Ok(observed) = crate::radio_follow::observation(projection, &selection, now) else { return; };
    let Some(band) = band_for_hz(observed.frequency) else { return; };
    if FOLLOWED_BAND.lock().ok().and_then(|value| *value) == Some(band) { return; }
    if FOLLOWING.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    std::thread::spawn(move || {
        let _ = follow_band(&path, band);
        FOLLOWING.store(false, std::sync::atomic::Ordering::Release);
    });
}

fn follow_band(path: &Path, band: &'static str) -> Result<(), String> {
    let config = load(path)?;
    if !config.follow_band || config.source != Source::Serial { return Ok(()); }
    let mut pair = DEVICES.lock().map_err(|_| "Serial controller unavailable.")?;
    let commands = band_commands(band, pair.kpa.is_some(), pair.kat.is_some());
    if commands.is_empty() { return Ok(()); }
    let mut all = true;
    for (device, key, code) in commands {
        let slot = if device == "kpa" { &mut pair.kpa } else { &mut pair.kat };
        let Some(instrument) = slot.as_mut() else { continue; };
        instrument.readings = None;
        if let Err(error) = control(instrument.port.as_mut(), device == "kpa", key, &code) {
            all = false;
            eprintln!("Elecraft {device} band follow to {band} failed: {error}");
        }
    }
    if all { if let Ok(mut followed) = FOLLOWED_BAND.lock() { *followed = Some(band); } }
    Ok(())
}

pub fn install(app: &tauri::AppHandle) -> Result<(), String> {
    let path = app.path().app_data_dir().map_err(|_| "Instrument settings directory unavailable.")?.join("elecraft.json");
    let _ = CONFIG_PATH.set(path.clone());
    // Only explicitly saved local ports are restored. Reading Agent defaults
    // must never implicitly seize the Agent's own serial handles.
    if !path.is_file() { return Ok(()); }
    tauri::async_runtime::spawn_blocking(move || {
        let mut pending = vec!["kpa", "kat"];
        for attempt in 0..4 {
            if attempt > 0 { std::thread::sleep(Duration::from_secs(5)); }
            pending.retain(|device| match run_scoped(&path, "connect", device, Value::Null, true) {
                Ok(_) => false,
                Err(error) => { if attempt == 3 { eprintln!("Saved {device} connection unavailable: {error}"); } true }
            });
            if pending.is_empty() { break; }
        }
    });
    Ok(())
}
fn window_action_allowed(window: &str, action: &str, agent: bool) -> bool {
    match window {
        "main" | "settings" => true,
        "monitor" => if agent { action == "meters" } else { matches!(action, "status" | "read") },
        "elecraft" => if agent { action == "meters" } else {
            matches!(action, "status" | "read" | "control" | "connect" | "disconnect")
        },
        _ => false,
    }
}
#[tauri::command]
pub async fn elecraft_request(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::acceptance::Acceptance>,
    action: String,
    device: String,
    params: Value,
) -> Result<Value, String> {
    if !window_action_allowed(window.label(), &action, false)
        || crate::runtime_profile::isolated(state.path.is_some())
    {
        return Err("Direct instruments require the local station window.".into());
    }
    if window.label() == "elecraft"
        && !matches!(
            action.as_str(),
            "status" | "read" | "control" | "connect" | "disconnect"
        )
    {
        return Err("Change instrument settings in Settings.".into());
    }
    let path = window
        .app_handle()
        .path()
        .app_data_dir()
        .map_err(|_| "Settings directory unavailable.")?
        .join("elecraft.json");
    tauri::async_runtime::spawn_blocking(move || run(&path, &action, &device, params))
        .await
        .map_err(|_| "Serial task stopped.".to_string())?
}
#[cfg(test)]
mod band_follow_tests {
    use super::*;
    #[test]
    fn amateur_frequencies_map_to_the_instrument_band_list() {
        assert_eq!(band_for_hz(1_840_000), Some("160m"));
        assert_eq!(band_for_hz(10_136_000), Some("30m"));
        assert_eq!(band_for_hz(14_074_000), Some("20m"));
        assert_eq!(band_for_hz(28_074_000), Some("10m"));
        assert_eq!(band_for_hz(50_313_000), Some("6m"));
        assert_eq!(band_for_hz(9_000_000), None, "between bands nothing is sent");
        for band in BANDS { assert!(band_code(band).is_ok(), "{band} has an instrument code"); }
    }
    /// Answers each "^BN;" / "BN;" query with the next scripted reply.
    struct ScriptedDevice { replies: std::collections::VecDeque<&'static str>, pending: Vec<u8> }
    impl std::io::Write for ScriptedDevice {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if data.ends_with(b"BN;") { if let Some(reply) = self.replies.pop_front() { self.pending.extend_from_slice(reply.as_bytes()); } }
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl std::io::Read for ScriptedDevice {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.is_empty() { return Err(std::io::ErrorKind::TimedOut.into()); }
            out[0] = self.pending.remove(0); Ok(1)
        }
    }
    #[test]
    fn a_band_change_is_confirmed_when_the_device_settles_or_answers_unpadded() {
        let mut settles = ScriptedDevice { replies: ["BN03;", "BN09;"].into(), pending: Vec::new() };
        assert!(write_checked(&mut settles, "BN", "09").is_ok(), "the old band first, then the new one");
        let mut unpadded = ScriptedDevice { replies: ["^BN9;"].into(), pending: Vec::new() };
        assert!(write_checked(&mut unpadded, "^BN", "09").is_ok());
        let mut stuck = ScriptedDevice { replies: ["BN03;"; 6].into(), pending: Vec::new() };
        assert!(write_checked(&mut stuck, "BN", "09").is_err(), "a band that never changes is still reported");
    }
    #[test]
    fn a_band_change_sets_only_connected_instruments_with_their_own_commands() {
        assert_eq!(band_commands("20m", true, true), vec![("kpa", "^BN", "05".to_string()), ("kat", "BN", "05".to_string())]);
        assert_eq!(band_commands("40m", false, true), vec![("kat", "BN", "03".to_string())]);
        assert!(band_commands("20m", false, false).is_empty());
        assert!(band_commands("2m", true, true).is_empty());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn instrument_projection_requires_fresh_connected_agent_telemetry() {
        let mut projection = serde_json::json!({"available":true,"provider":"direct_agent","received_at_ms":1000,
            "snapshot":{"source":"direct_agent","available":true,"elecraft":{"kpa500":{"connected":true,"output_power_w":0}}}});
        assert_eq!(super::remote_agent_meters(&projection, 2000).unwrap()["kpa500"]["output_power_w"], 0);
        assert!(super::remote_agent_meters(&projection, 6001).is_err());
        assert!(super::remote_agent_meters(&projection, 999).is_err());
        projection["available"] = serde_json::json!(false);
        assert!(super::remote_agent_meters(&projection, 2000).is_err());
        projection["available"] = serde_json::json!(true);
        projection["provider"] = serde_json::json!("direct_flex_lan");
        assert!(super::remote_agent_meters(&projection, 2000).is_err());
    }
    #[test]
    fn serial_paths_match_the_host_platform() {
        for port in ["/dev/cu.usbserial-A123", "/dev/tty.usbmodem101"] {
            assert_eq!(super::valid_serial_port(port), cfg!(target_os = "macos"));
        }
        assert_eq!(super::valid_serial_port("COM16"), !cfg!(target_os = "macos"));
        for port in ["/dev/cu.", "/dev/cu.abc/../../etc/passwd", "/tmp/device", "COM0"] {
            assert!(!super::valid_serial_port(port));
        }
    }
    #[test]
    fn power_policy_is_opt_in_and_rejects_missing_or_unsafe_band_values() {
        let mut p = super::PowerPolicy::default();
        assert!(!p.enabled);
        assert!(p.validate().is_ok());
        p.bands.get_mut("20m").unwrap().pa_w = 101;
        assert!(p.validate().is_err());
        p.bands.get_mut("20m").unwrap().pa_w = 25;
        p.bands.remove("40m");
        assert!(p.validate().is_err());
    }
    use super::*;
    #[test]
    fn station_monitor_can_read_both_instrument_sources_without_acquiring_controls() {
        for action in ["status", "read"] { assert!(window_action_allowed("monitor", action, false)); }
        assert!(window_action_allowed("monitor", "meters", true));
        for action in ["control", "connect", "disconnect", "save", "save_route", "restart"] {
            assert!(!window_action_allowed("monitor", action, false));
            assert!(!window_action_allowed("monitor", action, true));
        }
        assert!(!window_action_allowed("logbook", "read", false));
        assert!(window_action_allowed("elecraft", "control", false));
        assert!(window_action_allowed("settings", "save", true));
    }
    use std::collections::VecDeque;
    #[derive(Default)]
    struct Instrument {
        settings: BTreeMap<String, String>,
        replies: VecDeque<u8>,
        commands: Vec<String>,
    }
    impl Read for Instrument {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if let Some(b) = self.replies.pop_front() {
                buf[0] = b;
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }
    impl Write for Instrument {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let cmd = std::str::from_utf8(buf).unwrap().trim_end_matches(';');
            self.commands.push(cmd.into());
            if let Some(v) = self.settings.get(cmd) {
                self.replies.extend(format!("{cmd}{v};").bytes());
            } else if let Some((key, value)) = self
                .settings
                .iter_mut()
                .find(|(k, _)| cmd.starts_with(k.as_str()))
            {
                *value = if key == "^FC" {
                    format!("0{}", &cmd[key.len()..])
                } else {
                    cmd[key.len()..].into()
                };
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn windows_share_recent_meter_reads_and_expiry_requests_fresh_telemetry() {
        let mut dev = Instrument::default(); dev.settings.insert("^ON".into(), "0".into());
        let mut cache = None; let now = Instant::now();
        let first = shared_readings(&mut dev, &mut cache, true, now).unwrap();
        assert_eq!(shared_readings(&mut dev, &mut cache, true, now + Duration::from_millis(500)).unwrap(), first);
        assert_eq!(dev.commands.len(), 1);
        shared_readings(&mut dev, &mut cache, true, now + Duration::from_secs(1)).unwrap();
        assert_eq!(dev.commands.len(), 2);
        cache = None; // Every control and antenna write invalidates before I/O.
        shared_readings(&mut dev, &mut cache, true, now + Duration::from_millis(1100)).unwrap();
        assert_eq!(dev.commands.len(), 3);
    }
    #[test]
    fn saved_legacy_ports_enable_startup_and_explicit_opt_out_roundtrips() {
        let legacy = json!({"kpa_port":"COM16","kat_port":"COM14","kpa_baud":38400,"kat_baud":38400,
            "names":["Vertical","Yagi","Spare"],"source":"serial"});
        let mut config: Config = serde_json::from_value(legacy).unwrap();
        if cfg!(target_os = "macos") {
            config.kpa_port = "/dev/cu.usbserial-KPA".into();
            config.kat_port = "/dev/cu.usbserial-KAT".into();
        }
        assert!(config.kpa_auto_connect && config.kat_auto_connect);
        assert!(!config.power_policy.enabled);
        config.power_policy.bands.get_mut("20m").unwrap().pa_w = 18;
        config.kat_auto_connect = false;
        let copy: Config = serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
        assert!(copy.kpa_auto_connect && !copy.kat_auto_connect);
        assert_eq!(copy.power_policy.bands["20m"].pa_w, 18);
        assert!(copy.validate().is_ok());
    }
    #[test]
    #[ignore = "requires explicit KPA and KAT serial port coordinates"]
    fn reads_station_ports_without_changing_either_device() {
        let cfg = Config {
            kpa_port: std::env::var("XLOG_TEST_KPA_PORT").expect("KPA port"),
            kat_port: std::env::var("XLOG_TEST_KAT_PORT").expect("KAT port"),
            ..Config::initial()
        };
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("elecraft.json");
        run(&path, "save", "", json!(cfg)).unwrap();
        for (name, band_key) in [("kpa", "^BN"), ("kat", "BN")] {
            run(&path, "connect", name, Value::Null).unwrap();
            let data = run(&path, "read", name, Value::Null).unwrap();
            let band = data[band_key].as_str().unwrap().parse::<usize>().unwrap();
            println!("{name}: {data}");
            if name == "kat" {
                let assignment = run(&path, "route", "kat", json!({"band":BANDS[band]})).unwrap();
                println!("{} readback: {assignment}", BANDS[band]);
            }
            run(&path, "disconnect", name, Value::Null).unwrap();
        }
    }

    #[test]
    fn switches_preferred_before_disabling_former_antenna_and_confirms_device() {
        let mut dev = Instrument::default();
        for (k, v) in [
            ("AE051", "1"),
            ("AE052", "0"),
            ("AE053", "0"),
            ("AP05", "1"),
        ] {
            dev.settings.insert(k.into(), v.into());
        }
        let target = Route {
            enabled: [false, true, false],
            preferred: 2,
        };
        assert_eq!(save_route(&mut dev, "20m", &target).unwrap(), target);
        let writes: Vec<_> = dev
            .commands
            .iter()
            .filter(|s| ["AE0521", "AP052", "AE0510"].contains(&s.as_str()))
            .cloned()
            .collect();
        assert_eq!(writes, ["AE0521", "AP052", "AE0510"]);
        dev.commands.clear();
        assert!(save_route(
            &mut dev,
            "20m",
            &Route {
                enabled: [true, false, false],
                preferred: 2
            }
        )
        .is_err());
        assert!(dev.commands.is_empty());
    }
    #[test]
    fn requires_readback_and_accepts_only_instrument_commands() {
        let mut dev = Instrument::default();
        dev.settings.insert("^OS".into(), "0".into());
        dev.settings.insert("^FC".into(), "00".into());
        assert!(control(&mut dev, true, "^FC", "1").is_ok());
        assert_eq!(dev.settings["^FC"], "01");
        assert_eq!(
            control(&mut dev, true, "^OS", "1").unwrap(),
            "Confirmed by device"
        );
        assert!(control(&mut dev, true, "^OS", "1;^FLC;").is_err());
        assert!(control(&mut dev, false, "^OS", "1").is_err());
        assert!(control(&mut dev, false, "AN", "2").is_err());
    }
}
// Same-user Settings owns the local listener configuration. Ordinary telemetry and policy
// loads still return only public device sections. Reading the credential is an explicit action.
fn local_api_settings(path: &Path, action: &str, input: Option<&Value>) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    static WRITER: Mutex<()> = Mutex::new(());
    let _writer = WRITER
        .lock()
        .map_err(|_| "Local API settings unavailable.")?;
    let old = std::fs::read(path).map_err(|_| "Cannot read the installed Agent profile.")?;
    let mut doc: Value =
        serde_json::from_slice(&old).map_err(|_| "Invalid Agent configuration.")?;
    let revision = |server: &Value| format!("{:x}", Sha256::digest(server.to_string().as_bytes()));
    let summary = |server: &Value, rev: String| json!({"host":server["bind_host"],"port":server["bind_port"],"revision":rev});
    let server = &mut doc["server"];
    match action {
        "api_settings" => return Ok(summary(server, revision(server))),
        "api_token" => {
            return server["control_token"]
                .as_str()
                .map(|t| json!(t))
                .ok_or("Missing local Agent token.".into())
        }
        "save_api" => {}
        _ => return Err("Unknown local API action.".into()),
    }
    let params = input.ok_or("Missing local API settings.")?;
    if params["revision"].as_str() != Some(revision(server).as_str()) {
        return Err("Agent settings changed. Reload before saving.".into());
    }
    let host = params["host"]
        .as_str()
        .filter(|h| matches!(*h, "localhost" | "127.0.0.1" | "::1"))
        .ok_or("Select a loopback address.")?;
    let port = params["port"]
        .as_u64()
        .filter(|p| (1..=65535).contains(p))
        .ok_or("Port must be 1–65535.")?;
    if let Some(token) = params["token"].as_str() {
        if token.chars().count() < 32
            || token.len() > 4096
            || token.chars().any(char::is_whitespace)
        {
            return Err(
                "Use at least 32 characters with no whitespace for the bearer token.".into(),
            );
        }
        server["control_token"] = json!(token);
    }
    server["bind_host"] = json!(host);
    server["bind_port"] = json!(port);
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|_| "Cannot encode Agent settings.")?;
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing Agent directory.")?)
        .map_err(|_| "Cannot stage Agent settings.")?;
    tmp.write_all(&bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| "Cannot write Agent settings.")?;
    if std::fs::read(path).ok().as_deref() != Some(old.as_slice()) {
        return Err("Agent settings changed. Reload before saving.".into());
    }
    tmp.persist(path)
        .map_err(|_| "Cannot replace Agent settings.")?;
    Ok(summary(&doc["server"], revision(&doc["server"])))
}

#[cfg(windows)]
fn installed_agent_action(action: &str, config: Option<&Value>) -> Result<Value, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let shell = PathBuf::from(
        std::env::var_os("SystemRoot").ok_or("Windows system directory unavailable.")?,
    )
    .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut cmd = Command::new(shell);
    cmd.creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let script = match action {
        "discover" => {
            let info = config.ok_or("Enter the FLEX address and port.")?;
            let host = info["host"]
                .as_str()
                .filter(|h| {
                    !h.is_empty()
                        && h.len() <= 255
                        && !h.starts_with('-')
                        && h.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b".:%_-".contains(&c))
                })
                .ok_or("Invalid FLEX host.")?;
            let port = info["port"]
                .as_u64()
                .filter(|p| (1..=65535).contains(p))
                .ok_or("Invalid FLEX port.")?;
            cmd.env("XLOG_DISCOVERY_HOST", host)
                .env("XLOG_DISCOVERY_PORT", port.to_string());
            r#"$link=Join-Path ([Environment]::GetFolderPath('Programs')) 'OM0RX Remote Station/OM0RX Remote Station Agent.lnk'
if (!(Test-Path -LiteralPath $link -PathType Leaf)) {exit 2}
$shortcut=(New-Object -ComObject WScript.Shell).CreateShortcut($link)
if ($shortcut.Arguments -notmatch '-Python "([^"]+)"') {exit 2}
$python=$Matches[1]
Set-Location -LiteralPath $shortcut.WorkingDirectory
& $python -m remote_station.station_discovery --host $env:XLOG_DISCOVERY_HOST --port $env:XLOG_DISCOVERY_PORT
exit $LASTEXITCODE"#
        }
        "restart" => {
            r#"$task='OM0RX Remote Station Agent'
$before=Get-ScheduledTask -TaskName $task -ErrorAction Stop
if ($before.State -eq 'Running') {
    Stop-ScheduledTask -TaskName $task -ErrorAction Stop
    $deadline=[datetime]::UtcNow.AddSeconds(10)
    while ((Get-ScheduledTask -TaskName $task -ErrorAction Stop).State -eq 'Running') {
        if ([datetime]::UtcNow -gt $deadline) {exit 3}
        Start-Sleep -Milliseconds 200
    }
}
Start-ScheduledTask -TaskName $task -ErrorAction Stop"#
        }
        _ => return Err("Unknown Agent operation.".into()),
    };
    let output = cmd
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|_| "Cannot reach the installed Agent tools.")?;
    if !output.status.success() {
        return Err(format!(
            "Station Agent {action} failed. Check the installed Agent and its configuration."
        ));
    }
    if action == "restart" {
        return Ok(Value::Null);
    }
    let found: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Could not read FLEX GUI stations.")?;
    if found["success"] != true {
        return Err("Station discovery failed.".into());
    }
    Ok(json!(found["stations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["station_name"].as_str())
        .collect::<Vec<_>>()))
}

#[tauri::command]
pub async fn elecraft_agent(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::acceptance::Acceptance>,
    action: String,
    config: Option<Value>,
) -> Result<Value, String> {
    if !window_action_allowed(window.label(), &action, true)
        || crate::runtime_profile::isolated(state.path.is_some())
    {
        return Err("Open the local station instrument window.".into());
    }
    if window.label() == "elecraft" && action != "meters" {
        return Err("Change Agent settings in Settings.".into());
    }
    if cfg!(target_os = "macos") {
        if action != "meters" {
            return Err("Use the connected Remote Station Agent in Radio Settings.".into());
        }
        let radio_id = crate::radio_slots::active(&app)?;
        let projection = crate::station_telemetry::retained_snapshots().into_iter()
            .find(|value| value["radio_id"] == radio_id)
            .ok_or("Connect to the Remote Station Agent in Radio Settings.")?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Station clock unavailable.")?.as_millis() as u64;
        return remote_agent_meters(&projection, now);
    }
    #[cfg(windows)]
    if matches!(action.as_str(), "discover" | "restart") {
        return tauri::async_runtime::spawn_blocking(move || {
            installed_agent_action(&action, config.as_ref())
        })
        .await
        .map_err(|_| "Station Agent task stopped.")?;
    }
    let base = std::env::var_os("APPDATA").ok_or("Windows Agent profile not found.")?;
    let path = PathBuf::from(base).join("OM0RX Remote Station/agent.json");
    if matches!(action.as_str(), "api_settings" | "api_token" | "save_api") {
        return local_api_settings(&path, &action, config.as_ref());
    }
    let settings: Value = serde_json::from_slice(
        &std::fs::read(&path).map_err(|_| "The local Station Agent profile is unavailable.")?,
    )
    .map_err(|_| "Invalid Agent configuration.")?;
    let token = settings["server"]["control_token"]
        .as_str()
        .ok_or("Missing local Agent credential.")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| "Cannot initialize local Agent client.")?;
    let host = match settings["server"]["bind_host"]
        .as_str()
        .unwrap_or("127.0.0.1")
    {
        "127.0.0.1" | "localhost" => "127.0.0.1",
        "::1" => "[::1]",
        _ => return Err("The local Agent must listen on a loopback address.".into()),
    };
    let port = settings["server"]["bind_port"].as_u64().unwrap_or(8840);
    let api = format!("http://{host}:{port}/api/v1");
    let request = match action.as_str() {
        "meters" => client.get(format!("{api}/state")),
        "load" => client.get(format!("{api}/config")),
        "save" => {
            let profile = config.ok_or("No reviewed Agent configuration.")?;
            if !profile.is_object() {
                return Err("Invalid Agent settings.".into());
            }
            let body = serde_json::to_vec(&json!({"config":profile}))
                .map_err(|_| "Cannot encode Agent configuration.")?;
            if body.len() > 32768 {
                return Err("Agent configuration is too large.".into());
            }
            client
                .put(format!("{api}/config"))
                .header("Content-Type", "application/json")
                .body(body)
        }
        _ => return Err("Unknown Agent operation.".into()),
    };
    let response = request
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "The local Station Agent did not answer.")?;
    if !response.status().is_success() {
        return Err(format!(
            "The Agent returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Incomplete Agent response.")?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Agent response.")?;
    if data["success"] != true {
        return Err("The Agent did not confirm the operation.".into());
    }
    if action == "meters" {
        let e = &data["state"]["elecraft"];
        let mut result = json!({});
        for (dev, fields) in [
            (
                "kpa500",
                &[
                    "connected",
                    "responsive",
                    "operate",
                    "band",
                    "temperature_c",
                    "current",
                    "voltage",
                    "output_power_w",
                    "swr",
                    "swr_live",
                    "fault",
                    "observed_at",
                    "quality",
                ][..],
            ),
            (
                "kat500",
                &[
                    "connected",
                    "responsive",
                    "antenna",
                    "band",
                    "mode",
                    "frequency_khz",
                    "forward_adc",
                    "reflected_adc",
                    "swr_sample",
                    "bypass_swr",
                    "amp_key_interrupted",
                    "fault",
                    "observed_at",
                    "quality",
                ][..],
            ),
        ] {
            for &key in fields {
                result[dev][key] = e[dev][key].clone();
            }
        }
        return Ok(result);
    }
    editable_agent_config(&data["config"])
}

fn editable_agent_config(c: &Value) -> Result<Value, String> {
    // Only the redacted, editable station sections can cross into the window.
    if !c["schema_version"].is_u64() {
        return Err("The Agent returned no configuration schema version.".into());
    }
    let mut result = json!({});
    for field in [
        "schema_version",
        "flex",
        "kpa500",
        "kat500",
        "policy",
        "commissioned",
    ] {
        result[field] = c[field].clone();
    }
    Ok(result)
}

#[cfg(test)]
mod agent_config_tests {
    use super::*;

    #[test]
    fn listener_settings_preserve_profiles_and_detect_concurrent_token_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent.json");
        let doc = json!({"policy_marker":42,"server":{"bind_host":"localhost","bind_port":8840,"control_token":"a".repeat(40)}});
        std::fs::write(&path, doc.to_string()).unwrap();
        let before = local_api_settings(&path, "api_settings", None).unwrap();
        assert!(before.get("control_token").is_none());
        let mut update =
            json!({"host":"::1","port":8888,"revision":before["revision"],"token":"b".repeat(40)});
        let after = local_api_settings(&path, "save_api", Some(&update)).unwrap();
        assert_eq!(after["port"], 8888);
        assert!(local_api_settings(&path, "save_api", Some(&update)).is_err());
        update["revision"] = after["revision"].clone();
        update["host"] = json!("0.0.0.0");
        assert!(local_api_settings(&path, "save_api", Some(&update)).is_err());
        assert_eq!(
            local_api_settings(&path, "api_token", None).unwrap(),
            json!("b".repeat(40))
        );
        let reloaded: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(reloaded["policy_marker"], 42);
    }

    #[test]
    fn editor_retains_required_schema_and_excludes_server_credentials() {
        let config = json!({"schema_version": 1, "server": {"control_token": "fixture"},
            "commissioned": false, "policy": {"kat_antenna": {"40m": 2}}});
        let edited = editable_agent_config(&config).unwrap();
        assert_eq!(edited["schema_version"], 1);
        assert_eq!(edited["policy"], config["policy"]);
        assert!(edited.get("server").is_none());
        assert!(editable_agent_config(&json!({})).is_err());
    }
}
