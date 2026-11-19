//! One optional read job per worker lifetime. This module cannot write to a
//! worker, start a thread, restart a session, or control station equipment.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::mpsc, time::{Duration, Instant}};

pub const MAX_BYTES: usize = 16 * 1024;
pub const MAX_REPLY: usize = 64 * 1024;
pub const POLICY: &str = "xlog-call-dxcc-needs-v1";
pub const MAX_REVISION: u64 = 9_007_199_254_740_990;
pub const BUDGET: Duration = Duration::from_secs(1);
pub const QUEUE_BUDGET: Duration = Duration::from_secs(3);
/// An authenticated pending reply is distinct from unavailable/stale authority.
pub const PENDING: &str = "needs_pending";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub query_id: String,
    pub callsign: String,
    pub dial_frequency_hz: u64,
    pub mode: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Batch { pub queries: Vec<Query> }

fn printable(value: &Value, max: usize) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= max
        && s.bytes().all(|b| (32..=126).contains(&b)))
}
fn text(value: &Value, max: usize) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
pub fn valid_call(call: &str) -> bool {
    if !(3..=32).contains(&call.len()) || !call.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'/') { return false; }
    let parts: Vec<_> = call.split('/').collect();
    if parts.iter().any(|s| s.is_empty()) { return false; }
    // Native message parsing establishes sender context. This syntax check also
    // allows digit-ending event calls (GB50, 9A2024), while excluding protocol
    // reports, unresolved hashes and free text. Never strip portable suffixes.
    if matches!(call,"RR73"|"RRR"|"R73"|"73") || (call.len() <= 3 && call.as_bytes()[0].is_ascii_digit()
        && call.bytes().all(|b| b.is_ascii_digit() || b == b'N')) { return false; }
    call.bytes().any(|b| b.is_ascii_uppercase()) && call.bytes().any(|b| b.is_ascii_digit())
}
pub fn parse(bytes: &[u8]) -> Result<Batch, String> {
    if bytes.len() > MAX_BYTES { return Err("Needs query exceeds its byte limit.".into()); }
    let mut batch: Batch = serde_json::from_slice(bytes).map_err(|_| "Invalid needs query.")?;
    if batch.queries.is_empty() || batch.queries.len() > 32 { return Err("Needs query requires 1–32 entries.".into()); }
    let mut ids = HashSet::new();
    for q in &mut batch.queries {
        if q.callsign.chars().any(char::is_control) { return Err("Invalid needs callsign.".into()); }
        q.callsign = q.callsign.trim().to_ascii_uppercase();
        if q.query_id.is_empty() || q.query_id.len() > 128
            || !q.query_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !ids.insert(q.query_id.clone()) || !valid_call(&q.callsign)
            || q.dial_frequency_hz == 0 || q.dial_frequency_hz > 1_000_000_000_000
            || !matches!(q.mode.as_str(), "FT8" | "FT4") {
            return Err("Invalid needs query entry.".into());
        }
    }
    Ok(batch)
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}
pub fn authority_valid(a: &Value) -> bool {
    a["logbook_revision"].as_u64().is_some_and(|n| n <= MAX_REVISION)
        && a["classification_revision"].as_u64().is_some_and(|n| n <= MAX_REVISION)
        && printable(&a["country_reference_revision"], 512)
        && a["policy_revision"] == POLICY
}
pub fn unknown(batch: &Batch, readiness: &str) -> Value {
    json!({"generated_at_unix_ms":now_ms(),"max_age_ms":2000,"readiness":readiness,"authority":null,
        "results":batch.queries.iter().map(|q| json!({"query_id":q.query_id,"normalized_callsign":q.callsign,
            "dial_frequency_hz":q.dial_frequency_hz,"decoder_mode":q.mode,"classification_mode":"DIGI",
            "band":null,"country":null,"adif":null,"country_resolution":null,"call_status":"Unknown","dxcc_status":"Unknown",
            "need":{"status":"unknown","score":null,"reason_code":readiness,"reasons":[]}})).collect::<Vec<_>>()})
}

