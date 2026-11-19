//! Bounded append/sync operation decisions. A failed append is indeterminate;
//! callers must mute/revoke and retain the release obligation, never retry start.
use crate::{
    companion_tx_contract::{Binding, Identity, Operation, Outcome, OutputRoute, Phase, Receipt},
    output_authority::Reservation,
    ptt_session::Target,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Above this size, or half the operation limit, a launch compacts the journal.
const COMPACT_BYTES: u64 = 1024 * 1024;
/// A resolved operation that ended longer ago than this can no longer overlap a
/// new slot or receive a meaningful replay, so compaction may drop it.
const RETAIN_RESOLVED_US: u64 = 3_600_000_000;
pub const MAX_OPERATIONS: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub binding: Binding,
    pub client_id: String,
    pub route: OutputRoute,
    pub identity: Identity,
    pub target: Target,
    pub ticket: Reservation,
    pub slot_start_us: u64,
    pub latest_start_us: u64,
    pub end_us: u64,
    pub release_id: String,
    pub ptt_attempted: bool,
    pub receipt: Receipt,
    /// Exact request digests by event ID, including action. Never evicted.
    pub events: BTreeMap<String, String>,
}
impl Record {
    pub fn unresolved(&self) -> bool {
        self.receipt.phase != Phase::Terminal || self.receipt.outcome == Outcome::Uncertain
    }
    pub fn possible(&self) -> bool {
        self.ptt_attempted
    }
    pub fn validate(&self) -> Result<(), String> {
        use crate::companion_tx_prepare::{canonical_uuid, digest};
        for id in [
            &self.binding.server_instance,
            &self.binding.workspace_id,
            &self.binding.context_id,
            &self.binding.clock_epoch,
            &self.client_id,
            &self.receipt.operation_id,
            &self.receipt.event_id,
            &self.release_id,
        ] {
            if !canonical_uuid(id) {
                return Err("Invalid TX journal identity".into());
            }
        }
        self.ticket.validate()?;
        self.target.validate()?;
        let r = &self.receipt;
        if r.schema_version != 1
            || !digest(&r.body_sha256)
            || r.server_instance != self.binding.server_instance
            || r.workspace_id != self.binding.workspace_id
            || r.context_id != self.binding.context_id
            || r.clock_epoch != self.binding.clock_epoch
            || r.operation_id != self.ticket.operation_id()
            || self.slot_start_us > self.latest_start_us
            || self.latest_start_us >= self.end_us
            || self.end_us > 9_007_199_254_740_991
            || self.events.len() > 32
            || self
                .events
                .iter()
                .any(|(id, hash)| !canonical_uuid(id) || !digest(hash))
        {
            return Err("Invalid TX journal binding".into());
        }
        let legal = match (&r.phase, &r.outcome) {
            (Phase::Prepared, Outcome::NoStart) | (Phase::Terminal, Outcome::NoStart) => {
                !self.ptt_attempted
                    && r.actual_start_unix_us.is_none()
                    && r.release_observed_unix_us.is_none()
            }
            (Phase::Starting, Outcome::Uncertain) => {
                r.actual_start_unix_us.is_none() && r.release_observed_unix_us.is_none()
            }
            (Phase::Active, Outcome::Uncertain) => {
                self.ptt_attempted
                    && r.actual_start_unix_us.is_some()
                    && r.release_observed_unix_us.is_none()
            }
            (Phase::Terminal, Outcome::Uncertain) => r.release_observed_unix_us.is_none(),
            (Phase::Terminal, Outcome::ReleaseObserved) => {
                self.ptt_attempted && r.release_observed_unix_us.is_some()
            }
            _ => false,
        };
        if !legal
            || r.actual_start_unix_us
                .is_some_and(|t| t < self.slot_start_us || t > self.latest_start_us)
            || r.release_observed_unix_us.is_some_and(|t| {
                t > 9_007_199_254_740_991 || r.actual_start_unix_us.is_some_and(|start| t < start)
            })
        {
            return Err("Invalid TX journal receipt".into());
        }
        Ok(())
    }
    fn follows(&self, old: &Self) -> bool {
        let mut same = self.clone();
        same.receipt = old.receipt.clone();
        same.events = old.events.clone();
        // Acquisition refreshes observation/control revisions only, never route ownership.
        same.target = old.target.clone();
        same.ptt_attempted = old.ptt_attempted;
        if same != *old
            || self.target.provider != old.target.provider
            || self.target.selection_id != old.target.selection_id
            || self.target.selection_generation != old.target.selection_generation
            || self.target.connection_generation != old.target.connection_generation
            || self.target.worker_generation != old.target.worker_generation
            || self.target.radio_id != old.target.radio_id
            || self.target.tci != old.target.tci
            || old.ptt_attempted && !self.ptt_attempted
            || old.ptt_attempted && self.target != old.target
            || old
                .events
                .iter()
                .any(|(id, hash)| self.events.get(id) != Some(hash))
        {
            return false;
        }
        let a = &old.receipt;
        let b = &self.receipt;
        if a.event_id != b.event_id
            || a.body_sha256 != b.body_sha256
            || a.operation_id != b.operation_id
            || a.actual_start_unix_us
                .is_some_and(|t| b.actual_start_unix_us != Some(t))
        {
            return false;
        }
        if !old.unresolved() {
            return a == b;
        }
        match (&a.phase, &b.phase) {
            (Phase::Prepared, Phase::Prepared | Phase::Starting | Phase::Terminal) => true,
            (Phase::Starting, Phase::Starting | Phase::Active | Phase::Terminal) => {
                b.outcome != Outcome::NoStart || !self.ptt_attempted
            }
            (Phase::Active, Phase::Active | Phase::Terminal) => b.outcome != Outcome::NoStart,
            (Phase::Terminal, Phase::Terminal) => b.outcome != Outcome::NoStart,
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", deny_unknown_fields)]
enum Line {
    Record(Record),
    Stop(Operation),
    NoStart(Receipt),
    /// The latest state of an operation, written by compaction in place of its
    /// superseded history. Validated as a record; its earlier lines are gone.
    Compacted(Record),
}
pub struct Journal {
    path: PathBuf,
    file: Option<File>,
    pub records: BTreeMap<String, Record>,
    pub stopped: BTreeMap<String, Operation>,
    pub no_start: BTreeMap<String, Receipt>,
    faulted: bool,
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self, String> {
        if std::fs::symlink_metadata(path).is_ok_and(|m| {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                m.file_attributes() & 0x400 != 0
            }
            #[cfg(not(windows))]
            {
                m.file_type().is_symlink()
            }
        }) {
            return Err("TX journal link refused".into());
        }
        let mut file = open_file(path)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES || !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            return Err("Incomplete or oversized TX journal; preserved without repair".into());
        }
        let mut records: BTreeMap<String, Record> = BTreeMap::new();
        let mut stopped: BTreeMap<String, Operation> = BTreeMap::new();
        let mut no_start: BTreeMap<String, Receipt> = BTreeMap::new();
        for line in bytes.split(|b| *b == b'\n').filter(|s| !s.is_empty()) {
            let entry: Line =
                serde_json::from_slice(line).map_err(|_| "Invalid TX journal; preserved")?;
            let r = match entry {
                Line::Record(r) => r,
                Line::Compacted(r) => {
                    r.validate()?;
                    if records.contains_key(&r.receipt.operation_id)
                        || no_start.contains_key(&r.receipt.operation_id)
                        || r.unresolved() && records.values().any(Record::unresolved)
                        || r.ptt_attempted && overlaps(&records, &r)
                    {
                        return Err("Conflicting compacted TX operation".into());
                    }
                    records.insert(r.receipt.operation_id.clone(), r);
                    if records.len() > MAX_OPERATIONS {
                        return Err("TX journal full; no decisions evicted".into());
                    }
                    continue;
                }
                Line::NoStart(r) => {
                    validate_no_start(&r, stopped.get(&r.operation_id))?;
                    if no_start.get(&r.operation_id).is_some_and(|old| old != &r)
                        || records.contains_key(&r.operation_id)
                    {
                        return Err("Conflicting retired TX preparation".into());
                    }
                    no_start.insert(r.operation_id.clone(), r);
                    continue;
                }
                Line::Stop(q) => {
                    validate_stop(&q)?;
                    if stopped
                        .get(&q.operation_id)
                        .is_some_and(|old| !same_operation(old, &q))
                    {
                        return Err("Conflicting retained TX Stop".into());
                    }
                    stopped.insert(q.operation_id.clone(), q);
                    if stopped.len() > MAX_OPERATIONS {
                        return Err("TX Stop journal full".into());
                    }
                    continue;
                }
            };
            r.validate()?;
            if no_start.contains_key(&r.receipt.operation_id) {
                return Err("A stopped-before-prepare operation reappeared".into());
            }
            if r.ptt_attempted && overlaps(&records, &r) {
                return Err("Overlapping consumed TX slots".into());
            }
            if let Some(old) = records.get(&r.receipt.operation_id) {
                if !r.follows(old) {
                    return Err("Conflicting TX journal transition".into());
                }
            } else if r.receipt.phase != Phase::Prepared || records.values().any(Record::unresolved)
            {
                return Err("TX journal preparation missing or overlapping".into());
            }
            records.insert(r.receipt.operation_id.clone(), r);
            if records.len() > MAX_OPERATIONS {
                return Err("TX journal full; no decisions evicted".into());
            }
        }
        if records.values().filter(|r| r.unresolved()).count() > 1 {
            return Err("Multiple unresolved TX operations".into());
        }
        let mut journal = Self {
            path: path.to_path_buf(),
            file: Some(file),
            records,
            stopped,
            no_start,
            faulted: false,
        };
        if bytes.len() as u64 > COMPACT_BYTES || journal.records.len() > MAX_OPERATIONS / 2 {
            journal.compact(now_us(), true)?;
        }
        Ok(journal)
    }
    /// Rewrite the journal with only what can still matter, never dropping an
    /// unresolved operation: each kept operation's latest state, every resolved
    /// operation that ended within the retention window, and the Stop/NoStart lines
    /// of kept operations. At launch (`previous_run`) Stops and retired preparations
    /// of operations no longer kept are dropped: they bind a previous bridge
    /// instance and can never match a request again. During a run, a Stop that
    /// arrived before its preparation is kept. The replacement is written and synced
    /// first and replaces the file in one rename; on failure the existing file stays.
    fn compact(&mut self, now: u64, previous_run: bool) -> Result<(), String> {
        let keep = |r: &Record| r.unresolved() || r.end_us.saturating_add(RETAIN_RESOLVED_US) > now;
        let records: BTreeMap<String, Record> =
            self.records.iter().filter(|(_, r)| keep(r)).map(|(k, r)| (k.clone(), r.clone())).collect();
        let stopped: BTreeMap<String, Operation> = self
            .stopped
            .iter()
            .filter(|(k, _)| {
                records.contains_key(*k)
                    || !previous_run && !self.records.contains_key(*k) && !self.no_start.contains_key(*k)
            })
            .map(|(k, q)| (k.clone(), q.clone()))
            .collect();
        let no_start: BTreeMap<String, Receipt> = if previous_run {
            BTreeMap::new()
        } else {
            self.no_start.iter().filter(|(k, _)| stopped.contains_key(*k)).map(|(k, r)| (k.clone(), r.clone())).collect()
        };
        let dir = self.path.parent().ok_or("TX journal directory unavailable")?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let mut push = |line: Line| -> Result<(), String> {
            bytes.extend(serde_json::to_vec(&line).map_err(|e| e.to_string())?);
            bytes.push(b'\n');
            Ok(())
        };
        for q in stopped.values() {
            push(Line::Stop(q.clone()))?;
        }
        for r in no_start.values() {
            push(Line::NoStart(r.clone()))?;
        }
        let mut ordered: Vec<&Record> = records.values().collect();
        ordered.sort_by_key(|r| (r.unresolved(), r.slot_start_us));
        for r in ordered {
            push(Line::Compacted(r.clone()))?;
        }
        tmp.write_all(&bytes).and_then(|_| tmp.as_file().sync_all()).map_err(|e| e.to_string())?;
        // The live handle is exclusive; close it for the rename, then reopen.
        drop(self.file.take());
        // `persist` hands back the renamed file's handle; release it before the exclusive reopen.
        let persisted = tmp.persist(&self.path).map(drop).map_err(|e| e.error.to_string());
        match open_file(&self.path) {
            Ok(file) => self.file = Some(file),
            Err(e) => {
                self.faulted = true;
                return Err(format!("TX journal reopen failed after compaction: {e}"));
            }
        }
        persisted?;
        self.records = records;
        self.stopped = stopped;
        self.no_start = no_start;
        Ok(())
    }
    pub fn healthy(&self) -> bool {
        !self.faulted
    }
    pub fn append(&mut self, record: Record) -> Result<(), String> {
        if self.faulted {
            return Err("TX journal durability unresolved; no further acquisition".into());
        }
        record.validate()?;
        if self.no_start.contains_key(&record.receipt.operation_id) {
            return Err("TX preparation was already durably cancelled".into());
        }
        if let Some(old) = self.records.get(&record.receipt.operation_id) {
            if old == &record {
                // Identical to what is already durable: a repeated Stop or release attempt
                // that changed nothing must not grow the journal.
                return Ok(());
            }
            if !record.follows(old) {
                return Err("TX operation mutation refused".into());
            }
        } else {
            if record.receipt.phase != Phase::Prepared {
                return Err("TX journal admission full or missing preparation".into());
            }
            if self.records.len() >= MAX_OPERATIONS {
                self.compact(now_us(), false)?;
            }
            if self.records.len() >= MAX_OPERATIONS {
                return Err("TX journal admission full or missing preparation".into());
            }
        }
        self.write(&Line::Record(record.clone()))?;
        self.records
            .insert(record.receipt.operation_id.clone(), record);
        Ok(())
    }
    pub fn stop(&mut self, q: Operation) -> Result<(), String> {
        validate_stop(&q)?;
        if let Some(old) = self.stopped.get(&q.operation_id) {
            return if same_operation(old, &q) {
                Ok(())
            } else {
                Err("TX Stop body conflict".into())
            };
        }
        if self.stopped.len() >= MAX_OPERATIONS {
            self.compact(now_us(), false)?;
        }
        if self.stopped.len() >= MAX_OPERATIONS {
            return Err("TX Stop journal full; no decisions evicted".into());
        }
        self.write(&Line::Stop(q.clone()))?;
        self.stopped.insert(q.operation_id.clone(), q);
        Ok(())
    }
    pub fn retire_unprepared(&mut self, r: Receipt) -> Result<(), String> {
        validate_no_start(&r, self.stopped.get(&r.operation_id))?;
        if self.records.contains_key(&r.operation_id) {
            return Err("TX operation was already prepared".into());
        }
        if let Some(old) = self.no_start.get(&r.operation_id) {
            return if old == &r {
                Ok(())
            } else {
                Err("TX retired preparation conflict".into())
            };
        }
        self.write(&Line::NoStart(r.clone()))?;
        self.no_start.insert(r.operation_id.clone(), r);
        Ok(())
    }
    fn len(&self) -> Result<u64, String> {
        Ok(self.file.as_ref().ok_or("TX journal unavailable")?.metadata().map_err(|e| e.to_string())?.len())
    }
    fn write(&mut self, line: &Line) -> Result<(), String> {
        if self.faulted {
            return Err("TX journal durability unresolved".into());
        }
        let mut bytes = serde_json::to_vec(line).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        if self.len()? + bytes.len() as u64 > MAX_BYTES {
            self.compact(now_us(), false)?;
            if self.len()? + bytes.len() as u64 > MAX_BYTES {
                return Err("TX journal capacity exhausted; no decisions evicted".into());
            }
        }
        let file = self.file.as_mut().ok_or("TX journal unavailable")?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            self.faulted = true;
            return Err(format!("TX journal durability unresolved: {error}"));
        }
        Ok(())
    }
}
fn open_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path).map_err(|e| e.to_string())
}
fn overlaps(records: &BTreeMap<String, Record>, r: &Record) -> bool {
    records.values().any(|old| {
        old.receipt.operation_id != r.receipt.operation_id
            && old.ptt_attempted
            && old.slot_start_us < r.end_us
            && r.slot_start_us < old.end_us
    })
}
fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
pub fn same_operation(a: &Operation, b: &Operation) -> bool {
    a.binding == b.binding
        && a.client_id == b.client_id
        && a.operation_id == b.operation_id
        && a.body_sha256 == b.body_sha256
}
pub fn validate_stop(q: &Operation) -> Result<(), String> {
    use crate::companion_tx_prepare::{canonical_uuid, digest};
    if [
        &q.binding.server_instance,
        &q.binding.workspace_id,
        &q.binding.context_id,
        &q.binding.clock_epoch,
        &q.client_id,
        &q.operation_id,
        &q.event_id,
    ]
    .iter()
    .any(|id| !canonical_uuid(id))
        || !digest(&q.body_sha256)
    {
        return Err("Invalid exact TX Stop".into());
    }
    Ok(())
}
fn validate_no_start(r: &Receipt, q: Option<&Operation>) -> Result<(), String> {
    let q = q.ok_or("TX no-start receipt lacks retained Stop")?;
    if r.schema_version != 1
        || r.phase != Phase::Terminal
        || r.outcome != Outcome::NoStart
        || r.actual_start_unix_us.is_some()
        || r.release_observed_unix_us.is_some()
        || !crate::companion_tx_prepare::canonical_uuid(&r.event_id)
        || r.operation_id != q.operation_id
        || r.body_sha256 != q.body_sha256
        || r.context_id != q.binding.context_id
        || r.clock_epoch != q.binding.clock_epoch
        || r.server_instance != q.binding.server_instance
        || r.workspace_id != q.binding.workspace_id
    {
        return Err("Invalid retained TX no-start proof".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn record() -> Record {
        let binding = Binding {
            server_instance: id(),
            workspace_id: id(),
            context_id: id(),
            clock_epoch: id(),
        };
        let operation = id();
        let authority = crate::output_authority::Authority::default();
        let ticket = authority
            .prepare(&operation, &authority.intent().unwrap())
            .unwrap();
        let event = id();
        let body = "a".repeat(64);
        Record {
            binding: binding.clone(),
            client_id: id(),
            route: OutputRoute {
                radio_id: "radio1".into(),
                receiver_id: "synthetic/A".into(),
                transmit_slice: "A".into(),
                device_id: "synthetic".into(),
                route_epoch: id(),
                sample_rate_hz: 48000,
                channels: 2,
                selected_channels: vec![0, 1],
                frequency_hz: 14074000,
                radio_mode: "DIGU".into(),
            },
            identity: Identity {
                mycall: "K1ABC".into(),
                mygrid: "FN42".into(),
                worker_generation: 1,
                settings_revision: 1,
                identity_epoch: id(),
                identity_revision: 1,
            },
            target: Target {
                provider: "direct_flex_lan".into(),
                selection_generation: 1,
                selection_id: id(),
                worker_generation: 1,
                connection_generation: 1,
                control_revision: 1,
                observation_revision: 1,
                received_at_ms: 1,
                radio_id: Some("radio1".into()),
                tci: None,
            },
            ticket,
            slot_start_us: 15_000_000,
            latest_start_us: 15_050_000,
            end_us: 30_000_000,
            release_id: id(),
            ptt_attempted: false,
            receipt: Receipt {
                schema_version: 1,
                server_instance: binding.server_instance.clone(),
                workspace_id: binding.workspace_id.clone(),
                operation_id: operation,
                body_sha256: body.clone(),
                context_id: binding.context_id.clone(),
                clock_epoch: binding.clock_epoch.clone(),
                event_id: event.clone(),
                phase: Phase::Prepared,
                outcome: Outcome::NoStart,
                actual_start_unix_us: None,
                release_observed_unix_us: None,
                reason: None,
            },
            events: BTreeMap::from([(event, body)]),
        }
    }
    fn stop(r: &Record) -> Operation {
        Operation {
            binding: r.binding.clone(),
            client_id: r.client_id.clone(),
            operation_id: r.receipt.operation_id.clone(),
            body_sha256: r.receipt.body_sha256.clone(),
            event_id: id(),
        }
    }
    fn resolved(mut r: Record) -> Record {
        r.receipt.phase = Phase::Terminal;
        if r.ptt_attempted {
            r.receipt.outcome = Outcome::ReleaseObserved;
            r.receipt.release_observed_unix_us = Some(r.end_us);
        } else {
            r.receipt.outcome = Outcome::NoStart;
            r.receipt.reason = Some("stopped_before_acquisition".into());
        }
        r
    }
    /// Operator station 2026-11-18: a retained Stop flood filled the journal to its 8 MiB cap
    /// and every later preparation failed with "TX journal capacity exhausted".
    #[test]
    fn a_flooded_journal_compacts_at_launch_without_losing_an_unresolved_or_recent_decision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.jsonl");
        let now = now_us();
        let mut j = Journal::open(&path).unwrap();
        // Long finished, from an earlier run: may be dropped, with its Stop.
        let old = record();
        j.append(old.clone()).unwrap();
        j.stop(stop(&old)).unwrap();
        j.append(resolved(old.clone())).unwrap();
        // Finished a minute ago: kept, it could still overlap a new slot.
        let mut recent = record();
        recent.slot_start_us = now - 60_000_000;
        recent.latest_start_us = recent.slot_start_us + 50_000;
        recent.end_us = now - 45_000_000;
        recent.ptt_attempted = true;
        j.append({ let mut p = recent.clone(); p.ptt_attempted = false; p }).unwrap();
        j.append(resolved(recent.clone())).unwrap();
        // Keyed and unresolved: must survive with its Stop.
        let mut open = record();
        open.slot_start_us = now - 30_000_000;
        open.latest_start_us = open.slot_start_us + 50_000;
        open.end_us = now - 15_000_000;
        j.append(open.clone()).unwrap();
        let open_stop = stop(&open);
        j.stop(open_stop.clone()).unwrap();
        open.receipt.phase = Phase::Terminal;
        open.receipt.outcome = Outcome::Uncertain;
        open.ptt_attempted = true;
        for n in 0..700 {
            open.receipt.reason = Some(format!("explicit_retained_stop {n} {}", "x".repeat(1200)));
            j.append(open.clone()).unwrap();
        }
        // An identical repeat is not written again.
        let before = std::fs::metadata(&path).unwrap().len();
        j.append(open.clone()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
        assert!(before > COMPACT_BYTES);
        drop(j);

        let j = Journal::open(&path).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < 64 * 1024, "compacted to {size} bytes");
        assert_eq!(j.records.get(&open.receipt.operation_id), Some(&open), "unresolved kept at its latest state");
        assert!(j.records.contains_key(&recent.receipt.operation_id), "recent decision kept");
        assert!(!j.records.contains_key(&old.receipt.operation_id), "long-finished operation dropped");
        assert!(j.stopped.contains_key(&open_stop.operation_id) && !j.stopped.contains_key(&old.receipt.operation_id));
        drop(j);
        // The compacted form reloads to the same state, and the operation can still close.
        let mut j = Journal::open(&path).unwrap();
        assert_eq!(j.records.get(&open.receipt.operation_id), Some(&open));
        j.append(resolved(open.clone())).unwrap();
        let next = record();
        j.append(next.clone()).unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert!(!j.records[&open.receipt.operation_id].unresolved());
        assert!(j.records.contains_key(&next.receipt.operation_id));
    }
    #[test]
    fn the_operation_limit_compacts_finished_history_instead_of_refusing_tx_for_ever() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.jsonl");
        let mut j = Journal::open(&path).unwrap();
        for _ in 0..MAX_OPERATIONS {
            let r = record();
            j.append(r.clone()).unwrap();
            j.append(resolved(r)).unwrap();
        }
        assert_eq!(j.records.len(), MAX_OPERATIONS);
        let fresh = record();
        j.append(fresh.clone()).unwrap();
        assert!(j.records.contains_key(&fresh.receipt.operation_id));
        assert!(j.records.len() < MAX_OPERATIONS);
        drop(j);
        assert!(Journal::open(&path).unwrap().records.contains_key(&fresh.receipt.operation_id));
    }
    #[test]
    fn stop_before_prepare_is_retained_across_restart_and_body_conflict_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.jsonl");
        let r = record();
        let q = stop(&r);
        let mut j = Journal::open(&path).unwrap();
        j.stop(q.clone()).unwrap();
        drop(j);
        let mut j = Journal::open(&path).unwrap();
        assert!(same_operation(j.stopped.get(&q.operation_id).unwrap(), &q));
        let mut changed = q.clone();
        changed.body_sha256 = "b".repeat(64);
        assert!(j.stop(changed).is_err());
        let mut receipt = r.receipt.clone();
        receipt.phase = Phase::Terminal;
        receipt.reason = Some("stop_retained_before_prepare".into());
        j.retire_unprepared(receipt.clone()).unwrap();
        j.retire_unprepared(receipt.clone()).unwrap();
        assert!(j.append(r.clone()).is_err(), "a cancelled preparation cannot reappear or corrupt restart");
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert_eq!(j.no_start.get(&q.operation_id), Some(&receipt));
        assert!(j.records.is_empty());
    }
    #[test]
    fn restart_retains_possible_effect_and_never_restores_a_prepared_audio_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.jsonl");
        let mut j = Journal::open(&path).unwrap();
        let mut r = record();
        j.append(r.clone()).unwrap();
        r.receipt.phase = Phase::Starting;
        r.receipt.outcome = Outcome::Uncertain;
        r.ptt_attempted = true;
        j.append(r.clone()).unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        let retained = j.records.get(&r.receipt.operation_id).unwrap();
        assert!(retained.unresolved());
        assert!(retained.possible());
        assert_eq!(retained.ticket, r.ticket);
        drop(j); // The production journal deliberately denies concurrent opens on Windows.
        let bytes = std::fs::read_to_string(path).unwrap();
        assert!(!bytes.contains("samples"));
    }
    #[test]
    fn an_attempt_refused_before_ptt_on_can_close_as_released_and_stays_closed_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.jsonl");
        let mut j = Journal::open(&path).unwrap();
        let mut r = record();
        j.append(r.clone()).unwrap();
        r.receipt.phase = Phase::Starting;
        r.receipt.outcome = Outcome::Uncertain;
        r.ptt_attempted = true;
        j.append(r.clone()).unwrap();
        // Also from the stuck state an older build left behind: Terminal and Uncertain.
        r.receipt.phase = Phase::Terminal;
        j.append(r.clone()).unwrap();
        r.receipt.outcome = Outcome::ReleaseObserved;
        r.receipt.release_observed_unix_us = Some(r.slot_start_us + 1);
        j.append(r.clone()).unwrap();
        assert!(!j.records[&r.receipt.operation_id].unresolved());
        // Never-started is never rewritten as NoStart once PTT was attempted.
        let mut no_start = r.clone();
        no_start.receipt.outcome = Outcome::NoStart;
        no_start.receipt.release_observed_unix_us = None;
        assert!(j.append(no_start).is_err());
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert!(!j.records.values().any(Record::unresolved));
    }
    #[test]
    fn unknown_release_can_refine_but_start_time_and_final_release_are_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::open(&dir.path().join("tx.jsonl")).unwrap();
        let mut r = record();
        j.append(r.clone()).unwrap();
        r.receipt.phase = Phase::Starting;
        r.receipt.outcome = Outcome::Uncertain;
        r.ptt_attempted = true;
        j.append(r.clone()).unwrap();
        r.receipt.phase = Phase::Active;
        r.receipt.actual_start_unix_us = Some(r.slot_start_us);
        j.append(r.clone()).unwrap();
        r.receipt.phase = Phase::Terminal;
        j.append(r.clone()).unwrap();
        let mut changed = r.clone();
        changed.receipt.actual_start_unix_us = None;
        assert!(j.append(changed).is_err());
        r.receipt.outcome = Outcome::ReleaseObserved;
        r.receipt.release_observed_unix_us = Some(r.end_us);
        j.append(r.clone()).unwrap();
        let mut changed = r.clone();
        changed.receipt.release_observed_unix_us = Some(r.end_us + 1);
        assert!(j.append(changed).is_err());
        r.receipt.outcome = Outcome::Uncertain;
        r.receipt.release_observed_unix_us = None;
        assert!(j.append(r).is_err());
    }
    #[test]
    fn known_no_start_cannot_follow_possible_ptt_or_mutate_route() {
        let mut r = record();
        let old = r.clone();
        r.receipt.phase = Phase::Starting;
        r.receipt.outcome = Outcome::Uncertain;
        r.ptt_attempted = true;
        assert!(r.follows(&old));
        let mut bad = r.clone();
        bad.receipt.phase = Phase::Terminal;
        bad.receipt.outcome = Outcome::NoStart;
        assert!(bad.validate().is_err());
        assert!(!bad.follows(&r));
        bad = r.clone();
        bad.route.device_id = "other".into();
        assert!(!bad.follows(&r));
        bad = r.clone();
        bad.target.control_revision += 1;
        assert!(!bad.follows(&r));
    }
    #[test]
    fn partial_append_is_preserved_and_blocks_recovery_instead_of_discarding_release_duty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.jsonl");
        let mut j = Journal::open(&path).unwrap();
        j.append(record()).unwrap();
        drop(j);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{partial")
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(Journal::open(&path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    #[test]
    fn event_body_and_obligation_cannot_be_replaced_even_when_phase_is_unchanged() {
        let r = record();
        let mut changed = r.clone();
        changed
            .events
            .insert(r.receipt.event_id.clone(), "b".repeat(64));
        assert!(!changed.follows(&r));
        changed = r.clone();
        changed.binding.context_id = id();
        assert!(!changed.follows(&r));
        changed = r.clone();
        changed.ticket = record().ticket;
        assert!(!changed.follows(&r));
    }
}
