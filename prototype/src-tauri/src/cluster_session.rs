//! Native cluster projection owner. Rendering, hiding, or reloading a window
//! cannot create a second cluster poller or become the command transport.
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

type Transport = dyn Fn(Value) -> Result<Value, String> + Send + Sync;
type Publish = dyn Fn(Value) + Send + Sync;

pub fn handles(method: &str) -> bool {
    matches!(
        method,
        "cluster_snapshot"
            | "cluster_resume_initialize"
            | "cluster_filter"
            | "cluster_connect"
            | "cluster_disconnect"
            | "cluster_history"
            | "cluster_command"
            | "cluster_console_clear"
            | "cluster_push_spots"
            | "cluster_configure"
            | "cluster_radio_spots_configure"
            | "cluster_contest_radio_spots_publish"
            | "cluster_password_set"
    )
}

pub fn validate(label: &str, request: &Value) -> Result<(), String> {
    let contest = crate::workspace_windows::WorkspaceWindowIdentity::parse_native_label(label).ok()
        .filter(|identity| identity.scope == crate::workspace_windows::WorkspaceWindowScope::Contesting);
    let label = contest.map_or_else(|| crate::workspace_windows::surface_role(label), |identity| identity.panel_id);
    crate::worker::validate_request(request)?;
    let method = request["method"].as_str().unwrap_or("");
    if !handles(method)
        || !matches!(
            label,
            "main" | "settings" | "dx-cluster" | "special" | "monitor" | "world-map" | "band-maps" | "contest-setup"
        )
    {
        return Err("This window cannot access the cluster session.".into());
    }
    if method == "cluster_contest_radio_spots_publish" && label != "contest-setup" {
        return Err("Only the active Contest Setup owner may publish classified contest spots.".into());
    }
    let exact_recent_spots_command = method == "cluster_command"
        && request["params"].as_object().is_some_and(|params| {
            params.len() == 1 && params.get("command").and_then(Value::as_str) == Some("SH/DX/100")
        });
    if contest.is_some()
        && !((label == "dx-cluster" && (matches!(method, "cluster_snapshot" | "cluster_connect" | "cluster_disconnect")
                || exact_recent_spots_command))
            || (label == "contest-setup" && method == "cluster_contest_radio_spots_publish"))
    {
        return Err("The contest Cluster window may only read, connect, disconnect, or request recent spots from the shared cluster session.".into());
    }
    if label == "settings" && !matches!(method, "cluster_snapshot" | "cluster_radio_spots_configure" | "cluster_configure" | "cluster_connect" | "cluster_disconnect" | "cluster_password_set") {
        return Err("This cluster action is unavailable in Settings.".into());
    }
    if label != "main" && label != "settings"
        && (method == "cluster_password_set"
            || label != "dx-cluster" && method != "cluster_snapshot"
                && !(label == "contest-setup" && method == "cluster_contest_radio_spots_publish"))
    {
        return Err("This cluster action is unavailable in this window.".into());
    }
    Ok(())
}

struct Projection {
    revision: u64,
    selection_revision: u64,
    response: Option<Value>,
    selected: Option<String>,
    available: bool,
    received_at_ms: u64,
    identities: crate::spot_identity::SpotIdentities,
    /// Needs evidence from the dedicated needs poll, retained apart from the
    /// spot snapshot so its freshness never waits for the large snapshot reply.
    needs: Option<Value>,
    needs_received_at_ms: u64,
}

pub struct ClusterSession {
    activity_tick: Arc<dyn Fn() + Send + Sync>,
    transport: Arc<Transport>,
    needs_transport: Option<Arc<Transport>>,
    publish: Arc<Publish>,
    command: Mutex<()>,
    projection: Mutex<Projection>,
    started: AtomicBool,
    stopped: AtomicBool,
    startup_resume_started: AtomicBool,
    restored_worker_generation: AtomicU64,
    sequence: AtomicU64,
    selection_holds: AtomicUsize,
    wake: (Mutex<()>, Condvar),
}

pub struct SelectionFence(Arc<ClusterSession>);
impl Drop for SelectionFence {
    fn drop(&mut self) { self.0.selection_holds.fetch_sub(1, Ordering::AcqRel); }
}

impl ClusterSession {
    fn new(transport: Arc<Transport>, publish: Arc<Publish>) -> Self {
        Self {
            activity_tick: Arc::new(|| {}),
            transport,
            needs_transport: None,
            publish,
            command: Mutex::new(()),
            projection: Mutex::new(Projection {
                revision: 0,
                selection_revision: 0,
                response: None,
                selected: None,
                available: false,
                received_at_ms: 0,
                identities: crate::spot_identity::SpotIdentities::new(crate::worker::generation()),
                needs: None,
                needs_received_at_ms: 0,
            }),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            startup_resume_started: AtomicBool::new(false),
            restored_worker_generation: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            selection_holds: AtomicUsize::new(0),
            wake: (Mutex::new(()), Condvar::new()),
        }
    }

