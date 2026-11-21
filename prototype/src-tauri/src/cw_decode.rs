//! Decoded CW text from OM0RX-Skimmer for the Active QSO and contest entry.
//!
//! Receive-only. Two sources share one record format (Skimmer decode-stream
//! protocol 1): with the Remote Station Agent provider the Swift worker
//! fetches `/api/v1/cw_text?after=<seq>` through the authenticated Agent
//! client; with any other provider xLog connects straight to Skimmer's
//! loopback stream (default 127.0.0.1:7310, `XLOG_SKIMMER_TEXT` overrides).
//! The feed runs only while a window keeps asking for it.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const DIRECT_DEFAULT: &str = "127.0.0.1:7310";
/// A signal this close to the slice frequency is the one being copied.
const MATCH_HZ: f64 = 150.0;
/// Keep following the selected signal while it stays this close.
const STICKY_HZ: f64 = 300.0;
const DEMAND_TIMEOUT: Duration = Duration::from_secs(4);
const AGENT_POLL: Duration = Duration::from_millis(300);
const RETRY: Duration = Duration::from_secs(2);
const MAX_LINE: u64 = 4 * 1024 * 1024;
const MAX_SIGNALS: usize = 4000;

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Agent { radio_id: String },
    Direct { address: String },
}

impl Source {
    fn label(&self) -> &'static str {
        match self { Source::Agent { .. } => "agent", Source::Direct { .. } => "skimmer" }
    }
}

#[derive(Default, Debug)]
pub struct Feed {
    signals: HashMap<i64, Value>,
    seq: Option<i64>,
    connected: bool,
    error: Option<String>,
    coverage: Value,
    version: Option<String>,
    selected: Option<i64>,
}

impl Feed {
    fn clear(&mut self) {
        self.signals.clear();
        self.seq = None;
        self.selected = None;
    }

    /// Applies a Skimmer `signals`/`hello`/`heartbeat` message or an Agent `cw_text` update.
    pub fn apply(&mut self, message: &Value) {
        match message["type"].as_str() {
            Some("hello") => {
                self.version = message["version"].as_str().map(str::to_owned);
                if message["coverage"].is_object() { self.coverage = message["coverage"].clone(); }
            }
            Some("heartbeat") => {
                if message["coverage"].is_object() { self.coverage = message["coverage"].clone(); }
            }
            Some("signals") | Some("cw_text") => {
                if message["full"] == true { self.signals.clear(); self.selected = None; }
                if let Some(source) = message["source"].as_object() {
                    self.connected = source.get("connected") == Some(&Value::Bool(true));
                    self.error = source.get("error").and_then(Value::as_str).map(str::to_owned);
                    self.version = source.get("version").and_then(Value::as_str).map(str::to_owned);
                    self.coverage = source.get("coverage").cloned().unwrap_or(Value::Null);
                }
                for record in message["signals"].as_array().into_iter().flatten() {
                    let (Some(id), Some(hz)) = (record["id"].as_i64(), record["hz"].as_f64()) else { continue };
                    if !(hz > 0.0 && hz < 1e10) { continue; }
                    self.signals.insert(id, record.clone());
                }
                for id in message["gone"].as_array().into_iter().flatten().filter_map(Value::as_i64) {
                    self.signals.remove(&id);
                }
                if self.signals.len() > MAX_SIGNALS {
                    let mut ids: Vec<(i64, i64)> = self.signals.iter()
                        .map(|(id, r)| (r["last_heard"].as_i64().unwrap_or(0), *id)).collect();
                    ids.sort();
                    for (_, id) in ids.iter().take(self.signals.len() - MAX_SIGNALS) { self.signals.remove(id); }
                }
                if let Some(seq) = message["seq"].as_i64() { self.seq = Some(seq); }
            }
            _ => {}
        }
    }

