//! xLog-owned, opt-in Normal Logging receive capture. No output or radio writes.
use crate::companion_rx_buffer::{self as rules, Binding, Buffer, Route};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Manager;

static BUFFER: OnceLock<Arc<Mutex<Buffer>>> = OnceLock::new();
static STOP: OnceLock<Mutex<Option<Arc<AtomicBool>>>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
static ROUTE: OnceLock<Mutex<Option<Route>>> = OnceLock::new();
static CONTROL: Mutex<()> = Mutex::new(());
static NATIVE_THREAD: AtomicBool = AtomicBool::new(false);
fn buffer() -> Arc<Mutex<Buffer>> {
    BUFFER
        .get_or_init(|| Arc::new(Mutex::new(Buffer::default())))
        .clone()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn stop(reason: &str) {
    let Ok(_control) = CONTROL.lock() else { return };
    stop_locked(reason);
}
fn stop_locked(reason: &str) {
    let _ = GENERATION.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        Some(n.saturating_add(1))
    });
    if let Ok(mut control) = STOP.get_or_init(|| Mutex::new(None)).lock() {
        if let Some(stop) = control.take() {
            stop.store(true, Ordering::Release);
        }
    }
    if let Ok(mut b) = buffer().lock() {
        b.stop(reason);
    }
    if let Ok(mut route) = ROUTE.get_or_init(|| Mutex::new(None)).lock() {
        *route = None;
    }
}
/// Route invalidation preserves the independently saved automatic binding.
pub fn invalidate_route(reason: &str) {
    stop(reason);
}
pub fn current_route() -> Option<Route> {
    ROUTE.get_or_init(|| Mutex::new(None)).lock().ok()?.clone()
}
pub fn native_running() -> bool {
    NATIVE_THREAD.load(Ordering::Acquire)
}
pub fn status() -> Value {
    buffer()
        .lock()
        .map(|b| json!({"running":b.running(now()),"descriptor":b.descriptor,"reason":b.reason}))
        .unwrap_or(Value::Null)
}
fn routes() -> Vec<Route> {
    if crate::operating_module::selected_worker_value().ok() != Some("normal_logging") {
        return vec![];
    }
    crate::station_telemetry::retained_snapshots()
        .iter()
        .flat_map(|p| {
            ["A", "B"]
                .into_iter()
                .filter_map(move |slice| rules::route_for(p, now(), slice))
        })
        .collect()
}
fn still_current(expected: &Route) -> bool {
    routes().iter().any(|r| r == expected)
}
pub fn read(after: u64) -> Result<Value, String> {
    let (generation, retained) = {
        let _control = CONTROL.lock().map_err(|_| "RX lifecycle unavailable.")?;
        (
            GENERATION.load(Ordering::Acquire),
            ROUTE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .map_err(|_| "RX route unavailable.")?
                .clone(),
        )
    };
    // Retained radio reads remain outside CONTROL: never introduce a reverse
    // lock order between the station owner and Stop. Recheck the captured
    // generation before any effect or audio response.
    let expired = retained.as_ref().is_some_and(|r| !still_current(r));
    let _control = CONTROL.lock().map_err(|_| "RX lifecycle unavailable.")?;
    crate::companion_rx_lifecycle::require_current(generation, GENERATION.load(Ordering::Acquire))?;
    if expired {
        stop_locked("FLEX slice route changed or expired; waiting for the saved audio route.");
    }
    Ok(buffer()
        .lock()
        .map_err(|_| "RX buffer unavailable.")?
        .response(after, now()))
}
pub fn request(
    app: &tauri::AppHandle,
    action: &str,
    params: Value,
    lease: Arc<AtomicBool>,
) -> Result<Value, String> {
    if action == "stop" {
        stop("RX sharing stopped by operator.");
        return read(0).map(without_pcm);
    }
    if action == "status" {
        return read(0).map(without_pcm);
    }
    #[cfg(not(windows))]
    {
        let _ = (app, params, lease);
        return Err("Receive sharing currently requires Windows WASAPI.".into());
    }
    #[cfg(windows)]
    {
        use cpal::traits::DeviceTrait;
        let path = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("companion-rx-binding.json");
        if action == "devices" {
            let devices = crate::contest_recorder_audio::devices()?
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|mut d| {
                    let channel = rules::device_name_channel(d["name"].as_str()?)?;
                    if d["knownTx"] == true || d["inputs"].as_array()?.is_empty() {
                        return None;
                    }
                    let native = crate::contest_recorder_audio::device(d["id"].as_str()?)
                        .ok()?
                        .default_input_config()
                        .ok()?;
                    d["inputChannels"] = json!(native.channels());
                    d["dax_channel"] = json!(channel);
                    Some(d)
                })
                .collect::<Vec<_>>();
            let saved = std::fs::read(&path)
                .ok()
                .filter(|b| b.len() <= 4096)
                .and_then(|b| serde_json::from_slice::<Binding>(&b).ok());
            return Ok(json!({"devices":devices,"routes":routes(),"saved_binding":saved}));
        }
        if !matches!(action, "start" | "start_saved") {
            return Err("Unknown receive sharing setting.".into());
        }
        let binding: Binding = serde_json::from_value(params).map_err(|e| e.to_string())?;
        binding.validate()?;
        let route = routes()
            .into_iter()
            .find(|r| {
                r.radio_id == binding.radio_id
                    && r.receiver_id == binding.receiver_id
                    && r.dax_channel == binding.dax_channel
            })
            .ok_or("The confirmed FLEX slice / DAX route is no longer current.")?;
        let device = crate::contest_recorder_audio::device(&binding.device_id)?;
        if rules::device_name_channel(device.description().map_err(|e| e.to_string())?.name())
            != Some(binding.dax_channel)
        {
            return Err("The selected endpoint is not the confirmed DAX RX channel.".into());
        }
        let format = device.default_input_config().map_err(|e| e.to_string())?;
        if format.sample_rate() != binding.sample_rate
            || format.channels() != binding.source_channels
        {
            return Err(
                "The selected endpoint native rate or channels changed; select its current format."
                    .into(),
            );
        }
        let endpoint = device.id().map_err(|e| e.to_string())?.id().to_owned();
        let control = CONTROL.lock().map_err(|_| "RX lifecycle unavailable.")?;
        if !lease.load(Ordering::Acquire) || !still_current(&route) {
            return Err("Pairing or FLEX receive route changed before capture startup.".into());
        }
        let permit = crate::companion_rx_lifecycle::NativePermit::acquire(&NATIVE_THREAD)?;
        stop_locked("Starting RX sharing.");
        let generation = GENERATION.load(Ordering::Acquire);
        let flag = Arc::new(AtomicBool::new(false));
        *STOP
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "RX owner unavailable.")? = Some(flag.clone());
        *ROUTE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "RX route unavailable.")? = Some(route.clone());
        buffer()
            .lock()
            .map_err(|_| "RX buffer unavailable.")?
            .start(&binding, &route, generation)?;
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let input = binding.clone();
        let input_route = route.clone();
        let input_lease = lease.clone();
        std::thread::Builder::new()
            .name("xlog-companion-rx".into())
            .spawn(move || {
                let _permit = permit;
                capture(
                    input,
                    endpoint,
                    input_route,
                    generation,
                    flag,
                    input_lease,
                    ready_tx,
                );
            })
            .map_err(|e| {
                stop_locked("RX thread could not start.");
                e.to_string()
            })?;
        drop(control);
        let ready = ready_rx.recv_timeout(Duration::from_secs(4));
        let _control = CONTROL.lock().map_err(|_| "RX lifecycle unavailable.")?;
        crate::companion_rx_lifecycle::require_current(
            generation,
            GENERATION.load(Ordering::Acquire),
        )?;
        if !lease.load(Ordering::Acquire) || !still_current(&route) {
            stop_locked("Pairing or FLEX receive route changed during capture startup.");
            return Err("Pairing or FLEX receive route changed during capture startup.".into());
        }
        match ready {
            Ok(Ok(())) => (),
            Ok(Err(e)) => {
                stop_locked(&e);
                return Err(e);
            }
            Err(_) => {
                stop_locked("RX startup timed out.");
                return Err("RX startup timed out.".into());
            }
        }
        // Legacy explicit start retains its old binding; automatic capture persists only stable settings.
        if action == "start" {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    stop_locked("Could not save RX binding.");
                    e.to_string()
                })?;
            }
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&binding).map_err(|e| e.to_string())?,
            )
            .map_err(|e| {
                stop_locked("Could not save RX binding.");
                e.to_string()
            })?;
        }
        Ok(without_pcm(
            buffer()
                .lock()
                .map_err(|_| "RX buffer unavailable.")?
                .response(0, now()),
        ))
    }
}
fn without_pcm(mut value: Value) -> Value {
    value.as_object_mut().map(|v| v.remove("chunks"));
    value
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct Packet {
    first_frame: u64,
    qpc_100ns: u64,
    utc_100ns: u64,
    uncertainty_100ns: u64,
    frames: u32,
    flags: u32,
}
#[cfg(windows)]
extern "C" {
    fn xlog_rx_open(
        endpoint: *const u16,
        rate: u32,
        channels: u16,
        channel: u16,
        result: *mut i32,
    ) -> *mut std::ffi::c_void;
    fn xlog_rx_read(
        owner: *mut std::ffi::c_void,
        samples: *mut i16,
        capacity: u32,
        packet: *mut Packet,
    ) -> i32;
    fn xlog_rx_close(owner: *mut std::ffi::c_void);
}
#[cfg(windows)]
fn capture(
    binding: Binding,
    endpoint: String,
    route: Route,
    generation: u64,
    stop_flag: Arc<AtomicBool>,
    lease: Arc<AtomicBool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) {
    let endpoint: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
    let mut result = 0;
    // The COM object is created, read and destroyed exclusively on this thread.
    let raw = unsafe {
        xlog_rx_open(
            endpoint.as_ptr(),
            binding.sample_rate,
            binding.source_channels,
            binding.selected_channel,
            &mut result,
        )
    };
    if raw.is_null() {
        let _ = ready.send(Err(format!(
            "Exact WASAPI receive endpoint failed ({result:#x})."
        )));
        return;
    }
    let _ = ready.send(Ok(()));
    let mut samples = [0i16; 4096];
    let mut last_route = Instant::now() - Duration::from_millis(100);
    let mut last_packet = Instant::now();
    let mut reason = "RX sharing stopped.".to_owned();
    while !stop_flag.load(Ordering::Acquire)
        && lease.load(Ordering::Acquire)
        && GENERATION.load(Ordering::Acquire) == generation
    {
        if last_route.elapsed() >= Duration::from_millis(100) {
            if !still_current(&route) {
                reason = "FLEX route changed or expired; restart RX sharing.".into();
                break;
            }
            last_route = Instant::now();
        }
        let mut packet = Packet::default();
        let result =
            unsafe { xlog_rx_read(raw, samples.as_mut_ptr(), samples.len() as u32, &mut packet) };
        if result < 0 {
            reason = format!("RX device/timestamp discontinuity ({result:#x}); restart sharing.");
            break;
        }
        if result > 3 {
            reason = "Receive endpoint returned an unknown packet state; restart sharing.".into();
            break;
        }
        if result == 1 || result == 3 {
            let b = buffer();
            let Ok(mut b) = b.lock() else { break };
            if GENERATION.load(Ordering::Acquire) != generation || !lease.load(Ordering::Acquire) {
                break;
            }
            let accepted = if result == 3 {
                b.discard_gap(
                    packet.first_frame,
                    packet.qpc_100ns,
                    packet.utc_100ns,
                    packet.uncertainty_100ns,
                    packet.frames as usize,
                )
                .map(|()| false)
            } else {
                b.push(
                    packet.first_frame,
                    packet.qpc_100ns,
                    packet.utc_100ns,
                    packet.uncertainty_100ns,
                    &samples[..packet.frames as usize],
                )
            };
            let published = match accepted {
                Ok(published) => published,
                Err(error) => {
                    reason = error;
                    break;
                }
            };
            // Neither startup priming nor discarded gaps extend qualified
            // audio liveness. Repeated invalid packets cannot hold RX open.
            if published {
                last_packet = Instant::now();
            } else if last_packet.elapsed() > Duration::from_millis(500) {
                reason = "Receive endpoint stopped producing timestamped samples.".into();
                break;
            }
        } else if last_packet.elapsed() > Duration::from_millis(500) {
            reason = "Receive endpoint stopped producing timestamped samples.".into();
            break;
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    unsafe { xlog_rx_close(raw) };
    if let Ok(_control) = CONTROL.lock() {
        if GENERATION.load(Ordering::Acquire) == generation {
            stop_locked(&reason);
        }
    }
}