struct Job {
    id: String,
    workspace: String,
    batch: Batch,
    deadline: Instant,
    expires_ms: u64,
    poll: Option<String>,
    sender: mpsc::Sender<Result<Value, String>>,
}
#[derive(Default)]
pub struct Mailbox { job: Option<Job> }
impl Mailbox {
    pub fn enqueue(&mut self, batch: Batch, workspace: String) -> Result<mpsc::Receiver<Result<Value, String>>, String> {
        self.expire_queued();
        if self.job.is_some() { return Err("Needs lookup is busy.".into()); }
        let (sender, receiver) = mpsc::channel();
        self.job = Some(Job { id: uuid::Uuid::new_v4().to_string(), workspace, batch,
            deadline: Instant::now() + QUEUE_BUDGET, expires_ms: 0, poll: None, sender });
        Ok(receiver)
    }
    fn expire_queued(&mut self) {
        if self.job.as_ref().is_some_and(|j| j.poll.is_none() && Instant::now() >= j.deadline) {
            if let Some(job) = self.job.take() { let _ = job.sender.send(Err("Needs lookup expired before its scheduled poll.".into())); }
        }
    }
    /// Called only after the owner's dedicated needs poll has registered its
    /// routing slot. HTTP timeouts never clear an attached job. The large spot
    /// snapshot poll never carries a lookup, so a lookup cannot delay it.
    pub fn attach(&mut self, request: &mut Value) {
        self.expire_queued();
        if request["method"] != "cluster_snapshot" || request["params"] != json!({"needs_only":true}) { return; }
        let Some(job) = self.job.as_mut().filter(|j| j.poll.is_none()) else { return; };
        let Some(id) = request["id"].as_str() else { return; };
        // Queue residence is not worker execution. Start the existing strict
        // one-second Swift deadline only when the ordinary poll carries it.
        job.deadline = Instant::now() + BUDGET;
        job.expires_ms = now_ms().saturating_add(1000);
        job.poll = Some(id.to_owned());
        request["params"]["needs_query"] = json!({"batch_id":job.id,"workspace_id":job.workspace,
            "expires_at_unix_ms":job.expires_ms,"queries":job.batch.queries});
    }
    /// Invoked by stdout routing even for a late abandoned poll response.
    /// Strip private data before renderer/event projection. A malformed optional
    /// lookup result fails only this lookup, never the ordinary worker session.
    pub fn complete(&mut self, response: &mut Value) {
        let optional = response["result"].as_object_mut().and_then(|r| r.remove("needs_reply"));
        if !self.job.as_ref().is_some_and(|j| j.poll.as_deref() == response["id"].as_str()) { return; }
        let job = self.job.take().unwrap();
        let outcome = if Instant::now() >= job.deadline {
            Err("Needs lookup expired.".into())
        } else {
            optional.ok_or_else(|| "Needs result unavailable.".to_owned()).and_then(|r| {
                if r["batch_id"] != job.id || r["workspace_id"] != job.workspace { return Err("Needs result identity changed.".into()); }
                let result = r["result"].clone();
                validate_reply(&result, &job.batch)?;
                Ok(result)
            })
        };
        let _ = job.sender.send(outcome);
    }
    /// Only a proven reaped worker can release an attached unanswered job.
    pub fn retired(&mut self) {
        if let Some(job) = self.job.take() { let _ = job.sender.send(Err("Needs worker retired.".into())); }
    }
}