    /// The signal being copied at `frequency_hz`: the previous one while it stays
    /// close, otherwise the nearest within the match window.
    pub fn select(&mut self, frequency_hz: f64) -> Option<Value> {
        let distance = |record: &Value| (record["hz"].as_f64().unwrap_or(f64::MAX) - frequency_hz).abs();
        if let Some(record) = self.selected.and_then(|id| self.signals.get(&id)) {
            if distance(record) <= STICKY_HZ { return Some(record.clone()); }
        }
        let best = self.signals.iter()
            .filter(|(_, record)| distance(record) <= MATCH_HZ)
            .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
            .map(|(id, record)| (*id, record.clone()));
        self.selected = best.as_ref().map(|(id, _)| *id);
        best.map(|(_, record)| record)
    }
}

struct Shared {
    feed: Mutex<(Option<Source>, Feed)>,
    demand: Mutex<Option<(Instant, Source)>>,
    running: Mutex<bool>,
}

fn shared() -> &'static Arc<Shared> {
    static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
    SHARED.get_or_init(|| Arc::new(Shared {
        feed: Mutex::new((None, Feed::default())),
        demand: Mutex::new(None),
        running: Mutex::new(false),
    }))
}

fn direct_address() -> String {
    std::env::var("XLOG_SKIMMER_TEXT").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| DIRECT_DEFAULT.into())
}

/// Chooses the source from the radio slot's existing telemetry projection.
fn source_for(radio_id: &str) -> Source {
    let provider = crate::station_telemetry::retained_snapshots().into_iter()
        .find(|projection| projection["radio_id"] == radio_id)
        .and_then(|projection| projection["provider"].as_str().map(str::to_owned));
    if provider.as_deref() == Some("direct_agent") {
        Source::Agent { radio_id: radio_id.into() }
    } else {
        Source::Direct { address: direct_address() }
    }
}

fn wanted(shared: &Shared) -> Option<Source> {
    let demand = shared.demand.lock().ok()?;
    demand.as_ref().filter(|(at, _)| at.elapsed() < DEMAND_TIMEOUT).map(|(_, source)| source.clone())
}

/// Resets the feed when the source changes.
fn begin(shared: &Shared, source: &Source) {
    if let Ok(mut guard) = shared.feed.lock() {
        if guard.0.as_ref() != Some(source) {
            guard.1 = Feed::default();
            guard.0 = Some(source.clone());
        }
    }
}

fn update(shared: &Shared, change: impl FnOnce(&mut Feed)) {
    if let Ok(mut guard) = shared.feed.lock() { change(&mut guard.1); }
}

fn fail(shared: &Shared, message: String) {
    update(shared, |feed| { feed.clear(); feed.connected = false; feed.error = Some(message); });
}

fn run(shared: Arc<Shared>) {
    loop {
        let Some(source) = wanted(&shared) else { break };
        begin(&shared, &source);
        match &source {
            Source::Agent { radio_id } => poll_agent(&shared, &source, radio_id),
            Source::Direct { address } => read_direct(&shared, &source, address),
        }
    }
    if let Ok(mut guard) = shared.feed.lock() { *guard = (None, Feed::default()); }
    if let Ok(mut running) = shared.running.lock() { *running = false; }
}

fn poll_agent(shared: &Shared, source: &Source, radio_id: &str) {
    while wanted(shared).as_ref() == Some(source) {
        let after = shared.feed.lock().ok().and_then(|guard| guard.1.seq);
        let mut params = json!({"provider":"direct_agent","radio_id":radio_id});
        if let Some(seq) = after { params["after"] = json!(seq); }
        let request = json!({"id":format!("cw-text-{}", uuid::Uuid::new_v4()),"method":"station_cw_text","params":params});
        let id = request["id"].clone();
        match crate::worker::request(request) {
            Ok(reply) if reply["ok"] == true && reply["id"] == id && reply["result"]["cw_text"].is_object() => {
                update(shared, |feed| feed.apply(&reply["result"]["cw_text"]));
                std::thread::sleep(AGENT_POLL);
            }
            Ok(reply) => {
                let message = reply["error"]["message"].as_str().unwrap_or("Decoded CW text is unavailable from the Remote Station Agent.");
                fail(shared, message.into());
                std::thread::sleep(RETRY);
            }
            Err(error) => { fail(shared, error); std::thread::sleep(RETRY); }
        }
    }
}

