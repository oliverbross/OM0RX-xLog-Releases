//! Companion final-output owner. Output follows the current active xLog route;
//! explicit client enable is still required before any output or PTT acquisition.
use crate::{
    companion_tx_audio::Signals,
    companion_tx_context::Commission,
    companion_tx_contract::*,
    companion_tx_journal::{Journal, Record},
    companion_tx_prepare::Prepared,
    output_authority::Reservation,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

const MIN_OFFSET: u32 = 100;
const MAX_OFFSET: u32 = 4500;
fn now_us() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .map_err(|_| "TX UTC unavailable".into())
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn event_hash(action: &str, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(action.as_bytes());
    h.update([0]);
    h.update(body);
    format!("{:x}", h.finalize())
}
fn already_presenting(signals: &Signals) -> bool {
    observed_start(signals).is_some()
        && !signals.cancelled.load(Ordering::Acquire)
        && !signals.done.load(Ordering::Acquire)
}
fn observed_start(signals: &Signals) -> Option<u64> {
    crate::companion_tx_timing::observed_start(
        signals.started_unix_us.load(Ordering::Acquire),
        signals.presented_unix_us.load(Ordering::Acquire),
    )
}

fn tune_parameters(q: &Tune) -> Result<(f32, f32), String> {
    let hz = f32::from_bits(q.tone_hz_f32_bits);
    let gain = f32::from_bits(q.gain_f32_bits);
    if !gain.is_finite()
        || gain <= 0.0
        || gain > 0.8
        || !hz.is_finite()
        || hz < MIN_OFFSET as f32
        || hz > MAX_OFFSET as f32
        || q.duration_ms == 0
        || q.duration_ms > 30_000
    {
        return Err("Tune requires100..4500Hz, gain>0..0.8 and1..30000ms".into());
    }
    Ok((hz, gain))
}
fn tune_samples(hz: f32, gain: f32, duration_ms: u32) -> Arc<[f32]> {
    let frames = duration_ms as usize * 48;
    let samples: Arc<[f32]> = (0..frames)
        .map(|n| {
            let ramp = (n.min(frames - 1 - n) as f32 / 240.0).min(1.0);
            (std::f64::consts::TAU * hz as f64 * n as f64 / 48_000.0).sin() as f32 * gain * ramp
        })
        .collect::<Vec<_>>()
        .into();
    samples
}
fn local_enable_revision_matches(current: u64, expected: u64) -> bool {
    current == expected && current != u64::MAX
}
struct State {
    commission: Option<Commission>,
    auto_route_epoch: String,
    output_metadata: Option<(Instant, Result<String, String>)>,
    local_revision: u64,
    output_intent: Option<String>,
    client: Option<String>,
    disarmed: bool,
    binding: Option<Binding>,
    fingerprint: String,
    clock_epoch: String,
    clock_anchor: Option<(Instant, u64)>,
    heartbeat: Option<Instant>,
    enable_events: BTreeMap<String, String>,
}
struct Active {
    operation_id: String,
    ticket: Reservation,
    signals: Arc<Signals>,
    binding: Binding,
    client_id: String,
    body_sha256: String,
    running: AtomicBool,
    completing: AtomicBool,
    releasing: AtomicBool,
}
struct Plan {
    binding: Binding,
    client_id: String,
    operation_id: String,
    event_id: String,
    slot_start_unix_us: u64,
    latest_start_unix_us: u64,
    end_unix_us: u64,
    body_sha256: String,
    samples: Arc<[f32]>,
}
pub struct Owner {
    app: tauri::AppHandle,
    state: Mutex<State>,
    journal: Mutex<Journal>,
    prepared: Mutex<Option<(String, Arc<[f32]>)>>,
    active: Mutex<Option<Arc<Active>>>,
    stopped: Mutex<BTreeMap<String, Operation>>,
}
static OWNER: OnceLock<Mutex<Option<Arc<Owner>>>> = OnceLock::new();
static SETTINGS: Mutex<()> = Mutex::new(());
thread_local! { static EXACT_PTT_DISPATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub fn is_exact_ptt_dispatch() -> bool {
    EXACT_PTT_DISPATCH.with(|v| v.get())
}
fn execute_ptt(
    app: &tauri::AppHandle,
    q: Value,
    t: &crate::ptt_session::Target,
    intent: &str,
) -> Result<Value, String> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EXACT_PTT_DISPATCH.with(|v| v.set(false));
        }
    }
    EXACT_PTT_DISPATCH.with(|v| v.set(true));
    let _reset = Reset;
    crate::ptt_session::execute(app, q, t, intent)
}
/// The time the radio was observed released for an operation whose PTT ON was never requested:
/// the PTT owner holds no receipt for it, and fresh telemetry (at most 1 s old) shows the radio
/// available and not transmitting. `None` whenever that cannot be shown.
fn radio_fresh_and_idle(radio: &Value, now_ms: u64) -> bool {
    radio["available"] == true
        && radio["snapshot"]["radio"]["connected"] == true
        && radio["snapshot"]["radio"]["transmitting"] == false
        && radio["received_at_ms"].as_u64().is_some_and(|t| t <= now_ms && now_ms - t <= 1000)
}
/// The PTT owner already settled this operation's release (a confirmed OFF or a recovery
/// decision), but the TX record could not be closed at the time, e.g. the journal was full.
/// Closed only on fresh idle telemetry as well.
fn ptt_owner_already_released(app: &tauri::AppHandle, operation_id: &str) -> Option<u64> {
    let radio = crate::station_telemetry::owner(app).ok()?.snapshot().ok()?;
    let now = now_us().ok()?;
    (radio_fresh_and_idle(&radio, now / 1000)
        && crate::ptt_session::owner(app).ok()?.owner_released(operation_id).ok()?)
    .then_some(now)
}
fn ptt_prior_app_duty_retired(app: &tauri::AppHandle, operation_id: &str) -> Option<u64> {
    let radio = crate::station_telemetry::owner(app).ok()?.snapshot().ok()?;
    let now = now_us().ok()?;
    crate::ptt_session::owner(app)
        .ok()?
        .retire_prior_app_duty(operation_id, &radio, now / 1000)
        .ok()?
        .then_some(now)
}
fn ptt_never_requested_and_idle(app: &tauri::AppHandle, operation_id: &str) -> Option<u64> {
    let receipt = crate::ptt_session::owner(app).ok()?.receipt(operation_id).ok()?;
    let radio = crate::station_telemetry::owner(app).ok()?.snapshot().ok()?;
    let now = now_us().ok()?;
    never_keyed(&receipt, &radio, now / 1000).then_some(now)
}
fn never_keyed(ptt_receipt: &Value, radio: &Value, now_ms: u64) -> bool {
    ptt_receipt.is_null()
        && radio["available"] == true
        && radio["snapshot"]["radio"]["transmitting"] == false
        && radio["received_at_ms"]
            .as_u64()
            .is_some_and(|t| t <= now_ms && now_ms - t <= 1000)
}
/// Whether UTC stepped relative to the monotonic clock since `anchor`. The
/// tolerance is 50 ms plus 100 ppm of the interval, so a disciplined clock's
/// slew over a long gap between observations is not mistaken for a step.
fn clock_discontinuity(anchor: Option<(Instant, u64)>, mono: Instant, utc_us: u64) -> bool {
    anchor.is_some_and(|(then, wall_us)| {
        let elapsed_us = mono.saturating_duration_since(then).as_micros() as u64;
        utc_us.abs_diff(wall_us.saturating_add(elapsed_us)) > 50_000 + elapsed_us / 10_000
    })
}
/// PTT is keyed this long before the prepared slot, not as soon as the output
/// stream is ready: a commit may arrive many seconds early, and keying then held a
/// silent carrier for the whole wait. One second covers PTT acquisition and its
/// observed confirmation, which must land before the slot's 50 ms start window.
const PTT_LEAD_US: u64 = 1_000_000;
/// How long to wait before keying PTT for a slot, or None when it is time.
fn ptt_key_wait(now_us: u64, slot_start_us: u64) -> Option<Duration> {
    let key_at = slot_start_us.saturating_sub(PTT_LEAD_US);
    (now_us < key_at).then(|| Duration::from_micros(key_at - now_us))
}
fn lease_until(signals: &Signals) -> Result<u64, String> {
    let elapsed = signals.origin.elapsed().as_nanos() as u64;
    let remaining = signals
        .heartbeat_deadline_ns
        .load(Ordering::Acquire)
        .saturating_sub(elapsed)
        / 1_000_000;
    if signals.cancelled.load(Ordering::Acquire)
        || remaining == 0
        || remaining > HEARTBEAT_TIMEOUT_MS
    {
        return Err("Original TX heartbeat lease expired".into());
    }
    Ok(now_us()? / 1000 + remaining)
}
fn execute_lease_renewal(
    app: &tauri::AppHandle,
    q: Value,
    t: &crate::ptt_session::Target,
    exact: &Reservation,
) -> Result<Value, String> {
    crate::output_authority::shared().require_exact(exact)?;
    crate::worker::require_existing_generation(t.worker_generation)?;
    let selected = crate::station_selection::owner_for(app,t.radio_id.as_deref().ok_or("PTT lease slot unavailable")?)?.snapshot()?;
    if selected["provider"] != t.provider
        || selected["selection_id"] != t.selection_id
        || selected["generation"].as_u64() != Some(t.selection_generation)
    {
        return Err("PTT lease selection changed".into());
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EXACT_PTT_DISPATCH.with(|v| v.set(false));
        }
    }
    EXACT_PTT_DISPATCH.with(|v| v.set(true));
    let _reset = Reset;
    crate::worker::request_existing_generation(q, t.worker_generation)
}
pub fn existing() -> Option<Arc<Owner>> {
    OWNER.get()?.lock().ok()?.clone()
}
/// Cancellation is independent of configuration, journal and ordinary HTTP.
pub fn stop_all() {
    if let Some(owner) = existing() {
        owner.cancel();
        if let Ok(mut s) = owner.state.lock() {
            s.local_revision = s.local_revision.saturating_add(1);
            s.disarmed = true;
            s.fingerprint.clear();
            s.client = None;
            s.heartbeat = None;
        }
    }
}
pub fn owner(app: &tauri::AppHandle) -> Result<Arc<Owner>, String> {
    let mut cell = OWNER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "TX owner unavailable")?;
    if let Some(owner) = cell.as_ref() {
        return Ok(owner.clone());
    }
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let journal = Journal::open(&directory.join("companion-digital-tx.jsonl"))?;
    // Legacy commissioning files remain untouched; current active telemetry
    // and the unique compatible output define the normal route.
    let commission = None;
    let stopped = journal.stopped.clone();
    let result = Arc::new(Owner {
        app: app.clone(),
        state: Mutex::new(State {
            commission,
            auto_route_epoch: id(),
            output_metadata: None,
            local_revision: 0,
            output_intent: None,
            client: None,
            disarmed: true,
            binding: None,
            fingerprint: String::new(),
            clock_epoch: id(),
            clock_anchor: None,
            heartbeat: None,
            enable_events: BTreeMap::new(),
        }),
        journal: Mutex::new(journal),
        prepared: Mutex::new(None),
        active: Mutex::new(None),
        stopped: Mutex::new(stopped),
    });
    // Restore only a release duty. No audio object or client enable survives.
    let pending = result
        .journal
        .lock()
        .map_err(|_| "TX journal unavailable")?
        .records
        .values()
        .find(|r| r.unresolved())
        .cloned();
    if let Some(mut r) = pending {
        crate::output_authority::shared().restore_unresolved(&r.ticket)?;
        let signals = Signals::new(Instant::now());
        signals.stop();
        *result
            .active
            .lock()
            .map_err(|_| "TX lifecycle unavailable")? = Some(Arc::new(Active {
            operation_id: r.receipt.operation_id.clone(),
            ticket: r.ticket.clone(),
            signals,
            binding: r.binding.clone(),
            client_id: r.client_id.clone(),
            body_sha256: r.receipt.body_sha256.clone(),
            running: AtomicBool::new(false),
            completing: AtomicBool::new(false),
            releasing: AtomicBool::new(false),
        }));
        if !r.ptt_attempted {
            r.receipt.phase = Phase::Terminal;
            r.receipt.outcome = Outcome::NoStart;
            r.receipt.reason = Some("restart_retired_unstarted_preparation".into());
            result
                .journal
                .lock()
                .map_err(|_| "TX journal unavailable")?
                .append(r.clone())?;
            crate::output_authority::shared().retire_exact(&r.ticket)?;
            *result
                .active
                .lock()
                .map_err(|_| "TX lifecycle unavailable")? = None;
        } else {
            // A keyed operation restored from a previous app run can no longer be addressed by
            // its original companion, and its PTT owner ended with that app. Re-check it once a
            // second for two minutes: `finish` closes it only on the strict idle-radio proof
            // (`ptt_prior_app_duty_retired`) once this app's worker reports fresh telemetry.
            let owner = result.clone();
            std::thread::spawn(move || {
                for _ in 0..120 {
                    std::thread::sleep(Duration::from_secs(1));
                    let Some(a) = owner.active.lock().ok().and_then(|a| a.clone()) else { return };
                    if owner.record(&a.operation_id).map_or(true, |r| !r.unresolved()) { return; }
                    // Only attempt once the radio is reported fresh and idle, so waiting for the
                    // worker to reconnect appends nothing to the journal.
                    let idle = crate::station_telemetry::owner(&owner.app)
                        .and_then(|t| t.snapshot())
                        .ok()
                        .zip(now_us().ok())
                        .is_some_and(|(radio, now)| radio_fresh_and_idle(&radio, now / 1000));
                    if idle {
                        owner.finish(&a, "restart_retained_release");
                    }
                }
            });
        }
    }
    *cell = Some(result.clone());
    Ok(result)
}
/// Local native Settings only. No output/stream/PTT is opened by commissioning.
pub fn settings(
    app: &tauri::AppHandle,
    action: &str,
    params: Value,
    workspace: &str,
    worker: u64,
) -> Result<Value, String> {
    let owner = owner(app)?;
    if action == "disable" || action == "stop" {
        stop_all();
        return settings(app, "status", Value::Null, workspace, worker);
    }
    if action == "status" {
        let s = owner.state.lock().map_err(|_| "TX settings unavailable")?;

        let commissioned = s.commission.clone();
        drop(s);
        let receipts = owner
            .journal
            .lock()
            .map_err(|_| "TX journal unavailable")?
            .records
            .values()
            .filter(|r| r.unresolved())
            .map(|r| r.receipt.clone())
            .collect::<Vec<_>>();
        let (route, identity, _, reason) = owner
            .live(worker, workspace)
            .unwrap_or_else(|e| (None, None, Value::Null, Some(e)));
        return Ok(
            json!({"local_enabled":true,"commission":commissioned,"unresolved":receipts,
            "ready":reason.is_none(),"reason":reason,"route":route,"identity":identity}),
        );
    }
    #[cfg(not(windows))]
    {
        let _ = (params, workspace, worker);
        return Err("Digital TX commissioning requires Windows".into());
    }
    #[cfg(windows)]
    {
        use cpal::traits::DeviceTrait;
        let _settings = SETTINGS
            .try_lock()
            .map_err(|_| "TX settings change in progress")?;
        if action == "devices" {
            let devices = crate::contest_recorder_audio::devices()?
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|d| {
                    matches!(
                        d["name"].as_str(),
                        Some("DAX Audio TX" | "DAX TX" | "DAX TX (FlexRadio DAX)")
                    ) && d["id"]
                        .as_str()
                        .and_then(|id| crate::contest_recorder_audio::device(id).ok())
                        .and_then(|d| d.default_output_config().ok())
                        .is_some_and(|f| {
                            f.channels() == 2
                                && f.sample_rate() == 48_000
                                && f.sample_format() == cpal::SampleFormat::F32
                        })
                })
                .collect::<Vec<_>>();
            let now = now_us()? / 1000;
            let routes = crate::station_telemetry::retained_snapshots()
                .into_iter()
                .filter_map(|r| {
                    let base = crate::companion_rx_buffer::route(&r, now)?;
                    let radio = &r["snapshot"]["radio"];
                    if radio["transmit_vfo"] != "A"
                        || radio["receive_vfo"] != "A"
                        || radio["transmit_frequency_hz"].as_u64() != Some(base.frequency_hz)
                    {
                        return None;
                    }
                    Some(base)
                })
                .collect::<Vec<_>>();
            let identity = crate::companion_identity::project(
                &crate::station_identity::snapshot()?,
                worker,
                workspace,
                crate::operating_module::selected_worker_value().ok(),
            );
            return Ok(json!({"devices":devices,"routes":routes,"identity":identity}));
        }
        if action == "configure" {
            stop_all();
            let mut commission: Commission =
                serde_json::from_value(params).map_err(|_| "Invalid explicit TX commissioning")?;
            // The route epoch is owner-created; never accept caller epoch reuse.
            commission.route_epoch = id();
            commission.validate()?;
            let device = crate::contest_recorder_audio::device(&commission.device_id)?;
            let description = device.description().map_err(|e| e.to_string())?;
            let format = device.default_output_config().map_err(|e| e.to_string())?;
            if !matches!(
                description.name(),
                "DAX Audio TX" | "DAX TX" | "DAX TX (FlexRadio DAX)"
            ) || format.channels() != 2
                || format.sample_rate() != 48_000
                || format.sample_format() != cpal::SampleFormat::F32
            {
                return Err("Exact DAX TX48k stereo float endpoint required".into());
            }
            let radio = crate::station_telemetry::owner(app)?.snapshot()?;
            crate::companion_tx_context::route_checked(&radio, &commission, now_us()? / 1000)?;
            let projected = crate::companion_identity::project(
                &crate::station_identity::snapshot()?,
                worker,
                workspace,
                crate::operating_module::selected_worker_value().ok(),
            );
            crate::companion_tx_context::identity(&projected, &commission)
                .ok_or("Confirm the current saved station callsign and grid")?;
            let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let mut file =
                tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
            use std::io::Write;
            file.write_all(&serde_json::to_vec(&commission).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.as_file().sync_all().map_err(|e| e.to_string())?;
            file.persist(directory.join("companion-tx-binding.json"))
                .map_err(|e| e.to_string())?;
            #[cfg(unix)]
            std::fs::File::open(&directory)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            let mut s = owner.state.lock().map_err(|_| "TX settings unavailable")?;
            s.commission = Some(commission);
            s.binding = None;
            s.fingerprint.clear();
        } else if action == "enable" {
            let revision = owner
                .state
                .lock()
                .map_err(|_| "TX settings unavailable")?
                .local_revision;
            let (route, identity, _, reason) = owner.live(worker, workspace)?;
            if route.is_none() || identity.is_none() {
                return Err(reason.unwrap_or_else(|| "Current TX route is unavailable".into()));
            }
            let journal = owner.journal.lock().map_err(|_| "TX journal unavailable")?;
            if !journal.healthy() || journal.records.values().any(Record::unresolved) {
                return Err("Resolve retained TX release duty before enabling".into());
            }
            drop(journal);
            crate::ptt_session::require_idle(app)?;
            let mut s = owner.state.lock().map_err(|_| "TX settings unavailable")?;
            if !local_enable_revision_matches(s.local_revision, revision) {
                return Err("A later Stop retired local TX enable".into());
            }
            // Compatibility action only; client Work still explicitly arms TX.
            s.client = None;
            s.heartbeat = None;
        } else {
            return Err("Unknown TX setting".into());
        }
        drop(_settings);
        settings(app, "status", Value::Null, workspace, worker)
    }
}
impl Owner {
    fn cancel(&self) {
        if let Some(active) = self.active.lock().ok().and_then(|a| a.clone()) {
            active.signals.stop();
            let _ = crate::output_authority::shared().revoke_exact(&active.ticket);
        }
    }
    fn record(&self, operation: &str) -> Result<Record, String> {
        self.journal
            .lock()
            .map_err(|_| "TX journal unavailable")?
            .records
            .get(operation)
            .cloned()
            .ok_or("Unknown TX operation".into())
    }
    fn append(&self, r: Record) -> Result<(), String> {
        let result = self
            .journal
            .lock()
            .map_err(|_| "TX journal unavailable")?
            .append(r);
        if result.is_err() {
            self.cancel();
        }
        result
    }
    fn output_device(&self) -> Result<String,String> {
        let configured=crate::companion_audio_config::selected().ok_or("Choose Slice, RX DAX and TX DAX in Audio settings")?;
        if let Some((at,Ok(id)))=self.state.lock().map_err(|_|"TX settings unavailable")?.output_metadata.clone() {
            if at.elapsed()<Duration::from_secs(1)&&id==configured.tx_device_id {return Ok(id);}
        }
        let result=crate::companion_audio_config::valid_tx_device(&configured.tx_device_id).map(|()|configured.tx_device_id);
        self.state.lock().map_err(|_|"TX settings unavailable")?.output_metadata=Some((Instant::now(),result.clone()));
        result
    }
    fn live(
        &self,
        worker: u64,
        workspace: &str,
    ) -> Result<(Option<OutputRoute>, Option<Identity>, Value, Option<String>), String> {
        crate::worker::require_existing_generation(worker)?;
        let module = crate::operating_module::selected_worker_value().ok();
        if module != Some("normal_logging") {
            return Ok((
                None,
                None,
                Value::Null,
                Some("Digital TX requires normal logging".into()),
            ));
        }
        let configured=match crate::companion_audio_config::selected(){Some(c)=>c,None=>return Ok((None,None,Value::Null,Some("Choose Slice, RX DAX and TX DAX in Audio settings".into())))};
        let radio=match crate::companion_audio_config::selected_projection(){Ok(p)=>p,Err(e)=>return Ok((None,None,Value::Null,Some(e)))};
        let identity = crate::companion_identity::project(
            &crate::station_identity::snapshot()?,
            worker,
            workspace,
            module,
        );
        let device = match self.output_device() {
            Ok(device) => device,
            Err(reason) => return Ok((None, None, radio, Some(reason))),
        };
        let epoch = self
            .state
            .lock()
            .map_err(|_| "TX settings unavailable")?
            .auto_route_epoch
            .clone();
        match crate::companion_tx_context::automatic_for(
            &radio,
            &identity,
            device,
            &epoch,
            now_us()? / 1000,
            &configured.slice,
        ) {
            Ok((route, identity)) => Ok((Some(route), Some(identity), radio, None)),
            Err(reason) => Ok((None, None, radio, Some(reason.into()))),
        }
    }
    pub fn context(
        &self,
        server: &str,
        workspace: &str,
        worker: u64,
        lease: &AtomicBool,
    ) -> Result<Context, String> {
        let utc = now_us()?;
        let mono = Instant::now();
        let (route, identity, radio, route_reason) = if lease.load(Ordering::Acquire) {
            self.live(worker, workspace)?
        } else {
            (None, None, Value::Null, Some("pairing_revoked".into()))
        };
        let fingerprint = serde_json::to_string(&(
            server,
            workspace,
            &route,
            &identity,
            &radio["selection_id"],
            &radio["selection_generation"],
            &radio["snapshot"]["generation"],
        ))
        .map_err(|e| e.to_string())?;
        let mut s = self.state.lock().map_err(|_| "TX settings unavailable")?;
        let output_intent = crate::output_authority::shared().intent()?;
        let clock_bad = clock_discontinuity(s.clock_anchor, mono, utc);
        if clock_bad {
            s.clock_epoch = id();
            s.disarmed = true;
            s.fingerprint.clear();
            s.client = None;
            s.heartbeat = None;
        }
        // Re-anchor on every evaluation: only a step between consecutive
        // observations is a discontinuity, never NTP slew accumulated since
        // pairing (which used to disarm TX after tens of minutes).
        s.clock_anchor = Some((mono, utc));
        let changed = s.fingerprint != fingerprint
            || s.output_intent.as_ref() != Some(&output_intent)
            || clock_bad;
        if changed {
            s.fingerprint = fingerprint;
            s.output_intent = Some(output_intent);
            s.client = None;
            s.heartbeat = None;
            s.binding = Some(Binding {
                server_instance: server.into(),
                workspace_id: workspace.into(),
                context_id: id(),
                clock_epoch: s.clock_epoch.clone(),
            });
        }
        let healthy_heartbeat = s.heartbeat.is_some_and(|until| until > mono);
        let client_enabled = s.client.is_some() && healthy_heartbeat && !s.disarmed;
        let reason = if !lease.load(Ordering::Acquire) {
            Some("pairing_revoked".to_owned())
        } else {
            route_reason
        };
        let result = Context {
            schema_version: 1,
            binding: s.binding.clone().ok_or("TX context unavailable")?,
            ready: reason.is_none(),
            reason,
            local_enabled: true,
            client_enabled,
            identity,
            route,
            generated_at_unix_ms: utc / 1000,
            max_age_ms: 1000,
            heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
            heartbeat_timeout_ms: HEARTBEAT_TIMEOUT_MS,
            max_prepare_bytes: MAX_PREPARE_BYTES,
            min_offset_hz: MIN_OFFSET,
            max_offset_hz: MAX_OFFSET,
            max_abs_sample_f32_bits: 0.95f32.to_bits(),
        };
        drop(s);
        if changed || !result.ready {
            self.cancel();
        }
        Ok(result)
    }
    fn admit(
        &self,
        b: &Binding,
        client: &str,
        server: &str,
        workspace: &str,
        worker: u64,
        lease: &AtomicBool,
        enabled: bool,
    ) -> Result<Context, String> {
        if !crate::companion_tx_prepare::canonical_uuid(client) {
            return Err("Invalid TX client identity".into());
        }
        let c = self.context(server, workspace, worker, lease)?;
        if c.binding != *b || !c.ready {
            return Err("TX context changed or is unavailable".into());
        }
        if enabled {
            let s = self.state.lock().map_err(|_| "TX settings unavailable")?;
            if !c.client_enabled || s.client.as_deref() != Some(client) {
                return Err("TX client enable/heartbeat expired".into());
            }
        }
        Ok(c)
    }
    fn matches(r: &Record, q: &Operation) -> bool {
        r.binding == q.binding
            && r.client_id == q.client_id
            && r.receipt.operation_id == q.operation_id
            && r.receipt.body_sha256 == q.body_sha256
    }
    fn event(
        &self,
        mut r: Record,
        event: &str,
        action: &str,
        body: &[u8],
    ) -> Result<(Record, bool), String> {
        if !crate::companion_tx_prepare::canonical_uuid(event) {
            return Err("Invalid TX event identity".into());
        }
        let digest = event_hash(action, body);
        if self
            .journal
            .lock()
            .map_err(|_| "TX journal unavailable")?
            .records
            .values()
            .any(|old| {
                old.receipt.operation_id != r.receipt.operation_id && old.events.contains_key(event)
            })
        {
            return Err("TX event belongs to another operation".into());
        }
        if let Some(old) = r.events.get(event) {
            return if *old == digest {
                Ok((r, true))
            } else {
                Err("TX event body conflict".into())
            };
        }
        if r.events.len() >= 32 {
            return Err("TX event journal full; retained operation remains stoppable".into());
        }
        r.events.insert(event.into(), digest);
        self.append(r.clone())?;
        Ok((r, false))
    }
    fn prepare(
        self: &Arc<Self>,
        p: Prepared,
        server: &str,
        workspace: &str,
        worker: u64,
        lease: &AtomicBool,
    ) -> Result<Receipt, String> {
        let d = p.descriptor;
        self.reserve(
            Plan {
                binding: d.binding,
                client_id: d.client_id,
                operation_id: d.operation_id,
                event_id: d.event_id,
                slot_start_unix_us: d.slot_start_unix_us,
                latest_start_unix_us: d.latest_start_unix_us,
                end_unix_us: d.slot_start_unix_us + d.manifest.period_duration_us,
                body_sha256: p.body_sha256,
                samples: p.samples,
            },
            server,
            workspace,
            worker,
            lease,
        )
    }
    fn reserve(
        self: &Arc<Self>,
        p: Plan,
        server: &str,
        workspace: &str,
        worker: u64,
        lease: &AtomicBool,
    ) -> Result<Receipt, String> {
        let d = &p;
        let prior_stop = {
            self.stopped
                .lock()
                .map_err(|_| "TX Stop retention unavailable")?
                .get(&d.operation_id)
                .cloned()
        };
        if let Some(stop) = prior_stop {
            if stop.binding != d.binding
                || stop.client_id != d.client_id
                || stop.body_sha256 != p.body_sha256
            {
                return Err("TX operation conflicts with retained Stop".into());
            }
            let mut journal = self.journal.lock().map_err(|_| "TX journal unavailable")?;
            if let Some(old) = journal.records.get(&d.operation_id) {
                return Ok(old.receipt.clone());
            }
            journal.stop(stop)?;
            let receipt = Receipt {
                schema_version: 1,
                server_instance: d.binding.server_instance.clone(),
                workspace_id: d.binding.workspace_id.clone(),
                operation_id: d.operation_id.clone(),
                body_sha256: p.body_sha256.clone(),
                context_id: d.binding.context_id.clone(),
                clock_epoch: d.binding.clock_epoch.clone(),
                event_id: d.event_id.clone(),
                phase: Phase::Terminal,
                outcome: Outcome::NoStart,
                actual_start_unix_us: None,
                release_observed_unix_us: None,
                reason: Some("stop_retained_before_prepare".into()),
            };
            journal.retire_unprepared(receipt.clone())?;
            return Ok(receipt);
        }
        if let Ok(old) = self.record(&d.operation_id) {
            return if old.receipt.body_sha256 == p.body_sha256
                && old.binding == d.binding
                && old.client_id == d.client_id
            {
                Ok(old.receipt)
            } else {
                Err("TX operation body conflict".into())
            };
        }
        let c = self.admit(
            &d.binding,
            &d.client_id,
            server,
            workspace,
            worker,
            lease,
            true,
        )?;
        let now = now_us()?;
        if d.slot_start_unix_us < now + 100_000 || d.slot_start_unix_us > now + 30_000_000 {
            return Err("Prepare the next slot at least 100 ms before its start".into());
        }
        let target: crate::ptt_session::Target =
            serde_json::from_value(crate::ptt_session::context_for(&self.app,&c.route.as_ref().ok_or("TX route unavailable")?.radio_id)?["target"].clone())
                .map_err(|_| "Exact PTT target unavailable")?;
        crate::ptt_session::require_idle_for(&self.app,&target.radio_id.clone().ok_or("TX slot unavailable")?)?;
        let intent = crate::output_authority::shared().intent()?;
        crate::ptt_session::preflight_leased(&self.app, &target, &intent,&c.route.as_ref().ok_or("TX route unavailable")?.transmit_slice)?;
        let mut journal = self.journal.lock().map_err(|_| "TX journal unavailable")?;
        if journal
            .records
            .values()
            .any(|r| r.events.contains_key(&d.event_id))
        {
            return Err("TX originating event already retained".into());
        }
        if self
            .stopped
            .lock()
            .map_err(|_| "TX Stop retention unavailable")?
            .contains_key(&d.operation_id)
        {
            return Err("TX Stop overtook preparation; no acquisition".into());
        }
        if !journal.healthy()
            || journal.records.len() >= crate::companion_tx_journal::MAX_OPERATIONS
            || journal.records.values().any(Record::unresolved)
            || journal.records.values().any(|r| {
                r.ptt_attempted
                    && d.slot_start_unix_us < r.end_us
                    && r.slot_start_us < d.end_unix_us
            })
        {
            return Err("TX admission occupied or slot already consumed".into());
        }
        let ticket = crate::output_authority::shared().prepare(&d.operation_id, &intent)?;
        let signals = Signals::new(Instant::now());
        signals
            .heartbeat_deadline_ns
            .store(HEARTBEAT_TIMEOUT_MS * 1_000_000, Ordering::Release);
        let active = Arc::new(Active {
            operation_id: d.operation_id.clone(),
            ticket: ticket.clone(),
            signals,
            binding: d.binding.clone(),
            client_id: d.client_id.clone(),
            body_sha256: p.body_sha256.clone(),
            running: AtomicBool::new(false),
            completing: AtomicBool::new(false),
            releasing: AtomicBool::new(false),
        });
        *self.active.lock().map_err(|_| "TX lifecycle unavailable")? = Some(active.clone());
        let receipt = Receipt {
            schema_version: 1,
            server_instance: server.into(),
            workspace_id: workspace.into(),
            operation_id: d.operation_id.clone(),
            body_sha256: p.body_sha256.clone(),
            context_id: d.binding.context_id.clone(),
            clock_epoch: d.binding.clock_epoch.clone(),
            event_id: d.event_id.clone(),
            phase: Phase::Prepared,
            outcome: Outcome::NoStart,
            actual_start_unix_us: None,
            release_observed_unix_us: None,
            reason: None,
        };
        let r = Record {
            binding: d.binding.clone(),
            client_id: d.client_id.clone(),
            route: c.route.ok_or("TX route unavailable")?,
            identity: c.identity.ok_or("TX identity unavailable")?,
            target,
            ticket,
            slot_start_us: d.slot_start_unix_us,
            latest_start_us: d.latest_start_unix_us,
            end_us: d.end_unix_us,
            release_id: id(),
            ptt_attempted: false,
            receipt: receipt.clone(),
            events: BTreeMap::from([(d.event_id.clone(), p.body_sha256.clone())]),
        };
        if let Err(e) = journal.append(r) {
            drop(journal);
            self.cancel();
            return Err(e);
        }
        *self
            .prepared
            .lock()
            .map_err(|_| "TX preparation unavailable")? =
            Some((d.operation_id.clone(), p.samples.clone()));
        drop(journal);
        let owner = self.clone();
        let latest = d.latest_start_unix_us;
        std::thread::spawn(move || loop {
            if active.completing.load(Ordering::Acquire) {
                return;
            }
            let running = active.running.load(Ordering::Acquire);
            if active.signals.cancelled.load(Ordering::Acquire)
                || active.signals.origin.elapsed().as_nanos() as u64
                    >= active.signals.heartbeat_deadline_ns.load(Ordering::Acquire)
                || !running && now_us().map_or(true, |now| now > latest)
            {
                if active.completing.load(Ordering::Acquire) {
                    return;
                }
                active.signals.stop();
                let _ = crate::output_authority::shared().revoke_exact(&active.ticket);
                owner.finish(&active, "preparation_expired_or_cancelled");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        });
        Ok(receipt)
    }
    fn tune(
        self: &Arc<Self>,
        q: Tune,
        body: &[u8],
        server: &str,
        workspace: &str,
        worker: u64,
        lease: Arc<AtomicBool>,
    ) -> Result<Receipt, String> {
        let digest = hash(body);
        if let Ok(r) = self.record(&q.operation_id) {
            return if r.receipt.body_sha256 == digest
                && r.binding == q.binding
                && r.client_id == q.client_id
            {
                Ok(r.receipt)
            } else {
                Err("Tune operation conflict".into())
            };
        }
        for value in [&q.operation_id, &q.event_id, &q.client_id] {
            if !crate::companion_tx_prepare::canonical_uuid(value) {
                return Err("Invalid Tune identity".into());
            }
        }
        let (hz, gain) = tune_parameters(&q)?;
        self.admit(
            &q.binding,
            &q.client_id,
            server,
            workspace,
            worker,
            &lease,
            true,
        )?;
        // Original owner-side adjustment tone; no Nexus/GPL encoder or resampler.
        // The final stereo mapper still uses exactly48k, explicit channels and watchdog.
        let samples = tune_samples(hz, gain, q.duration_ms);
        let slot = now_us()? + 750_000;
        let r = self.reserve(
            Plan {
                binding: q.binding.clone(),
                client_id: q.client_id.clone(),
                operation_id: q.operation_id.clone(),
                event_id: q.event_id,
                slot_start_unix_us: slot,
                latest_start_unix_us: slot + 50_000u64.min(q.duration_ms as u64 * 1000 - 1),
                end_unix_us: slot + q.duration_ms as u64 * 1000,
                body_sha256: digest.clone(),
                samples,
            },
            server,
            workspace,
            worker,
            &lease,
        )?;
        if r.phase != Phase::Prepared {
            return Ok(r);
        }
        let commit = Operation {
            binding: q.binding,
            client_id: q.client_id,
            operation_id: q.operation_id,
            body_sha256: digest,
            event_id: id(),
        };
        let bytes = serde_json::to_vec(&commit).map_err(|e| e.to_string())?;
        self.commit(commit, &bytes, server, workspace, worker, lease)
    }
    fn stop(self: &Arc<Self>, q: Operation, body: &[u8]) -> Result<Receipt, String> {
        crate::companion_tx_journal::validate_stop(&q)?;
        // Mute/revoke before disk/event bookkeeping, including a full event journal.
        if let Some(a) = self.active.lock().ok().and_then(|a| a.clone()).filter(|a| {
            a.operation_id == q.operation_id
                && a.binding == q.binding
                && a.client_id == q.client_id
                && a.body_sha256 == q.body_sha256
        }) {
            a.signals.stop();
            let _ = crate::output_authority::shared().revoke_exact(&a.ticket);
        }
        {
            let mut pending = self
                .stopped
                .lock()
                .map_err(|_| "TX Stop retention unavailable")?;
            if let Some(old) = pending.get(&q.operation_id) {
                if !crate::companion_tx_journal::same_operation(old, &q) {
                    return Err("TX Stop body conflict".into());
                }
            } else {
                if pending.len() >= crate::companion_tx_journal::MAX_OPERATIONS {
                    return Err("TX Stop capacity exhausted".into());
                }
                pending.insert(q.operation_id.clone(), q.clone());
            }
        }
        let mut journal = self.journal.lock().map_err(|_| "TX journal unavailable")?;
        if journal
            .records
            .get(&q.operation_id)
            .is_some_and(|r| !Self::matches(r, &q))
        {
            return Err("Exact retained TX operation mismatch".into());
        }
        journal.stop(q.clone())?;
        if let Some(r) = journal.no_start.get(&q.operation_id) {
            return Ok(r.clone());
        }
        let r = journal
            .records
            .get(&q.operation_id)
            .cloned()
            .ok_or("stop_retained_before_prepare")?;
        drop(journal);
        let (mut r, _) = self.event(r, &q.event_id, "stop", body)?;
        if !r.unresolved() {
            return Ok(r.receipt);
        }
        if !r.ptt_attempted && r.receipt.phase == Phase::Prepared {
            r.receipt.phase = Phase::Terminal;
            r.receipt.outcome = Outcome::NoStart;
            r.receipt.reason = Some("stopped_before_acquisition".into());
            self.append(r.clone())?;
            crate::output_authority::shared().retire_exact(&r.ticket)?;
            *self
                .prepared
                .lock()
                .map_err(|_| "TX preparation unavailable")? = None;
            return Ok(r.receipt);
        }
        if let Some(a) = self
            .active
            .lock()
            .ok()
            .and_then(|a| a.clone())
            .filter(|a| a.operation_id == q.operation_id)
        {
            let owner = self.clone();
            std::thread::spawn(move || owner.finish(&a, "explicit_retained_stop"));
        }
        Ok(r.receipt)
    }
    fn commit(
        self: &Arc<Self>,
        q: Operation,
        body: &[u8],
        server: &str,
        workspace: &str,
        worker: u64,
        lease: Arc<AtomicBool>,
    ) -> Result<Receipt, String> {
        let r = self.record(&q.operation_id)?;
        if !Self::matches(&r, &q) {
            return Err("Exact TX operation mismatch".into());
        }
        let (mut r, duplicate) = self.event(r, &q.event_id, "commit", body)?;
        if duplicate || r.receipt.phase != Phase::Prepared {
            return Ok(r.receipt);
        }
        self.admit(
            &q.binding,
            &q.client_id,
            server,
            workspace,
            worker,
            &lease,
            true,
        )?;
        if now_us()? > r.slot_start_us.saturating_sub(50_000) {
            return Err("Commit arrived too late for bounded output startup".into());
        }
        let a = self
            .active
            .lock()
            .map_err(|_| "TX lifecycle unavailable")?
            .clone()
            .filter(|a| a.operation_id == q.operation_id)
            .ok_or("TX preparation retired")?;
        if self
            .stopped
            .lock()
            .map_err(|_| "TX Stop retention unavailable")?
            .get(&q.operation_id)
            .is_some_and(|s| crate::companion_tx_journal::same_operation(s, &q))
        {
            a.signals.stop();
            return Err("TX Stop retired acquisition".into());
        }
        crate::output_authority::shared().require_exact(&a.ticket)?;
        let samples = self
            .prepared
            .lock()
            .map_err(|_| "TX preparation unavailable")?
            .as_ref()
            .filter(|(id, _)| *id == q.operation_id)
            .map(|(_, s)| s.clone())
            .ok_or("PCM unavailable after restart; never replayed")?;
        r.receipt.phase = Phase::Starting;
        r.receipt.outcome = Outcome::Uncertain;
        self.append(r.clone())?;
        a.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "TX already started")?;
        let owner = self.clone();
        std::thread::spawn(move || {
            owner.run(a.clone(), samples, worker, lease);
            a.running.store(false, Ordering::Release);
        });
        Ok(r.receipt)
    }
    fn run(
        self: Arc<Self>,
        a: Arc<Active>,
        samples: Arc<[f32]>,
        worker: u64,
        lease: Arc<AtomicBool>,
    ) {
        let result = (|| -> Result<(), String> {
            let r = self.record(&a.operation_id)?;
            #[cfg(not(windows))]
            {
                let _ = (&samples, worker, &lease, &r);
                return Err("Digital output requires Windows".into());
            }
            #[cfg(windows)]
            {
                let output = crate::companion_tx_audio::open(
                    &r.route,
                    samples,
                    r.slot_start_us,
                    r.latest_start_us,
                    r.ticket.clone(),
                    a.signals.clone(),
                )?;
                let ready_deadline = Instant::now() + Duration::from_millis(500);
                while !a.signals.ready.load(Ordering::Acquire) {
                    if a.signals.cancelled.load(Ordering::Acquire)
                        || a.signals.failed.load(Ordering::Acquire)
                        || Instant::now() >= ready_deadline
                        || now_us()? > r.slot_start_us.saturating_sub(20_000)
                    {
                        return Err("Native output readiness deadline missed".into());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                // Hold PTT until shortly before the slot while the silent, unkeyed
                // stream keeps running; every retirement check still applies.
                let mut last_check = Instant::now();
                while let Some(wait) = ptt_key_wait(now_us()?, r.slot_start_us) {
                    let elapsed = a.signals.origin.elapsed().as_nanos() as u64;
                    if !lease.load(Ordering::Acquire)
                        || a.signals.cancelled.load(Ordering::Acquire)
                        || a.signals.failed.load(Ordering::Acquire)
                        || elapsed >= a.signals.heartbeat_deadline_ns.load(Ordering::Acquire)
                        || elapsed.saturating_sub(a.signals.callback_ns.load(Ordering::Acquire))
                            > 250_000_000
                        || crate::output_authority::shared().require_exact(&a.ticket).is_err()
                    {
                        return Err("TX lease, output clock or release intent retired before PTT".into());
                    }
                    if last_check.elapsed() >= Duration::from_millis(100) {
                        self.admit(
                            &r.binding,
                            &r.client_id,
                            &r.binding.server_instance,
                            &r.binding.workspace_id,
                            worker,
                            &lease,
                            false,
                        )?;
                        let s = self.state.lock().map_err(|_| "TX heartbeat state unavailable")?;
                        if s.client.as_deref() != Some(&r.client_id)
                            || !s.heartbeat.is_some_and(|until| until > Instant::now())
                        {
                            return Err("Retained TX heartbeat retired before PTT".into());
                        }
                        drop(s);
                        last_check = Instant::now();
                    }
                    std::thread::sleep(wait.min(Duration::from_millis(20)));
                }
                self.admit(
                    &r.binding,
                    &r.client_id,
                    &r.binding.server_instance,
                    &r.binding.workspace_id,
                    worker,
                    &lease,
                    true,
                )?;
                let mut latest = self.record(&a.operation_id)?;
                latest.target = serde_json::from_value(
                    crate::ptt_session::context_for(&self.app,&r.route.radio_id)?["target"].clone(),
                )
                .map_err(|_| "PTT context unavailable")?;
                latest.ptt_attempted = true;
                self.append(latest.clone())?;
                crate::output_authority::shared().require_exact(&a.ticket)?;
                if a.signals.cancelled.load(Ordering::Acquire) {
                    return Err("TX stopped before PTT".into());
                }
                let ptt = crate::ptt_session::owner(&self.app)?;
                let response = ptt.apply_exact_leased(
                    &a.operation_id,
                    latest.target.clone(),
                    &a.ticket,
                    lease_until(&a.signals)?,
                    &r.route.transmit_slice,
                    |t| crate::ptt_session::preflight_leased(&self.app, t, a.ticket.intent_id(),&r.route.transmit_slice),
                    |q, t| execute_ptt(&self.app, q, t, a.ticket.intent_id()),
                )?;
                if response["status"] != "confirmed" {
                    return Err("PTT acquisition not observed; audio remains muted".into());
                }
                crate::output_authority::shared().require_exact(&a.ticket)?;
                if a.signals.cancelled.load(Ordering::Acquire) {
                    return Err("TX stopped during PTT acquisition".into());
                }
                a.signals.enabled.store(true, Ordering::Release);
                let mut last_check = Instant::now();
                let mut recorded_start = false;
                loop {
                    let elapsed = a.signals.origin.elapsed().as_nanos() as u64;
                    if !lease.load(Ordering::Acquire)
                        || a.signals.cancelled.load(Ordering::Acquire)
                        || a.signals.failed.load(Ordering::Acquire)
                        || elapsed >= a.signals.heartbeat_deadline_ns.load(Ordering::Acquire)
                        || elapsed.saturating_sub(a.signals.callback_ns.load(Ordering::Acquire))
                            > 250_000_000
                        || crate::output_authority::shared()
                            .require_exact(&a.ticket)
                            .is_err()
                    {
                        return Err("TX lease, output clock or release intent retired".into());
                    }
                    if last_check.elapsed() >= Duration::from_millis(100) {
                        self.admit(
                            &r.binding,
                            &r.client_id,
                            &r.binding.server_instance,
                            &r.binding.workspace_id,
                            worker,
                            &lease,
                            false,
                        )?;
                        let s = self
                            .state
                            .lock()
                            .map_err(|_| "TX heartbeat state unavailable")?;
                        if s.client.as_deref() != Some(&r.client_id)
                            || !s.heartbeat.is_some_and(|until| until > Instant::now())
                        {
                            return Err("Retained TX heartbeat retired".into());
                        }
                        last_check = Instant::now();
                    }
                    if let Some(start) = observed_start(&a.signals).filter(|_| !recorded_start) {
                        let mut current = self.record(&a.operation_id)?;
                        current.receipt.phase = Phase::Active;
                        current.receipt.actual_start_unix_us = Some(start);
                        self.append(current)?;
                        recorded_start = true;
                    }
                    if a.signals.done.load(Ordering::Acquire) {
                        break;
                    }
                    if now_us()? > r.end_us + 1_000_000 {
                        return Err("Native final-presentation watchdog expired".into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                a.completing.store(true, Ordering::Release);
                a.signals.stop();
                drop(output);
                Ok(())
            }
        })();
        a.signals.stop(); // stream above is dropped before any release attempt.
        self.finish(
            &a,
            result
                .as_ref()
                .err()
                .map(String::as_str)
                .unwrap_or("period_presented"),
        );
    }
    fn finish(&self, a: &Active, reason: &str) {
        let normal = reason == "period_presented"
            && a.completing.load(Ordering::Acquire)
            && a.signals.done.load(Ordering::Acquire)
            && a.signals.output_closed.load(Ordering::Acquire);
        a.signals.stop();
        if !normal {
            let _ = crate::output_authority::shared().revoke_exact(&a.ticket);
        }
        if a.releasing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        struct Reset<'a>(&'a AtomicBool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _release = Reset(&a.releasing);
        let Ok(mut r) = self.record(&a.operation_id) else {
            return;
        };
        if !r.unresolved() {
            return;
        }
        if let Some(start) = observed_start(&a.signals) {
            r.receipt.actual_start_unix_us = Some(start);
        }
        r.receipt.phase = Phase::Terminal;
        r.receipt.reason = Some(reason.into());
        let never_keyed_at = if r.ptt_attempted {
            ptt_never_requested_and_idle(&self.app, &a.operation_id)
        } else {
            None
        };
        let prior_app_retired_at = if r.ptt_attempted && never_keyed_at.is_none() {
            ptt_prior_app_duty_retired(&self.app, &a.operation_id)
                .or_else(|| ptt_owner_already_released(&self.app, &a.operation_id))
        } else {
            None
        };
        if !r.ptt_attempted {
            r.receipt.outcome = Outcome::NoStart;
        } else if let Some(observed) = prior_app_retired_at {
            // The PTT owner belonged to a previous app run and fresh telemetry on the same
            // station shows the radio connected and idle (`retire_prior_app_duty`). No Off can
            // be sent for that owner any more; the idle radio is the observed release.
            r.receipt.outcome = Outcome::ReleaseObserved;
            r.receipt.release_observed_unix_us = Some(observed);
        } else if let Some(observed) = never_keyed_at {
            // `ptt_attempted` is journaled before PTT ON is admitted. When admission then refused,
            // or Stop/an output check intervened first, no ON was ever journaled by the PTT owner
            // (which journals before dispatching), so there is no PTT obligation to release and the
            // OFF path below would fail with "No retained GUI PTT owner", leaving this record
            // Uncertain and blocking every later prepare, a restart included. Fresh telemetry
            // shows the radio not transmitting: the release is observed.
            r.receipt.outcome = Outcome::ReleaseObserved;
            r.receipt.release_observed_unix_us = Some(observed);
        } else {
            let release = crate::ptt_session::owner(&self.app).and_then(|ptt| {
                if let Ok(radio) =
                    crate::station_telemetry::owner(&self.app).and_then(|s| s.snapshot())
                {
                    if radio["received_at_ms"].as_u64().is_some_and(|t| {
                        now_us().is_ok_and(|now| t <= now / 1000 && now / 1000 - t <= 1000)
                    }) {
                        ptt.reconcile(&radio)?;
                    }
                }
                let receipt = if normal
                    && crate::output_authority::shared()
                        .require_exact(&r.ticket)
                        .is_ok()
                {
                    ptt.release_completed_exact(
                        &r.release_id,
                        r.target.clone(),
                        &r.ticket,
                        |t| {
                            crate::ptt_session::preflight(&self.app, t, false, r.ticket.intent_id())
                        },
                        |q, t| execute_ptt(&self.app, q, t, r.ticket.intent_id()),
                    )?
                } else {
                    ptt.apply_exact_admitted(
                        &r.release_id,
                        false,
                        r.target.clone(),
                        &r.ticket,
                        |t| {
                            crate::ptt_session::preflight(&self.app, t, false, r.ticket.intent_id())
                        },
                        |q, t| execute_ptt(&self.app, q, t, r.ticket.intent_id()),
                    )?
                };
                if !ptt.snapshot()?["release_required"].is_null() {
                    return Err("PTT acquisition/release still unresolved".into());
                }
                Ok(receipt)
            });
            if release.is_ok_and(|v| v["status"] == "confirmed")
                && a.signals.output_closed.load(Ordering::Acquire)
            {
                r.receipt.outcome = Outcome::ReleaseObserved;
                r.receipt.release_observed_unix_us = now_us().ok();
            } else {
                r.receipt.outcome = Outcome::Uncertain;
            }
        }
        if self.append(r.clone()).is_ok() && !r.unresolved() {
            if normal && r.receipt.outcome == Outcome::ReleaseObserved {
                // Same lock order as context(): state, then Authority. Context
                // cannot see the expected rotation before this exact successor.
                if let Ok(mut s) = self.state.lock() {
                    if let Ok(Some(next)) =
                        crate::output_authority::shared().complete_exact(&r.ticket)
                    {
                        if s.binding.as_ref() == Some(&r.binding)
                            && s.output_intent.as_deref() == Some(r.ticket.intent_id())
                        {
                            s.output_intent = Some(next);
                        }
                    }
                }
            }
            let _ = crate::output_authority::shared().retire_exact(&r.ticket);
        } else {
            let _ = crate::output_authority::shared().revoke_exact(&r.ticket);
        }
        if let Ok(mut prepared) = self.prepared.lock() {
            if prepared
                .as_ref()
                .is_some_and(|(id, _)| *id == a.operation_id)
            {
                *prepared = None;
            }
        }
    }
    pub fn request(
        self: &Arc<Self>,
        method: &str,
        path: &str,
        body: &[u8],
        server: &str,
        workspace: &str,
        worker: u64,
        lease: Arc<AtomicBool>,
    ) -> Result<Value, String> {
        if method == "GET" && path == "/xlog/v1/tx/context" {
            return serde_json::to_value(self.context(server, workspace, worker, &lease)?)
                .map_err(|e| e.to_string());
        }
        if method == "GET" && path.starts_with("/xlog/v1/tx/status?operation_id=") {
            let operation = &path["/xlog/v1/tx/status?operation_id=".len()..];
            if !crate::companion_tx_prepare::canonical_uuid(operation) {
                return Err("Invalid TX status identity".into());
            }
            let journal = self.journal.lock().map_err(|_| "TX journal unavailable")?;
            let receipt = journal
                .records
                .get(operation)
                .filter(|r| r.binding.workspace_id == workspace)
                .map(|r| r.receipt.clone())
                .or_else(|| {
                    journal
                        .no_start
                        .get(operation)
                        .filter(|r| r.workspace_id == workspace)
                        .cloned()
                });
            return serde_json::to_value(Status {
                schema_version: 1,
                server_instance: server.into(),
                workspace_id: workspace.into(),
                operation_id: operation.into(),
                receipt,
            })
            .map_err(|e| e.to_string());
        }
        if method != "POST" {
            return Err("Unknown digital TX route".into());
        }
        let value = match path {
            "/xlog/v1/tx/prepare" => serde_json::to_value(self.prepare(
                crate::companion_tx_prepare::validate(body, MIN_OFFSET, MAX_OFFSET, 0.95)?,
                server,
                workspace,
                worker,
                &lease,
            )?),
            "/xlog/v1/tx/commit" => serde_json::to_value(self.commit(
                serde_json::from_slice(body).map_err(|_| "Invalid TX commit")?,
                body,
                server,
                workspace,
                worker,
                lease,
            )?),
            "/xlog/v1/tx/stop" => serde_json::to_value(self.stop(
                serde_json::from_slice(body).map_err(|_| "Invalid TX Stop")?,
                body,
            )?),
            "/xlog/v1/tx/tune" => serde_json::to_value(self.tune(
                serde_json::from_slice(body).map_err(|_| "Invalid TX Tune")?,
                body,
                server,
                workspace,
                worker,
                lease,
            )?),
            "/xlog/v1/tx/enable" => {
                let q: Enable = serde_json::from_slice(body).map_err(|_| "Invalid TX enable")?;
                if !crate::companion_tx_prepare::canonical_uuid(&q.event_id) {
                    return Err("Invalid enable event".into());
                }
                let digest = event_hash("enable", body);
                let previous = self
                    .state
                    .lock()
                    .map_err(|_| "TX settings unavailable")?
                    .enable_events
                    .get(&q.event_id)
                    .cloned();
                if let Some(old) = previous {
                    if old != digest {
                        return Err("TX enable event body conflict".into());
                    }
                    return serde_json::to_value(self.context(server, workspace, worker, &lease)?)
                        .map_err(|e| e.to_string());
                }
                if q.enabled {
                    self.admit(
                        &q.binding,
                        &q.client_id,
                        server,
                        workspace,
                        worker,
                        &lease,
                        false,
                    )?;
                }
                let mut s = self.state.lock().map_err(|_| "TX settings unavailable")?;
                if s.binding.as_ref() != Some(&q.binding)
                    || s.client.as_ref().is_some_and(|id| *id != q.client_id)
                {
                    return Err("TX client binding mismatch".into());
                }
                if s.enable_events.len() >= 256 {
                    return Err("TX enable event capacity exhausted; re-pair explicitly".into());
                }
                s.enable_events.insert(q.event_id, digest);
                let continuing = !q.enabled
                    && self
                        .active
                        .lock()
                        .ok()
                        .and_then(|a| a.clone())
                        .is_some_and(|a| {
                            a.binding == q.binding
                                && a.client_id == q.client_id
                                && already_presenting(&a.signals)
                        });
                s.disarmed = !q.enabled;
                if q.enabled {
                    s.client = Some(q.client_id);
                    s.heartbeat =
                        Some(Instant::now() + Duration::from_millis(HEARTBEAT_TIMEOUT_MS));
                } else if !continuing {
                    s.client = None;
                    s.heartbeat = None;
                }
                drop(s);
                if !q.enabled && !continuing {
                    self.cancel();
                }
                serde_json::to_value(self.context(server, workspace, worker, &lease)?)
            }
            "/xlog/v1/tx/heartbeat" => {
                let q: Heartbeat =
                    serde_json::from_slice(body).map_err(|_| "Invalid TX heartbeat")?;
                let c = self.context(server, workspace, worker, &lease)?;
                let mut s = self.state.lock().map_err(|_| "TX settings unavailable")?;
                if c.binding != q.binding
                    || s.client.as_deref() != Some(&q.client_id)
                    || !s.heartbeat.is_some_and(|until| until > Instant::now())
                    || s.disarmed
                        && !self
                            .active
                            .lock()
                            .ok()
                            .and_then(|a| a.clone())
                            .is_some_and(|a| {
                                a.binding == q.binding
                                    && a.client_id == q.client_id
                                    && already_presenting(&a.signals)
                            })
                {
                    return Err("TX heartbeat binding retired".into());
                }
                s.heartbeat = Some(Instant::now() + Duration::from_millis(HEARTBEAT_TIMEOUT_MS));
                drop(s);
                if let Some(a) = self.active.lock().ok().and_then(|a| a.clone()) {
                    a.signals.heartbeat_deadline_ns.store(
                        a.signals.origin.elapsed().as_nanos() as u64
                            + HEARTBEAT_TIMEOUT_MS * 1_000_000,
                        Ordering::Release,
                    );
                    if let Ok(record) = self.record(&a.operation_id) {
                        if record.unresolved()
                            && !a.completing.load(Ordering::Acquire)
                            && !a.signals.cancelled.load(Ordering::Acquire)
                            && record.ptt_attempted
                            && crate::ptt_session::owner(&self.app)?.receipt(&a.operation_id)?
                                ["status"]
                                == "confirmed"
                        {
                            let result = crate::ptt_session::owner(&self.app)?.renew_exact(
                                &record.target,
                                &a.ticket,
                                lease_until(&a.signals)?,
                                &record.route.transmit_slice,
                                |q, t| execute_lease_renewal(&self.app, q, t, &a.ticket),
                            );
                            if let Err(error) = result {
                                self.cancel();
                                return Err(error);
                            }
                        }
                    }
                }
                serde_json::to_value(self.context(server, workspace, worker, &lease)?)
            }
            _ => return Err("Unknown digital TX route".into()),
        };
        value.map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ptt_is_keyed_one_second_before_the_slot_not_when_an_early_commit_is_ready() {
        let slot = 1_000_000_000_000u64;
        // A commit that is ready nine seconds early waits eight of them unkeyed.
        assert_eq!(ptt_key_wait(slot - 9_000_000, slot), Some(Duration::from_secs(8)));
        assert_eq!(ptt_key_wait(slot - 1_000_001, slot), Some(Duration::from_micros(1)));
        // From the lead onwards, and for a late-ready output, keying is immediate.
        assert_eq!(ptt_key_wait(slot - PTT_LEAD_US, slot), None);
        assert_eq!(ptt_key_wait(slot - 20_000, slot), None);
        assert_eq!(ptt_key_wait(slot + 10_000, slot), None);
        // The lead still leaves PTT confirmation ahead of the 50 ms start window.
        assert!(PTT_LEAD_US > 50_000 && PTT_LEAD_US <= 1_000_000);
    }
    #[test]
    fn an_operation_whose_ptt_on_was_never_requested_is_released_only_on_fresh_idle_telemetry() {
        let radio = |transmitting: bool, at: u64| {
            serde_json::json!({"available": true, "received_at_ms": at,
                "snapshot": {"radio": {"transmitting": transmitting}}})
        };
        let now = 1_800_000_000_000;
        assert!(never_keyed(&Value::Null, &radio(false, now - 200), now));
        // A PTT receipt exists (ON was journaled, so possibly dispatched): never inferred.
        let receipt = serde_json::json!({"operation_id": "x", "status": "unknown"});
        assert!(!never_keyed(&receipt, &radio(false, now - 200), now));
        // The radio is transmitting, telemetry is stale or from the future, or unavailable.
        assert!(!never_keyed(&Value::Null, &radio(true, now - 200), now));
        assert!(!never_keyed(&Value::Null, &radio(false, now - 1500), now));
        assert!(!never_keyed(&Value::Null, &radio(false, now + 5), now));
        let mut off = radio(false, now - 200);
        off["available"] = serde_json::json!(false);
        assert!(!never_keyed(&Value::Null, &off, now));
    }
    #[test]
    fn clock_check_catches_steps_but_not_slew_between_observations() {
        let then = Instant::now();
        let wall = 1_800_000_000_000_000u64;
        assert!(!clock_discontinuity(None, then, wall));
        let later = then + Duration::from_millis(100);
        // 100 ms later on both clocks: continuous.
        assert!(!clock_discontinuity(Some((then, wall)), later, wall + 100_000));
        // UTC stepped by 60 ms between consecutive observations.
        assert!(clock_discontinuity(Some((then, wall)), later, wall + 160_000));
        assert!(clock_discontinuity(Some((then, wall)), later, wall + 40_000));
        // An hour between observations with 30 ppm slew (108 ms) is not a step.
        let hour = then + Duration::from_secs(3600);
        assert!(!clock_discontinuity(Some((then, wall)), hour, wall + 3_600_108_000));
    }
    #[test]
    fn a_later_local_stop_blocks_an_already_admitted_enable() {
        assert!(local_enable_revision_matches(7, 7));
        assert!(!local_enable_revision_matches(8, 7));
        assert!(!local_enable_revision_matches(u64::MAX, u64::MAX));
    }
    #[test]
    fn tune_drive_is_explicit_bounded_and_bound_into_immutable_body() {
        let mut q = Tune {
            binding: Binding {
                server_instance: id(),
                workspace_id: id(),
                context_id: id(),
                clock_epoch: id(),
            },
            client_id: id(),
            operation_id: id(),
            event_id: id(),
            tone_hz_f32_bits: 1000f32.to_bits(),
            gain_f32_bits: 0.25f32.to_bits(),
            duration_ms: 100,
        };
        let first = serde_json::to_vec(&q).unwrap();
        let (hz, gain) = tune_parameters(&q).unwrap();
        let low = tune_samples(hz, gain, q.duration_ms);
        q.gain_f32_bits = 0.5f32.to_bits();
        assert_ne!(hash(&first), hash(&serde_json::to_vec(&q).unwrap()));
        let high = tune_samples(hz, 0.5, q.duration_ms);
        assert_eq!(low.len(), 4800);
        assert!(low
            .iter()
            .zip(high.iter())
            .all(|(a, b)| (*a * 2.0 - *b).abs() < 1e-7));
        assert!(high.iter().all(|v| v.abs() <= 0.5));
        for invalid in [0.0, -0.1, 0.81, f32::NAN, f32::INFINITY] {
            q.gain_f32_bits = invalid.to_bits();
            assert!(tune_parameters(&q).is_err());
        }
    }
    #[test]
    fn disarm_can_finish_only_a_period_that_has_reached_native_presentation() {
        let s = Signals::new(Instant::now());
        assert!(!already_presenting(&s));
        s.started_unix_us.store(1000, Ordering::Release);
        s.presented_unix_us.store(999, Ordering::Release);
        assert!(
            !already_presenting(&s),
            "queued future audio is not an already-started over"
        );
        s.presented_unix_us.store(1000, Ordering::Release);
        assert!(already_presenting(&s));
        s.stop();
        assert!(
            !already_presenting(&s),
            "explicit Stop never inherits finish-current permission"
        );
    }
    #[test]
    fn worker_lease_uses_remaining_original_deadline_and_never_reopens_after_stop() {
        let s = Signals::new(Instant::now());
        assert!(lease_until(&s).is_err());
        s.heartbeat_deadline_ns
            .store(800_000_000, Ordering::Release);
        let before = now_us().unwrap() / 1000;
        let until = lease_until(&s).unwrap();
        assert!(until > before && until <= before + 800);
        s.stop();
        assert!(lease_until(&s).is_err());
    }
}