    fn active(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            Err("The cluster session is shutting down.".into())
        } else {
            Ok(())
        }
    }

    fn snapshot_request(&self) -> Value {
        json!({"id":format!("native-cluster-{}", self.sequence.fetch_add(1, Ordering::AcqRel)),
            "method":"cluster_snapshot","params":{}})
    }

    fn needs_request(&self) -> Value {
        json!({"id":format!("native-cluster-needs-{}", self.sequence.fetch_add(1, Ordering::AcqRel)),
            "method":"cluster_snapshot","params":{"needs_only":true}})
    }

    /// One dedicated needs poll. It never takes the command lock, never
    /// replaces the spot projection and never emits to renderers.
    fn refresh_needs(&self) {
        let Some(transport) = self.needs_transport.as_ref() else { return; };
        if self.active().is_err() { return; }
        let generation = crate::worker::generation();
        let reply = transport(self.needs_request());
        self.accept_needs(reply, generation, crate::worker::generation());
    }

    fn accept_needs(&self, reply: Result<Value, String>, generation: u64, current_generation: u64) {
        let needs = reply.ok().filter(|response| response["ok"] == true && generation == current_generation)
            .map(|response| response["result"]["needs"].clone()).filter(Value::is_object);
        if let Ok(mut state) = self.projection.lock() {
            state.needs_received_at_ms = if needs.is_some() {
                SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
            } else { 0 };
            state.needs = needs;
        }
    }

    fn resume_request(&self) -> Value {
        json!({"id":format!("native-cluster-{}", self.sequence.fetch_add(1, Ordering::AcqRel)),
            "method":"cluster_resume","params":{}})
    }

    pub fn projection_value(&self) -> Result<Value, String> {
        let state = self
            .projection
            .lock()
            .map_err(|_| "Cluster projection unavailable")?;
        Ok(
            json!({"revision":state.revision,"selection_revision":state.selection_revision,"response":state.response,"selected":state.selected,"available":state.available,"received_at_ms":state.received_at_ms,
                "needs":state.needs,"needs_received_at_ms":state.needs_received_at_ms}),
        )
    }

    pub fn fence_selection(self: &Arc<Self>, expected_revision: u64) -> Result<SelectionFence, String> {
        let state = self.projection.lock().map_err(|_| "Cluster projection unavailable")?;
        if state.selection_revision != expected_revision { return Err("Cluster selection changed before QSY clear.".into()); }
        self.selection_holds.fetch_add(1, Ordering::AcqRel);
        Ok(SelectionFence(self.clone()))
    }

    fn accept_at_generation(&self, response: &Value, generation: u64, current_generation: u64) -> Result<(), String> {
        self.active()?;
        if generation != current_generation { return Err("Cluster worker changed before the snapshot was accepted.".into()); }
        let payload = &response["result"];
        if response["ok"] != true
            || !payload["snapshot"]["spots"]
                .as_array()
                .is_some_and(|spots| spots.len() <= 500)
            || !payload["contest_spots"].as_array().is_some_and(|spots| spots.len() <= 500)
        {
            return Err("The cluster service did not provide a valid snapshot.".into());
        }
        {
            let mut state = self
                .projection
                .lock()
                .map_err(|_| "Cluster projection unavailable")?;
            state.revision += 1;
            let mut annotated = payload.clone();
            state.identities.annotate(&mut annotated, generation);
            state.response = Some(annotated);
            state.available = true;
            state.received_at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0);
            if state.selected.as_ref().is_some_and(|id| {
                !payload["snapshot"]["spots"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|spot| spot["id"].as_str() == Some(id))
            }) {
                state.selected = None;
                state.selection_revision += 1;
            }
        }
        self.emit();
        Ok(())
    }

    fn accept(&self, response: &Value, generation: u64) -> Result<(), String> {
        self.accept_at_generation(response, generation, crate::worker::generation())
    }

    fn emit(&self) {
        if self.active().is_ok() {
            if let Ok(value) = self.projection_value() {
                (self.publish)(value);
            }
        }
    }

    fn unavailable(&self) {
        if let Ok(mut state) = self.projection.lock() {
            state.available = false;
            state.revision += 1;
            state.identities.invalidate(crate::worker::generation());
        }
        self.emit();
    }

    fn refresh(&self) -> Result<(), String> {
        self.active()?;
        // Background reads never queue behind a slow user action or accumulate
        // blocking tasks. Its returned snapshot will update the projection.
        let Ok(_guard) = self.command.try_lock() else {
            return Ok(());
        };
        self.active()?;
        let generation = crate::worker::generation();
        // Startup owns exactly one resume attempt. Every later cycle is a
        // passive snapshot; a timed-out resume is never replayed in the same
        // worker generation.
        let request = if !self.startup_resume_started.swap(true, Ordering::AcqRel) {
            self.resume_request()
        } else {
            self.snapshot_request()
        };
        let result = (self.transport)(request).and_then(|response| self.accept(&response, generation));
        if result.is_err() {
            self.unavailable();
        }
        result
    }

    pub fn refresh_and_publish(&self) -> Result<(), String> {
        self.refresh()?;
        self.emit();
        Ok(())
    }

    pub(crate) fn resume_after_worker_restart(
        &self,
        worker_generation: u64,
        dispatch: impl FnOnce(Value) -> Result<Value, String>,
    ) -> Result<(), String> {
        self.resume_after_worker_restart_with_generation(
            worker_generation,
            crate::worker::generation,
            dispatch,
        )
    }

    fn resume_after_worker_restart_with_generation(
        &self,
        worker_generation: u64,
        current_generation: impl FnOnce() -> u64,
        dispatch: impl FnOnce(Value) -> Result<Value, String>,
    ) -> Result<(), String> {
        if worker_generation <= 1 { return Ok(()); }
        let _guard = self.command.try_lock()
            .map_err(|_| "Cluster session is busy; its regular poll will resume the replacement worker.".to_string())?;
        self.active()?;
        if self.restored_worker_generation.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |previous| (worker_generation > previous).then_some(worker_generation),
        ).is_err() {
            return Ok(());
        }
        self.unavailable();
        let response = dispatch(self.resume_request())?;
        self.accept_at_generation(&response, worker_generation, current_generation())
    }

    /// Immutable fields come exclusively from this native retained snapshot.
    pub fn activation_spot(&self, epoch: &str, id: &str, version: &str) -> Result<Value, String> {
        self.active()?;
        let state = self.projection.lock().map_err(|_| "Cluster projection unavailable")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "Cluster clock unavailable")?.as_millis() as u64;
        if !state.available || now.saturating_sub(state.received_at_ms) > 5000 || state.received_at_ms > now + 1000 {
            return Err("Cluster reports are stale. Refresh before selecting a report.".into());
        }
        state.identities.resolve(epoch, id, version, crate::worker::generation())
    }

    /// Refresh only the authority for an equivalent native alert report; never its tuning fields.
    pub fn matching_alert_observation(&self, entry: &Value) -> Result<Option<Value>, String> {
        self.active()?;
        if entry["test"] != false || entry["source"] != "cluster" { return Ok(None); }
        let state = self.projection.lock().map_err(|_| "Cluster projection unavailable")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "Cluster clock unavailable")?.as_millis() as u64;
        if !state.available || now.saturating_sub(state.received_at_ms) > 5000 || state.received_at_ms > now + 1000 { return Ok(None); }
        let Some(response) = &state.response else { return Ok(None); };
        let Some(frequency) = entry["frequencyKHz"].as_f64().filter(|f| f.is_finite() && *f > 0.0) else { return Ok(None); };
        for spot in ["/snapshot/spots", "/contest_spots"].into_iter().filter_map(|path| response.pointer(path)?.as_array()).flatten() {
            if spot["callsign"] != entry["callsign"] || spot["band"] != entry["band"] || spot["mode"] != entry["mode"]
                || spot["frequencyKHz"].as_f64().is_none_or(|f| (f * 1000.0).round() != (frequency * 1000.0).round()) { continue; }
            let Some(((epoch, id), version)) = spot["activation"]["sourceEpoch"].as_str().zip(spot["id"].as_str()).zip(spot["activation"]["spotVersion"].as_str()) else { continue; };
            if state.identities.resolve(epoch, id, version, crate::worker::generation()).is_ok() {
                return Ok(Some(json!({"id":id,"activation":spot["activation"]})));
            }
        }
        Ok(None)
    }

    pub fn request(&self, request: Value) -> Result<Value, String> {
        self.active()?;
        crate::worker::validate_request(&request)?;
        if !handles(request["method"].as_str().unwrap_or("")) {
            return Err("Unknown cluster action.".into());
        }
        if request["method"] == "cluster_snapshot" {
            if request["params"]
                .as_object()
                .is_none_or(|params| !params.is_empty())
            {
                return Err("Cluster snapshot takes no parameters.".into());
            }
            // Renderer reads reuse the native owner's last verified projection.
            // Initial load is synchronized with the one background poller.
            let state = self
                .projection
                .lock()
                .map_err(|_| "Cluster projection unavailable")?;
            if state.available {
                return Ok(json!({"id":request["id"],"ok":true,"result":state.response}));
            }
        }
        let explicit_setup = matches!(request["method"].as_str(), Some("cluster_connect" | "cluster_disconnect" | "cluster_configure" | "cluster_password_set"));
        let _guard = if explicit_setup {
            self.command.lock().map_err(|_| "Cluster session unavailable.")?
        } else {
            self.command.try_lock().map_err(|_| "Cluster session is busy. Wait for the current action to finish.")?
        };
        self.active()?;
        if matches!(request["method"].as_str(), Some("cluster_connect" | "cluster_disconnect" | "cluster_configure")) {
            // Retire displayed activation tokens before a source/session action,
            // including uncertain outcomes. No retained report is replayable.
            self.unavailable();
        }
        let generation = crate::worker::generation();
        let preference_initialization = request["method"] == "cluster_resume_initialize";
        let mut response = match (self.transport)(request.clone()) {
            Ok(value) => value,
            Err(error) => {
                self.unavailable();
                return Err(error);
            }
        };
        self.active()?;
        // Legacy preference migration returns a receipt, not a feed snapshot.
        // A successful receipt cannot replace reports or their activation tokens.
        if preference_initialization {
            if response["ok"] == true && !response["result"]["resume_initialized"].is_boolean() {
                return Err("The cluster restart preference receipt is invalid.".into());
            }
            return Ok(response);
        }
        // History and contest radio publication return their own results, not
        // cluster snapshots. Accepting a delivery receipt as a snapshot would
        // invalidate the retained feed after the radio already accepted spots.
        if response["ok"] == true && !matches!(request["method"].as_str(),
            Some("cluster_history" | "cluster_contest_radio_spots_publish")) {
            if let Err(error) = self.accept(&response, generation) {
                self.unavailable();
                return Err(error);
            }
            // Direct replies and background projections carry the same native
            // identity tokens; a fresh window must not need a second refresh.
            response["result"] = self.projection.lock().map_err(|_| "Cluster projection unavailable")?
                .response.clone().ok_or("Cluster snapshot unavailable")?;
        } else if request["method"] == "cluster_snapshot" {
            self.unavailable();
        }
        // An explicit command is sent once. Neither a transport failure nor a
        // worker error retries it. The UI can inspect fresh state before acting.
        Ok(response)
    }

    pub fn post_contest_spot(&self, label: &str, isolated: bool, callsign: &str, frequency_hz: u64) -> Result<(), String> {
        if label != "contest-setup" {
            return Err("Only Contest Setup can post a contest spot.".into());
        }
        if isolated {
            return Err("External services and station I/O are disabled in OM0RX-xLog Contesting Test.".into());
        }
        let callsign = callsign.trim().to_ascii_uppercase();
        if !(2..=24).contains(&callsign.len())
            || !callsign.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-'))
            || frequency_hz == 0
        {
            return Err("A valid callsign and contest frequency are required before posting a spot.".into());
        }
        let frequency_khz = frequency_hz as f64 / 1_000.0;
        let frequency = format!("{frequency_khz:.3}").trim_end_matches('0').trim_end_matches('.').to_owned();
        let request = json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "cluster_command",
            "params": { "command": format!("DX {frequency} {callsign}") }
        });
        let response = self.request(request)?;
        if response["ok"] != true {
            return Err(response["error"]["message"].as_str().unwrap_or("The cluster rejected the contest spot.").into());
        }
        Ok(())
    }

    pub fn select(&self, selected: Option<String>) -> Result<(), String> {
        self.active()?;
        let mut state = self
            .projection
            .lock()
            .map_err(|_| "Cluster projection unavailable")?;
        if self.selection_holds.load(Ordering::Acquire) != 0 {
            return Err("Wait for the confirmed QSY draft update before selecting another spot.".into());
        }
        if let Some(id) = &selected {
            if id.is_empty()
                || id.len() > 128
                || !state.available
                || !state
                    .response
                    .as_ref()
                    .and_then(|response| response["snapshot"]["spots"].as_array())
                    .is_some_and(|spots| spots.iter().any(|spot| spot["id"].as_str() == Some(id)))
            {
                return Err("The selected spot is no longer available.".into());
            }
        }
        if state.selected != selected {
            state.selected = selected;
            state.selection_revision += 1;
            state.revision += 1;
        }
        drop(state);
        self.emit();
        Ok(())
    }

    fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let owner = Arc::clone(self);
        std::thread::spawn(move || {
            let mut started = std::time::Instant::now();
            while owner.active().is_ok() {
                let Ok(guard) = owner.wake.0.lock() else {
                    break;
                };
                let _ = owner
                    .wake
                    .1
                    .wait_timeout_while(guard, poll_wait(started.elapsed()), |_| {
                        !owner.stopped.load(Ordering::Acquire)
                    });
                started = std::time::Instant::now();
                owner.cycle();
            }
        });
        if self.needs_transport.is_none() { return; }
        let owner = Arc::clone(self);
        std::thread::spawn(move || {
            let mut started = std::time::Instant::now();
            while owner.active().is_ok() {
                let Ok(guard) = owner.wake.0.lock() else { break; };
                let _ = owner.wake.1.wait_timeout_while(guard, needs_poll_wait(started.elapsed()), |_| {
                    !owner.stopped.load(Ordering::Acquire)
                });
                started = std::time::Instant::now();
                if needs_wanted(now_ms(), NEEDS_READER_AT_MS.load(Ordering::Acquire)) { owner.refresh_needs(); }
            }
        });
    }

    fn with_needs_transport(mut self, transport: Arc<Transport>) -> Self {
        self.needs_transport = Some(transport);
        self
    }

    fn with_activity_tick(mut self, tick: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.activity_tick = tick;
        self
    }

    fn cycle(&self) {
        if self.active().is_err() { return; }
        // Passive activity is sampled even when cluster refresh is unavailable or busy.
        (self.activity_tick)();
        let _ = self.refresh();
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.wake.1.notify_all();
    }
}