fn read_direct(shared: &Shared, source: &Source, address: &str) {
    let connected = address.to_socket_addrs().ok().and_then(|mut addresses| addresses.next())
        .ok_or_else(|| format!("Invalid OM0RX-Skimmer address {address}."))
        .and_then(|target| TcpStream::connect_timeout(&target, Duration::from_secs(2))
            .map_err(|_| format!("OM0RX-Skimmer is not sending decoded text on {address}.")));
    let stream = match connected {
        Ok(stream) => stream,
        Err(message) => { fail(shared, message); std::thread::sleep(RETRY); return; }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    update(shared, |feed| { feed.clear(); feed.connected = true; feed.error = None; });
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut last_message = Instant::now();
    while wanted(shared).as_ref() == Some(source) {
        match (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line) {
            Ok(0) => { fail(shared, "OM0RX-Skimmer closed the decoded text stream.".into()); std::thread::sleep(RETRY); return; }
            Ok(_) if line.ends_with(b"\n") => {
                last_message = Instant::now();
                if let Ok(message) = serde_json::from_slice::<Value>(&line) {
                    update(shared, |feed| { feed.connected = true; feed.apply(&message); });
                }
                line.clear();
            }
            Ok(_) => { fail(shared, "OM0RX-Skimmer sent an oversized line.".into()); return; }
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                // Skimmer sends a heartbeat every 5 s.
                if last_message.elapsed() > Duration::from_secs(15) {
                    fail(shared, "OM0RX-Skimmer decoded text stopped.".into());
                    return;
                }
            }
            Err(_) => { fail(shared, "OM0RX-Skimmer decoded text disconnected.".into()); std::thread::sleep(RETRY); return; }
        }
    }
}

pub fn validate(isolated: bool) -> Result<(), String> {
    if isolated { return Err("Decoded CW text is unavailable in isolated acceptance.".into()); }
    Ok(())
}

