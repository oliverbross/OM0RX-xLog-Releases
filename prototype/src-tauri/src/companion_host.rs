//! Automatic local connection and projection adapter for the existing owners.
use crate::companion_bridge::Bridge;
use crate::companion_projection::{fresh, project, NeedsFence};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

struct Session {
    bridge: Arc<Bridge>,
    pairing: PathBuf,
    lease: Arc<AtomicBool>,
    worker: u64,
    workspace: String,
}
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static CONFIGURATION: Mutex<()> = Mutex::new(());
static WORKSPACE_EPOCH: AtomicU64 = AtomicU64::new(0);
static NEEDS: OnceLock<Mutex<NeedsFence>> = OnceLock::new();
pub fn shutdown() {
    crate::companion_tx::stop_all();
    crate::companion_rx::stop("Pairing revoked or workspace changed.");
    WORKSPACE_EPOCH.fetch_add(1, Ordering::AcqRel);
    if let Some(session) = SESSION.get().and_then(|owner| owner.lock().ok()?.take()) {
        session.lease.store(false, Ordering::Release);
        session.bridge.revoke();
        crate::companion_connection::remove_matching(&session.pairing, &session.bridge.endpoint, &session.bridge.token);
    }
}
// Invoked before workspace-changing requests reach the existing worker. Needs
// become conservative until a newer authoritative classification is retained.
// This is only a receive-route classification, never PTT admission. The marker
// is private to the already admitted companion PTT/lease dispatch stack.
fn preserves_receive_route(request: &Value, exact_dispatch: bool) -> bool {
    let p = &request["params"];
    if !exact_dispatch || request["method"] != "station_radio_control"
        || p["provider"] != "direct_flex_lan" || p["source"] != "gui"
        || !p["ptt_owner_id"].as_str().is_some_and(|id| !id.is_empty()) {
        return false;
    }
    let leased = p["digital_lease"]["lease_id"].as_str().is_some_and(|id| !id.is_empty())
        && p["digital_lease"]["expires_at_unix_ms"].as_u64().is_some();
    match p["control"].as_str() {
        Some("ptt") => p["enabled"] == false || p["enabled"] == true && leased,
        Some("ptt_lease") => leased,
        _ => false,
    }
}
pub(crate) fn is_receive_preserving_ptt(request: &Value) -> bool {
    preserves_receive_route(request, crate::companion_tx::is_exact_ptt_dispatch())
}
pub fn before_worker_request(request: &Value) -> Option<String> {
    if crate::station_telemetry::changes_radio(request) && !is_receive_preserving_ptt(request) {
        crate::companion_tx::stop_all();
        crate::companion_rx::invalidate_route("Radio context changed; reconnecting receive audio.");
    }
    let method = request["method"].as_str().unwrap_or("");
    if matches!(method, "workspace_create" | "backup_restore") {
        shutdown();
    }
    if crate::companion_projection::invalidates_needs(request) {
        let revision = crate::cluster_session::retained_owner()
            .and_then(|owner| owner.projection_value().ok())
            .and_then(|p| retained_cluster_revisions(&p).0)
            .unwrap_or(0);
        let key = if method == "logbook_companion_apply" {
            format!(
                "qso:{}",
                request["params"]["operationID"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
            )
        } else {
            format!("request:{method}:{}", request["id"].as_str().unwrap_or(""))
        };
        if let Ok(mut needs) = NEEDS
            .get_or_init(|| Mutex::new(NeedsFence::default()))
            .lock()
        {
            needs.begin(key.clone(), revision);
        }
        return Some(key);
    }
    None
}

#[cfg(test)]
mod receive_preserving_ptt_tests {
    use super::*;
    #[test]
    fn only_internal_exact_ptt_and_renewal_preserve_receive_route() {
        let mut request = json!({"method":"station_radio_control","params":{
            "provider":"direct_flex_lan","source":"gui","ptt_owner_id":"owned-operation",
            "control":"ptt","enabled":true,"digital_lease":{"lease_id":"owned-lease","expires_at_unix_ms":1000}}});
        assert!(preserves_receive_route(&request, true));
        assert!(!preserves_receive_route(&request, false));
        request["params"]["control"] = json!("ptt_lease");
        assert!(preserves_receive_route(&request, true));
        request["params"]["control"] = json!("set_frequency_a");
        assert!(!preserves_receive_route(&request, true));
        request["method"] = json!("station_reconnect");
        assert!(!preserves_receive_route(&request, true));
        request["method"] = json!("station_radio_control");
        request["params"]["control"] = json!("ptt");
        request["params"]["digital_lease"] = Value::Null;
        assert!(!preserves_receive_route(&request, true));
        request["params"]["enabled"] = json!(false);
        assert!(preserves_receive_route(&request, true));
        assert!(!preserves_receive_route(&request, false));
        request["params"]["enabled"] = Value::Null;
        assert!(!preserves_receive_route(&request, true));
    }
}
pub fn after_worker_request(key: Option<String>, response: &Result<Value, String>) {
    if let Some(key) = key {
        if let Ok(mut needs) = NEEDS
            .get_or_init(|| Mutex::new(NeedsFence::default()))
            .lock()
        {
            needs.complete(&key, response);
        }
    }
}

/// The saved audio route follows this exact connection lifetime.
pub fn active_lease() -> Option<Arc<AtomicBool>> {
    SESSION.get()?.lock().ok()?.as_ref()
        .filter(|s| s.bridge.active() && s.lease.load(Ordering::Acquire))
        .map(|s| s.lease.clone())
}

/// Production startup owns discovery; Settings reads never create a second session.
pub fn install(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        while !crate::APP_EXITING.load(Ordering::Acquire) {
            if crate::operating_module::selected_worker_value().ok() == Some("normal_logging") {
                let _ = request(app.clone(), "enable");
            } else if active_lease().is_some() {
                shutdown();
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

pub fn request(app: tauri::AppHandle, action: &str) -> Result<Value, String> {
    if cfg!(target_os = "macos") && action == "enable" {
        return Err("Companion integration is not enabled in the macOS application.".into());
    }
    let _configuration = CONFIGURATION
        .lock()
        .map_err(|_| "Companion settings unavailable.")?;
    if action == "revoke" {
        shutdown();
        return Ok(json!({"enabled":false}));
    }
    if action == "status" {
        let owner = SESSION
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "Companion settings unavailable.")?;
        return Ok(owner.as_ref().map(|s| json!({"enabled":s.bridge.active(),"endpoint":s.bridge.endpoint,"pairing_file":s.pairing})).unwrap_or(json!({"enabled":false})));
    }
    if action != "enable" {
        return Err("Unknown companion setting.".into());
    }
    if crate::APP_EXITING.load(Ordering::Acquire) { return Err("xLog is closing".into()); }
    if !crate::runtime_profile::companion_loopback() && crate::operating_module::selected_worker_value().ok() != Some("normal_logging") {
        return Err("xNexus uses the Normal Logging workspace".into());
    }
    let current = SESSION.get_or_init(|| Mutex::new(None)).lock()
        .map_err(|_| "Companion connection unavailable")?.as_ref()
        .filter(|s| s.bridge.active() && s.lease.load(Ordering::Acquire))
        .map(|s| (s.worker, s.workspace.clone(), s.bridge.endpoint.clone()));
    if let Some((worker, workspace, endpoint)) = current {
        if crate::worker::existing_generation().ok() == Some(worker) {
            // Workspace replacement can retain the same worker process. Never
            // reuse the old logbook namespace merely because its PID survived.
            let actual = crate::worker::request_existing_generation(
                json!({"id":uuid::Uuid::new_v4().to_string(),"method":"workspace_status","params":{}}), worker)?;
            if actual["ok"] == true && actual["result"]["workspace_exists"] == true
                && actual["result"]["generation"] == workspace {
                return Ok(json!({"enabled":true,"endpoint":endpoint}));
            }
        }
    }
    // Wait for ordinary xLog startup to create its worker; never start a replacement here.
    crate::worker::existing_generation()?;
    shutdown();
    let epoch = WORKSPACE_EPOCH.load(Ordering::Acquire);
    let (worker, workspace) = crate::worker::request_existing_with_generation(
        json!({"id":uuid::Uuid::new_v4().to_string(),"method":"workspace_status","params":{}}),
    )?;
    if workspace["ok"] != true || workspace["result"]["workspace_exists"] != true {
        return Err("Open an existing xLog workspace before pairing.".into());
    }
    let workspace_id = workspace["result"]["generation"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or("Workspace identity unavailable.")?
        .to_owned();
    NEEDS
        .get_or_init(|| Mutex::new(NeedsFence::default()))
        .lock()
        .map_err(|_| "Contact evidence unavailable.")?
        .bind_workspace(&workspace_id);
    let cache = Arc::new(Mutex::new(Cache::default()));
    let lease = Arc::new(AtomicBool::new(true));
    let admitted = lease.clone();
    let queries = Mutex::new(crate::companion_needs::QueryCache::default());
    let identity_workspace = workspace_id.clone();
    let tx_app=app.clone();
    let provider = Arc::new(move |command: Option<Value>| {
        if !admitted.load(Ordering::Acquire) {
            return Err("Pairing revoked.".into());
        }
        if let Some(query) = command.as_ref().filter(|c| !c["needs_query"].is_null()) {
            return needs_query(worker, &admitted, &queries, query);
        }
        crate::worker::require_existing_generation(worker)?;
        if let Some(after) = command.as_ref().and_then(|c| c["audio_after"].as_u64()) {
            return crate::companion_rx::read(after);
        }
        match command {
            None => snapshot(&cache, worker, &identity_workspace),
            Some(command) => execute(&app, worker, &cache, &admitted, command),
        }
    });
    let bridge = Bridge::start_with_controls(
        workspace_id.clone(),
        provider,
        crate::runtime_profile::offline_root().is_none(),
    )?;
    #[cfg(windows)]
    if crate::runtime_profile::offline_root().is_none(){
        let tx=match crate::companion_tx::owner(&tx_app) { Ok(tx)=>tx, Err(error)=>{bridge.revoke();return Err(error);} };
        let tx_lease=lease.clone();
        if let Err(error)=bridge.set_tx_provider(Arc::new(move|method,path,body,server,workspace|
            tx.request(method,path,body,server,workspace,worker,tx_lease.clone()))) {bridge.revoke();return Err(error);}
    }
    #[cfg(not(windows))]let _=tx_app;
    let result = write_pairing(&bridge);
    let pairing = match result {
        Ok(path) => path,
        Err(error) => {
            bridge.revoke();
            return Err(error);
        }
    };
    let value = json!({"enabled":true,"endpoint":bridge.endpoint,"pairing_file":pairing});
    let mut session = SESSION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Companion settings unavailable.")?;
    if WORKSPACE_EPOCH.load(Ordering::Acquire) != epoch {
        bridge.revoke();
        lease.store(false, Ordering::Release);
        crate::companion_connection::remove_matching(&pairing, &bridge.endpoint, &bridge.token);
        return Err("Workspace changed during pairing.".into());
    }
    *session = Some(Session {
        bridge,
        pairing,
        lease,
        worker,
        workspace:workspace_id,
    });
    Ok(value)
}

pub fn tx_request(app:tauri::AppHandle,action:&str,params:Value)->Result<Value,String>{
    if matches!(action,"stop"|"disable"){
        return crate::companion_tx::settings(&app,action,params,"",0);
    }
    let session=SESSION.get_or_init(||Mutex::new(None)).lock().map_err(|_|"Companion settings unavailable")?
        .as_ref().filter(|s|s.bridge.active()&&s.lease.load(Ordering::Acquire)).map(|s|(s.worker,s.workspace.clone()));
    if action == "status" {
        let (worker, workspace) = session.unwrap_or_default();
        return crate::companion_tx::settings(&app, action, params, &workspace, worker);
    }
    let (worker,workspace)=session.ok_or("Enable xNexus pairing before using digital TX")?;
    crate::companion_tx::settings(&app,action,params,&workspace,worker)
}

pub fn rx_request(app: tauri::AppHandle, action: &str, params: Value) -> Result<Value, String> {
    if action == "stop" || action == "status" {
        return crate::companion_rx::request(
            &app,
            action,
            params,
            Arc::new(AtomicBool::new(false)),
        );
    }
    let _configuration = CONFIGURATION
        .lock()
        .map_err(|_| "Companion settings unavailable.")?;
    let lease = SESSION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Companion settings unavailable.")?
        .as_ref()
        .filter(|s| s.bridge.active())
        .map(|s| s.lease.clone())
        .ok_or("Enable xNexus pairing before sharing receive audio.")?;
    // Native open may block in a driver. Pairing revoke must remain independent
    // of that wait; the RX owner checks this exact lease and its own generation.
    drop(_configuration);
    crate::companion_rx::request(&app, action, params, lease)
}

fn write_pairing(bridge: &Bridge) -> Result<PathBuf, String> {
    crate::companion_connection::publish(&json!({"protocol_version":1,"application":"OM0RX-xLog",
        "endpoint":bridge.endpoint,"token":bridge.token}))
}

#[derive(Default)]
struct Cache {
    revision: u64,
    current: Value,
    evidence: Value,
}
// ClusterSession retains the worker RESULT payload, not its RPC envelope.
// Share this read between mutation admission and publication fence settlement.
fn retained_cluster_revisions(cluster: &Value) -> (Option<u64>, Option<u64>) {
    let payload = &cluster["response"];
    (payload["snapshot"]["contactRevision"].as_u64(),
     payload["contact_logbook_revision"].as_u64())
}
fn projections() -> Value {
    let radio = if let Some(config) = crate::companion_audio_config::selected() {
        let mut chosen = crate::companion_audio_config::selected_projection().unwrap_or(Value::Null);
        if let Some(route) = crate::companion_rx_buffer::route_for(&chosen, crate::companion_projection::now(), &config.slice) {
            // The companion display follows its chosen slice, independently of
            // which slice has keyboard focus in SmartSDR.
            chosen["snapshot"]["radio"]["frequency_hz"] = json!(route.frequency_hz);
            chosen["snapshot"]["radio"]["mode"] = json!(route.mode);
        }
        chosen
    } else {
        let active = crate::radio_slots::retained_active().ok();
        crate::station_telemetry::retained_snapshots().into_iter()
            .find(|v| active.as_ref().is_some_and(|active| v["radio_id"] == active.radio_id))
            .unwrap_or(Value::Null)
    };
    let rotor = crate::rotor_telemetry::retained_snapshot().unwrap_or(Value::Null);
    let cluster = crate::cluster_session::retained_owner()
        .and_then(|owner| owner.projection_value().ok())
        .unwrap_or(Value::Null);
    let (classification_revision, logbook_revision) = retained_cluster_revisions(&cluster);
    let invalid = NEEDS
        .get_or_init(|| Mutex::new(NeedsFence::default()))
        .lock()
        .map(|mut needs| {
            needs.invalid(
                classification_revision,
                logbook_revision,
            )
        })
        .unwrap_or(true);
    json!({"radio":radio,"rotor":rotor,"cluster":cluster,"needs_invalid":invalid})
}
fn snapshot(cache: &Mutex<Cache>, worker: u64, workspace: &str) -> Result<Value, String> {
    let evidence = projections();
    let mut value = project(&evidence);
    value["needs"] = needs_metadata(&evidence);
    value["station_identity"] = crate::companion_identity::project(
        &crate::station_identity::snapshot().unwrap_or(Value::Null), worker, workspace,
        crate::operating_module::selected_worker_value().ok(),
    );
    let mut cache = cache
        .lock()
        .map_err(|_| "Companion snapshot unavailable.")?;
    if cache.evidence != evidence
        || cache.current["station"] != value["station"]
        || cache.current["rotor"] != value["rotor"]
        || cache.current["spots"] != value["spots"]
        || cache.current["needs"] != value["needs"]
        || cache.current["station_identity"] != value["station_identity"]
        || !fresh(&cache.current["generated_at_unix_ms"], 1_000)
    {
        cache.revision = cache
            .revision
            .checked_add(1)
            .ok_or("Companion revision exhausted.")?;
        cache.current = value;
        cache.current["revision"] = json!(cache.revision);
        cache.evidence = evidence;
    }
    Ok(cache.current.clone())
}

fn needs_metadata(evidence: &Value) -> Value {
    // A companion is reading needs: keep the dedicated needs poll running.
    crate::cluster_session::note_needs_reader();
    let cluster = &evidence["cluster"];
    // Prefer the dedicated needs poll's evidence: it is not delayed behind the
    // large spot snapshot reply. The spot snapshot must still be available,
    // because the needs fence reads its classification/logbook revisions.
    let dedicated = cluster["needs"].is_object();
    let retained = if dedicated { &cluster["needs"] } else { &cluster["response"]["needs"] };
    let received = if dedicated { &cluster["needs_received_at_ms"] } else { &cluster["received_at_ms"] };
    let fresh = cluster["available"] == true && fresh(received, 2000)
        && retained["generated_at_unix_ms"].as_u64().is_some_and(|t| {
            let now = crate::companion_needs::now_ms();
            t <= now.saturating_add(50) && now.saturating_sub(t) <= 2000
        });
    let state = if evidence["needs_invalid"] == true {
        if NEEDS.get().and_then(|n| n.lock().ok()).is_some_and(|n| n.uncertain()) { "uncertain" } else { "refreshing" }
    } else if !fresh { "unavailable" }
    else if retained["readiness"] == "ready" && crate::companion_needs::authority_valid(&retained["authority"]) { "ready" }
    else { match retained["readiness"].as_str() { Some("refreshing") => "refreshing", Some("stale") => "stale", _ => "unavailable" } };
    json!({"readiness":state,"authority":if state == "ready" {retained["authority"].clone()} else {Value::Null},
        "generated_at_unix_ms":if fresh {retained["generated_at_unix_ms"].clone()} else {json!(crate::companion_needs::now_ms())},"max_age_ms":2000})
}

// Tests feed this through the actual native producer without installing a
// global owner, launching a worker, accessing a database or touching hardware.
#[cfg(test)]
pub(crate) fn inspect_retained_cluster_for_test(cluster: &Value, fence: &mut NeedsFence)
    -> (Option<u64>, Option<u64>, Value, Value)
{
    let (classification, logbook) = retained_cluster_revisions(cluster);
    let evidence = json!({"cluster":cluster,"needs_invalid":fence.invalid(classification, logbook)});
    (classification, logbook, project(&evidence), needs_metadata(&evidence))
}

fn needs_query(worker: u64, lease: &AtomicBool, queries: &Mutex<crate::companion_needs::QueryCache>, value: &Value) -> Result<Value,String> {
    let batch = crate::companion_needs::parse(&serde_json::to_vec(&value["needs_query"]).map_err(|_| "Invalid needs query.")?)?;
    let workspace = value["workspace_id"].as_str().ok_or("Needs workspace unavailable.")?;
    let guard = || {
        if !lease.load(Ordering::Acquire) { return Err("Pairing revoked.".to_owned()); }
        crate::worker::require_needs_generation(worker)
    };
    guard()?;
    // Stamp before reading retained metadata; any begin/complete during either
    // observation makes this optional result stale. No fence lock spans IO.
    let stamp = NEEDS.get_or_init(|| Mutex::new(NeedsFence::default())).lock()
        .map_err(|_| "Needs fence unavailable.")?.stamp();
    let before = needs_metadata(&projections());
    if before["readiness"] != "ready" {
        queries.lock().map_err(|_| "Needs cache unavailable.")?.clear();
        return Ok(crate::companion_needs::unknown(&batch, before["readiness"].as_str().unwrap_or("unavailable")));
    }
    if crate::cluster_session::retained_owner().is_none() { return Ok(crate::companion_needs::unknown(&batch,"unavailable")); }
    let context = json!({"worker":worker,"workspace":workspace,"stamp":stamp,"authority":before["authority"]});
    let result = queries.try_lock().map_err(|_| "Needs lookup busy.")?
        .query(&batch, context, |batch| crate::worker::enqueue_needs(batch, workspace.to_owned(), worker))?;
    guard()?;
    let after = needs_metadata(&projections());
    let current_stamp = NEEDS.get().ok_or("Needs fence unavailable.")?.lock().map_err(|_| "Needs fence unavailable.")?.stamp();
    if stamp != current_stamp || after["readiness"] != "ready"
        || before["authority"] != after["authority"] || result["authority"] != after["authority"] {
        queries.lock().map_err(|_| "Needs cache unavailable.")?.clear();
        return Ok(crate::companion_needs::unknown(&batch, "stale"));
    }
    crate::companion_needs::validate_reply(&result, &batch)?;
    Ok(result)
}
fn execute(
    app: &tauri::AppHandle,
    worker: u64,
    cache: &Mutex<Cache>,
    lease: &AtomicBool,
    value: Value,
) -> Result<Value, String> {
    let guard = || {
        if !lease.load(Ordering::Acquire) {
            return Err("Pairing revoked.".into());
        }
        crate::worker::require_existing_generation(worker)
    };
    guard()?;
    if let Some(call) = value["bearing_lookup"].as_str() {
        let workspace = value["workspace_id"].as_str().ok_or("bearing_workspace_changed")?;
        if crate::companion_bearing::normalized_call(call).is_none() { return Err("bearing_invalid_request".into()); }
        let began = std::time::Instant::now();
        let started = crate::companion_bearing::now_ms()?;
        // Only the retained Settings identity supplies origin. Never load
        // Settings/credentials or accept a companion-provided locator here.
        let identity = || -> Result<Value, String> {
            let retained = crate::station_identity::snapshot().map_err(|_| "bearing_identity_unavailable")?;
            Ok(crate::companion_identity::project(&retained, worker, workspace,
                crate::operating_module::selected_worker_value().ok()))
        };
        let before = identity()?;
        let origin = crate::companion_bearing::identity_origin(&before, workspace, worker)?;
        let response = crate::worker::request_existing_generation(
            json!({"id":uuid::Uuid::new_v4().to_string(),"method":"logbook_companion_bearing",
                "params":{"callsign":call,"originGrid":&origin,"generation":workspace}}), worker);
        guard()?;
        crate::companion_bearing::validate_identity_pair(&before, &identity()?, workspace, worker)?;
        let response = response.map_err(|_| "bearing_unavailable")?;
        if response["ok"] != true {
            return Err(match response["error"]["code"].as_str() {
                Some("workspace_busy") => "bearing_busy",
                Some("bearing_changed") => "bearing_stale",
                Some("workspace_changed") => "bearing_workspace_changed",
                _ => "bearing_unavailable",
            }.into());
        }
        let mut result = response["result"].clone();
        crate::companion_bearing::validate_owner(&result, call, workspace, Some(&origin), started,
            crate::companion_bearing::now_ms()?, began.elapsed())?;
        result["worker_generation"] = json!(worker);
        result["station_identity_binding"] = before["binding"].clone();
        return Ok(result);
    }
    if let Some(id) = value["contact_detail_lookup"].as_str() {
        let workspace = value["workspace_id"].as_str().ok_or("contact_detail_workspace_changed")?;
        if !crate::companion_contact_detail::canonical_id(id) { return Err("contact_detail_invalid_request".into()); }
        let began = std::time::Instant::now();
        let started = crate::companion_contact_detail::now_ms()?;
        let response = crate::worker::request_existing_generation(
            json!({"id":uuid::Uuid::new_v4().to_string(),"method":"logbook_companion_contact_detail",
                "params":{"contactID":id,"generation":workspace}}), worker);
        guard()?;
        let response = response.map_err(|_| "contact_detail_unavailable")?;
        if response["ok"] != true {
            return Err(if response["error"]["code"] == "workspace_busy" { "contact_detail_busy" }
                else { "contact_detail_unavailable" }.into());
        }
        let mut result = response["result"].clone();
        crate::companion_contact_detail::validate_owner(&result, id, workspace, started,
            crate::companion_contact_detail::now_ms()?, began.elapsed())?;
        result["worker_generation"] = json!(worker);
        return Ok(result);
    }
    if value["history_lookup"].is_string() || value["recent_lookup"] == true {
        let mut params = json!({"generation":value["workspace_id"]});
        let method = if let Some(call) = value["history_lookup"].as_str() {
            params["callsign"] = json!(call);
            for key in ["band", "mode"] { if !value[key].is_null() { params[key] = value[key].clone(); } }
            "logbook_companion_history"
        } else { "logbook_companion_recent" };
        let response = crate::worker::request_existing_generation(
            json!({"id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params}),
            worker,
        )?;
        guard()?;
        if response["ok"] != true || response["result"]["status"] == "workspace_changed"
            || response["result"]["generation"] != value["workspace_id"] {
            return Err("History unavailable for this workspace.".into());
        }
        return Ok(response["result"].clone());
    }
    if let Some(id) = value["receipt_lookup"].as_str() {
        let response = crate::worker::request_existing_generation(
            json!({"id":uuid::Uuid::new_v4().to_string(),"method":"logbook_companion_receipt","params":{"operationID":id,"generation":value["workspace_id"]}}),
            worker,
        )?;
        if response["ok"] != true || response["result"]["status"] == "workspace_changed" {
            return Err("Durable QSO receipt unavailable for this workspace.".into());
        }
        if response["result"]["receipt"].is_null() {
            return Ok(Value::Null);
        }
        after_worker_request(
            Some(format!("qso:{}", id.to_ascii_lowercase())),
            &Ok(response.clone()),
        );
        return Ok(contact_receipt(&response["result"]["receipt"]));
    }
    let command = &value["command"];
    let kind = command["kind"].as_str().ok_or("Missing command kind.")?;
    // Defense in depth: the transport's reduced capability list is not authority
    // to issue a physical command from a disposable profile.
    if crate::runtime_profile::offline_root().is_some()
        && !matches!(kind, "logbook.create" | "logbook.update")
    {
        return Err(
            "Station and rotor controls are disabled in disposable companion acceptance.".into(),
        );
    }
    if kind == "rotor.stop" {
        let route = command["rotor_id"]
            .as_str()
            .ok_or("Unknown rotor identity.")?;
        // The coordinator binds the retained route to its actual connection,
        // bypassing telemetry/motion freshness without targeting a replacement.
        guard()?;
        return Ok(match crate::rotor::companion_stop(route, guard) {
            Ok(_) => {
                json!({"status":"accepted","message":"xLog acknowledged rotor Stop; inspect physical motion."})
            }
            Err(e) => json!({"status":"uncertain","message":e}),
        });
    }
    if !matches!(
        kind,
        "station.tune" | "rotor.point" | "logbook.create" | "logbook.update"
    ) {
        return Err("Capability not implemented by this xLog owner.".into());
    }
    let _module = crate::operating_module::owner()?.admit_current()?;
    let (observation, evidence) = {
        let cache = cache.lock().map_err(|_| "Snapshot unavailable.")?;
        if value["expected_revision"].as_u64() != Some(cache.revision)
            || !fresh(&cache.current["generated_at_unix_ms"], 3_000)
        {
            return Err("Snapshot changed or expired. Review the latest xLog state.".into());
        }
        (cache.current.clone(), cache.evidence.clone())
    };
    let id = value["request_id"]
        .as_str()
        .ok_or("Missing operation identity.")?;
    if matches!(kind, "logbook.create" | "logbook.update") {
        if !command["qso"].is_object() {
            return Err("A complete immutable QSO draft is required.".into());
        }
        let mut params =
            json!({"operationID":id,"generation":value["workspace_id"],"qso":command["qso"]});
        if kind == "logbook.update" {
            params["expectedRevision"] = json!(command["expected_contact_revision"]
                .as_u64()
                .filter(|revision| *revision > 0 && *revision < 9_007_199_254_740_991)
                .ok_or("An exact contact revision is required.")?);
        }
        guard()?;
        return Ok(
            match crate::worker::request_existing_generation(
                json!({"id":uuid::Uuid::new_v4().to_string(),"method":"logbook_companion_apply","params":params}),
                worker,
            ) {
                Ok(response)
                    if response["ok"] == true && response["result"]["status"] == "applied" =>
                {
                    contact_receipt(&response["result"]["receipt"])
                }
                Ok(response)
                    if response["ok"] == true
                        && matches!(
                            response["result"]["status"].as_str(),
                            Some("rejected" | "workspace_changed")
                        ) =>
                {
                    json!({"status":"rejected","message":"Contact or workspace changed; review xLog before editing."})
                }
                Ok(_) => {
                    json!({"status":"uncertain","message":"xLog did not confirm the QSO result; check its durable receipt before retrying."})
                }
                Err(error) => json!({"status":"uncertain","message":error}),
            },
        );
    }
    let result = if kind == "station.tune" {
        let radio_id = command["radio_id"]
            .as_str()
            .ok_or("Explicit radio identity required.")?;
        if observation["station"]["radio_id"] != radio_id
            || observation["station"]["connected"] != true
        {
            return Err("Radio identity or connection changed.".into());
        }
        let configured = crate::companion_audio_config::selected();
        let _active = if let Some(config) = configured.as_ref() {
            if config.radio_id != radio_id { return Err("Configured radio changed".into()); }
            None
        } else {
            let active = crate::radio_slots::retained_active()?;
            if active.radio_id != radio_id { return Err("Operating radio changed.".into()); }
            Some(crate::radio_slots::fence_retained_active(&active)?)
        };
        let owner = crate::station_session::owner_for(app, radio_id)?;
        let current = owner.snapshot()?;
        let selection = crate::station_selection::owner_for(app, radio_id)?.snapshot()?;
        let radio = &evidence["radio"];
        if selection["selection_id"] != radio["selection_id"]
            || selection["generation"] != radio["selection_generation"]
        {
            return Err("Radio selection changed.".into());
        }
        let mut params = json!({"action":"manual_tune","operation_id":id,"generation":current["generation"],"revision":current["revision"],"radio_id":radio_id,
            "selection_id":selection["selection_id"],"selection_generation":selection["generation"],"control_revision":selection["control_revision"],"provider":radio["provider"],
            "connection_generation":radio["snapshot"]["generation"],"worker_generation":worker,"frequency_hz":command["frequency_hz"],"mode":command["mode"]});
        if let Some(config) = configured.as_ref() { params["companion_slice"] = json!(config.slice); }
        crate::manual_tune::params(&params)?;
        owner.apply(&params, |request, generation, identity| {
            if configured.is_some() {
                crate::companion_audio_config::execute_tune(app, request, radio, guard)
            } else {
                crate::manual_tune::execute_guarded(app, &params, request, generation, identity, guard)
            }
        })
    } else {
        let rotor = &evidence["rotor"];
        let rotor_id = command["rotor_id"]
            .as_str()
            .ok_or("Explicit rotor identity required.")?;
        if observation["rotor"]["rotor_id"] != rotor_id
            || observation["rotor"]["connected"] != true
        {
            return Err("Rotor identity or connection changed.".into());
        }
        let rotor_owner = crate::rotor::companion_owner(rotor_id)?;
        let owner = crate::station_session::owner(app)?;
        let current = owner.snapshot()?;
        let params = json!({"action":"manual_rotor_goto","operation_id":id,"generation":current["generation"],"revision":current["revision"],"rotor_owner":rotor_owner,
            "rotor_generation":rotor["snapshot"]["generation"],"rotor_revision":rotor["revision"],"worker_generation":worker,"bearing":command["azimuth_deg"],"label":"xNexus manual target"});
        crate::manual_rotor::params(&params)?;
        owner.apply(&params, |request, _, _| {
            guard()?;
            crate::manual_rotor::execute(&params, request)
        })
    };
    Ok(match result {
        Ok(receipt) => {
            json!({"status":match receipt["status"].as_str(){Some("confirmed")=>"observed",Some("acknowledged")=>"accepted",Some("rejected")=>"rejected",_=>"uncertain"},"message":"Inspect xLog's retained station state and operation receipt."})
        }
        Err(error) => json!({"status":"uncertain","message":error}),
    })
}

fn contact_receipt(receipt: &Value) -> Value {
    let contact = receipt["contact"]["qso"]["id"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok());
    if contact.is_none() || receipt["contact"]["revision"].as_u64().is_none() {
        return json!({"status":"uncertain","message":"The durable owner receipt was incomplete; review the contact in xLog."});
    }
    json!({"status":"applied","message":"QSO and receipt committed together in xLog.","contact_id":contact.unwrap().to_string(),"contact_revision":receipt["contact"]["revision"]})
}
