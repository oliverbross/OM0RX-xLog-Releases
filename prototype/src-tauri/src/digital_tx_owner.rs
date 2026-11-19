//! Offline owner model. No endpoint, device backend, audio output or PTT wiring.
//! A transport implementation must arbitrate cancellation at its actual effect
//! boundary; checking a ticket here is not physical transmission authority.
use crate::output_authority::{Authority, Reservation};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::{File, OpenOptions}, io::{Read, Write}, path::Path,
    sync::{Arc, atomic::{AtomicU8, Ordering}}};

const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_OPERATIONS: usize = 256;
const READY: u8 = 0;
const POSSIBLE: u8 = 1;
const STOPPED_BEFORE_START: u8 = 2;
const STOPPED_POSSIBLE: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Body {
    pub operation_id: String,
    /// Canonical producer descriptor, compared byte-for-byte. This offline
    /// slice does not certify a waveform or parse a live Nexus manifest.
    pub manifest: String,
    /// Complete opaque synthetic route/epochs/policy binding, never a radio ID
    /// alone. A qualified live context builder is deliberately absent.
    pub context: String,
    pub clock_epoch: String,
    pub slot_start_us: u64,
    pub slot_duration_us: u64,
    pub latest_start_us: u64,
}
impl Body {
    fn validate(&self) -> Result<(), String> {
        let id = uuid::Uuid::parse_str(&self.operation_id).map_err(|_| "Invalid operation ID")?;
        if id.is_nil() || id.to_string() != self.operation_id || self.manifest.is_empty()
            || self.manifest.len() > 16384 || self.context.is_empty() || self.context.len() > 16384
            || self.clock_epoch.is_empty() || self.clock_epoch.len() > 128
            || ![7_500_000, 15_000_000].contains(&self.slot_duration_us)
            || self.slot_start_us % self.slot_duration_us != 0
            || self.latest_start_us < self.slot_start_us
            || self.slot_start_us.checked_add(self.slot_duration_us).is_none()
            || self.latest_start_us >= self.slot_start_us + self.slot_duration_us {
            return Err("Invalid offline transmit descriptor".into());
        }
        Ok(())
    }
    fn overlaps(&self, other: &Self) -> bool {
        // Deliberately independent of caller context/clock epoch: changing an
        // epoch or choosing FT4 cannot circumvent an already consumed FT8 slot.
        self.slot_start_us < other.slot_start_us + other.slot_duration_us
            && other.slot_start_us < self.slot_start_us + self.slot_duration_us
    }
}

pub(crate) struct Observation<'a> { pub context: &'a str, pub clock_epoch: &'a str, pub now_us: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Phase { Prepared, Starting, Active, Cancelled, Completed }
impl Phase { fn terminal(self) -> bool { matches!(self, Self::Cancelled | Self::Completed) } }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record { pub body: Body, ticket: Reservation, pub phase: Phase, consumed: bool }

/// Append and sync are one fallible operation. An error is indeterminate: the
/// owner retains admission and refuses further starts until recovery.
pub(crate) trait Journal: Send {
    fn read(&mut self) -> Result<Vec<Record>, String>;
    fn append(&mut self, record: &Record) -> Result<(), String>;
}
/// Explicitly constructed by offline tests only. No app startup opens this.
/// A partial final line is corruption, never discarded or silently repaired.
pub(crate) struct FileJournal { file: File }
impl FileJournal {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.read(true).append(true).create(true);
        #[cfg(windows)] {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0); // One owner, including during recovery.
        }
        Ok(Self { file: options.open(path).map_err(|e| e.to_string())? })
    }
}
impl Journal for FileJournal {
    fn read(&mut self) -> Result<Vec<Record>, String> {
        use std::io::{Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.file).take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES || (!bytes.is_empty() && bytes.last() != Some(&b'\n')) {
            return Err("Oversized or incomplete output journal; preserve it".into());
        }
        bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).map_err(|e| format!("Invalid output journal: {e}"))).collect()
    }
    fn append(&mut self, record: &Record) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        if self.file.metadata().map_err(|e| e.to_string())?.len() + bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err("Output journal full; no decisions were evicted".into());
        }
        self.file.write_all(&bytes).and_then(|_| self.file.sync_all()).map_err(|e| e.to_string())
    }
}