/// Current copy at the radio frequency. Each call keeps the feed alive for a few seconds.
pub fn snapshot(radio_id: &str, frequency_hz: f64) -> Result<Value, String> {
    crate::radio_slots::index(radio_id)?;
    if !(frequency_hz.is_finite() && frequency_hz > 0.0 && frequency_hz < 1e10) {
        return Err("Invalid radio frequency.".into());
    }
    let source = source_for(radio_id);
    let shared = shared().clone();
    if let Ok(mut demand) = shared.demand.lock() { *demand = Some((Instant::now(), source.clone())); }
    {
        let mut running = shared.running.lock().map_err(|_| "Decoded CW text is unavailable.")?;
        if !*running {
            *running = true;
            let worker = shared.clone();
            std::thread::Builder::new().name("cw-decode".into()).spawn(move || run(worker))
                .map_err(|_| { *running = false; "Decoded CW text is unavailable." })?;
        }
    }
    let mut guard = shared.feed.lock().map_err(|_| "Decoded CW text is unavailable.")?;
    let active = guard.0.as_ref() == Some(&source);
    let feed = &mut guard.1;
    let signal = if active { feed.select(frequency_hz) } else { None };
    let in_coverage = match (feed.coverage["lower_hz"].as_f64(), feed.coverage["upper_hz"].as_f64()) {
        (Some(lower), Some(upper)) => Value::Bool(frequency_hz >= lower && frequency_hz <= upper),
        _ => Value::Null,
    };
    Ok(json!({
        "source": source.label(),
        "status": if !active { "connecting" } else if feed.connected { "connected" } else { "unavailable" },
        "message": feed.error,
        "version": feed.version,
        "coverage": feed.coverage,
        "in_coverage": in_coverage,
        "signal": signal,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(id: i64, hz: f64, text: &str) -> Value {
        json!({"id":id,"hz":hz,"text":text,"pending":"","wpm":24.0,"snr":10.0,"call":null,"cq":false,"last_heard":id})
    }

    #[test]
    fn skimmer_and_agent_messages_share_one_feed() {
        let mut feed = Feed::default();
        feed.apply(&json!({"type":"hello","version":"0.3.2","coverage":{"lower_hz":14000000.0,"upper_hz":14100000.0,"state":"receiving"}}));
        feed.apply(&json!({"type":"signals","full":true,"signals":[signal(1,14025000.0,"CQ"),signal(2,14030000.0,"TU")],"gone":[]}));
        assert_eq!(feed.signals.len(), 2);
        feed.apply(&json!({"type":"signals","full":false,"signals":[signal(1,14025002.0,"CQ DE")],"gone":[2]}));
        assert_eq!(feed.signals.len(), 1);
        assert_eq!(feed.signals[&1]["text"], "CQ DE");

        let mut agent = Feed::default();
        agent.apply(&json!({"type":"cw_text","seq":9,"full":true,"source":{"connected":true,"version":"0.3.2","coverage":null,"error":null},
            "signals":[signal(5,7025000.0,"TEST")],"gone":[]}));
        assert!(agent.connected);
        assert_eq!(agent.seq, Some(9));
        agent.apply(&json!({"type":"cw_text","seq":10,"full":false,"source":{"connected":false,"error":"down"},"signals":[],"gone":[5]}));
        assert!(agent.signals.is_empty());
        assert_eq!(agent.error.as_deref(), Some("down"));
    }

    #[test]
    fn selection_takes_nearest_within_window_and_stays_with_it() {
        let mut feed = Feed::default();
        feed.apply(&json!({"type":"signals","full":true,"signals":[signal(1,14025000.0,"A"),signal(2,14025120.0,"B"),signal(3,14026000.0,"C")],"gone":[]}));
        assert_eq!(feed.select(14025100.0).unwrap()["id"], 2);
        // A small retune keeps the same station even though another is nearer.
        assert_eq!(feed.select(14025000.0).unwrap()["id"], 2);
        assert_eq!(feed.select(14025600.0), None);
        assert_eq!(feed.select(14025010.0).unwrap()["id"], 1);
    }

    #[test]
    fn malformed_records_are_ignored() {
        let mut feed = Feed::default();
        feed.apply(&json!({"type":"signals","full":false,"signals":[{"id":"x","hz":1.0},{"id":2,"hz":-1.0},signal(3,7000000.0,"OK")],"gone":["y"]}));
        assert_eq!(feed.signals.keys().copied().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn direct_source_reads_a_skimmer_stream() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            writeln!(socket, "{}", json!({"type":"hello","protocol":1,"version":"0.3.2","coverage":{"lower_hz":14000000.0,"upper_hz":14100000.0,"state":"receiving"}})).unwrap();
            writeln!(socket, "{}", json!({"type":"signals","full":true,"signals":[signal(4,14027150.0,"CQ CQ DE OM0RX")],"gone":[]})).unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        let shared = Arc::new(Shared { feed: Mutex::new((None, Feed::default())), demand: Mutex::new(None), running: Mutex::new(true) });
        let source = Source::Direct { address: address.clone() };
        *shared.demand.lock().unwrap() = Some((Instant::now(), source.clone()));
        let worker = shared.clone();
        let handle = std::thread::spawn(move || run(worker));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut text = None;
        while Instant::now() < deadline && text.is_none() {
            std::thread::sleep(Duration::from_millis(20));
            text = shared.feed.lock().unwrap().1.select(14027150.0).map(|record| record["text"].clone());
        }
        assert_eq!(text, Some(json!("CQ CQ DE OM0RX")));
        *shared.demand.lock().unwrap() = None;
        handle.join().unwrap();
        assert!(!*shared.running.lock().unwrap());
    }
}