/// One bounded asynchronous lookup per paired consumer. It neither waits nor
/// creates a poller. Completed evidence retains its original timestamp/TTL.
#[derive(Default)]
pub struct QueryCache { pending: Option<Pending> }
struct Pending {
    context: Value,
    batch: Batch,
    receiver: mpsc::Receiver<Result<Value, String>>,
    result: Option<Value>,
}
impl QueryCache {
    pub fn clear(&mut self) { self.pending = None; }
    pub fn query(&mut self, batch: &Batch, context: Value,
        enqueue: impl FnOnce(Batch) -> Result<mpsc::Receiver<Result<Value,String>>,String>) -> Result<Value,String> {
        if self.pending.as_ref().is_some_and(|p| p.context != context) {
            // Dropping a receiver never releases an attached mailbox routing slot.
            self.pending = None;
        }
        if let Some(pending) = self.pending.as_mut() {
            if pending.result.is_none() {
                match pending.receiver.try_recv() {
                    Ok(Ok(result)) => pending.result = Some(result),
                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => self.pending = None,
                    Err(mpsc::TryRecvError::Empty) => return Err(PENDING.into()),
                }
            }
        }
        if let Some(pending) = self.pending.as_ref() {
            if let Some(result) = pending.result.as_ref().filter(|r| validate_reply(r, &pending.batch).is_ok()) {
                if let Some(rows) = batch.queries.iter().map(|q| {
                    let source = pending.batch.queries.iter().find(|old| old.callsign == q.callsign
                        && old.dial_frequency_hz == q.dial_frequency_hz && old.mode == q.mode)?;
                    let mut row = result["results"].as_array()?.iter().find(|r| r["query_id"] == source.query_id)?.clone();
                    row["query_id"] = json!(q.query_id);
                    Some(row)
                }).collect::<Option<Vec<Value>>>() {
                    let mut answer = result.clone();
                    answer["results"] = json!(rows);
                    validate_reply(&answer, batch)?;
                    return Ok(answer);
                }
            }
            self.pending = None;
        }
        let receiver = enqueue(batch.clone())?;
        self.pending = Some(Pending { context, batch: batch.clone(), receiver, result: None });
        Err(PENDING.into())
    }
}