/// Polls start once per second, measured start to start. The retained snapshot
/// carries needs evidence with a 2 s lifetime, and a worker reply takes about
/// 1.2–1.7 s on a full logbook. Waiting a further second after each reply made
/// the period 2.2–2.7 s, so the evidence was expired most of the time and every
/// companion needs read answered Unknown.
/// A slow reply is followed by a short gap, never an immediate re-poll, so
/// other worker requests still interleave.
const POLL_PERIOD: Duration = Duration::from_secs(1);
const POLL_MIN_GAP: Duration = Duration::from_millis(100);
fn poll_wait(since_last_start: Duration) -> Duration {
    POLL_PERIOD.saturating_sub(since_last_start).max(POLL_MIN_GAP)
}

/// Needs evidence lives two seconds. Its small dedicated reply is refreshed
/// every half second, start to start, so a single slow or missed reply still
/// leaves fresh evidence; the large spot snapshot keeps its own one-second poll.
const NEEDS_POLL_PERIOD: Duration = Duration::from_millis(500);
fn needs_poll_wait(since_last_start: Duration) -> Duration {
    NEEDS_POLL_PERIOD.saturating_sub(since_last_start).max(POLL_MIN_GAP)
}

/// The dedicated needs evidence has one reader, the paired companion. Each
/// needs poll is a full-logbook classification costing the worker hundreds of
/// milliseconds, so it runs only while a companion has read needs recently;
/// with no companion (full Nexus, or none) it costs nothing. 2026-11-19: two
/// polls a second with no reader kept the Windows worker saturated.
const NEEDS_READER_WINDOW_MS: u64 = 10_000;
static NEEDS_READER_AT_MS: AtomicU64 = AtomicU64::new(0);
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
}
/// The companion read (or is about to read) the needs evidence.
pub(crate) fn note_needs_reader() { NEEDS_READER_AT_MS.store(now_ms(), Ordering::Release); }
fn needs_wanted(now: u64, last_reader: u64) -> bool {
    last_reader != 0 && now.saturating_sub(last_reader) <= NEEDS_READER_WINDOW_MS
}

