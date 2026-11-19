//! Opt-in, native-only loopback boundary. No hardware or database ownership.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub type Provider = dyn Fn(Option<Value>) -> Result<Value, String> + Send + Sync;
pub type TxProvider = dyn Fn(&str, &str, &[u8], &str, &str) -> Result<Value, String> + Send + Sync;
fn history_query(raw: &str) -> Result<Value, String> {
    let invalid = || "Invalid history query.".to_owned();
    let mut parts = raw.split('&');
    let call = parts.next().ok_or_else(invalid)?.replace("%2F", "/").replace("%2f", "/");
    if call.is_empty() || call.len() > 32 || !call.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'/' || b == b'-') {
        return Err(invalid());
    }
    let mut result = json!({"history_lookup":call});
    for part in parts {
        let (key, value) = part.split_once('=').ok_or_else(invalid)?;
        if !matches!(key, "band" | "mode") || !result[key].is_null() { return Err(invalid()); }
        if key == "band" {
            let band = value.to_ascii_lowercase();
            let number = band.strip_suffix("mm").or_else(|| band.strip_suffix("cm")).or_else(|| band.strip_suffix('m')).ok_or_else(invalid)?;
            let digits: Vec<_> = number.split('.').collect();
            if digits.is_empty() || digits.len() > 2 || digits.iter().enumerate().any(|(i, v)| v.is_empty() || v.len() > (if i == 0 { 4 } else { 3 }) || !v.bytes().all(|b| b.is_ascii_digit())) {
                return Err(invalid());
            }
            result[key] = json!(band);
        } else {
            if value.is_empty() || value.len() > 20 || !value.as_bytes()[0].is_ascii_alphanumeric()
                || !value.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-') { return Err(invalid()); }
            result[key] = json!(value);
        }
    }
    if !result["mode"].is_null() && result["band"].is_null() { return Err(invalid()); }
    Ok(result)
}
pub fn companion_fixture_marker(bytes: &[u8]) -> bool {
    bytes.len() <= 1024
        && serde_json::from_slice::<Value>(bytes)
            .ok()
            .is_some_and(|marker| {
                marker["schema"] == 1
                    && marker["companionLoopback"] == true
                    && marker["id"]
                        .as_str()
                        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            })
}
pub struct Bridge {
    pub endpoint: String,
    pub token: String,
    pub server_instance: String,
    pub workspace_id: String,
    active: AtomicBool,
    in_flight: AtomicUsize,
    ordinary: AtomicUsize,
    needs_read: AtomicUsize,
    contact_read: AtomicUsize,
    bearing_read: AtomicUsize,
    tx_ordinary: AtomicUsize,
    tx_provider: Mutex<Option<Arc<TxProvider>>>,
    provider: Arc<Provider>,
    controls_enabled: bool,
    receipts: Mutex<HashMap<String, (Value, Value)>>,
}