pub(crate) struct Permit { state: AtomicU8, authority: Arc<Authority>, ticket: Reservation, body: Body }
impl Permit {
    fn new(possible: bool, authority: Arc<Authority>, ticket: Reservation, body: Body) -> Self {
        Self { state: AtomicU8::new(if possible { STOPPED_POSSIBLE } else { READY }), authority, ticket, body }
    }
    /// A test transport must bind its immutable sample object to this complete
    /// owner body before interpreting a simulated start. This is not RF proof.
    pub(crate) fn matches_prepared_body(&self, body: &Body) -> bool { self.body == *body }
    /// Test transport calls this at its simulated effect boundary. Real output
    /// needs stronger transport-specific serialization and release evidence.
    pub(crate) fn begin_effect(&self, observation: Observation<'_>) -> bool {
        if self.authority.require_exact(&self.ticket).is_err()
            || self.body.context != observation.context || self.body.clock_epoch != observation.clock_epoch
            || observation.now_us < self.body.slot_start_us || observation.now_us > self.body.latest_start_us {
            self.stop(); return false;
        }
        // The preliminary check above is not sufficient: global Stop may
        // revoke its intent before the state transition. Recheck and perform
        // that transition under the authority's short state lock.
        match self.authority.compare_exchange_exact(&self.ticket, &self.state, READY, POSSIBLE) {
            Ok(started) => started,
            Err(_) => { self.stop(); false }
        }
    }
    fn stop(&self) -> bool {
        let prior = self.state.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |state|
            Some(if state == POSSIBLE || state == STOPPED_POSSIBLE { STOPPED_POSSIBLE } else { STOPPED_BEFORE_START }))
            .expect("unconditional update");
        prior == POSSIBLE || prior == STOPPED_POSSIBLE
    }
    pub(crate) fn stopped(&self) -> bool { self.state.load(Ordering::SeqCst) >= STOPPED_BEFORE_START }
}
pub(crate) trait Transport: Send + Sync {
    fn start(&self, exact: &Reservation, permit: &Permit) -> Result<(), String>;
    /// Must target only the retained exact operation. ACK is not receive proof.
    fn mute_and_release(&self, exact: &Reservation) -> Result<(), String>;
}
#[derive(Clone)]
pub(crate) struct ReleaseHandle { authority: Arc<Authority>, ticket: Reservation, permit: Arc<Permit>, transport: Arc<dyn Transport> }
impl ReleaseHandle {
    pub(crate) fn reservation(&self) -> &Reservation { &self.ticket }
    /// No acquisition mutex, journal lock, or ordinary transport queue. A stale
    /// handle affects neither current intent nor current transport.
    pub(crate) fn stop(&self) -> Result<bool, String> {
        let possible = self.permit.stop();
        if !self.authority.revoke_exact(&self.ticket)? { return Ok(false); }
        if possible { self.transport.mute_and_release(&self.ticket)?; }
        Ok(true)
    }
}