static OWNER: OnceLock<Arc<ClusterSession>> = OnceLock::new();
static STOPPED: AtomicBool = AtomicBool::new(false);

pub(crate) fn retained_owner() -> Option<Arc<ClusterSession>> {
    if STOPPED.load(Ordering::Acquire) { None } else { OWNER.get().cloned() }
}

pub fn owner(app: &tauri::AppHandle) -> Result<Arc<ClusterSession>, String> {
    if STOPPED.load(Ordering::Acquire) {
        return Err("The cluster session is shutting down.".into());
    }
    let owner = OWNER
        .get_or_init(|| {
            let app = app.clone();
            Arc::new(ClusterSession::new(
                Arc::new(crate::worker::request_cluster_poll),
                Arc::new(move |projection| {
                    if let Some(alerts) = crate::dx_alerts::existing() {
                        if let Err(error) = alerts.observe(&projection) { eprintln!("DX alert observation unavailable: {error}"); }
                    }
                    for label in ["dx-cluster", "world-map", "dxpedition-map", "band-maps", "monitor", "special"] {
                        let _ = app.emit_to(label, "xlog:cluster-projection", &projection);
                    }
                    for band in crate::workspace_windows::BAND_MAP_BANDS {
                        let _ = app.emit_to(format!("band-map-{band}").as_str(), "xlog:cluster-projection", &projection);
                    }
                }),
            ).with_needs_transport(Arc::new(crate::worker::request_cluster_poll))
            .with_activity_tick(Arc::new(|| {
                if let Some(alerts) = crate::dx_alerts::existing() { alerts.refresh_chases(); }
                // The Swift UDP receiver is only built for macOS.
                if !cfg!(target_os = "macos") { return; }
                if let Some(alerts) = crate::dx_alerts::existing() {
                    // Reuse the existing native loop and worker. Reading retained
                    // activity cannot start the default-off UDP receiver.
                    let generation = crate::worker::generation();
                    let request = json!({"id":uuid::Uuid::new_v4().to_string(),"method":"wsjtx_snapshot","params":{}});
                    match crate::worker::request_existing_generation(request, generation) {
                        Ok(reply) if crate::worker::require_existing_generation(generation).is_ok() => {
                            if alerts.observe_activity(&reply["result"]["activity"]).is_err() { alerts.activity_unavailable(); }
                        }
                        _ => alerts.activity_unavailable(),
                    }
                }
            })))
        })
        .clone();
    if STOPPED.load(Ordering::Acquire) {
        owner.stop();
        return Err("The cluster session is shutting down.".into());
    }
    owner.start();
    Ok(owner)
}