pub fn validate_reply(value: &Value, batch: &Batch) -> Result<(), String> {
    let bad = || "Invalid needs result.".to_owned();
    let has = |v: &Value, fields: &[&str]| v.as_object().is_some_and(|o| fields.iter().all(|f| o.contains_key(*f)));
    if !has(value,&["generated_at_unix_ms","max_age_ms","readiness","authority","results"]) { return Err(bad()); }
    if serde_json::to_vec(value).map_err(|_| bad())?.len() > MAX_REPLY { return Err(bad()); }
    let ready = value["readiness"].as_str().ok_or_else(bad)?;
    if !matches!(ready,"ready"|"unavailable"|"refreshing"|"uncertain"|"stale") { return Err(bad()); }
    if ready == "ready" { if !authority_valid(&value["authority"]) { return Err(bad()); } }
    else if !value["authority"].is_null() { return Err(bad()); }
    let generated = value["generated_at_unix_ms"].as_u64().ok_or_else(bad)?;
    let age = value["max_age_ms"].as_u64().filter(|n| (1..=2000).contains(n)).ok_or_else(bad)?;
    let now = now_ms();
    if generated > now.saturating_add(50) || now.saturating_sub(generated) > age { return Err(bad()); }
    let rows = value["results"].as_array().filter(|r| r.len() == batch.queries.len()).ok_or_else(bad)?;
    let mut ids = HashSet::new();
    for row in rows {
        if !has(row,&["query_id","normalized_callsign","dial_frequency_hz","decoder_mode","classification_mode",
            "band","country","adif","country_resolution","call_status","dxcc_status","need"])
            || !has(&row["need"],&["status","score","reason_code","reasons"]) { return Err(bad()); }
        let id = row["query_id"].as_str().ok_or_else(bad)?;
        if !ids.insert(id) { return Err(bad()); }
        let q = batch.queries.iter().find(|q| q.query_id == id).ok_or_else(bad)?;
        if row["normalized_callsign"] != q.callsign || row["dial_frequency_hz"] != q.dial_frequency_hz
            || row["decoder_mode"] != q.mode || row["classification_mode"] != "DIGI" { return Err(bad()); }
        for (key, max) in [("band",32),("country",128)] {
            if !row[key].is_null() && !text(&row[key],max) { return Err(bad()); }
        }
        if !row["adif"].is_null() && !row["adif"].as_u64().is_some_and(|n| (1..=9999).contains(&n)) { return Err(bad()); }
        let call = row["call_status"].as_str().ok_or_else(bad)?;
        let dxcc = row["dxcc_status"].as_str().ok_or_else(bad)?;
        if !matches!(call,"Unknown"|"New Call"|"New Band"|"New Mode"|"New Band+Mode"|"Worked"|"Confirmed")
            || !matches!(dxcc,"Unknown"|"ATNO"|"C/New Band"|"C/New Mode"|"C/New Band+Mode"|"W/New Band"|"W/New Mode"|"W/New Band+Mode"|"Worked"|"Confirmed") { return Err(bad()); }
        let need = &row["need"];
        let reasons = need["reasons"].as_array().filter(|r| r.len() <= 30).ok_or_else(bad)?;
        if reasons.iter().any(|r| !text(r,256)) { return Err(bad()); }
        if !row["country_resolution"].is_null() && !matches!(row["country_resolution"].as_str(),
            Some("resolved"|"knownNoEntity"|"ambiguous"|"unknown"|"invalid"|"missingDate")) { return Err(bad()); }
        match need["status"].as_str() {
            Some("unknown") if need["score"].is_null() && matches!(need["reason_code"].as_str(),
                Some("unavailable"|"refreshing"|"uncertain"|"stale"|"country_ambiguous"|"country_unknown"|"known_no_entity"|"unsupported_band")) => {},
            Some("needed"|"worked") if ready == "ready" && need["score"].as_i64().is_some_and(|n| i32::try_from(n).is_ok()) && need["reason_code"].is_null() => {},
            _ => return Err(bad()),
        }
        if ready != "ready" {
            if need["reason_code"] != ready || call != "Unknown" || dxcc != "Unknown" || !reasons.is_empty()
                || ["band","country","adif","country_resolution"].iter().any(|k| !row[*k].is_null()) { return Err(bad()); }
        } else {
            let resolution = row["country_resolution"].as_str().ok_or_else(bad)?;
            let no_entity = matches!(resolution,"knownNoEntity"|"ambiguous"|"unknown"|"invalid"|"missingDate");
            if no_entity && (!row["country"].is_null() || !row["adif"].is_null() || dxcc != "Unknown") { return Err(bad()); }
            if resolution == "resolved" && (row["country"].is_null() || row["adif"].is_null()) { return Err(bad()); }
            let required_reason = if row["band"].is_null() { Some("unsupported_band") }
                else { match resolution { "knownNoEntity" => Some("known_no_entity"), "ambiguous" => Some("country_ambiguous"),
                    "unknown"|"invalid"|"missingDate" => Some("country_unknown"), _ => None } };
            if let Some(reason) = required_reason {
                if need["status"] != "unknown" || need["reason_code"] != reason || !reasons.is_empty() { return Err(bad()); }
            } else if need["status"] == "unknown" { return Err(bad()); }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn batch() -> Batch { parse(br#"{"queries":[{"query_id":"q1","callsign":"K1ABC/P","dial_frequency_hz":14074000,"mode":"FT8"}]}"#).unwrap() }
    fn poll(id: &str) -> Value { json!({"id":id,"method":"cluster_snapshot","params":{"needs_only":true}}) }
    fn ready(batch: &Batch) -> Value {
        let mut value = unknown(batch, "refreshing");
        value["readiness"] = json!("ready");
        value["authority"] = json!({"logbook_revision":69977,"classification_revision":1,
            "country_reference_revision":"test-cty","policy_revision":POLICY});
        for row in value["results"].as_array_mut().unwrap() {
            row["band"] = json!("20m"); row["country"] = json!("United States"); row["adif"] = json!(291);
            row["country_resolution"] = json!("resolved"); row["call_status"] = json!("New Call");
            row["dxcc_status"] = json!("C/New Band");
            row["need"] = json!({"status":"needed","score":58,"reason_code":null,"reasons":["New Call"]});
        }
        value
    }
    #[test]
    fn queue_wait_does_not_consume_the_worker_execution_budget() {
        let mut owner = Mailbox::default();
        let _rx = owner.enqueue(batch(), "a".into()).unwrap();
        // Reproduce the measured 1738 ms poll cadence without a sleeping test.
        owner.job.as_mut().unwrap().deadline -= Duration::from_millis(1738);
        let mut request = poll("late-poll");
        owner.attach(&mut request);
        assert_eq!(owner.job.as_ref().unwrap().poll.as_deref(), Some("late-poll"));
        assert!(request["params"]["needs_query"]["expires_at_unix_ms"].as_u64().unwrap() >= now_ms() + 950);
        assert!(owner.job.as_ref().unwrap().deadline.duration_since(Instant::now()) > Duration::from_millis(950));
    }
    #[test]
    fn cache_retries_without_waiting_or_duplicate_jobs_and_preserves_fact_age() {
        let mut cache = QueryCache::default();
        let (sender, receiver) = mpsc::channel();
        let context = json!({"workspace":"a","stamp":1});
        assert_eq!(cache.query(&batch(), context.clone(), |_| Ok(receiver)).unwrap_err(), "needs_pending");
        assert_eq!(cache.query(&batch(), context.clone(), |_| panic!("duplicate job")).unwrap_err(), "needs_pending");
        let mut answer = ready(&batch());
        answer["generated_at_unix_ms"] = json!(now_ms() - 500);
        sender.send(Ok(answer.clone())).unwrap();
        let mut current = batch(); current.queries[0].query_id = "retry-id".into();
        let actual = cache.query(&current, context, |_| panic!("fresh fact already cached")).unwrap();
        assert_eq!(actual["generated_at_unix_ms"], answer["generated_at_unix_ms"]);
        assert_eq!(actual["max_age_ms"], answer["max_age_ms"]);
        assert_eq!(actual["results"][0]["query_id"], "retry-id");
    }
    #[test]
    fn cached_facts_cannot_cross_context_frequency_mode_or_expiry() {
        for change in 0..9 {
            let mut cache = QueryCache::default();
            let (sender, receiver) = mpsc::channel();
            let mut context = json!({"workspace":"a","stamp":1});
            assert!(cache.query(&batch(), context.clone(), |_| Ok(receiver)).is_err());
            let mut answer = ready(&batch());
            if change == 4 { answer["generated_at_unix_ms"] = json!(now_ms() - 2001); }
            sender.send(Ok(answer)).unwrap();
            let mut next = batch();
            match change {
                0 => context["stamp"] = json!(2),
                1 => context["workspace"] = json!("b"),
                2 => next.queries[0].dial_frequency_hz += 1,
                3 => next.queries[0].mode = "FT4".into(),
                5 => context["authority"] = json!({"logbook_revision":69978}),
                6 => context["worker"] = json!(2),
                7 => cache.clear(), // Readiness loss / write / lease revocation.
                8 => next.queries[0].callsign = "VK8RX".into(),
                _ => (),
            }
            let mut enqueued = false;
            assert!(cache.query(&next, context, |_| { enqueued = true; Ok(mpsc::channel().1) }).is_err());
            assert!(enqueued);
        }
    }
    #[test]
    fn strict_queries_reject_untrusted_facts_and_non_calls() {
        for call in ["5NN","599","59","59N","5N9","RR73","<K1ABC>","CQ DX","K1ABC//P",""] { assert!(!valid_call(call),"{call}"); }
        for call in ["EA8/K1ABC/P","3D2ABC","4U1UN","K1ABC/MM","VK8RX","GB50","9A2024","GB50/P"] { assert!(valid_call(call),"{call}"); }
        let mut raw = serde_json::to_value(batch()).unwrap();
        raw["queries"][0]["adif"] = json!(291);
        assert!(parse(&serde_json::to_vec(&raw).unwrap()).is_err());
        raw["queries"][0].as_object_mut().unwrap().remove("adif");
        let duplicate = raw["queries"][0].clone();
        raw["queries"].as_array_mut().unwrap().push(duplicate);
        assert!(parse(&serde_json::to_vec(&raw).unwrap()).is_err());
        raw["queries"] = json!([]);
        assert!(parse(&serde_json::to_vec(&raw).unwrap()).is_err());
        assert!(parse(&vec![b' '; MAX_BYTES+1]).is_err());
    }
    #[test]
    fn timeout_and_repairing_never_release_an_attached_job() {
        let mut owner = Mailbox::default();
        let first = owner.enqueue(batch(),"workspace-a".into()).unwrap();
        let mut request = poll("poll-a"); owner.attach(&mut request);
        assert!(!request["params"]["needs_query"].is_null());
        owner.job.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
        drop(first); // HTTP/client departure is not worker completion.
        for _ in 0..100 { assert!(owner.enqueue(batch(),"workspace-b".into()).is_err()); }
        let mut next = poll("poll-b"); owner.attach(&mut next);
        assert_eq!(next["params"],json!({"needs_only":true}));
        owner.complete(&mut json!({"id":"poll-b","ok":true,"result":{}}));
        assert!(owner.job.is_some());
        owner.complete(&mut json!({"id":"poll-a","ok":true,"result":{}}));
        assert!(owner.enqueue(batch(),"workspace-b".into()).is_ok());
    }
    #[test]
    fn expired_queued_jobs_do_not_write_and_resume_is_never_enriched() {
        let mut owner = Mailbox::default();
        let _rx = owner.enqueue(batch(),"workspace-a".into()).unwrap();
        let mut resume = json!({"id":"resume","method":"cluster_resume","params":{}});
        owner.attach(&mut resume); assert_eq!(resume["params"],json!({}));
        // The large spot snapshot poll never carries a lookup; only the needs poll does.
        let mut spots = json!({"id":"spots","method":"cluster_snapshot","params":{}});
        owner.attach(&mut spots); assert_eq!(spots["params"],json!({}));
        assert!(owner.job.as_ref().unwrap().poll.is_none());
        owner.job.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
        let mut request = poll("expired"); owner.attach(&mut request);
        assert_eq!(request["params"],json!({"needs_only":true})); assert!(owner.job.is_none());
    }
    #[test]
    fn result_identity_and_exact_id_set_are_checked_and_private_reply_is_stripped() {
        let mut owner = Mailbox::default();
        let rx = owner.enqueue(batch(),"workspace-a".into()).unwrap();
        let mut request = poll("p"); owner.attach(&mut request);
        let mut reply = json!({"id":"p","ok":true,"result":{"ordinary":"preserved","needs_reply":{
            "batch_id":request["params"]["needs_query"]["batch_id"],"workspace_id":"workspace-a","result":unknown(&batch(),"refreshing")}}});
        owner.complete(&mut reply);
        assert!(rx.recv().unwrap().is_ok()); assert!(reply["result"]["needs_reply"].is_null());
        assert_eq!(reply["result"]["ordinary"],"preserved");
        let mut value = unknown(&batch(),"refreshing");
        value["results"][0]["query_id"] = json!("another"); assert!(validate_reply(&value,&batch()).is_err());
        value = unknown(&batch(),"refreshing"); value["results"][0]["need"]["score"] = json!(0);
        assert!(validate_reply(&value,&batch()).is_err());
        value = unknown(&batch(),"refreshing"); value["generated_at_unix_ms"] = json!(now_ms()+1000);
        assert!(validate_reply(&value,&batch()).is_err());
    }
    #[test]
    fn retirement_not_repairing_releases_worker_lifetime_slot() {
        let mut owner = Mailbox::default(); let rx = owner.enqueue(batch(),"a".into()).unwrap();
        owner.attach(&mut poll("p")); owner.retired();
        assert!(rx.recv().unwrap().is_err()); assert!(owner.job.is_none());
    }
    #[test]
    fn missing_nullable_fields_and_contradictory_unavailable_facts_fail() {
        for field in ["authority","readiness","generated_at_unix_ms","max_age_ms","results"] {
            let mut value = unknown(&batch(),"unavailable"); value.as_object_mut().unwrap().remove(field);
            assert!(validate_reply(&value,&batch()).is_err(),"{field}");
        }
        for field in ["band","country","adif","country_resolution"] {
            let mut value = unknown(&batch(),"unavailable"); value["results"][0].as_object_mut().unwrap().remove(field);
            assert!(validate_reply(&value,&batch()).is_err(),"{field}");
        }
        for field in ["score","reason_code","reasons","status"] {
            let mut value = unknown(&batch(),"unavailable"); value["results"][0]["need"].as_object_mut().unwrap().remove(field);
            assert!(validate_reply(&value,&batch()).is_err(),"{field}");
        }
        let mut value = unknown(&batch(),"unavailable"); value["results"][0]["band"] = json!("20m");
        assert!(validate_reply(&value,&batch()).is_err());
    }
}