pub(crate) struct Owner<J: Journal> {
    authority: Arc<Authority>, journal: J, transport: Arc<dyn Transport>,
    records: BTreeMap<String, Record>, release: Option<ReleaseHandle>, faulted: bool,
}
impl<J: Journal> Owner<J> {
    pub(crate) fn open(authority: Arc<Authority>, mut journal: J, transport: Arc<dyn Transport>) -> Result<Self, String> {
        let mut records: BTreeMap<String, Record> = BTreeMap::new();
        for record in journal.read()? {
            record.body.validate()?;
            record.ticket.validate()?;
            if record.ticket.operation_id() != record.body.operation_id { return Err("Journal identity mismatch".into()); }
            if let Some(old) = records.get(&record.body.operation_id) {
                if old.body != record.body || old.ticket != record.ticket || !Self::transition(old, &record) {
                    return Err("Invalid journal transition; preserve it".into());
                }
            } else if record.phase != Phase::Prepared || record.consumed || records.values().any(|r| !r.phase.terminal()) {
                return Err("Journal missing preparation".into());
            }
            if record.consumed && records.values().any(|other| other.body.operation_id != record.body.operation_id
                && other.consumed && other.body.overlaps(&record.body)) {
                return Err("Journal contains overlapping consumed slots".into());
            }
            records.insert(record.body.operation_id.clone(), record);
            if records.len() > MAX_OPERATIONS { return Err("Journal operation capacity exceeded".into()); }
        }
        let pending: Vec<_> = records.values().filter(|r| !r.phase.terminal()).collect();
        if pending.len() > 1 { return Err("Journal has multiple unresolved owners".into()); }
        let release = if let Some(record) = pending.first() {
            authority.restore_unresolved(&record.ticket)?;
            let permit = Arc::new(Permit::new(record.consumed, authority.clone(), record.ticket.clone(), record.body.clone()));
            permit.stop(); // Restart never restores permission, including Prepared.
            Some(ReleaseHandle { authority: authority.clone(), ticket: record.ticket.clone(), permit, transport: transport.clone() })
        } else { None };
        Ok(Self { authority, journal, transport, records, release, faulted: false })
    }
    fn transition(old: &Record, new: &Record) -> bool {
        matches!((old.phase, new.phase, old.consumed, new.consumed),
            (Phase::Prepared, Phase::Starting, false, true) | (Phase::Prepared, Phase::Cancelled, false, false)
            | (Phase::Starting, Phase::Active, true, true) | (Phase::Starting, Phase::Completed, true, true)
            | (Phase::Active, Phase::Completed, true, true))
    }
    fn persist(&mut self, record: Record) -> Result<(), String> {
        if let Err(error) = self.journal.append(&record) {
            self.faulted = true;
            if let Some(handle) = &self.release { let _ = handle.stop(); }
            return Err(error);
        }
        self.records.insert(record.body.operation_id.clone(), record);
        Ok(())
    }
    pub(crate) fn prepare(&mut self, body: Body, observation: Observation<'_>) -> Result<Phase, String> {
        body.validate()?;
        if self.faulted { return Err("Journal outcome unresolved".into()); }
        if let Some(old) = self.records.get(&body.operation_id) {
            return if old.body == body { Ok(old.phase) } else { Err("Operation body conflict".into()) };
        }
        if self.records.len() >= MAX_OPERATIONS || self.release.is_some() { return Err("Owner admission occupied or full".into()); }
        if body.context != observation.context || body.clock_epoch != observation.clock_epoch || observation.now_us > body.latest_start_us {
            return Err("Preparation context or clock expired".into());
        }
        if self.records.values().any(|old| old.consumed && old.body.overlaps(&body)) { return Err("Slot already consumed".into()); }
        let ticket = self.authority.prepare(&body.operation_id, &self.authority.intent()?)?;
        self.release = Some(ReleaseHandle { authority: self.authority.clone(), ticket: ticket.clone(),
            permit: Arc::new(Permit::new(false, self.authority.clone(), ticket.clone(), body.clone())), transport: self.transport.clone() });
        self.persist(Record { body, ticket, phase: Phase::Prepared, consumed: false })?;
        Ok(Phase::Prepared)
    }
    pub(crate) fn release_handle(&self) -> Option<ReleaseHandle> { self.release.clone() }
    pub(crate) fn commit(&mut self, body: &Body, observation: Observation<'_>) -> Result<Phase, String> {
        if self.faulted { return Err("Journal outcome unresolved".into()); }
        let mut record = self.records.get(&body.operation_id).cloned().ok_or("Unknown operation")?;
        if record.body != *body { return Err("Operation body conflict".into()); }
        if record.phase != Phase::Prepared { return Ok(record.phase); }
        let handle = self.release.clone().ok_or("No retained release handle")?;
        if body.context != observation.context || body.clock_epoch != observation.clock_epoch
            || observation.now_us < body.slot_start_us || observation.now_us > body.latest_start_us
            || handle.permit.stopped() || self.authority.require_exact(&record.ticket).is_err() {
            handle.stop()?;
            record.phase = Phase::Cancelled;
            self.persist(record.clone())?;
            self.authority.retire_exact(&record.ticket)?;
            self.release = None;
            return Err("Commit retired: context, clock or intent changed".into());
        }
        // Persist possible output BEFORE dispatch. A crash after this line
        // recovers a release obligation, even if dispatch never happened.
        record.phase = Phase::Starting;
        record.consumed = true;
        self.persist(record.clone())?;
        self.authority.require_exact(&record.ticket)?;
        if handle.permit.stopped() { return Err("Start cancelled; retained release duty".into()); }
        self.transport.start(&record.ticket, &handle.permit)?;
        Ok(Phase::Starting) // An ACK cannot establish Active or completion.
    }
    /// Synthetic owner observation only. No radio-ID-only release API.
    pub(crate) fn observe(&mut self, exact: &Reservation, receiving: bool) -> Result<Phase, String> {
        if self.faulted { return Err("Journal outcome unresolved".into()); }
        let handle = self.release.clone().ok_or("No retained operation")?;
        if handle.ticket != *exact { return Err("Foreign operation observation".into()); }
        let mut record = self.records.get(exact.operation_id()).cloned().ok_or("Unknown operation")?;
        if receiving {
            handle.stop()?;
            record.phase = if record.consumed { Phase::Completed } else { Phase::Cancelled };
            self.persist(record.clone())?;
            self.authority.retire_exact(exact)?;
            self.release = None;
        } else {
            if record.phase != Phase::Starting || handle.permit.stopped() { return Err("Late or unexpected active observation".into()); }
            self.authority.require_exact(exact)?;
            record.phase = Phase::Active;
            self.persist(record.clone())?;
        }
        Ok(record.phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    #[derive(Default)]
    struct MemoryState { rows: Vec<Record>, writes: usize, fail_at: Option<usize>, fail_after_write: bool }
    #[derive(Clone, Default)]
    struct MemoryJournal(Arc<Mutex<MemoryState>>);
    impl Journal for MemoryJournal {
        fn read(&mut self) -> Result<Vec<Record>, String> { Ok(self.0.lock().unwrap().rows.clone()) }
        fn append(&mut self, record: &Record) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.writes += 1;
            let fail = state.fail_at == Some(state.writes);
            if !fail || state.fail_after_write { state.rows.push(record.clone()); }
            if fail { Err("Injected indeterminate journal failure".into()) } else { Ok(()) }
        }
    }
    #[derive(Default)]
    struct Inert {
        starts: Mutex<Vec<Reservation>>, releases: Mutex<Vec<Reservation>>,
        entered: Mutex<Option<mpsc::Sender<()>>>, proceed: Mutex<Option<mpsc::Receiver<()>>>,
        lose_start_reply: bool, fail_release: bool,
        callback_observation: Mutex<Option<(String, String, u64)>>,
    }
    impl Transport for Inert {
        fn start(&self, exact: &Reservation, permit: &Permit) -> Result<(), String> {
            if let Some(sender) = self.entered.lock().unwrap().take() { sender.send(()).unwrap(); }
            if let Some(receiver) = self.proceed.lock().unwrap().take() { receiver.recv_timeout(Duration::from_secs(3)).unwrap(); }
            let observed = self.callback_observation.lock().unwrap();
            let observed = if let Some((context, epoch, now)) = observed.as_ref() {
                Observation { context, clock_epoch: epoch, now_us: *now }
            } else { observation(&permit.body) };
            if !permit.begin_effect(observed) { return Err("Inert callback revoked".into()); }
            self.starts.lock().unwrap().push(exact.clone());
            if self.lose_start_reply { Err("Start reply lost".into()) } else { Ok(()) }
        }
        fn mute_and_release(&self, exact: &Reservation) -> Result<(), String> {
            self.releases.lock().unwrap().push(exact.clone());
            if self.fail_release { Err("Release outcome unknown".into()) } else { Ok(()) }
        }
    }
    fn body() -> Body {
        Body { operation_id: uuid::Uuid::new_v4().to_string(), manifest: "synthetic FT8 descriptor; no samples".into(),
            context: "server/workspace/worker/selection/route/policy/enable-generation=1".into(), clock_epoch: "clock1".into(),
            slot_start_us: 30_000_000, slot_duration_us: 15_000_000, latest_start_us: 30_100_000 }
    }
    fn observation(body: &Body) -> Observation<'_> {
        Observation { context: &body.context, clock_epoch: &body.clock_epoch, now_us: body.slot_start_us }
    }
    fn fixture() -> (Owner<MemoryJournal>, Arc<Authority>, MemoryJournal, Arc<Inert>) {
        let authority = Arc::new(Authority::default()); let journal = MemoryJournal::default(); let transport = Arc::new(Inert::default());
        (Owner::open(authority.clone(), journal.clone(), transport.clone()).unwrap(), authority, journal, transport)
    }
    #[test]
    fn one_shot_commit_requires_observation_and_rejects_changed_body() {
        let (mut owner, authority, journal, transport) = fixture(); let body = body();
        assert_eq!(owner.prepare(body.clone(), observation(&body)).unwrap(), Phase::Prepared);
        let handle = owner.release_handle().unwrap();
        assert_eq!(owner.commit(&body, observation(&body)).unwrap(), Phase::Starting);
        assert_eq!(owner.commit(&body, observation(&body)).unwrap(), Phase::Starting);
        let mut changed = body.clone(); changed.manifest.push('x');
        assert!(owner.commit(&changed, observation(&changed)).is_err());
        assert_eq!(transport.starts.lock().unwrap().len(), 1);
        assert_eq!(journal.0.lock().unwrap().rows[1].phase, Phase::Starting);
        assert_eq!(owner.observe(&handle.ticket, false).unwrap(), Phase::Active);
        assert_eq!(owner.observe(&handle.ticket, true).unwrap(), Phase::Completed);
        assert!(authority.reserve_ordinary().is_ok());
        assert_eq!(owner.commit(&body, observation(&body)).unwrap(), Phase::Completed);
        assert_eq!(transport.starts.lock().unwrap().len(), 1);
    }
    #[test]
    fn stop_before_commit_never_sends_release_to_a_foreign_transmitter() {
        let (mut owner, authority, _, transport) = fixture(); let body = body();
        owner.prepare(body.clone(), observation(&body)).unwrap();
        assert!(owner.release_handle().unwrap().stop().unwrap());
        assert!(owner.commit(&body, observation(&body)).is_err());
        assert!(transport.starts.lock().unwrap().is_empty());
        assert!(transport.releases.lock().unwrap().is_empty());
        assert!(authority.reserve_ordinary().is_ok());
    }
    #[test]
    fn blocked_start_does_not_block_stop_and_late_callback_cannot_start() {
        let authority = Arc::new(Authority::default()); let journal = MemoryJournal::default();
        let (entered, wait_entered) = mpsc::channel(); let (proceed, wait_proceed) = mpsc::channel();
        let transport = Arc::new(Inert { entered: Mutex::new(Some(entered)), proceed: Mutex::new(Some(wait_proceed)), ..Inert::default() });
        let mut owner = Owner::open(authority.clone(), journal, transport.clone()).unwrap(); let body = body();
        owner.prepare(body.clone(), observation(&body)).unwrap(); let handle = owner.release_handle().unwrap();
        let running = std::thread::spawn(move || { assert!(owner.commit(&body, observation(&body)).is_err()); owner });
        wait_entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(handle.stop().unwrap()); // Returns while the ordinary path remains blocked.
        assert!(authority.reserve_ordinary().is_err());
        proceed.send(()).unwrap(); let mut owner = running.join().unwrap();
        assert!(transport.starts.lock().unwrap().is_empty());
        assert!(transport.releases.lock().unwrap().is_empty());
        assert_eq!(owner.observe(&handle.ticket, true).unwrap(), Phase::Completed);
        assert!(authority.reserve_ordinary().is_ok());
    }
    #[test]
    fn global_stop_rejects_late_inert_callback() {
        let authority = Arc::new(Authority::default()); let journal = MemoryJournal::default();
        let (entered, wait_entered) = mpsc::channel(); let (proceed, wait_proceed) = mpsc::channel();
        let transport = Arc::new(Inert { entered: Mutex::new(Some(entered)), proceed: Mutex::new(Some(wait_proceed)), ..Inert::default() });
        let mut owner = Owner::open(authority.clone(), journal, transport.clone()).unwrap(); let body = body();
        owner.prepare(body.clone(), observation(&body)).unwrap();
        let running = std::thread::spawn(move || owner.commit(&body, observation(&body)));
        wait_entered.recv_timeout(Duration::from_secs(2)).unwrap(); authority.revoke().unwrap(); proceed.send(()).unwrap();
        assert!(running.join().unwrap().is_err()); assert!(transport.starts.lock().unwrap().is_empty());
        assert!(authority.reserve_ordinary().is_err());
    }
    #[test]
    fn consumed_slots_survive_restart_and_cannot_be_bypassed_by_mode_or_epoch() {
        let (mut owner, _, journal, transport) = fixture(); let body = body();
        owner.prepare(body.clone(), observation(&body)).unwrap(); let handle = owner.release_handle().unwrap();
        owner.commit(&body, observation(&body)).unwrap(); owner.observe(&handle.ticket, true).unwrap(); drop(owner);
        let mut recovered = Owner::open(Arc::new(Authority::default()), journal, transport.clone()).unwrap();
        assert_eq!(recovered.commit(&body, observation(&body)).unwrap(), Phase::Completed);
        let mut same_slot = body.clone(); same_slot.operation_id = uuid::Uuid::new_v4().to_string();
        same_slot.slot_duration_us = 7_500_000; same_slot.clock_epoch = "new clock".into(); same_slot.context = "new context".into();
        assert!(recovered.prepare(same_slot.clone(), observation(&same_slot)).is_err());
        assert_eq!(transport.starts.lock().unwrap().len(), 1);
    }
    #[test]
    fn restart_retains_exact_release_duty_without_rearming() {
        for consumed in [false, true] {
            let (mut owner, _, journal, transport) = fixture(); let body = body();
            owner.prepare(body.clone(), observation(&body)).unwrap();
            if consumed { owner.commit(&body, observation(&body)).unwrap(); }
            drop(owner); let authority = Arc::new(Authority::default());
            let mut recovered = Owner::open(authority.clone(), journal, transport.clone()).unwrap(); let handle = recovered.release_handle().unwrap();
            assert!(authority.reserve_ordinary().is_err()); assert!(authority.require_exact(&handle.ticket).is_err());
            assert!(!handle.permit.begin_effect(observation(&body))); assert!(handle.stop().unwrap());
            let prior_starts = transport.starts.lock().unwrap().len();
            if consumed { assert_eq!(recovered.commit(&body, observation(&body)).unwrap(), Phase::Starting); }
            else { assert!(recovered.commit(&body, observation(&body)).is_err()); }
            assert_eq!(transport.starts.lock().unwrap().len(), prior_starts);
            if consumed { recovered.observe(&handle.ticket, true).unwrap(); }
            assert!(authority.reserve_ordinary().is_ok());
        }
    }
    #[test]
    fn journal_failures_before_and_after_each_write_never_report_success() {
        for boundary in 1..=4 { for after in [false, true] {
            let (mut owner, authority, journal, transport) = fixture(); let body = body();
            { let mut state = journal.0.lock().unwrap(); state.fail_at = Some(boundary); state.fail_after_write = after; }
            let prepared = owner.prepare(body.clone(), observation(&body));
            let handle = owner.release_handle().unwrap();
            let result = match boundary {
                1 => prepared,
                2 => owner.commit(&body, observation(&body)),
                3 => { owner.commit(&body, observation(&body)).unwrap(); owner.observe(&handle.ticket, false) },
                4 => { owner.commit(&body, observation(&body)).unwrap(); owner.observe(&handle.ticket, false).unwrap(); owner.observe(&handle.ticket, true) },
                _ => unreachable!(),
            };
            assert!(result.is_err()); assert!(owner.commit(&body, observation(&body)).is_err());
            assert!(authority.reserve_ordinary().is_err());
            if boundary <= 2 { assert!(transport.starts.lock().unwrap().is_empty()); }
            // An indeterminate live owner is never silently cleared. Recovery
            // is a new process/authority, using whatever bytes actually survived.
            drop(owner); journal.0.lock().unwrap().fail_at = None;
            let fresh = Arc::new(Authority::default()); let recovered = Owner::open(fresh.clone(), journal, transport).unwrap();
            if recovered.release_handle().is_some() { assert!(fresh.reserve_ordinary().is_err()); }
        }}
    }
    #[test]
    fn changed_context_clock_early_late_and_revoked_intent_cancel_before_dispatch() {
        for change in 0..5 {
            let (mut owner, authority, _, transport) = fixture(); let body = body(); owner.prepare(body.clone(), observation(&body)).unwrap();
            let mut observed = observation(&body);
            match change { 0 => observed.context = "different selection", 1 => observed.clock_epoch = "jump",
                2 => observed.now_us -= 1, 3 => observed.now_us = body.latest_start_us + 1, _ => authority.revoke().unwrap() }
            assert!(owner.commit(&body, observed).is_err()); assert!(transport.starts.lock().unwrap().is_empty());
            assert!(authority.reserve_ordinary().is_ok());
        }
    }
    #[test]
    fn callback_rechecks_context_and_clock_after_commit_was_consumed() {
        for change in 0..4 {
            let (mut owner, authority, _, transport) = fixture(); let body = body();
            owner.prepare(body.clone(), observation(&body)).unwrap();
            let mut context = body.context.clone(); let mut epoch = body.clock_epoch.clone(); let mut now = body.slot_start_us;
            match change { 0 => context.push('2'), 1 => epoch.push('2'), 2 => now -= 1, _ => now = body.latest_start_us + 1 }
            *transport.callback_observation.lock().unwrap() = Some((context, epoch, now));
            assert!(owner.commit(&body, observation(&body)).is_err());
            assert!(transport.starts.lock().unwrap().is_empty());
            assert!(authority.reserve_ordinary().is_err());
        }
    }
    #[test]
    fn lost_start_reply_and_unknown_release_keep_admission_until_exact_receive() {
        let authority = Arc::new(Authority::default()); let transport = Arc::new(Inert { lose_start_reply: true, fail_release: true, ..Inert::default() });
        let journal = MemoryJournal::default(); let mut owner = Owner::open(authority.clone(), journal, transport.clone()).unwrap(); let body = body();
        owner.prepare(body.clone(), observation(&body)).unwrap(); let handle = owner.release_handle().unwrap();
        assert!(owner.commit(&body, observation(&body)).is_err()); assert!(handle.stop().is_err());
        assert!(authority.reserve_ordinary().is_err()); assert_eq!(transport.starts.lock().unwrap().len(), 1);
        let foreign = Authority::default(); let ticket = foreign.prepare(&uuid::Uuid::new_v4().to_string(), &foreign.intent().unwrap()).unwrap();
        assert!(owner.observe(&ticket, true).is_err());
        // A release command failure cannot be papered over by this API: exact
        // receive reconciliation is conservative until release lane succeeds.
        assert!(owner.observe(&handle.ticket, true).is_err()); assert!(authority.reserve_ordinary().is_err());
    }
    #[test]
    fn stale_handle_cannot_release_replacement_or_ordinary_owner() {
        let (mut owner, authority, _, transport) = fixture(); let body = body(); owner.prepare(body.clone(), observation(&body)).unwrap();
        let old = owner.release_handle().unwrap(); owner.commit(&body, observation(&body)).unwrap(); owner.observe(&old.ticket, true).unwrap();
        let releases = transport.releases.lock().unwrap().len(); let ordinary = authority.intent().unwrap();
        assert!(!old.stop().unwrap()); authority.require_intent(&ordinary).unwrap();
        let mut next = body.clone(); next.operation_id = uuid::Uuid::new_v4().to_string(); next.slot_start_us += 15_000_000; next.latest_start_us += 15_000_000;
        owner.prepare(next.clone(), observation(&next)).unwrap(); assert!(!old.stop().unwrap());
        assert_eq!(transport.releases.lock().unwrap().len(), releases);
        owner.commit(&next, observation(&next)).unwrap();
    }
    #[test]
    fn full_journal_refuses_new_identity_without_eviction() {
        let (mut owner, _, journal, _) = fixture();
        for _ in 0..MAX_OPERATIONS {
            let body = body(); owner.prepare(body.clone(), observation(&body)).unwrap();
            let handle = owner.release_handle().unwrap(); handle.stop().unwrap(); assert!(owner.commit(&body, observation(&body)).is_err());
        }
        let body = body(); assert!(owner.prepare(body.clone(), observation(&body)).is_err());
        assert_eq!(owner.records.len(), MAX_OPERATIONS); assert_eq!(journal.0.lock().unwrap().rows.len(), MAX_OPERATIONS * 2);
    }
    #[test]
    fn file_journal_round_trip_preserves_partial_corruption() {
        let path = std::env::temp_dir().join(format!("xlog-offline-owner-{}.jsonl", uuid::Uuid::new_v4()));
        let authority = Arc::new(Authority::default()); let transport = Arc::new(Inert::default()); let body = body();
        let mut owner = Owner::open(authority, FileJournal::open(&path).unwrap(), transport.clone()).unwrap();
        owner.prepare(body.clone(), observation(&body)).unwrap(); owner.commit(&body, observation(&body)).unwrap(); drop(owner);
        let recovered = Owner::open(Arc::new(Authority::default()), FileJournal::open(&path).unwrap(), transport.clone()).unwrap();
        assert!(recovered.release_handle().is_some()); drop(recovered);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap(); file.write_all(b"{partial").unwrap(); file.sync_all().unwrap(); drop(file);
        let before = std::fs::read(&path).unwrap();
        assert!(Owner::open(Arc::new(Authority::default()), FileJournal::open(&path).unwrap(), transport).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before); std::fs::remove_file(path).unwrap();
    }
}