impl Bridge {
    pub fn start(workspace_id: String, provider: Arc<Provider>) -> Result<Arc<Self>, String> {
        Self::start_with_controls(workspace_id, provider, true)
    }
    pub fn start_with_controls(
        workspace_id: String,
        provider: Arc<Provider>,
        controls_enabled: bool,
    ) -> Result<Arc<Self>, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let service = Arc::new(Self {
            endpoint: format!(
                "http://{}",
                listener.local_addr().map_err(|e| e.to_string())?
            ),
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            server_instance: uuid::Uuid::new_v4().to_string(),
            workspace_id,
            active: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
            ordinary: AtomicUsize::new(0),
            needs_read: AtomicUsize::new(0),
            contact_read: AtomicUsize::new(0),
            bearing_read: AtomicUsize::new(0),
            tx_ordinary: AtomicUsize::new(0),
            tx_provider: Mutex::new(None),
            provider,
            controls_enabled,
            receipts: Mutex::new(HashMap::new()),
        });
        let owner = service.clone();
        std::thread::Builder::new()
            .name("xlog-companion-listener".into())
            .spawn(move || {
                while owner.active.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, peer)) if peer.ip().is_loopback() => {
                            if owner
                                .in_flight
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                    (n < 64).then_some(n + 1)
                                })
                                .is_err()
                            {
                                continue;
                            }
                            let owner = owner.clone();
                            std::thread::spawn(move || {
                                owner.serve(stream);
                                owner.in_flight.fetch_sub(1, Ordering::AcqRel);
                            });
                        }
                        Ok(_) => (),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(service)
    }
    pub fn set_tx_provider(&self, provider: Arc<TxProvider>) -> Result<(), String> {
        if !self.controls_enabled { return Err("Isolated companion cannot install a live TX owner".into()); }
        let mut current = self.tx_provider.lock().map_err(|_| "TX owner unavailable")?;
        if current.is_some() { return Err("TX owner already installed".into()); }
        *current = Some(provider);
        Ok(())
    }
    pub fn revoke(&self) {
        self.active.store(false, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    fn serve(&self, mut stream: TcpStream) {
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let mut reader = DeadlineSocket {
            stream: &mut stream,
            deadline: std::time::Instant::now() + Duration::from_secs(2),
        };
        let authorization = format!("Bearer {}", self.token);
        let result = read_request_authenticated(&mut reader, Duration::from_secs(2),
            Some((&authorization, self.endpoint.trim_start_matches("http://"))))
            .and_then(|request| self.route(request));
        let (status, body) =
            result.unwrap_or_else(|(status, message)| (status, json!({"error":message})));
        let body = serde_json::to_vec(&body).unwrap_or_default();
        if body.len() > 2 * 1024 * 1024 {
            return;
        }
        let _ = write!(stream, "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n", if status == 200 { "OK" } else { "Error" }, body.len());
        let _ = stream.write_all(&body);
    }
    fn route(&self, request: Request) -> Result<(u16, Value), (u16, String)> {
        if !self.active() {
            return Err((401, "Pairing revoked.".into()));
        }
        let expected = format!("Bearer {}", self.token);
        if request
            .authorization
            .as_deref()
            .is_none_or(|value| !constant_eq(value.as_bytes(), expected.as_bytes()))
        {
            return Err((401, "Pairing required.".into()));
        }
        if request.host != self.endpoint.trim_start_matches("http://") {
            return Err((400, "Invalid loopback Host.".into()));
        }
        let digital_stop = request.method == "POST" && request.path == "/xlog/v1/tx/stop";
        let stop = digital_stop || request.method == "POST"
            && request.path == "/xlog/v1/command"
            && serde_json::from_slice::<Value>(&request.body)
                .ok()
                .is_some_and(|value| value["command"]["kind"] == "rotor.stop");
        // Slow ordinary owner work has its own bounded budget. Stop keeps an
        // independent admission lane after bounded parsing and authentication.
        struct Slot<'a>(Option<&'a AtomicUsize>);
        impl Drop for Slot<'_> {
            fn drop(&mut self) {
                if let Some(count) = self.0 {
                    count.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
        let _slot = if stop {
            Slot(None)
        } else if request.path.starts_with("/xlog/v1/tx/") {
            if request.method == "GET" || request.path == "/xlog/v1/tx/heartbeat" {
                Slot(None)
            } else {
                self.tx_ordinary.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .map_err(|_| (503, "Digital TX preparation busy; exact Stop remains available.".into()))?;
                Slot(Some(&self.tx_ordinary))
            }
        } else if request.method == "POST" && request.path == "/xlog/v1/needs/query" {
            self.needs_read.compare_exchange(0,1,Ordering::AcqRel,Ordering::Acquire)
                .map_err(|_| (503,"Needs lookup is busy.".into()))?;
            Slot(Some(&self.needs_read))
        } else if request.path == "/xlog/v1/bearing" || request.path.starts_with("/xlog/v1/bearing?") {
            self.bearing_read.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| (503, "bearing_busy".into()))?;
            Slot(Some(&self.bearing_read))
        } else if request.path.starts_with("/xlog/v1/logbook/contacts/") {
            self.contact_read.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .map_err(|_| (503, "contact_detail_busy".into()))?;
            Slot(Some(&self.contact_read))
        } else {
            self.ordinary
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 8).then_some(n + 1)
                })
                .map_err(|_| (503, "Companion reads busy; Stop remains available.".into()))?;
            Slot(Some(&self.ordinary))
        };
        if request.method == "GET" && request.path == "/xlog/v1/hello" {
            // The provider also fences a retired worker/workspace before hello.
            (self.provider)(None).map_err(|e| (503, e))?;
            let mut capabilities = if self.controls_enabled {
                vec![
                    "snapshot.read",
                    "station.tune",
                    "rotor.point",
                    "rotor.stop",
                    "logbook.read",
                    "logbook.write",
                ]
            } else {
                vec!["snapshot.read", "logbook.read", "logbook.write"]
            };
            if cfg!(windows) && self.controls_enabled {
                capabilities.push("audio.rx");
            }
            capabilities.push("logbook.classify");
            capabilities.push("logbook.contact.read");
            capabilities.push("bearing.read");
            if self.controls_enabled && self.tx_provider.lock().map_err(|_| (503, "TX owner unavailable".into()))?.is_some() {
                capabilities.push("digital.tx");
            }
            return Ok((
                200,
                json!({"application":"OM0RX-xLog","protocol_version":1,"server_instance":self.server_instance,
                "workspace_id":self.workspace_id,"capabilities":capabilities}),
            ));
        }
        if request.path.starts_with("/xlog/v1/tx/") {
            let provider = self.tx_provider.lock().map_err(|_| (503, "TX owner unavailable".into()))?
                .clone().ok_or((404, "Digital TX owner unavailable".into()))?;
            // Do not hold the provider/configuration or ordinary-read locks
            // while preparation or release runs. Exact Stop has its own lane.
            return provider(&request.method, &request.path, &request.body, &self.server_instance, &self.workspace_id)
                .map(|value| (200, value)).map_err(|error| (409, error));
        }
        if request.method == "GET" && request.path == "/xlog/v1/snapshot" {
            let mut value = (self.provider)(None).map_err(|e| (503, e))?;
            value["protocol_version"] = json!(1);
            value["server_instance"] = json!(self.server_instance);
            value["workspace_id"] = json!(self.workspace_id);
            return Ok((200, value));
        }
        if request.method == "POST" && request.path == "/xlog/v1/needs/query" {
            let batch = crate::companion_needs::parse(&request.body).map_err(|e| (400,e))?;
            let provided = (self.provider)(Some(json!({"needs_query":batch,"workspace_id":self.workspace_id})));
            if !self.active() { return Err((401,"Pairing revoked.".into())); }
            let mut result = match provided {
                Err(error) if error == crate::companion_needs::PENDING => {
                    return Ok((503, json!({"protocol_version":1,"server_instance":self.server_instance,
                        "workspace_id":self.workspace_id,"error":"needs_pending"})));
                }
                result => result.map_err(|error| (503,error))?,
            };
            crate::companion_needs::validate_reply(&result, &batch).map_err(|e| (503,e))?;
            result["protocol_version"] = json!(1);
            result["server_instance"] = json!(self.server_instance);
            result["workspace_id"] = json!(self.workspace_id);
            if serde_json::to_vec(&result).map_err(|_| (503,"Needs serialization failed.".into()))?.len() > crate::companion_needs::MAX_REPLY {
                return Err((503,"Needs result exceeds its byte limit.".into()));
            }
            return Ok((200,result));
        }
        if request.method == "GET" && request.path.starts_with("/xlog/v1/audio?after=") {
            if !cfg!(windows) || !self.controls_enabled {
                return Err((404, "RX sharing is unavailable in this profile.".into()));
            }
            let raw = request.path.trim_start_matches("/xlog/v1/audio?after=");
            if raw.is_empty() || raw.len() > 20 || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err((400, "Invalid audio cursor.".into()));
            }
            let after = raw
                .parse::<u64>()
                .map_err(|_| (400, "Invalid audio cursor.".into()))?;
            if after >= 9_007_199_254_740_990 {
                return Err((
                    400,
                    "Audio cursor exceeds exact JSON integer bounds.".into(),
                ));
            }
            let mut value =
                (self.provider)(Some(json!({"audio_after":after}))).map_err(|e| (503, e))?;
            if !self.active() {
                return Err((401, "Pairing revoked.".into()));
            }
            value["protocol_version"] = json!(1);
            value["server_instance"] = json!(self.server_instance);
            value["workspace_id"] = json!(self.workspace_id);
            return Ok((200, value));
        }
        if request.path == "/xlog/v1/bearing" || request.path.starts_with("/xlog/v1/bearing?") {
            if request.method != "GET" || !request.body.is_empty() {
                return Err((400, "bearing_invalid_request".into()));
            }
            let encoded = request.path.strip_prefix("/xlog/v1/bearing?callsign=")
                .ok_or((400, "bearing_invalid_request".into()))?;
            let call = encoded.replace("%2F", "/").replace("%2f", "/");
            if crate::companion_bearing::normalized_call(&call).is_none() {
                return Err((400, "bearing_invalid_request".into()));
            }
            let began = std::time::Instant::now();
            let started = crate::companion_bearing::now_ms().map_err(|e| (503, e))?;
            let response = (self.provider)(Some(json!({"bearing_lookup":call,"workspace_id":self.workspace_id})));
            if !self.active() { return Err((401, "Pairing revoked.".into())); }
            let mut value = response.map_err(|e| (503, match e.as_str() {
                "bearing_busy" | "bearing_stale" | "bearing_workspace_changed" |
                "bearing_identity_changed" | "bearing_identity_unavailable" => e,
                _ => "bearing_unavailable".into(),
            }))?;
            crate::companion_bearing::validate_provider(&value, &call, &self.workspace_id, started,
                crate::companion_bearing::now_ms().map_err(|e| (503, e))?, began.elapsed()).map_err(|e| (503, e))?;
            value["protocol_version"] = json!(1);
            value["server_instance"] = json!(self.server_instance);
            value["workspace_id"] = json!(self.workspace_id);
            if serde_json::to_vec(&value).map_err(|_| (503, "bearing_invalid".into()))?.len() > crate::companion_bearing::MAX_ENVELOPE {
                return Err((503, "bearing_too_large".into()));
            }
            return Ok((if value["bearing"]["status"] == "unavailable" { 503 } else { 200 }, value));
        }
        if let Some(id) = request.path.strip_prefix("/xlog/v1/logbook/contacts/") {
            if request.method != "GET" || !request.body.is_empty() || !crate::companion_contact_detail::canonical_id(id) {
                return Err((400, "contact_detail_invalid_request".into()));
            }
            let began = std::time::Instant::now();
            let started = crate::companion_contact_detail::now_ms().map_err(|e| (503,e))?;
            let response = (self.provider)(Some(json!({"contact_detail_lookup":id,"workspace_id":self.workspace_id})));
            if !self.active() { return Err((401, "Pairing revoked.".into())); }
            let mut value = response.map_err(|e| (503, match e.as_str() {
                "contact_detail_busy" | "contact_detail_stale" | "contact_detail_workspace_changed" => e,
                _ => "contact_detail_unavailable".into(),
            }))?;
            crate::companion_contact_detail::validate_provider(&value, id, &self.workspace_id, started,
                crate::companion_contact_detail::now_ms().map_err(|e| (503,e))?, began.elapsed()).map_err(|e| (503,e))?;
            value["protocol_version"] = json!(1);
            value["server_instance"] = json!(self.server_instance);
            value["workspace_id"] = json!(self.workspace_id);
            if serde_json::to_vec(&value).map_err(|_| (503,"contact_detail_invalid".into()))?.len() > crate::companion_contact_detail::MAX_ENVELOPE {
                return Err((503,"contact_detail_too_large".into()));
            }
            return Ok((if value["detail"]["status"] == "unavailable" { 503 } else { 200 }, value));
        }
        if request.method == "GET" && (request.path.starts_with("/xlog/v1/history?callsign=") || request.path == "/xlog/v1/recent") {
            let mut query = if request.path == "/xlog/v1/recent" { json!({"recent_lookup":true}) }
                else { history_query(request.path.trim_start_matches("/xlog/v1/history?callsign=")).map_err(|e| (400,e))? };
            let reply_limit = if query["recent_lookup"] == true || query["band"].is_string() { 512 * 1024 } else { 256 * 1024 };
            query["workspace_id"] = json!(self.workspace_id);
            let mut history = (self.provider)(Some(query)).map_err(|e| (503,e))?;
            if !self.active() { return Err((401,"Pairing revoked.".into())); }
            if !history.is_object() { return Err((503,"History result is unavailable.".into())); }
            history["protocol_version"] = json!(1);
            history["server_instance"] = json!(self.server_instance);
            history["workspace_id"] = json!(self.workspace_id);
            if serde_json::to_vec(&history).map_err(|_| (503,"History serialization failed.".into()))?.len() > reply_limit {
                return Err((503,"History result exceeds its byte limit.".into()));
            }
            return Ok((200, history));
        }
        if request.method == "GET" && request.path.starts_with("/xlog/v1/receipt/") {
            let id = request.path.trim_start_matches("/xlog/v1/receipt/");
            if uuid::Uuid::parse_str(id).is_err() {
                return Err((400, "Invalid receipt identity.".into()));
            }
            let (retained, qso) = self
                .receipts
                .lock()
                .map_err(|_| (503, "Receipts unavailable.".into()))?
                .get(id)
                .map(|(request, value)| {
                    (
                        Some(value.clone()),
                        matches!(
                            request["command"]["kind"].as_str(),
                            Some("logbook.create" | "logbook.update")
                        ),
                    )
                })
                .unwrap_or((None, false));
            if let Some(value) = retained
                .as_ref()
                .filter(|value| value["status"] != "uncertain" || !qso)
            {
                return Ok((200, value.clone()));
            }
            let durable = (self.provider)(Some(
                json!({"receipt_lookup":id,"workspace_id":self.workspace_id}),
            ))
            .map_err(|e| (503, e))?;
            if durable.is_null() {
                // A missing durable receipt authorizes no effect by itself.
                // It only allows a later explicit identical-QSO retry; the
                // owner's transaction still resolves any concurrent old send.
                if qso {
                    if let Some((_, receipt)) = self
                        .receipts
                        .lock()
                        .map_err(|_| (503, "Receipts unavailable.".into()))?
                        .get_mut(id)
                    {
                        if receipt["status"] == "uncertain" {
                            receipt["_retry_missing"] = json!(true);
                        }
                    }
                }
                return Err((
                    404,
                    "No durable QSO receipt found; an identical QSO may be explicitly retried."
                        .into(),
                ));
            }
            let mut receipt = durable;
            receipt["request_id"] = json!(id);
            receipt["server_instance"] = json!(self.server_instance);
            receipt["workspace_id"] = json!(self.workspace_id);
            if let Some((_, stored)) = self
                .receipts
                .lock()
                .map_err(|_| (503, "Receipts unavailable.".into()))?
                .get_mut(id)
            {
                *stored = receipt.clone();
            }
            return Ok((200, receipt));
        }
        if request.method != "POST" || request.path != "/xlog/v1/command" {
            return Err((404, "Unknown endpoint.".into()));
        }
        let value: Value = serde_json::from_slice(&request.body)
            .map_err(|_| (400, "Invalid command JSON.".into()))?;
        if !self.controls_enabled
            && !matches!(
                value["command"]["kind"].as_str(),
                Some("logbook.create" | "logbook.update")
            )
        {
            return Err((
                403,
                "Station and rotor controls are disabled in disposable companion acceptance."
                    .into(),
            ));
        }
        let id = value["request_id"]
            .as_str()
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .ok_or((400, "UUID request_id required.".into()))?
            .to_owned();
        if value["server_instance"] != self.server_instance
            || value["workspace_id"] != self.workspace_id
        {
            return Err((409, "Server or workspace changed.".into()));
        }
        let pending = json!({"request_id":id,"server_instance":self.server_instance,"workspace_id":self.workspace_id,"status":"uncertain","message":"Command admitted; inspect owner state before another action."});
        {
            let mut receipts = self
                .receipts
                .lock()
                .map_err(|_| (503, "Receipts unavailable.".into()))?;
            if let Some((previous, receipt)) = receipts.get(&id) {
                if previous["workspace_id"] != value["workspace_id"]
                    || previous["command"] != value["command"]
                {
                    return Err((
                        409,
                        "Request identity reused for a different command.".into(),
                    ));
                }
                if receipt["_retry_missing"] != true
                    || !matches!(
                        value["command"]["kind"].as_str(),
                        Some("logbook.create" | "logbook.update")
                    )
                {
                    return Ok((200, receipt.clone()));
                }
            }
            // Never evict an identity and accidentally replay a command.
            if receipts.len() >= 4096 && !receipts.contains_key(&id) {
                if !stop {
                    return Err((503,"Session receipt limit reached; pair again after reviewing pending actions.".into()));
                }
                // Stop alone has a reserved bounded slot. Retiring an older
                // Stop receipt can only permit another Stop, never replay a
                // tune, turn or QSO write. Ordinary receipts stay immutable.
                if let Some(old) = receipts
                    .iter()
                    .find(|(_, (request, _))| request["command"]["kind"] == "rotor.stop")
                    .map(|(id, _)| id.clone())
                {
                    receipts.remove(&old);
                }
            }
            receipts.insert(id.clone(), (value.clone(), pending.clone()));
        }
        let mut receipt = pending;
        match (self.provider)(Some(value.clone())) {
            Ok(result) => {
                receipt["status"] = result["status"].clone();
                receipt["message"] = result["message"].clone();
                for key in ["contact_id", "contact_revision"] {
                    if let Some(value) = result.get(key) {
                        receipt[key] = value.clone();
                    }
                }
            }
            Err(message) => {
                receipt["status"] = json!("uncertain");
                receipt["message"] = json!(message);
            }
        }
        if let Some(stored) = self
            .receipts
            .lock()
            .map_err(|_| (503, "Receipts unavailable.".into()))?
            .get_mut(&id)
        {
            // An older emergency receipt can be retired while its owner is
            // finishing. Do not reinsert it beyond the reserved capacity.
            *stored = (value, receipt.clone());
        }
        Ok((200, receipt))
    }
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
struct Request {
    method: String,
    path: String,
    host: String,
    authorization: Option<String>,
    body: Vec<u8>,
}
struct DeadlineSocket<'a> {
    stream: &'a mut TcpStream,
    deadline: std::time::Instant,
}
impl Read for DeadlineSocket<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        // Per-read timeout shrinks with the overall request budget instead of
        // granting another two seconds for each trickled fragment.
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}
fn read_request(stream: &mut impl Read) -> Result<Request, (u16, String)> {
    read_request_with_budget(stream, Duration::from_secs(2))
}
fn read_request_with_budget(
    stream: &mut impl Read,
    budget: Duration,
) -> Result<Request, (u16, String)> {
    read_request_authenticated(stream, budget, None)
}
fn read_request_authenticated(
    stream: &mut impl Read,
    budget: Duration,
    authenticated: Option<(&str, &str)>,
) -> Result<Request, (u16, String)> {
    let bad = || (400, "Invalid bounded native request.".to_owned());
    let mut header = Vec::new();
    let deadline = std::time::Instant::now();
    // Read exactly the header; do not consume an unbounded body or pipelined request.
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 || deadline.elapsed() > budget {
            return Err((431, "Header limit exceeded.".into()));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(|_| bad())?;
        header.push(byte[0]);
    }
    if deadline.elapsed() > budget {
        return Err((408, "Request deadline exceeded.".into()));
    }
    let header = std::str::from_utf8(&header).map_err(|_| bad())?;
    let mut lines = header.split("\r\n");
    let parts: Vec<_> = lines.next().ok_or_else(bad)?.split(' ').collect();
    if parts.len() != 3
        || parts[2] != "HTTP/1.1"
        || !matches!(parts[0], "GET" | "POST")
        || !parts[1].starts_with("/xlog/v1/")
    {
        return Err(bad());
    }
    let mut fields = HashMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(bad)?;
        if key.trim() != key
            || key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(bad());
        }
        let key = key.to_ascii_lowercase();
        if matches!(key.as_str(), "origin" | "transfer-encoding" | "expect")
            || fields.insert(key, value.trim().to_owned()).is_some()
        {
            return Err(bad());
        }
    }
    // The new larger binary route authenticates BEFORE body allocation/read.
    // Old unit-parser callers retain the old 16 KiB ceiling, never TX admission.
    if let Some((authorization, host)) = authenticated {
        if fields.get("authorization").is_none_or(|v| !constant_eq(v.as_bytes(), authorization.as_bytes())) {
            return Err((401, "Pairing required.".into()));
        }
        if fields.get("host").map(String::as_str) != Some(host) { return Err(bad()); }
    }
    let tx_prepare = authenticated.is_some() && parts[0] == "POST" && parts[1] == "/xlog/v1/tx/prepare";
    if tx_prepare && fields.get("content-type").map(String::as_str) != Some("application/octet-stream") {
        return Err((415, "Prepared TX requires bounded binary content.".into()));
    }
    let length = fields
        .get("content-length")
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| bad())?
        .unwrap_or(0);
    if length > if tx_prepare { 4 * 1024 * 1024 } else { 16 * 1024 } {
        return Err((413, "Command too large.".into()));
    }
    if parts[0] == "GET" && length != 0 {
        return Err(bad());
    }
    let mut body = vec![0; length];
    let mut received = 0;
    while received < length {
        if deadline.elapsed() > budget {
            return Err((408, "Request deadline exceeded.".into()));
        }
        let count = stream.read(&mut body[received..]).map_err(|_| bad())?;
        if count == 0 {
            return Err(bad());
        }
        received += count;
    }
    // A final read may finish after the deadline; it must never be dispatched.
    if deadline.elapsed() > budget {
        return Err((408, "Request deadline exceeded.".into()));
    }
    Ok(Request {
        method: parts[0].into(),
        path: parts[1].into(),
        host: fields.remove("host").ok_or_else(bad)?,
        authorization: fields.remove("authorization"),
        body,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn digital_prepare_authentication_precedes_large_body_read_and_old_limits_stay_small() {
        struct HeaderOnly(std::io::Cursor<Vec<u8>>);
        impl std::io::Read for HeaderOnly {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                assert!((self.0.position() as usize) < self.0.get_ref().len(), "unauthorized/oversized body was read");
                std::io::Read::read(&mut self.0, out)
            }
        }
        let make = |path: &str, token: &str, size: usize| HeaderOnly(std::io::Cursor::new(format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:123\r\nAuthorization: {token}\r\nContent-Type: application/octet-stream\r\nContent-Length: {size}\r\n\r\n").into_bytes()));
        let auth=Some(("Bearer correct", "127.0.0.1:123"));
        assert_eq!(super::read_request_authenticated(&mut make("/xlog/v1/tx/prepare","Bearer wrong",2_880_000),std::time::Duration::from_secs(2),auth).err().unwrap().0,401);
        assert_eq!(super::read_request_authenticated(&mut make("/xlog/v1/command","Bearer correct",2_880_000),std::time::Duration::from_secs(2),auth).err().unwrap().0,413);
        assert_eq!(super::read_request_authenticated(&mut make("/xlog/v1/tx/prepare","Bearer correct",4*1024*1024+1),std::time::Duration::from_secs(2),auth).err().unwrap().0,413);
        let body=b"binary\0not-json";
        let mut bytes=format!("POST /xlog/v1/tx/prepare HTTP/1.1\r\nHost: 127.0.0.1:123\r\nAuthorization: Bearer correct\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",body.len()).into_bytes();
        bytes.extend_from_slice(body);
        let parsed=super::read_request_authenticated(&mut bytes.as_slice(),std::time::Duration::from_secs(2),auth).unwrap();
        assert_eq!(parsed.body,body);
    }
    const BEARING_WORKSPACE: &str = "00000000-0000-4000-8000-000000000001";
    fn bearing_request(bridge: &Bridge, query: &str) -> Request {
        Request { method:"GET".into(),path:format!("/xlog/v1/bearing?{query}"),
            host:bridge.endpoint.trim_start_matches("http://").into(),
            authorization:Some(format!("Bearer {}",bridge.token)),body:vec![] }
    }
    fn bearing_reply(bearing: Value) -> Value {
        json!({"generation":BEARING_WORKSPACE,"worker_generation":7,
            "generated_at_unix_ms":crate::companion_bearing::now_ms().unwrap(),"max_age_ms":5000,"bearing":bearing,
            "station_identity_binding":{"workspace_generation":BEARING_WORKSPACE,"worker_generation":7,
                "settings_revision":11,"identity_epoch":"00000000-0000-4000-8000-000000000002",
                "identity_revision":3,"operating_module":"normal_logging"}})
    }
    #[test]
    fn bearing_endpoint_returns_actual_swift_states_with_controls_disabled() {
        for name in ["historical_grid","cty_approximation","coincident","reference_unavailable"] {
            let fixture=crate::companion_bearing::fixtures()[name].clone();
            let call=fixture["requested_callsign"].as_str().unwrap().to_owned();
            let expected=call.clone();let available=fixture["status"]=="available";
            let bridge=Bridge::start_with_controls(BEARING_WORKSPACE.into(),Arc::new(move |query| {
                if let Some(query)=query {
                    assert_eq!(query,json!({"bearing_lookup":expected,"workspace_id":BEARING_WORKSPACE}));
                    Ok(bearing_reply(fixture.clone()))
                } else {Ok(json!({}))}
            }),false).unwrap();
            let (status,response)=bridge.route(bearing_request(&bridge,&format!("callsign={}",call.replace('/',"%2F")))).unwrap();
            assert_eq!(status,if available {200}else{503});
            assert_eq!(response.as_object().unwrap().len(),9);
            assert_eq!(response["protocol_version"],1);
            assert_eq!(response["workspace_id"],BEARING_WORKSPACE);
            assert_eq!(response["server_instance"],bridge.server_instance);
            let mut hello=bearing_request(&bridge,"");hello.path="/xlog/v1/hello".into();
            assert!(bridge.route(hello).unwrap().1["capabilities"].as_array().unwrap().contains(&json!("bearing.read")));
            bridge.revoke();
        }
    }
    #[test]
    fn bearing_endpoint_rejects_extra_coordinates_queries_body_and_auth_before_provider() {
        let calls=Arc::new(AtomicUsize::new(0));let count=calls.clone();
        let bridge=Bridge::start_with_controls(BEARING_WORKSPACE.into(),Arc::new(move |_| {
            count.fetch_add(1,Ordering::Relaxed);Err("must not dispatch".into())
        }),false).unwrap();
        for query in ["","callsign=","callsign=VK8RX&grid=PH57","callsign=VK8RX&callsign=W1AW",
            "callsign=VK8RX%252FP","callsign=VK8RX%20","callsign=VK8RX//P","grid=JN88&callsign=VK8RX"] {
            assert_eq!(bridge.route(bearing_request(&bridge,query)).unwrap_err().0,400);
        }
        let mut r=bearing_request(&bridge,"callsign=VK8RX");r.body=b"{}".to_vec();assert_eq!(bridge.route(r).unwrap_err().0,400);
        let mut r=bearing_request(&bridge,"callsign=VK8RX");r.method="POST".into();assert_eq!(bridge.route(r).unwrap_err().0,400);
        let mut r=bearing_request(&bridge,"callsign=VK8RX");r.authorization=None;assert_eq!(bridge.route(r).unwrap_err().0,401);
        let mut r=bearing_request(&bridge,"callsign=VK8RX");r.host="example.org".into();assert_eq!(bridge.route(r).unwrap_err().0,400);
        assert_eq!(calls.load(Ordering::Relaxed),0);bridge.revoke();
    }
    #[test]
    fn bearing_has_independent_admission_and_revocation_retires_pending_reply() {
        let bridge=Bridge::start_with_controls(BEARING_WORKSPACE.into(),Arc::new(|_|
            Ok(bearing_reply(crate::companion_bearing::fixtures()["historical_grid"].clone()))),false).unwrap();
        bridge.ordinary.store(8,Ordering::Release);bridge.contact_read.store(1,Ordering::Release);bridge.needs_read.store(1,Ordering::Release);
        assert_eq!(bridge.route(bearing_request(&bridge,"callsign=VK8RX/P")).unwrap().0,200);
        bridge.bearing_read.store(1,Ordering::Release);
        assert_eq!(bridge.route(bearing_request(&bridge,"callsign=VK8RX/P")).unwrap_err(),(503,"bearing_busy".into()));
        bridge.bearing_read.store(0,Ordering::Release);bridge.revoke();
        let (entered_tx,entered_rx)=std::sync::mpsc::channel();let (release_tx,release_rx)=std::sync::mpsc::channel();
        let release=Mutex::new(release_rx);
        let bridge=Bridge::start_with_controls(BEARING_WORKSPACE.into(),Arc::new(move |_| {
            entered_tx.send(()).unwrap();release.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(bearing_reply(crate::companion_bearing::fixtures()["historical_grid"].clone()))
        }),false).unwrap();
        let other=bridge.clone();let reader=std::thread::spawn(move ||other.route(bearing_request(&other,"callsign=VK8RX/P")));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();bridge.revoke();release_tx.send(()).unwrap();
        assert_eq!(reader.join().unwrap().unwrap_err().0,401);
    }
    #[test]
    fn bearing_drops_stale_or_foreign_results_and_redacts_provider_errors() {
        for variant in 0..7 {
            let bridge=Bridge::start_with_controls(BEARING_WORKSPACE.into(),Arc::new(move |_| {
                if variant==6 {return Err("private filesystem or credential error".into());}
                let mut v=bearing_reply(crate::companion_bearing::fixtures()["historical_grid"].clone());
                match variant {
                    0=>v["generation"]=json!("foreign"),
                    1=>v["worker_generation"]=json!(0),
                    2=>v["generated_at_unix_ms"]=json!(0),
                    3=>v["station_identity_binding"]["worker_generation"]=json!(8),
                    4=>v["bearing"]["geometry"]["short_path_deg"]=json!(360),
                    _=>v["bearing"]["requested_callsign"]=json!("VK8RX"),
                };Ok(v)
            }),false).unwrap();
            let error=bridge.route(bearing_request(&bridge,"callsign=VK8RX/P")).unwrap_err();
            assert_eq!(error.0,503);assert!(!error.1.contains("private"));bridge.revoke();
        }
    }
    #[test]
    fn bearing_actual_loopback_response_uses_the_fixed_envelope() {
        let bridge=Bridge::start_with_controls(BEARING_WORKSPACE.into(),Arc::new(|_|
            Ok(bearing_reply(crate::companion_bearing::fixtures()["historical_grid"].clone()))),false).unwrap();
        let mut socket=TcpStream::connect(bridge.endpoint.trim_start_matches("http://")).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(socket,"GET /xlog/v1/bearing?callsign=VK8RX%2FP HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\r\n",
            bridge.endpoint.trim_start_matches("http://"),bridge.token).unwrap();
        let mut response=String::new();socket.read_to_string(&mut response).unwrap();bridge.revoke();
        assert!(response.starts_with("HTTP/1.1 200 "));
        let value:Value=serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(value.as_object().unwrap().len(),9);
        assert_eq!(value["bearing"]["target"]["contact_started_at_unix_ms"],1_700_000_000_000_i64);
        assert_eq!(value["bearing"]["motion_authority"],false);
    }
    fn contact_request(bridge: &Bridge, id: &str) -> Request {
        Request { method:"GET".into(),path:format!("/xlog/v1/logbook/contacts/{id}"),
            host:bridge.endpoint.trim_start_matches("http://").into(),
            authorization:Some(format!("Bearer {}",bridge.token)),body:vec![] }
    }
    fn contact_reply(detail: Value) -> Value {
        json!({"generation":"workspace","worker_generation":1,"generated_at_unix_ms":crate::companion_contact_detail::now_ms().unwrap(),"max_age_ms":5000,"detail":detail})
    }
    #[test]
    fn contact_endpoint_returns_actual_typed_states_with_controls_disabled() {
        for state in ["found", "deleted", "not_found", "unavailable"] {
            let fixtures = crate::companion_contact_detail::fixtures();
            let mut detail = fixtures[if state == "unavailable" { "deleted" } else { state }].clone();
            if state == "unavailable" {
                detail["status"] = json!(state); detail["reason"] = json!("record_too_large");
                detail["editability"]["reason"] = json!("record_too_large");
            }
            let id = detail["contact_id"].as_str().unwrap().to_owned();
            let expected_id = id.clone();
            let bridge = Bridge::start_with_controls("workspace".into(),Arc::new(move |query| {
                assert_eq!(query.unwrap(), json!({"contact_detail_lookup":expected_id,"workspace_id":"workspace"}));
                Ok(contact_reply(detail.clone()))
            }),false).unwrap();
            let (status, response) = bridge.route(contact_request(&bridge,&id)).unwrap();
            assert_eq!(status, if state == "unavailable" {503} else {200});
            assert_eq!(response["detail"]["status"],state);
            assert_eq!(response["protocol_version"],1);
            assert_eq!(response["server_instance"],bridge.server_instance);
            assert_eq!(response["workspace_id"],"workspace");
            assert_eq!(response.as_object().unwrap().len(),8);
            bridge.revoke();
        }
    }
    #[test]
    fn contact_endpoint_rejects_noncanonical_paths_bodies_and_auth_before_provider() {
        let count = Arc::new(AtomicUsize::new(0)); let calls=count.clone();
        let bridge = Bridge::start_with_controls("workspace".into(),Arc::new(move |_| {
            calls.fetch_add(1,Ordering::Relaxed); Err("must not execute".into())
        }),false).unwrap();
        let id="abcdef12-3456-4abc-8def-123456abcdef";
        for path in [id.to_uppercase(),format!("{id}?x=1"),format!("{id}/"),id.replace('-',""),"../settings".into()] {
            assert_eq!(bridge.route(contact_request(&bridge,&path)).unwrap_err().0,400);
        }
        let mut r=contact_request(&bridge,id); r.method="POST".into(); assert_eq!(bridge.route(r).unwrap_err().0,400);
        let mut r=contact_request(&bridge,id); r.body=b"{}".to_vec(); assert_eq!(bridge.route(r).unwrap_err().0,400);
        let mut r=contact_request(&bridge,id); r.authorization=None; assert_eq!(bridge.route(r).unwrap_err().0,401);
        let mut r=contact_request(&bridge,id); r.host="example.org".into(); assert_eq!(bridge.route(r).unwrap_err().0,400);
        let wire=format!("GET /xlog/v1/logbook/contacts/{id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{{}}");
        assert!(read_request(&mut wire.as_bytes()).is_err());
        assert_eq!(count.load(Ordering::Relaxed),0);
        bridge.revoke();
    }
    #[test]
    fn contact_endpoint_has_separate_bounded_admission_and_checks_revocation_after_read() {
        let found=crate::companion_contact_detail::fixtures()["found"].clone();
        let bridge=Bridge::start_with_controls("workspace".into(),Arc::new(move |_| Ok(contact_reply(found.clone()))),false).unwrap();
        let id="abcdef12-3456-4abc-8def-123456abcdef";
        bridge.ordinary.store(8,Ordering::Release);
        assert_eq!(bridge.route(contact_request(&bridge,id)).unwrap().0,200);
        bridge.contact_read.store(1,Ordering::Release);
        assert_eq!(bridge.route(contact_request(&bridge,id)).unwrap_err(),(503,"contact_detail_busy".into()));
        bridge.contact_read.store(0,Ordering::Release); bridge.ordinary.store(0,Ordering::Release); bridge.revoke();
        let (started_tx,started_rx)=std::sync::mpsc::channel();
        let (release_tx,release_rx)=std::sync::mpsc::channel(); let release=Mutex::new(release_rx);
        let bridge=Bridge::start_with_controls("workspace".into(),Arc::new(move |_| {
            started_tx.send(()).unwrap(); release.lock().unwrap().recv().unwrap();
            Ok(contact_reply(crate::companion_contact_detail::fixtures()["found"].clone()))
        }),false).unwrap();
        let owner=bridge.clone();
        let task=std::thread::spawn(move || owner.route(contact_request(&owner,id)));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        bridge.revoke(); release_tx.send(()).unwrap();
        assert_eq!(task.join().unwrap().unwrap_err().0,401);
    }
    #[test]
    fn contact_endpoint_rejects_delayed_malformed_and_wrong_workspace_results() {
        for variant in 0..6 {
            let bridge=Bridge::start_with_controls("workspace".into(),Arc::new(move |_| {
                if variant==5 { return Err("private filesystem or service error".into()); }
                let mut v=contact_reply(crate::companion_contact_detail::fixtures()["found"].clone());
                match variant {
                    0=>v["generation"]=json!("other"),
                    1=>v["worker_generation"]=json!(0),
                    2=>v["generated_at_unix_ms"]=json!(0),
                    3=>v["detail"]["contact_id"]=json!("00000000-0000-4000-8000-000000000999"),
                    _=>v["detail"]["fields"]["contact"]["notes"]["value"]=json!("x".repeat(128*1024)),
                }; Ok(v)
            }),false).unwrap();
            let error=bridge.route(contact_request(&bridge,"abcdef12-3456-4abc-8def-123456abcdef")).unwrap_err();
            assert_eq!(error.0,503); assert!(!error.1.contains("private")); bridge.revoke();
        }
    }
    #[test]
    fn needs_pending_has_an_authenticated_identity_bound_envelope() {
        let bridge = Bridge::start_with_controls("fixture".into(), Arc::new(|_| Err("needs_pending".into())), false).unwrap();
        let request = |authorized: bool| Request {
            method: "POST".into(), path: "/xlog/v1/needs/query".into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: authorized.then(|| format!("Bearer {}", bridge.token)),
            body: serde_json::to_vec(&json!({"queries":[{"query_id":"q","callsign":"K1ABC","dial_frequency_hz":14074000,"mode":"FT8"}]})).unwrap(),
        };
        assert_eq!(bridge.route(request(false)).unwrap_err().0, 401);
        let (status, reply) = bridge.route(request(true)).unwrap();
        assert_eq!(status, 503);
        assert_eq!(reply, json!({"protocol_version":1,"server_instance":bridge.server_instance,
            "workspace_id":"fixture","error":"needs_pending"}));
        bridge.revoke();
        assert_eq!(bridge.route(request(true)).unwrap_err().0, 401);
    }
    #[test]
    fn ordinary_needs_failure_is_not_a_pending_envelope() {
        let bridge = Bridge::start_with_controls("fixture".into(), Arc::new(|_| Err("Needs lookup expired.".into())), false).unwrap();
        let request = Request { method:"POST".into(), path:"/xlog/v1/needs/query".into(),
            host:bridge.endpoint.trim_start_matches("http://").into(), authorization:Some(format!("Bearer {}",bridge.token)),
            body:serde_json::to_vec(&json!({"queries":[{"query_id":"q","callsign":"K1ABC","dial_frequency_hz":14074000,"mode":"FT8"}]})).unwrap() };
        assert_eq!(bridge.route(request).unwrap_err(), (503,"Needs lookup expired.".into()));
        bridge.revoke();
    }
    #[test]
    fn needs_route_is_read_only_bounded_and_independent_of_ordinary_slots() {
        use super::*;
        let bridge = Bridge::start_with_controls("fixture".into(),Arc::new(|command| {
            if let Some(command) = command {
                let batch = crate::companion_needs::parse(&serde_json::to_vec(&command["needs_query"]).unwrap())?;
                Ok(crate::companion_needs::unknown(&batch,"unavailable"))
            } else { Ok(json!({"spots":[]})) }
        }),false).unwrap();
        let request = |method: &str, path: &str, body: Value| Request {
            method:method.into(),path:path.into(),host:bridge.endpoint.trim_start_matches("http://").into(),
            authorization:Some(format!("Bearer {}",bridge.token)),body:serde_json::to_vec(&body).unwrap()
        };
        let body = json!({"queries":[{"query_id":"q","callsign":"k1abc/p","dial_frequency_hz":14074000,"mode":"FT8"}]});
        bridge.ordinary.store(8,Ordering::Release);
        let (_,reply) = bridge.route(request("POST","/xlog/v1/needs/query",body.clone())).unwrap();
        assert_eq!(reply["workspace_id"],"fixture");
        assert_eq!(reply["results"][0]["normalized_callsign"],"K1ABC/P");
        assert!(reply["results"][0]["need"]["score"].is_null());
        bridge.ordinary.store(0,Ordering::Release);
        bridge.needs_read.store(1,Ordering::Release);
        assert_eq!(bridge.route(request("POST","/xlog/v1/needs/query",body.clone())).unwrap_err().0,503);
        assert!(bridge.route(request("GET","/xlog/v1/snapshot",Value::Null)).is_ok());
        bridge.needs_read.store(0,Ordering::Release);
        assert_eq!(bridge.route(request("POST","/xlog/v1/needs/query",json!({"queries":[],"adif":291}))).unwrap_err().0,400);
        bridge.revoke();
        assert_eq!(bridge.route(request("POST","/xlog/v1/needs/query",body)).unwrap_err().0,401);
    }
    use super::*;
    #[test]
    fn slow_body_cannot_extend_header_deadline_or_dispatch_a_late_complete_body() {
        struct SlowBody {
            header: std::io::Cursor<Vec<u8>>,
            remaining: usize,
            chunk: usize,
            reads: usize,
        }
        impl Read for SlowBody {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if (self.header.position() as usize) < self.header.get_ref().len() {
                    return self.header.read(output);
                }
                self.reads += 1;
                std::thread::sleep(Duration::from_millis(30));
                let count = self.remaining.min(self.chunk).min(output.len());
                output[..count].fill(b'x');
                self.remaining -= count;
                Ok(count)
            }
        }
        for chunk in [1, 3] {
            let mut source = SlowBody { header: std::io::Cursor::new(b"POST /xlog/v1/command HTTP/1.1\r\nHost: 127.0.0.1:1\r\nContent-Length: 3\r\n\r\n".to_vec()), remaining: 3, chunk, reads: 0 };
            assert_eq!(
                read_request_with_budget(&mut source, Duration::from_millis(20))
                    .err()
                    .unwrap()
                    .0,
                408
            );
            assert_eq!(
                source.reads, 1,
                "body trickling must not reset the deadline"
            );
        }
    }
    #[test]
    fn disposable_pairing_requires_explicit_marker_and_never_dispatches_controls() {
        let marker =
            json!({"schema":1,"id":uuid::Uuid::new_v4().to_string(),"companionLoopback":true});
        assert!(companion_fixture_marker(marker.to_string().as_bytes()));
        for invalid in [
            json!({"schema":1,"companionLoopback":true}),
            json!({"schema":1,"id":uuid::Uuid::new_v4().to_string()}),
            json!({"schema":2,"id":uuid::Uuid::new_v4().to_string(),"companionLoopback":true}),
        ] {
            assert!(!companion_fixture_marker(invalid.to_string().as_bytes()));
        }
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let bridge = Bridge::start_with_controls(
            "fixture".into(),
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(json!({"status":"applied","message":"fixture"}))
            }),
            false,
        )
        .unwrap();
        let request = |method: &str, path: &str, body: Value| Request {
            method: method.into(),
            path: path.into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", bridge.token)),
            body: serde_json::to_vec(&body).unwrap(),
        };
        let hello = bridge
            .route(request("GET", "/xlog/v1/hello", Value::Null))
            .unwrap()
            .1;
        assert_eq!(
            hello["capabilities"],
            json!(["snapshot.read", "logbook.read", "logbook.write", "logbook.classify", "logbook.contact.read", "bearing.read"])
        );
        for kind in ["station.tune", "rotor.point", "rotor.stop", "unknown"] {
            let body = json!({"request_id":uuid::Uuid::new_v4().to_string(),"server_instance":bridge.server_instance,"workspace_id":"fixture","expected_revision":0,"command":{"kind":kind}});
            assert_eq!(
                bridge
                    .route(request("POST", "/xlog/v1/command", body))
                    .unwrap_err()
                    .0,
                403
            );
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let body = json!({"request_id":uuid::Uuid::new_v4().to_string(),"server_instance":bridge.server_instance,"workspace_id":"fixture","expected_revision":1,"command":{"kind":"logbook.create"}});
        assert!(bridge
            .route(request("POST", "/xlog/v1/command", body))
            .is_ok());
        assert_eq!(count.load(Ordering::Relaxed), 2);
        bridge.revoke();
    }
    #[test]
    fn parser_rejects_browser_smuggling_and_unbounded_bodies() {
        for headers in [
            "Origin: null\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Content-Length: 2\r\nContent-Length: 2\r\n",
            "Content-Length: 16385\r\n",
            " Host: evil\r\n",
        ] {
            let request =
                format!("POST /xlog/v1/command HTTP/1.1\r\nHost: 127.0.0.1:1\r\n{headers}\r\n");
            assert!(read_request(&mut request.as_bytes()).is_err());
        }
    }
    #[test]
    fn native_request_requires_exact_auth_and_host_and_revoke_is_immediate() {
        let bridge = Bridge::start("workspace".into(), Arc::new(|_| Ok(json!({})))).unwrap();
        let request = |auth: Option<String>| Request {
            method: "GET".into(),
            path: "/xlog/v1/hello".into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: auth,
            body: vec![],
        };
        assert_eq!(bridge.route(request(None)).unwrap_err().0, 401);
        assert!(bridge
            .route(request(Some(format!("Bearer {}", bridge.token))))
            .is_ok());
        bridge.revoke();
        assert_eq!(
            bridge
                .route(request(Some(format!("Bearer {}", bridge.token))))
                .unwrap_err()
                .0,
            401
        );
    }
    #[test]
    #[cfg(windows)]
    fn audio_reads_require_auth_profile_and_bounded_exact_cursor() {
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(|command| {
                assert_eq!(command.unwrap()["audio_after"], 42);
                Ok(json!({"state":"stopped","descriptor":null,"chunks":[]}))
            }),
        )
        .unwrap();
        let request = |path: &str, auth: bool| Request {
            method: "GET".into(),
            path: path.into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: auth.then(|| format!("Bearer {}", bridge.token)),
            body: vec![],
        };
        assert_eq!(
            bridge
                .route(request("/xlog/v1/audio?after=42", false))
                .unwrap_err()
                .0,
            401
        );
        for path in [
            "/xlog/v1/audio?after=-1",
            "/xlog/v1/audio?after=42&other=1",
            "/xlog/v1/audio?after=9007199254740991",
        ] {
            assert_eq!(bridge.route(request(path, true)).unwrap_err().0, 400);
        }
        let value = bridge
            .route(request("/xlog/v1/audio?after=42", true))
            .unwrap()
            .1;
        assert_eq!(value["workspace_id"], "workspace");
        assert_eq!(value["server_instance"], bridge.server_instance);
        bridge.revoke();
        assert_eq!(
            bridge
                .route(request("/xlog/v1/audio?after=42", true))
                .unwrap_err()
                .0,
            401
        );
        let offline = Bridge::start_with_controls(
            "workspace".into(),
            Arc::new(|_| panic!("Offline audio must not reach owner")),
            false,
        )
        .unwrap();
        let request = Request {
            method: "GET".into(),
            path: "/xlog/v1/audio?after=0".into(),
            host: offline.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", offline.token)),
            body: vec![],
        };
        assert_eq!(offline.route(request).unwrap_err().0, 404);
        offline.revoke();
    }
    #[test]
    fn identical_mutations_are_never_dispatched_twice() {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(json!({"status":"accepted","message":"ack"}))
            }),
        )
        .unwrap();
        let mut value = json!({"request_id":uuid::Uuid::new_v4().to_string(),"server_instance":bridge.server_instance,"workspace_id":"workspace","expected_revision":1,"command":{"kind":"rotor.stop","rotor_id":"rotor"}});
        let request = |value: &Value| Request {
            method: "POST".into(),
            path: "/xlog/v1/command".into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", bridge.token)),
            body: serde_json::to_vec(value).unwrap(),
        };
        assert!(bridge.route(request(&value)).is_ok());
        assert!(bridge.route(request(&value)).is_ok());
        assert_eq!(count.load(Ordering::Relaxed), 1);
        value["expected_revision"] = json!(2);
        assert!(bridge.route(request(&value)).is_ok());
        value["command"]["rotor_id"] = json!("different");
        assert_eq!(bridge.route(request(&value)).unwrap_err().0, 409);
        bridge.revoke();
    }
    #[test]
    fn real_loopback_native_request_and_stop_bypass_saturated_ordinary_budget() {
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(|_| Ok(json!({"status":"accepted","message":"synthetic"}))),
        )
        .unwrap();
        bridge.ordinary.store(8, Ordering::Release);
        let body=json!({"request_id":uuid::Uuid::new_v4().to_string(),"server_instance":bridge.server_instance,"workspace_id":"workspace","expected_revision":0,"command":{"kind":"rotor.stop","rotor_id":"native_dcu1"}}).to_string();
        let mut socket = TcpStream::connect(bridge.endpoint.trim_start_matches("http://")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(socket,"POST /xlog/v1/command HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{}",bridge.endpoint.trim_start_matches("http://"),bridge.token,body.len(),body).unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("accepted"));
        bridge.revoke();
    }
    #[test]
    fn uncertain_owner_failure_is_not_reported_as_a_safe_rejection() {
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(|_| Err("Lost owner response".into())),
        )
        .unwrap();
        let body = json!({"request_id":uuid::Uuid::new_v4().to_string(),"server_instance":bridge.server_instance,"workspace_id":"workspace","expected_revision":1,"command":{"kind":"rotor.stop","rotor_id":"native_dcu1"}});
        let request = Request {
            method: "POST".into(),
            path: "/xlog/v1/command".into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", bridge.token)),
            body: serde_json::to_vec(&body).unwrap(),
        };
        assert_eq!(bridge.route(request).unwrap().1["status"], "uncertain");
        bridge.revoke();
    }
    #[test]
    fn durable_receipt_is_rebound_to_current_paired_server_identity() {
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(|v| {
                assert!(v.unwrap()["receipt_lookup"].is_string());
                Ok(json!({"status":"applied","contact_id":"contact","contact_revision":3}))
            }),
        )
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let request = Request {
            method: "GET".into(),
            path: format!("/xlog/v1/receipt/{id}"),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", bridge.token)),
            body: vec![],
        };
        let value = bridge.route(request).unwrap().1;
        assert_eq!(value["server_instance"], bridge.server_instance);
        assert_eq!(value["workspace_id"], "workspace");
        assert_eq!(value["request_id"], id);
        assert_eq!(value["contact_revision"], 3);
        bridge.revoke();
    }
    #[test]
    fn history_context_is_bounded_and_canonical_without_query_injection() {
        assert_eq!(history_query("VK8RX&band=20M&mode=FT8").unwrap(),
            json!({"history_lookup":"VK8RX","band":"20m","mode":"FT8"}));
        assert_eq!(history_query("VK8RX&band=1.25m").unwrap()["band"], "1.25m");
        assert_eq!(history_query("VK8RX&mode=D-STAR&band=70cm").unwrap()["mode"], "D-STAR");
        for raw in ["VK8RX&mode=FT8", "VK8RX&band=20m&band=40m", "VK8RX&band=20m&mode=FT8&mode=FT4",
            "VK8RX&band=", "VK8RX&band=20..m", "VK8RX&band=12345m", "VK8RX&band=1.2345m",
            "VK8RX&band=20m&mode=ft8", "VK8RX&band=20m&mode=-CW", "VK8RX&other=x",
            "VK8RX%26band=20m", "VK8RX&band=20m#fragment", "VK8RX&band=20m&"] {
            assert!(history_query(raw).is_err(), "accepted {raw}");
        }
    }
    #[test]
    fn history_and_recent_forward_workspace_context_and_keep_existing_capability() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let bridge = Bridge::start_with_controls("workspace".into(), Arc::new(move |value| {
            if let Some(value) = value { captured.lock().unwrap().push(value); }
            Ok(json!({"contacts":[],"total":0,"complete":true,"max_age_ms":5000}))
        }), false).unwrap();
        let request = |path: &str| Request { method:"GET".into(), path:path.into(),
            host:bridge.endpoint.trim_start_matches("http://").into(),
            authorization:Some(format!("Bearer {}",bridge.token)), body:vec![] };
        let hello = bridge.route(request("/xlog/v1/hello")).unwrap().1;
        assert!(hello["capabilities"].as_array().unwrap().contains(&json!("logbook.read")));
        for path in ["/xlog/v1/history?callsign=VK8RX&band=20m&mode=FT8", "/xlog/v1/recent"] {
            let reply = bridge.route(request(path)).unwrap().1;
            assert_eq!(reply["workspace_id"],"workspace");
            assert_eq!(reply["server_instance"],bridge.server_instance);
            assert_eq!(reply["max_age_ms"],5000);
        }
        let values = seen.lock().unwrap();
        assert_eq!(values[0],json!({"history_lookup":"VK8RX","band":"20m","mode":"FT8","workspace_id":"workspace"}));
        assert_eq!(values[1],json!({"recent_lookup":true,"workspace_id":"workspace"}));
        assert_eq!(bridge.route(request("/xlog/v1/recent?limit=100")).unwrap_err().0,404);
        bridge.revoke();
    }
    #[test]
    fn read_reply_is_rejected_after_pairing_revocation_or_for_invalid_size() {
        let holder: Arc<Mutex<Option<std::sync::Weak<Bridge>>>> = Arc::new(Mutex::new(None));
        let captured = holder.clone();
        let bridge = Bridge::start("workspace".into(), Arc::new(move |_| {
            captured.lock().unwrap().as_ref().unwrap().upgrade().unwrap().revoke();
            Ok(json!({"contacts":[]}))
        })).unwrap();
        *holder.lock().unwrap() = Some(Arc::downgrade(&bridge));
        let request = |bridge: &Bridge| Request { method:"GET".into(), path:"/xlog/v1/recent".into(),
            host:bridge.endpoint.trim_start_matches("http://").into(),
            authorization:Some(format!("Bearer {}",bridge.token)), body:vec![] };
        assert_eq!(bridge.route(request(&bridge)).unwrap_err().0,401);
        for reply in [Value::Null, json!({"comment":"x".repeat(512 * 1024)})] {
            let bridge = Bridge::start("workspace".into(),Arc::new(move |_| Ok(reply.clone()))).unwrap();
            assert_eq!(bridge.route(request(&bridge)).unwrap_err().0,503);
            bridge.revoke();
        }
    }
    #[test]
    fn history_accepts_encoded_portable_callsign_and_rejects_query_injection() {
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(|v| {
                let v = v.unwrap();
                assert_eq!(v["history_lookup"], "W1AW/P");
                Ok(json!({"callsign":"W1AW/P","contacts":[],"matrix":[]}))
            }),
        )
        .unwrap();
        let request = |path: &str| Request {
            method: "GET".into(),
            path: path.into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", bridge.token)),
            body: vec![],
        };
        assert!(bridge
            .route(request("/xlog/v1/history?callsign=W1AW%2FP"))
            .is_ok());
        assert_eq!(
            bridge
                .route(request("/xlog/v1/history?callsign=W1AW&other=x"))
                .unwrap_err()
                .0,
            400
        );
        bridge.revoke();
    }
    #[test]
    fn receipt_capacity_never_blocks_emergency_stop_or_evicts_ordinary_commands() {
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(|_| Ok(json!({"status":"accepted"}))),
        )
        .unwrap();
        {
            let mut receipts = bridge.receipts.lock().unwrap();
            for n in 0..4096 {
                receipts.insert(
                    n.to_string(),
                    (
                        json!({"command":{"kind":"station.tune"}}),
                        json!({"status":"uncertain"}),
                    ),
                );
            }
        }
        for _ in 0..2 {
            let body = json!({"request_id":uuid::Uuid::new_v4().to_string(),"server_instance":bridge.server_instance,"workspace_id":"workspace","expected_revision":0,"command":{"kind":"rotor.stop","rotor_id":"native_dcu1"}});
            let request = Request {
                method: "POST".into(),
                path: "/xlog/v1/command".into(),
                host: bridge.endpoint.trim_start_matches("http://").into(),
                authorization: Some(format!("Bearer {}", bridge.token)),
                body: serde_json::to_vec(&body).unwrap(),
            };
            assert_eq!(bridge.route(request).unwrap().1["status"], "accepted");
        }
        let receipts = bridge.receipts.lock().unwrap();
        assert_eq!(receipts.len(), 4097);
        assert!(receipts.contains_key("0"));
        drop(receipts);
        bridge.revoke();
    }
    #[test]
    fn finishing_retired_stop_cannot_expand_the_reserved_receipt_budget() {
        let first = uuid::Uuid::new_v4().to_string();
        let blocked = first.clone();
        let (started, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let resume = Mutex::new(resume);
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(move |value| {
                if value.unwrap()["request_id"] == blocked {
                    started.send(()).unwrap();
                    resume.lock().unwrap().recv().unwrap();
                }
                Ok(json!({"status":"accepted"}))
            }),
        )
        .unwrap();
        {
            let mut receipts = bridge.receipts.lock().unwrap();
            for n in 0..4096 {
                receipts.insert(
                    n.to_string(),
                    (
                        json!({"command":{"kind":"station.tune"}}),
                        json!({"status":"uncertain"}),
                    ),
                );
            }
        }
        let request = |id: &str| {
            Request{method:"POST".into(),path:"/xlog/v1/command".into(),host:bridge.endpoint.trim_start_matches("http://").into(),authorization:Some(format!("Bearer {}",bridge.token)),body:serde_json::to_vec(&json!({"request_id":id,"server_instance":bridge.server_instance,"workspace_id":"workspace","expected_revision":0,"command":{"kind":"rotor.stop","rotor_id":"native_dcu1"}})).unwrap()}
        };
        let first_request = request(&first);
        let owner = bridge.clone();
        let pending = std::thread::spawn(move || owner.route(first_request));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(bridge
            .route(request(&uuid::Uuid::new_v4().to_string()))
            .is_ok());
        release.send(()).unwrap();
        assert!(pending.join().unwrap().is_ok());
        assert_eq!(bridge.receipts.lock().unwrap().len(), 4097);
        bridge.revoke();
    }
    #[test]
    fn only_identical_qso_can_be_explicitly_retried_after_missing_durable_receipt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let effects = calls.clone();
        let bridge = Bridge::start(
            "workspace".into(),
            Arc::new(move |value| {
                let v = value.unwrap();
                if v["receipt_lookup"].is_string() {
                    return Ok(Value::Null);
                }
                if effects.fetch_add(1, Ordering::AcqRel) == 0 {
                    Err("Lost before commit".into())
                } else {
                    Ok(json!({"status":"applied","contact_id":"contact","contact_revision":1}))
                }
            }),
        )
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut value = json!({"request_id":id,"server_instance":bridge.server_instance,"workspace_id":"workspace","expected_revision":1,"command":{"kind":"logbook.create","qso":{"callsign":"W1AW"}}});
        let request = |method: &str, path: &str, body: Vec<u8>| Request {
            method: method.into(),
            path: path.into(),
            host: bridge.endpoint.trim_start_matches("http://").into(),
            authorization: Some(format!("Bearer {}", bridge.token)),
            body,
        };
        for _ in 0..2 {
            assert_eq!(
                bridge
                    .route(request(
                        "POST",
                        "/xlog/v1/command",
                        serde_json::to_vec(&value).unwrap()
                    ))
                    .unwrap()
                    .1["status"],
                "uncertain"
            );
        }
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(
            bridge
                .route(request("GET", &format!("/xlog/v1/receipt/{id}"), vec![]))
                .unwrap_err()
                .0,
            404
        );
        let mut changed = value.clone();
        changed["command"]["qso"]["callsign"] = json!("K1ABC");
        assert_eq!(
            bridge
                .route(request(
                    "POST",
                    "/xlog/v1/command",
                    serde_json::to_vec(&changed).unwrap()
                ))
                .unwrap_err()
                .0,
            409
        );
        value["expected_revision"] = json!(2);
        assert_eq!(
            bridge
                .route(request(
                    "POST",
                    "/xlog/v1/command",
                    serde_json::to_vec(&value).unwrap()
                ))
                .unwrap()
                .1["status"],
            "applied"
        );
        assert_eq!(calls.load(Ordering::Acquire), 2);
        bridge.revoke();
    }
}