pub fn shutdown() {
    STOPPED.store(true, Ordering::Release);
    if let Some(owner) = OWNER.get() {
        owner.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply(request: &Value, query: &str) -> Value {
        json!({"id":request["id"],"ok":true,"result":{"snapshot":{"spots":[{"id":"spot-a"}]},"contest_spots":[{"id":"spot-a"}],"query":query}})
    }
    fn request(method: &str) -> Value {
        json!({"id":"fixture-12345678","method":method,"params":{}})
    }
    #[test]
    fn polls_run_once_per_second_start_to_start_so_two_second_needs_evidence_stays_fresh() {
        // A fast reply waits out the rest of the second.
        assert_eq!(poll_wait(Duration::from_millis(300)), Duration::from_millis(700));
        // A 1.5 s reply (measured on a full logbook) is followed only by the minimum gap,
        // so polls start every 1.6 s rather than every 2.5 s as before.
        assert_eq!(poll_wait(Duration::from_millis(1_500)), POLL_MIN_GAP);
        assert!(Duration::from_millis(1_500) + poll_wait(Duration::from_millis(1_500)) < Duration::from_secs(2));
        assert_eq!(poll_wait(Duration::ZERO), POLL_PERIOD);
    }
    fn command_request(command: &str) -> Value {
        json!({"id":"fixture-12345678","method":"cluster_command","params":{"command":command}})
    }

    #[test]
    fn surface_capabilities_are_explicit() {
        for method in [
            "cluster_snapshot",
            "cluster_resume_initialize",
            "cluster_filter",
            "cluster_connect",
            "cluster_disconnect",
            "cluster_history",
            "cluster_command",
            "cluster_console_clear",
            "cluster_push_spots",
            "cluster_configure",
            "cluster_radio_spots_configure",
        ] {
            assert!(validate("main", &request(method)).is_ok());
            assert!(validate("dx-cluster", &request(method)).is_ok());
            assert!(validate("unknown", &request(method)).is_err());
        }
        assert!(validate("main", &request("cluster_password_set")).is_ok());
        assert!(validate("dx-cluster", &request("cluster_password_set")).is_err());
        assert!(validate("settings", &request("cluster_snapshot")).is_ok());
        assert!(validate("settings", &request("cluster_radio_spots_configure")).is_ok());
        assert!(validate("settings", &request("cluster_configure")).is_ok());
        for label in ["monitor", "world-map", "band-maps", "contest-setup"] {
            assert!(validate(label, &request("cluster_snapshot")).is_ok());
            assert!(validate(label, &request("cluster_connect")).is_err());
        }
        for method in ["cluster_snapshot", "cluster_connect", "cluster_disconnect"] {
            assert!(validate("contest--dx-cluster", &request(method)).is_ok());
        }
        assert!(validate("contest--dx-cluster", &command_request("SH/DX/100")).is_ok());
        for command in ["sh/dx/100", "SH/DX/50", "SH/MYDX/100"] {
            assert!(validate("contest--dx-cluster", &command_request(command)).is_err());
        }
        assert!(validate("contest--dx-cluster", &json!({
            "id":"fixture-12345678","method":"cluster_command",
            "params":{"command":"SH/DX/100","server_id":"00000000-0000-4000-8000-000000000001"}
        })).is_err());
        assert!(validate("contest-setup", &request("cluster_contest_radio_spots_publish")).is_ok());
        assert!(validate("main", &request("cluster_contest_radio_spots_publish")).is_err());
        assert!(validate("dx-cluster", &request("cluster_contest_radio_spots_publish")).is_err());
        for method in ["cluster_filter", "cluster_command", "cluster_configure", "cluster_password_set"] {
            assert!(validate("contest--dx-cluster", &request(method)).is_err());
        }
        assert!(validate("contest--band-maps", &request("cluster_connect")).is_err());
        assert!(validate("dx-cluster", &request("station_tune")).is_err());
        assert!(validate("main", &request("cluster_resume")).is_err());
        for label in ["main", "monitor", "world-map", "band-maps", "dx-cluster", "logbook"] {
            assert!(crate::worker::validate_surface(label, &request("cluster_resume")).is_err());
        }
    }

    #[test]
    fn multiple_renderers_reuse_one_verified_snapshot() {
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let owner = ClusterSession::new(
            Arc::new(move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(reply(&request, "initial"))
            }),
            Arc::new(|_| {}),
        );
        owner.refresh().unwrap();
        for _ in 0..20 {
            assert_eq!(
                owner.request(request("cluster_snapshot")).unwrap()["result"]["query"],
                "initial"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn contest_radio_publication_receipts_preserve_the_cluster_projection() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let published = events.clone();
        let owner = ClusterSession::new(Arc::new(|request| {
            if request["method"] == "cluster_contest_radio_spots_publish" {
                let params = &request["params"];
                Ok(json!({"id":request["id"],"ok":true,"result":{
                    "accepted":params["accepted"],"removed":params["removed"],"unchanged":0,
                    "filtered":0,"rejected":params["rejected"],"pending":params["pending"],
                    "stale":params["stale"],"revision":7,"workspace_generation":"generation-1"
                }}))
            } else {
                Ok(reply(&request, "retained"))
            }
        }), Arc::new(move |value| published.lock().unwrap().push(value)));
        owner.refresh().unwrap();
        owner.select(Some("spot-a".into())).unwrap();
        let before = owner.projection_value().unwrap();
        let event_count = events.lock().unwrap().len();
        // Completed replace, intermediate chunk, stale publisher, delivery
        // rejection and clear all return receipts rather than snapshots.
        for (accepted, removed, rejected, pending, stale) in [
            (1, 0, 0, false, false), (0, 0, 0, true, false),
            (0, 0, 0, false, true), (0, 0, 1, false, false),
            (0, 1, 0, false, false),
        ] {
            let mut publication = request("cluster_contest_radio_spots_publish");
            publication["params"] = json!({"accepted":accepted, "removed":removed,
                "rejected":rejected, "pending":pending, "stale":stale});
            let result = owner.request(publication).unwrap();
            assert_eq!(result["result"], json!({"accepted":accepted, "removed":removed, "unchanged":0, "filtered":0,
                "rejected":rejected, "pending":pending, "stale":stale, "revision":7,"workspace_generation":"generation-1"}));
            assert_eq!(owner.projection_value().unwrap(), before);
            assert_eq!(events.lock().unwrap().len(), event_count);
            assert_eq!(owner.request(request("cluster_snapshot")).unwrap()["result"], before["response"]);
        }
    }

    #[test]
    fn contest_connection_actions_and_renderers_reuse_one_serialized_owner() {
        let methods = Arc::new(Mutex::new(Vec::new()));
        let recorded = methods.clone();
        let owner = ClusterSession::new(
            Arc::new(move |request| {
                recorded.lock().unwrap().push(request["method"].as_str().unwrap().to_string());
                Ok(reply(&request, "shared"))
            }),
            Arc::new(|_| {}),
        );
        assert_eq!(owner.request(request("cluster_connect")).unwrap()["result"]["query"], "shared");
        for _ in 0..20 {
            assert_eq!(owner.request(request("cluster_snapshot")).unwrap()["result"]["query"], "shared");
        }
        assert_eq!(*methods.lock().unwrap(), vec!["cluster_connect"]);
    }

    #[test]
    fn startup_resumes_once_and_periodic_refreshes_are_passive_snapshots() {
        let methods = Arc::new(Mutex::new(Vec::new()));
        let recorded = methods.clone();
        let owner = ClusterSession::new(
            Arc::new(move |request| {
                recorded.lock().unwrap().push(request["method"].as_str().unwrap().to_owned());
                Ok(reply(&request, "refresh"))
            }),
            Arc::new(|_| {}),
        );
        owner.refresh().unwrap();
        owner.refresh().unwrap();
        owner.refresh().unwrap();
        assert_eq!(
            *methods.lock().unwrap(),
            ["cluster_resume", "cluster_snapshot", "cluster_snapshot"]
        );
    }

    #[test]
    fn replacement_worker_is_resumed_once_per_generation_with_exact_resume_request() {
        let owner = ClusterSession::new(
            Arc::new(|_| panic!("retained transport must not replace the exact worker dispatch")),
            Arc::new(|_| {}),
        );
        let mut calls = 0;
        owner.resume_after_worker_restart_with_generation(1, || 1, |_| {
            panic!("initial worker generation must not be restored")
        }).unwrap();
        owner.resume_after_worker_restart_with_generation(2, || 2, |request| {
            calls += 1;
            assert_eq!(request["method"], "cluster_resume");
            assert_eq!(request["params"], json!({}));
            Ok(reply(&request, "generation-2"))
        }).unwrap();
        owner.resume_after_worker_restart_with_generation(2, || 2, |_| {
            panic!("same worker generation restored twice")
        }).unwrap();
        owner.resume_after_worker_restart_with_generation(3, || 3, |request| {
            calls += 1;
            assert_eq!(request["method"], "cluster_resume");
            assert_eq!(request["params"], json!({}));
            Ok(reply(&request, "generation-3"))
        }).unwrap();
        assert_eq!(calls, 2);
        let projection = owner.projection_value().unwrap();
        assert_eq!(projection["available"], true);
        assert_eq!(projection["response"]["query"], "generation-3");
    }
    #[test]
    fn failed_cluster_reads_still_sample_passive_activity_each_cycle() {
        let published = Arc::new(AtomicU64::new(0)); let count = published.clone();
        let owner = ClusterSession::new(Arc::new(|_| Err("No cluster configured".into())), Arc::new(|_| {}))
            .with_activity_tick(Arc::new(move || { count.fetch_add(1, Ordering::SeqCst); }));
        owner.cycle(); owner.cycle();
        assert_eq!(published.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn alert_refresh_uses_equivalent_current_native_report_without_reviving_old_authority() {
        let owner = ClusterSession::new(Arc::new(|_| unreachable!()), Arc::new(|_| {}));
        let report = json!({"id":"report","callsign":"3X4U","frequencyKHz":10107.0,"mode":"CW","band":"30m","comment":"old"});
        let mut response = json!({"ok":true,"result":{"snapshot":{"spots":[report.clone()]},"contest_spots":[]}});
        owner.accept(&response, crate::worker::generation()).unwrap();
        let original = owner.projection_value().unwrap()["response"]["snapshot"]["spots"][0]["activation"].clone();
        let mut entry = report.clone(); entry["test"] = json!(false); entry["source"] = json!("cluster");
        response["result"]["snapshot"]["spots"][0]["comment"] = json!("updated metadata");
        owner.accept(&response, crate::worker::generation()).unwrap();
        assert!(owner.activation_spot(original["sourceEpoch"].as_str().unwrap(), "report", original["spotVersion"].as_str().unwrap()).is_err());
        let current = owner.matching_alert_observation(&entry).unwrap().unwrap();
        assert_ne!(current["activation"], original);
        assert_eq!(owner.activation_spot(current["activation"]["sourceEpoch"].as_str().unwrap(), "report", current["activation"]["spotVersion"].as_str().unwrap()).unwrap()["frequencyHz"], 10_107_000);
        for (field, value) in [("frequencyKHz", json!(10108)), ("mode", json!("SSB")), ("band", json!("20m")), ("callsign", json!("OTHER1")), ("source", json!("wsjtx")), ("test", json!(true))] {
            let mut changed = entry.clone(); changed[field] = value;
            assert!(owner.matching_alert_observation(&changed).unwrap().is_none(), "{field}");
        }
        owner.projection.lock().unwrap().received_at_ms = 0;
        assert!(owner.matching_alert_observation(&entry).unwrap().is_none());
        owner.unavailable();
        assert!(owner.matching_alert_observation(&entry).unwrap().is_none());
    }

    #[test]
    fn first_direct_reply_has_native_tokens_and_old_worker_payload_is_never_relabelled() {
        let owner = ClusterSession::new(Arc::new(|request| Ok(json!({"id":request["id"],"ok":true,"result":{
            "snapshot":{"spots":[{"id":"source-report","callsign":"TEST1","frequencyKHz":14074.0,"mode":"FT8","band":"20m"}]},
            "contest_spots":[{"id":"source-report","callsign":"TEST1","frequencyKHz":14074.0,"mode":"FT8","band":"20m"}]
        }}))), Arc::new(|_| {}));
        let response = owner.request(request("cluster_snapshot")).unwrap();
        let token = &response["result"]["snapshot"]["spots"][0]["activation"];
        assert!(token["sourceEpoch"].is_string());
        assert_eq!(owner.activation_spot(token["sourceEpoch"].as_str().unwrap(), "source-report", token["spotVersion"].as_str().unwrap()).unwrap()["call"], "TEST1");
        let previous = owner.projection_value().unwrap();
        assert!(owner.accept(&response, crate::worker::generation().wrapping_add(1)).is_err());
        assert_eq!(owner.projection_value().unwrap(), previous);
    }

    #[test]
    fn resume_preference_receipt_does_not_replace_the_retained_cluster_projection() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let published = events.clone();
        let owner = ClusterSession::new(Arc::new(|request| {
            if request["method"] == "cluster_resume_initialize" {
                Ok(json!({"id":request["id"],"ok":true,"result":{"resume_initialized":true}}))
            } else {
                Ok(reply(&request, "retained"))
            }
        }), Arc::new(move |value| published.lock().unwrap().push(value)));
        owner.refresh_and_publish().unwrap();
        let before = owner.projection_value().unwrap();
        let event_count = events.lock().unwrap().len();
        let initialized = owner.request(request("cluster_resume_initialize")).unwrap();
        assert_eq!(initialized["result"], json!({"resume_initialized":true}));
        assert_eq!(owner.projection_value().unwrap(), before);
        assert_eq!(events.lock().unwrap().len(), event_count);
        assert_eq!(owner.request(request("cluster_snapshot")).unwrap()["result"], before["response"]);
    }

    #[test]
    fn startup_resume_initialization_busy_is_rejected_before_transport_then_confirms() {
        let (entered, observed) = std::sync::mpsc::channel();
        let (release, finish) = std::sync::mpsc::channel();
        let finish = Mutex::new(finish);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let owner = Arc::new(ClusterSession::new(Arc::new(move |request| {
            recorded.lock().unwrap().push(request.clone());
            if request["method"] == "cluster_resume" {
                entered.send(()).unwrap();
                finish.lock().unwrap().recv_timeout(Duration::from_secs(3)).unwrap();
            }
            let mut response = reply(&request, "initialized");
            response["result"]["resume_initialized"] = json!(true);
            Ok(response)
        }), Arc::new(|_| {})));
        let reading = owner.clone();
        let background = std::thread::spawn(move || reading.refresh());
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        let error = owner.request(request("cluster_resume_initialize")).unwrap_err();
        assert_eq!(error, "Cluster session is busy. Wait for the current action to finish.");
        assert_eq!(calls.lock().unwrap().len(), 1, "busy initialization never reached the worker");
        release.send(()).unwrap(); background.join().unwrap().unwrap();
        assert_eq!(owner.request(request("cluster_resume_initialize")).unwrap()["result"]["resume_initialized"], true);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["method"], "cluster_resume_initialize");
        assert_eq!(calls[1]["params"], json!({}));
        assert!(calls.iter().all(|request| !matches!(request["method"].as_str(), Some("cluster_connect" | "cluster_disconnect"))));
    }

    #[test]
    fn commands_publish_without_a_main_renderer_and_are_never_retried() {
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let published = events.clone();
        let owner = ClusterSession::new(
            Arc::new(move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                if request["method"] == "cluster_command" {
                    Err("lost response".into())
                } else {
                    Ok(reply(&request, "changed"))
                }
            }),
            Arc::new(move |value| published.lock().unwrap().push(value)),
        );
        owner.request(request("cluster_filter")).unwrap();
        assert_eq!(
            events.lock().unwrap().last().unwrap()["response"]["query"],
            "changed"
        );
        assert!(owner.request(request("cluster_command")).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(events.lock().unwrap().last().unwrap()["available"], false);
        assert_eq!(
            events.lock().unwrap().last().unwrap()["response"]["query"],
            "changed"
        );
    }

    #[test]
    fn contest_setup_posts_one_bounded_dx_command_and_test_profile_never_reaches_transport() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let owner = ClusterSession::new(Arc::new(move |request| {
            recorded.lock().unwrap().push(request.clone());
            Ok(reply(&request, "posted"))
        }), Arc::new(|_| {}));

        owner.post_contest_spot("contest-setup", false, "om0rx", 14_025_500).unwrap();
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["method"], "cluster_command");
        assert_eq!(captured[0]["params"], json!({"command":"DX 14025.5 OM0RX"}));
        drop(captured);

        assert!(owner.post_contest_spot("contest-setup", true, "OM0RX", 14_025_500)
            .unwrap_err().contains("External services"));
        assert!(owner.post_contest_spot("main", false, "OM0RX", 14_025_500)
            .unwrap_err().contains("Only Contest Setup"));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn shutdown_rejects_requests_and_late_results() {
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let owner = ClusterSession::new(
            Arc::new(move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(reply(&request, ""))
            }),
            Arc::new(|_| {}),
        );
        owner.stop();
        assert!(owner.request(request("cluster_connect")).is_err());
        assert!(owner.refresh().is_err());
        assert!(owner
            .accept(&reply(&request("cluster_snapshot"), "late"), crate::worker::generation())
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn selection_is_only_a_known_spot_not_a_radio_command() {
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let owner = ClusterSession::new(
            Arc::new(move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(reply(&request, ""))
            }),
            Arc::new(|_| {}),
        );
        owner.refresh().unwrap();
        let observed_at = owner.projection_value().unwrap()["received_at_ms"].clone();
        assert!(observed_at.as_u64().unwrap() > 0);
        owner.select(Some("spot-a".into())).unwrap();
        assert!(owner.select(Some("unknown".into())).is_err());
        assert_eq!(owner.projection_value().unwrap()["selected"], "spot-a");
        owner.select(None).unwrap();
        for _ in 0..20 { assert_eq!(owner.projection_value().unwrap()["received_at_ms"], observed_at); }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn qsy_transaction_fence_vetoes_a_new_cluster_selection_until_release() {
        let owner = Arc::new(ClusterSession::new(
            Arc::new(|request| Ok(reply(&request, ""))), Arc::new(|_| {})));
        owner.refresh().unwrap();
        let revision = owner.projection_value().unwrap()["selection_revision"].as_u64().unwrap();
        let fence = owner.fence_selection(revision).unwrap();
        assert!(owner.select(Some("spot-a".into())).is_err());
        drop(fence);
        owner.select(Some("spot-a".into())).unwrap();
    }

    #[test]
    fn a_slow_command_cannot_accumulate_pollers_or_be_replayed_during_shutdown() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let events = Arc::new(AtomicU64::new(0));
        let published = events.clone();
        let owner = Arc::new(ClusterSession::new(
            Arc::new(move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Ok(reply(&request, "late"))
            }),
            Arc::new(move |_| {
                published.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        let active = owner.clone();
        let command = std::thread::spawn(move || active.request(request("cluster_connect")));
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // Connecting retires old report tokens before transport starts. That
        // wake is required; only publication after shutdown is prohibited.
        let before_shutdown = events.load(Ordering::SeqCst);
        assert_eq!(before_shutdown, 1);
        for _ in 0..20 {
            owner.refresh().unwrap();
        }
        assert!(owner.request(request("cluster_filter")).is_err());
        owner.stop();
        release_tx.send(()).unwrap();
        assert!(command.join().unwrap().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(events.load(Ordering::SeqCst), before_shutdown);
    }

    #[test]
    fn start_is_idempotent_and_does_not_depend_on_a_renderer_timer() {
        let (tx, rx) = std::sync::mpsc::channel();
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let owner = Arc::new(ClusterSession::new(
            Arc::new(move |request| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(reply(&request, "background"))
            }),
            Arc::new(move |_| {
                let _ = tx.send(());
            }),
        ));
        for _ in 0..10 {
            owner.start();
        }
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        owner.stop();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_reads_still_validate_parameters_and_failed_snapshots_retain_last_good_data() {
        let owner = ClusterSession::new(
            Arc::new(|request| Ok(reply(&request, "verified"))),
            Arc::new(|_| {}),
        );
        owner.refresh().unwrap();
        let mut invalid = request("cluster_snapshot");
        invalid["params"] = json!({"unexpected":true});
        assert!(owner.request(invalid).is_err());
        assert!(owner
            .accept(&json!({"ok":true,"result":{"snapshot":{"spots":"bad"}}}), crate::worker::generation())
            .is_err());
        assert_eq!(
            owner.projection_value().unwrap()["response"]["query"],
            "verified"
        );
    }

    #[test]
    fn invalid_command_snapshot_cannot_leave_the_previous_projection_marked_live() {
        let owner = ClusterSession::new(
            Arc::new(|request| {
                if request["method"] == "cluster_filter" {
                    Ok(json!({"id":request["id"], "ok":true, "result":{}}))
                } else {
                    Ok(reply(&request, "last-good"))
                }
            }),
            Arc::new(|_| {}),
        );
        owner.refresh().unwrap();
        assert!(owner.request(request("cluster_filter")).is_err());
        let projection = owner.projection_value().unwrap();
        assert_eq!(projection["available"], false);
        assert_eq!(projection["response"]["query"], "last-good");
    }

    #[test]
    fn retained_projection_drives_companion_spots_metadata_and_mutation_fence() {
        use crate::companion_host::inspect_retained_cluster_for_test as inspect;
        use crate::companion_projection::{now, NeedsFence};
        // Actual owner accept/projection path, with no renderer, global owner,
        // native worker, network, database or device connection.
        let owner = ClusterSession::new(
            Arc::new(|_| panic!("producer contract test must not dispatch")),
            Arc::new(|_| {}),
        );
        let generation = crate::worker::generation();
        let reply = |classification: u64, logbook: u64, call_status: &str| {
            json!({"ok":true,"result":{
                "snapshot":{"spots":[{"id":"producer-spot","callsign":"W1AW",
                    "frequencyKHz":14025.0,"mode":"CW","source":"DX cluster",
                    "receivedAt":now() as f64 / 1000.0 - 978307200.0,
                    "callStatus":call_status,"dxccStatus":"","score":42,
                    "scoreReasons":["Actual retained producer contract"]}],
                    "contactRevision":classification,"logbookDataLoaded":true},
                "contest_spots":[],"contact_refresh_state":"current",
                "contact_refresh_error":null,"contact_logbook_revision":logbook,
                "needs":{"generated_at_unix_ms":now(),"max_age_ms":2000,"readiness":"ready",
                    "authority":{"classification_revision":classification,"logbook_revision":logbook,
                        "country_reference_revision":"fixture-country-v1",
                        "policy_revision":"xlog-call-dxcc-needs-v1"}}
            }})
        };
        owner.accept_at_generation(&reply(5, 40, "New Call"), generation, generation).unwrap();
        let projection = owner.projection_value().unwrap();
        assert!(projection["response"].get("result").is_none(), "owner retains payload, not RPC envelope");
        assert_eq!(projection["response"]["snapshot"]["spots"].as_array().unwrap().len(), 1);
        let mut fence = NeedsFence::default();
        let (missing_classification, missing_logbook, missing_rows, missing_metadata) = inspect(&Value::Null, &mut fence);
        assert_eq!((missing_classification, missing_logbook), (None, None));
        assert_eq!(missing_rows["spots"], json!([]));
        assert_eq!(missing_metadata["readiness"], "unavailable");
        assert!(missing_metadata["authority"].is_null());
        let (classification, logbook, projected, metadata) = inspect(&projection, &mut fence);
        assert_eq!((classification, logbook), (Some(5), Some(40)));
        assert_eq!(projected["spots"].as_array().unwrap().len(), 1);
        assert_eq!(projected["spots"][0]["callsign"], "W1AW");
        assert_eq!(projected["spots"][0]["need"]["status"], "needed");
        assert_eq!(projected["spots"][0]["need"]["score"], 42);
        assert_eq!(metadata["readiness"], "ready");
        assert_eq!(metadata["authority"]["logbook_revision"], 40);

        // Mutation admission uses the SAME revision reader as this inspection.
        fence.begin("qso:producer-contract".into(), classification.unwrap());
        let (_, _, held, held_metadata) = inspect(&projection, &mut fence);
        assert_eq!(held["spots"][0]["need"]["status"], "unknown");
        assert_ne!(held_metadata["readiness"], "ready");
        assert!(held_metadata["authority"].is_null());
        fence.complete("qso:producer-contract", &Ok(json!({"ok":true,
            "result":{"status":"applied","logbook_revision":41}})));
        assert_eq!(inspect(&projection, &mut fence).2["spots"][0]["need"]["status"], "unknown");
        owner.accept_at_generation(&reply(6, 41, "Worked"), generation, generation).unwrap();
        let settled = owner.projection_value().unwrap();
        let (classification, _, projected, metadata) = inspect(&settled, &mut fence);
        assert_eq!(projected["spots"][0]["need"]["status"], "worked");
        assert_eq!(metadata["readiness"], "ready");
        assert_eq!(metadata["authority"]["classification_revision"], 6);

        // Legacy owner writes without a returned database revision must wait
        // for classification newer than the retained pre-write baseline.
        fence.begin("request:logbook_contact_apply:legacy".into(), classification.unwrap());
        fence.complete("request:logbook_contact_apply:legacy", &Ok(json!({"ok":true,"result":{"status":"applied"}})));
        assert_eq!(inspect(&settled, &mut fence).2["spots"][0]["need"]["status"], "unknown");
        owner.accept_at_generation(&reply(7, 42, "Worked"), generation, generation).unwrap();
        let settled = owner.projection_value().unwrap();
        assert_eq!(inspect(&settled, &mut fence).3["readiness"], "ready");

        // A real retained publication becoming unavailable must lose authority;
        // its last rows may remain visible only as Unknown.
        owner.unavailable();
        let unavailable = owner.projection_value().unwrap();
        let (_, _, projected, metadata) = inspect(&unavailable, &mut fence);
        assert_eq!(projected["spots"][0]["need"]["status"], "unknown");
        assert_eq!(metadata["readiness"], "unavailable");
        assert!(metadata["authority"].is_null());
        let mut expired = settled;
        expired["received_at_ms"] = json!(now() - 60_001);
        let (_, _, projected, metadata) = inspect(&expired, &mut fence);
        assert_eq!(projected["spots"][0]["need"]["status"], "unknown");
        assert_eq!(metadata["readiness"], "unavailable");
    }
    #[test]
    fn needs_poll_refreshes_twice_a_second_so_one_slow_reply_still_leaves_fresh_evidence() {
        assert_eq!(needs_poll_wait(Duration::from_millis(100)), Duration::from_millis(400));
        assert_eq!(needs_poll_wait(Duration::from_millis(900)), POLL_MIN_GAP);
        // Even a missed reply between two on-time ones stays inside the two-second lifetime.
        assert!(NEEDS_POLL_PERIOD * 3 < Duration::from_secs(2));
    }

    #[test]
    fn needs_poll_runs_only_while_a_companion_has_read_needs_recently() {
        assert!(!needs_wanted(50_000, 0), "no reader yet: no full-logbook needs poll");
        assert!(needs_wanted(50_000, 45_000));
        assert!(needs_wanted(50_000, 50_000 - NEEDS_READER_WINDOW_MS));
        assert!(!needs_wanted(50_000, 50_000 - NEEDS_READER_WINDOW_MS - 1));
    }

    #[test]
    fn dedicated_needs_evidence_is_retained_apart_from_the_spot_snapshot() {
        let owner = ClusterSession::new(
            Arc::new(|_| panic!("needs acceptance test must not dispatch")),
            Arc::new(|_| panic!("needs evidence must never be emitted to renderers")),
        );
        let ready = || Ok(json!({"ok":true,"result":{"needs":{"readiness":"ready","generated_at_unix_ms":1}}}));
        owner.accept_needs(ready(), 3, 3);
        let projection = owner.projection_value().unwrap();
        assert_eq!(projection["needs"]["readiness"], "ready");
        assert!(projection["needs_received_at_ms"].as_u64().unwrap() > 0);
        assert!(projection["response"].is_null(), "the spot projection is untouched");
        assert_eq!(projection["revision"], 0);
        // A replaced worker, a transport failure or a malformed body each clear it.
        owner.accept_needs(ready(), 3, 4);
        assert!(owner.projection_value().unwrap()["needs"].is_null());
        owner.accept_needs(ready(), 3, 3);
        owner.accept_needs(Err("timeout".into()), 3, 3);
        let cleared = owner.projection_value().unwrap();
        assert!(cleared["needs"].is_null());
        assert_eq!(cleared["needs_received_at_ms"], 0);
        owner.accept_needs(Ok(json!({"ok":true,"result":{"needs":null}})), 3, 3);
        assert!(owner.projection_value().unwrap()["needs"].is_null());
        owner.accept_needs(Ok(json!({"ok":false,"error":{"message":"busy"}})), 3, 3);
        assert!(owner.projection_value().unwrap()["needs"].is_null());
    }

    #[test]
    fn companion_needs_freshness_follows_the_dedicated_poll_not_the_slow_spot_snapshot() {
        use crate::companion_host::inspect_retained_cluster_for_test as inspect;
        use crate::companion_projection::{now, NeedsFence};
        let authority = json!({"classification_revision":5,"logbook_revision":40,
            "country_reference_revision":"fixture-country-v1","policy_revision":"xlog-call-dxcc-needs-v1"});
        let needs = |generated: u64| json!({"generated_at_unix_ms":generated,"max_age_ms":2000,
            "readiness":"ready","authority":authority});
        // The spot snapshot arrived 1.8 s ago carrying evidence stamped 2.5 s ago: expired.
        let mut cluster = json!({"available":true,"received_at_ms":now() - 1800,
            "response":{"snapshot":{"spots":[],"contactRevision":5},"contact_logbook_revision":40,
                "needs":needs(now() - 2500)}});
        let mut fence = NeedsFence::default();
        assert_eq!(inspect(&cluster, &mut fence).3["readiness"], "unavailable");
        // The dedicated poll's evidence, 200 ms old, keeps the authority ready.
        cluster["needs"] = needs(now() - 200);
        cluster["needs_received_at_ms"] = json!(now() - 150);
        let metadata = inspect(&cluster, &mut fence).3;
        assert_eq!(metadata["readiness"], "ready");
        assert_eq!(metadata["authority"]["logbook_revision"], 40);
        // Stale dedicated evidence is not rescued by the older snapshot copy.
        cluster["needs_received_at_ms"] = json!(now() - 2500);
        assert_eq!(inspect(&cluster, &mut fence).3["readiness"], "unavailable");
        // The spot snapshot must still be available: the fence reads its revisions.
        cluster["needs_received_at_ms"] = json!(now() - 150);
        cluster["available"] = json!(false);
        assert_eq!(inspect(&cluster, &mut fence).3["readiness"], "unavailable");
    }
}
