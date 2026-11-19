//! Durable GUI PTT ownership. Review retires uncertainty, never a release duty.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::Manager;
/// PTT route evidence is provider-specific. TCI reports addressed TRX state,
/// without proving a Flex-style TX VFO or per-TRX TX_FREQUENCY.
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub(crate) provider:String,
    pub(crate) selection_generation:u64,
    pub(crate) selection_id:String,
    pub(crate) worker_generation:u64,
    pub(crate) connection_generation:u64,
    pub(crate) control_revision:u64,
    pub(crate) observation_revision:u64,
    pub(crate) received_at_ms:u64,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub(crate) radio_id:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub(crate) tci:Option<TCITarget>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TCITarget {trx:u8,channel:u8}
impl Target {
    pub(crate) fn validate(&self)->Result<(),String>{
        if !matches!(self.provider.as_str(),"direct_agent"|"direct_flex_lan"|"direct_tci") || !uuid(&self.selection_id)
            || self.worker_generation==0 || [self.selection_generation,self.worker_generation,self.connection_generation,self.control_revision,self.observation_revision,self.received_at_ms].iter().any(|v|*v>9_007_199_254_740_991)
            || self.radio_id.as_deref().is_some_and(|v|!matches!(v,"radio1"|"radio2"))
            || if self.provider=="direct_tci" {self.radio_id.is_none() || self.tci.as_ref().is_none_or(|v|v.trx>=16||v.channel>=16)}else{self.tci.is_some()} {
            return Err("Invalid provider-specific PTT target".into());
        }Ok(())
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    operation_id: String,
    enabled: bool,
    owner_id: String,
    target: Target,
    status: String,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Obligation {
    owner_id: String,
    target: Target,
    #[serde(default)]
    worker_epoch: Option<String>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    receipts: Vec<Receipt>,
    release_required: Option<Obligation>,
    #[serde(default)]
    recoveries: Vec<Recovery>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovery {
    operation_id: String,
    enabled: bool,
    #[serde(default)]
    worker_epoch: Option<String>,
    owner_id: String,
    retired_target: Target,
    target: Target,
    status: String,
}
pub struct Session {
    path: PathBuf,
    state: Mutex<Saved>,
    on: Mutex<()>,
    off: Mutex<()>,
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil())
}
fn same_session(a: &Target, b: &Target) -> bool {
    a.provider == b.provider
        && a.selection_generation == b.selection_generation
        && a.selection_id == b.selection_id
        && a.worker_generation == b.worker_generation
        && a.connection_generation == b.connection_generation
        && a.radio_id==b.radio_id && a.tci==b.tci
}
fn unresolved(r: &Receipt) -> bool {
    matches!(r.status.as_str(), "pending" | "unknown")
}
fn retire_release(saved: &mut Saved, owner: &str) {
    saved.release_required = None;
    // RX proof retires an acknowledged acquisition, without inventing TX-on
    // confirmation. Unknown acquisition history still requires explicit review.
    for r in &mut saved.receipts {
        if r.owner_id == owner && r.enabled && r.status == "acknowledged" {
            r.status = "released".into();
        }
    }
}
pub fn validate(label: &str, isolated: bool, exiting: bool, action: &str) -> Result<(), String> {
    if isolated
        || exiting
        || label != "main"
        || !matches!(
            action,
            "snapshot" | "prepare" | "on" | "off" | "receipt" | "review" | "recover"
        )
    {
        return Err("GUI PTT is available only in the main station controls".into());
    }
    Ok(())
}
impl Session {
    fn new(path: PathBuf) -> Result<Self, String> {
        let mut saved = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Saved>(&b)
                .map_err(|_| "Invalid PTT journal; it was not overwritten")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved {
                version: 1,
                receipts: vec![],
                release_required: None,
                recoveries: vec![],
            },
            Err(_) => return Err("PTT journal unavailable".into()),
        };
        if saved.version != 1 {
            return Err("Unsupported PTT journal".into());
        }
        let mut ids = std::collections::HashSet::new();
        for r in &mut saved.receipts {
            r.target.validate()?;
            if !uuid(&r.operation_id)
                || !uuid(&r.owner_id)
                || !ids.insert(r.operation_id.clone())
                || !matches!(
                    r.status.as_str(),
                    "pending" | "unknown" | "acknowledged" | "confirmed" | "released" | "reviewed"
                )
            {
                return Err("Invalid PTT receipt".into());
            }
            if r.status == "pending" {
                r.status = "unknown".into();
            }
        }
        if let Some(o) = &saved.release_required {
            o.target.validate()?;
            if !uuid(&o.owner_id)
                || o.worker_epoch.as_ref().is_some_and(|epoch| !uuid(epoch))
                || !saved.receipts.iter().any(|r| {
                    r.enabled && r.owner_id == o.owner_id && same_session(&r.target, &o.target)
                })
            {
                return Err("Invalid PTT release obligation".into());
            }
        }
        for r in &saved.recoveries {
            r.target.validate()?;
            r.retired_target.validate()?;
            if !uuid(&r.operation_id)
                || !uuid(&r.owner_id)
                || r.enabled
                || r.status != "recovered"
                || !ids.insert(r.operation_id.clone())
            {
                return Err("Invalid PTT recovery decision".into());
            }
        }
        Ok(Self {
            path,
            state: Mutex::new(saved),
            on: Mutex::new(()),
            off: Mutex::new(()),
        })
    }
    fn persist(&self, s: &Saved) -> Result<(), String> {
        let parent = self.path.parent().ok_or("PTT storage unavailable")?;
        std::fs::create_dir_all(parent).map_err(|_| "PTT storage unavailable")?;
        let mut f =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| "PTT receipt save failed")?;
        f.write_all(&serde_json::to_vec(s).map_err(|_| "PTT serialization failed")?)
            .map_err(|_| "PTT receipt save failed")?;
        f.as_file()
            .sync_all()
            .map_err(|_| "PTT receipt save failed")?;
        f.persist(&self.path)
            .map_err(|_| "PTT receipt save failed")?;
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Value, String> {
        let s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        Ok(
            json!({"receipts":s.receipts.iter().filter(|r|unresolved(r)||r.status=="acknowledged").collect::<Vec<_>>(),"release_required":s.release_required}),
        )
    }
    pub fn require_idle(&self) -> Result<(), String> {
        let s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        if s.release_required.is_some() || s.receipts.iter().any(unresolved) {
            return Err(
                "Resolve owned GUI PTT with PTT off and newer RX proof before another acquisition"
                    .into(),
            );
        }
        Ok(())
    }
    pub fn retained_target(&self)->Result<Option<Target>,String>{
        Ok(self.state.lock().map_err(|_|"PTT journal unavailable")?.release_required.as_ref().map(|o|o.target.clone()))
    }
    pub fn receipt(&self, id: &str) -> Result<Value, String> {
        let s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        if let Some(r) = s.recoveries.iter().find(|r| r.operation_id == id) {
            return Ok(json!(r));
        }
        Ok(s.receipts
            .iter()
            .find(|r| r.operation_id == id)
            .map_or(Value::Null, |r| json!(r)))
    }
    pub fn review(&self, id: &str) -> Result<Value, String> {
        let mut s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        let mut next = s.clone();
        let r = next
            .receipts
            .iter_mut()
            .find(|r| r.operation_id == id)
            .ok_or("PTT receipt missing")?;
        if r.status != "unknown" && r.status != "reviewed" {
            return Err("Only an unknown PTT receipt may be reviewed".into());
        }
        r.status = "reviewed".into();
        let v = json!(r);
        self.persist(&next)?;
        *s = next;
        Ok(v)
    }
    pub fn recover(
        &self,
        id: &str,
        owner_id: &str,
        target: Target,
        prove: impl FnOnce(&Target, &Target) -> Result<(), String>,
    ) -> Result<Value, String> {
        target.validate()?;
        if !uuid(id) || !uuid(owner_id) {
            return Err("Invalid PTT recovery identity".into());
        }
        let _output = crate::carrier_tune::reserve_output()?;
        let _on = self
            .on
            .try_lock()
            .map_err(|_| "PTT acquisition still pending")?;
        let _off = self
            .off
            .try_lock()
            .map_err(|_| "PTT release still pending")?;
        let mut s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        if let Some(r) = s.recoveries.iter().find(|r| r.operation_id == id) {
            if r.owner_id != owner_id || r.target != target {
                return Err("PTT recovery identity collision".into());
            }
            return Ok(json!(r));
        }
        if s.receipts.iter().any(|r| r.operation_id == id) {
            return Err("PTT recovery identity collision".into());
        }
        let duty = s
            .release_required
            .as_ref()
            .ok_or("No retired PTT duty to recover")?;
        if duty.owner_id != owner_id {
            return Err("PTT owner changed before recovery".into());
        }
        if duty.worker_epoch.as_deref() != Some(crate::worker::process_epoch()) {
            return Err("Original PTT worker process epoch is unavailable or retired in a previous app; recovery denied".into());
        }
        crate::carrier_tune::revoke_output_intent()?;
        prove(&duty.target, &target)?;
        let decision = Recovery {
            operation_id: id.into(),
            enabled: false,
            worker_epoch: duty.worker_epoch.clone(),
            owner_id: owner_id.into(),
            retired_target: duty.target.clone(),
            target,
            status: "recovered".into(),
        };
        let mut next = s.clone();
        retire_release(&mut next, owner_id);
        for r in &mut next.receipts {
            if r.owner_id == owner_id && unresolved(r) {
                r.status = "reviewed".into();
            }
        }
        next.recoveries.push(decision.clone());
        self.persist(&next)?;
        *s = next;
        Ok(json!(decision))
    }
    /// Retire a release duty left by a PREVIOUS app run, which the ordinary
    /// paths can never resolve: `reconcile`, Off and `recover` all require the
    /// original worker process epoch, and that process ended with its app.
    /// Before this, one uncertain release followed by a restart disabled TX
    /// for good. The only proof accepted is the radio itself: fresh telemetry
    /// (at most 1 s old, not from the future) on the same provider and selected
    /// station shows it connected and not transmitting, and the current worker
    /// owns no GUI PTT or carrier tune. Nothing is sent to the radio. A duty of
    /// the current worker epoch is never touched here.
    pub(crate) fn retire_prior_app_duty(&self, owner_id: &str, radio: &Value, now_ms: u64) -> Result<bool, String> {
        let mut s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        let Some(duty) = s.release_required.clone() else { return Ok(false) };
        if duty.owner_id != owner_id || duty.worker_epoch.as_deref() == Some(crate::worker::process_epoch()) {
            return Ok(false);
        }
        let t = &duty.target;
        let fresh = radio["received_at_ms"].as_u64().is_some_and(|at| at <= now_ms && now_ms - at <= 1000);
        let snapshot = &radio["snapshot"];
        if !fresh
            || radio["available"] != true
            || radio["provider"] != t.provider
            || radio["selection_id"] != t.selection_id
            || snapshot["source"] != t.provider
            || snapshot["radio"]["connected"] != true
            || snapshot["radio"]["transmitting"] != false
            || snapshot["ptt"]["release_required"] != false
            || snapshot["carrier_tune"]["release_required"] == true
        {
            return Ok(false);
        }
        let mut next = s.clone();
        retire_release(&mut next, owner_id);
        for r in &mut next.receipts {
            if r.owner_id == owner_id && unresolved(r) {
                r.status = "reviewed".into();
            }
        }
        next.recoveries.push(Recovery {
            operation_id: uuid::Uuid::new_v4().to_string(),
            enabled: false,
            worker_epoch: duty.worker_epoch.clone(),
            owner_id: owner_id.into(),
            retired_target: duty.target.clone(),
            target: duty.target.clone(),
            status: "recovered".into(),
        });
        self.persist(&next)?;
        *s = next;
        Ok(true)
    }
    /// Whether this owner's PTT obligation is already over: no retained duty names it,
    /// and a confirmed/released OFF or a recorded recovery decision does. Used to close a
    /// companion TX record whose release was settled here but could not be journaled.
    pub(crate) fn owner_released(&self, owner_id: &str) -> Result<bool, String> {
        let s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        Ok(s.release_required.as_ref().map_or(true, |d| d.owner_id != owner_id)
            && (s.recoveries.iter().any(|r| r.owner_id == owner_id && r.status == "recovered")
                || s.receipts.iter().any(|r| {
                    r.owner_id == owner_id && !r.enabled && matches!(r.status.as_str(), "confirmed" | "released")
                })))
    }
    pub fn reconcile(&self, radio: &Value) -> Result<(), String> {
        let mut s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
        let Some(owner) = s.release_required.clone() else {
            return Ok(());
        };
        let t = &owner.target;
        if owner.worker_epoch.as_deref() != Some(crate::worker::process_epoch())
            || radio["available"] != true
            || radio["provider"] != t.provider
            || radio["selection_generation"] != t.selection_generation
            || radio["selection_id"] != t.selection_id
            || radio["worker_generation"] != t.worker_generation
            || radio["snapshot"]["generation"] != t.connection_generation
            || radio["snapshot"]["source"] != t.provider
        {
            return Ok(());
        }
        let proof = &radio["snapshot"]["ptt"];
        if proof["owner_id"] != owner.owner_id
            || proof["source"] != "gui"
            || proof["confirmed"] != true
        {
            return Ok(());
        }
        let Some(on) = proof["transmitting"].as_bool() else {
            return Ok(());
        };
        if proof["pending"] != false || proof["release_required"] != on {
            return Ok(());
        }
        let mut next = s.clone();
        for r in &mut next.receipts {
            if r.owner_id == owner.owner_id
                && r.enabled == on
                && matches!(r.status.as_str(), "unknown" | "acknowledged" | "reviewed")
            {
                r.status = "confirmed".into();
            }
        }
        if !on
            && !next
                .receipts
                .iter()
                .any(|r| r.enabled && r.owner_id == owner.owner_id && r.status == "pending")
            && next
                .receipts
                .iter()
                .any(|r| !r.enabled && r.owner_id == owner.owner_id && r.status == "confirmed")
        {
            retire_release(&mut next, &owner.owner_id);
        }
        if next == *s {
            return Ok(());
        }
        self.persist(&next)?;
        *s = next;
        Ok(())
    }
    #[cfg(test)]
    fn apply(
        &self,
        id: &str,
        on: bool,
        target: Target,
        intent: &str,
        dispatch: impl FnOnce(Value, &Target) -> Result<Value, String>,
    ) -> Result<Value, String> {
        self.apply_admitted(id, on, target, intent, |_| Ok(()), dispatch)
    }
    pub fn apply_admitted(
        &self,
        id: &str,
        on: bool,
        target: Target,
        intent: &str,
        admit: impl FnOnce(&Target) -> Result<(), String>,
        dispatch: impl FnOnce(Value, &Target) -> Result<Value, String>,
    ) -> Result<Value, String> {
        self.apply_inner(id, on, target, intent, None, None, false, admit, dispatch)
    }
    /// Digital output participates in the SAME durable PTT owner and shared
    /// acquisition gate. It cannot use an ordinary acquisition to bypass an
    /// existing digital reservation, or release another retained PTT owner.
    pub(crate) fn apply_exact_admitted(
        &self,
        id: &str,
        on: bool,
        target: Target,
        exact: &crate::output_authority::Reservation,
        admit: impl FnOnce(&Target) -> Result<(), String>,
        dispatch: impl FnOnce(Value, &Target) -> Result<Value, String>,
    ) -> Result<Value, String> {
        exact.validate()?;
        if on && id != exact.operation_id() { return Err("Digital PTT acquisition identity mismatch".into()); }
        self.apply_inner(id, on, target, exact.intent_id(), Some(exact), None, false, admit, dispatch)
    }
    pub(crate) fn apply_exact_leased(&self,id:&str,target:Target,exact:&crate::output_authority::Reservation,
        until_unix_ms:u64,transmit_slice:&str,admit:impl FnOnce(&Target)->Result<(),String>,dispatch:impl FnOnce(Value,&Target)->Result<Value,String>)->Result<Value,String>{
        exact.validate()?;if id!=exact.operation_id()||target.provider!="direct_flex_lan"{return Err("Digital leased PTT route mismatch".into());}
        let lease=digital_lease(exact,until_unix_ms,transmit_slice)?;
        self.apply_inner(id,true,target,exact.intent_id(),Some(exact),Some(lease),false,admit,dispatch)
    }
    /// Normal completion still uses the same durable exact OFF path. The
    /// caller has closed its finished output; continuation is granted only by
    /// Authority::complete_exact after this returns observed release.
    pub(crate) fn release_completed_exact(&self,id:&str,target:Target,exact:&crate::output_authority::Reservation,
        admit:impl FnOnce(&Target)->Result<(),String>,dispatch:impl FnOnce(Value,&Target)->Result<Value,String>)->Result<Value,String>{
        crate::output_authority::shared().require_exact(exact)?;
        self.apply_inner(id,false,target,exact.intent_id(),Some(exact),None,true,admit,dispatch)
    }
    pub(crate) fn renew_exact(&self,target:&Target,exact:&crate::output_authority::Reservation,until_unix_ms:u64,transmit_slice:&str,
        dispatch:impl FnOnce(Value,&Target)->Result<Value,String>)->Result<(),String>{
        exact.validate()?;target.validate()?;crate::output_authority::shared().require_exact(exact)?;
        let lease=digital_lease(exact,until_unix_ms,transmit_slice)?;
        {
            let s=self.state.lock().map_err(|_|"PTT lease owner unavailable")?;
            let held=s.release_required.as_ref().ok_or("PTT lease has no retained owner")?;
            if held.owner_id!=exact.operation_id()||held.worker_epoch.as_deref()!=Some(crate::worker::process_epoch())||!same_session(&held.target,target)
                ||!s.receipts.iter().any(|r|r.operation_id==exact.operation_id()&&r.enabled&&r.status=="confirmed") {return Err("PTT lease cannot renew a different or unconfirmed acquisition".into());}
        }
        let request=json!({"id":uuid::Uuid::new_v4().to_string(),"method":"station_radio_control","params":{"provider":"direct_flex_lan","control":"ptt_lease","source":"gui","ptt_owner_id":exact.operation_id(),"expected_generation":target.connection_generation,"radio_id":target.radio_id,"digital_lease":lease}});
        let reply=dispatch(request,target)?;
        if reply["ok"]!=true||reply["result"]["lease_renewed"]!=true||reply["result"]["owner_id"]!=exact.operation_id()
            ||reply["result"]["lease_id"]!=exact.lease_id()||reply["result"]["expires_at_unix_ms"].as_u64()!=Some(until_unix_ms){return Err("PTT lease renewal was not confirmed".into());}Ok(())
    }
    fn apply_inner(
        &self,
        id: &str,
        on: bool,
        target: Target,
        intent: &str,
        exact: Option<&crate::output_authority::Reservation>,
        digital_lease: Option<Value>,
        completed_output: bool,
        admit: impl FnOnce(&Target) -> Result<(), String>,
        dispatch: impl FnOnce(Value, &Target) -> Result<Value, String>,
    ) -> Result<Value, String> {
        target.validate()?;
        if !uuid(id) {
            return Err("Invalid PTT identity".into());
        }
        let duplicate = |s: &Saved| -> Result<Option<Value>, String> {
            if s.recoveries.iter().any(|r| r.operation_id == id) {
                return Err("PTT identity collision".into());
            }
            if let Some(r) = s.receipts.iter().find(|r| r.operation_id == id) {
                if r.enabled != on || r.target != target {
                    return Err("PTT identity collision".into());
                }
                return Ok(Some(json!(r)));
            }
            Ok(None)
        };
        {
            let s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
            if let Some(r) = duplicate(&s)? {
                return Ok(r);
            }
        }
        if !on {
            if let Some(exact) = exact {
                if !completed_output && !crate::output_authority::shared().revoke_exact(exact)? {
                    return Err("The exact digital output reservation is no longer retained".into());
                }
            } else {
                crate::carrier_tune::revoke_output_intent()?;
            }
        }
        let _output = if on {
            Some(if let Some(exact) = exact {
                crate::output_authority::shared().reserve_exact(exact)?
            } else { crate::carrier_tune::reserve_output()? })
        } else {
            None
        };
        let _lane = if on {
            self.on.try_lock()
        } else {
            self.off.try_lock()
        }
        .map_err(|_| "This PTT lane is busy; release remains independent of acquisition")?;
        // The shared output guard already excludes CW/Tune preparation. Reject
        // known stale/not-ready acquisition before creating a release duty.
        admit(&target)?;
        let owner_id;
        {
            let mut s = self.state.lock().map_err(|_| "PTT journal unavailable")?;
            if let Some(r) = duplicate(&s)? {
                return Ok(r);
            }
            if on {
                crate::carrier_tune::require_output_intent(intent)?;
                if s.release_required.is_some() || s.receipts.iter().any(unresolved) {
                    return Err("PTT release or receipt review remains required".into());
                }
                owner_id = id.to_owned();
            } else {
                let o = s
                    .release_required
                    .as_ref()
                    .ok_or("No retained GUI PTT owner may be released")?;
                if exact.is_some_and(|ticket| ticket.operation_id() != o.owner_id) {
                    return Err("Digital Stop cannot release a different PTT owner".into());
                }
                if o.worker_epoch.as_deref() != Some(crate::worker::process_epoch())
                    || !same_session(&o.target, &target)
                {
                    return Err(
                        "PTT owner session changed; no unrelated TX will be released".into(),
                    );
                }
                owner_id = o.owner_id.clone();
            }
            let mut next = s.clone();
            if on {
                next.release_required = Some(Obligation {
                    owner_id: owner_id.clone(),
                    target: target.clone(),
                    worker_epoch: Some(crate::worker::process_epoch().to_owned()),
                });
            }
            next.receipts.push(Receipt {
                operation_id: id.into(),
                enabled: on,
                owner_id: owner_id.clone(),
                target: target.clone(),
                status: "pending".into(),
            });
            self.persist(&next)?;
            *s = next;
        }
        let mut request=json!({"id":id,"method":"station_radio_control","params":{"provider":target.provider,"control":"ptt","enabled":on,"source":"gui","ptt_owner_id":owner_id,"expected_generation":target.connection_generation}});
        if let Some(lease)=digital_lease{request["params"]["digital_lease"]=lease;}
        if let Some(radio_id)=&target.radio_id{request["params"]["radio_id"]=json!(radio_id);}
        let result = dispatch(
            request,
            &target,
        );
        let mut s = self.state.lock().map_err(|_| "PTT outcome unknown")?;
        let mut next = s.clone();
        let r = next
            .receipts
            .iter_mut()
            .find(|r| r.operation_id == id)
            .ok_or("PTT receipt missing")?;
        r.status = outcome(id, on, &owner_id, &target, &result).into();
        let v = json!(r);
        if !on
            && r.status == "confirmed"
            && !next
                .receipts
                .iter()
                .any(|r| r.enabled && r.owner_id == owner_id && r.status == "pending")
        {
            retire_release(&mut next, &owner_id);
        }
        if self.persist(&next).is_err() {
            next.receipts
                .iter_mut()
                .find(|r| r.operation_id == id)
                .unwrap()
                .status = "unknown".into();
            next.release_required = s.release_required.clone();
            *s = next;
            return Err("PTT outcome could not be saved; inspect its receipt; no replay".into());
        }
        *s = next;
        Ok(v)
    }
}
fn outcome(
    id: &str,
    on: bool,
    owner: &str,
    t: &Target,
    response: &Result<Value, String>,
) -> &'static str {
    let Ok(v) = response else { return "unknown" };
    let r = &v["result"];
    let s = &r["state"];
    if v["id"] != id
        || v["ok"] != true
        || r["accepted"] != true
        || r["control"] != "ptt"
        || r["enabled"] != on
        || r["owner_id"] != owner
        || s["source"] != t.provider
        || s["generation"] != t.connection_generation
    {
        return "unknown";
    }
    if matches!(t.provider.as_str(),"direct_flex_lan"|"direct_tci")
        && r["confirmed"] == true
        && s["ptt"]["source"] == "gui"
        && s["ptt"]["owner_id"] == owner
        && s["ptt"]["confirmed"] == true
        && s["ptt"]["transmitting"] == on
        && s["ptt"]["release_required"] == on
        && s["ptt"]["pending"] == false
        && s["radio"]["observed"] == true
        && s["radio"]["connected"] == true
        && s["radio"]["transmitting"] == on
    {
        "confirmed"
    } else if t.provider == "direct_agent" && r["confirmed"] == false {
        "acknowledged"
    } else {
        "unknown"
    }
}
fn digital_lease(exact:&crate::output_authority::Reservation,until:u64,transmit_slice:&str)->Result<Value,String>{
    let now=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|"PTT lease clock unavailable")?.as_millis() as u64;
    if !matches!(transmit_slice,"A"|"B")||until<=now||until>now+1000{return Err("PTT lease deadline must retain its original bounded lifetime".into());}
    Ok(json!({"lease_id":exact.lease_id(),"expires_at_unix_ms":until,"transmit_slice":transmit_slice}))
}
fn admission(
    selection: &Value,
    radio: &Value,
    t: &Target,
    on: bool,
    now: u64,
    cw: &Value,
) -> Result<(), String> {
    admission_for(selection,radio,t,on,now,cw,None)
}
fn admission_for(selection:&Value,radio:&Value,t:&Target,on:bool,now:u64,cw:&Value,leased_slice:Option<&str>)->Result<(),String> {
    let s = &radio["snapshot"];
    let station = &s["station"];
    let r = &s["radio"];
    let exact = selection["provider"] == t.provider
        && t.radio_id.as_ref().is_none_or(|id|s["radio_id"]==*id)
        && selection["generation"] == t.selection_generation
        && selection["selection_id"] == t.selection_id
        && selection["control_revision"] == t.control_revision
        && radio["provider"] == t.provider
        && radio["selection_generation"] == t.selection_generation
        && radio["selection_id"] == t.selection_id
        && radio["worker_generation"] == t.worker_generation
        && s["source"] == t.provider
        && s["generation"] == t.connection_generation;
    if !exact {
        return Err("PTT exact native session changed".into());
    }
    if !on{return Ok(());}
    let recent = |v: &Value, k: f64| {
        v.as_f64()
            .is_some_and(|v| v.is_finite() && (-1000.0..=3000.0).contains(&(now as f64 - v * k)))
    };
    let cap = if t.provider == "direct_agent" {
        station["supported_commands"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("set_ptt")))
            && station["commissioned"] == true
            && station["radio_known"] == true
            && station["radio_stale"] == false
            && station["inhibitors"]
                .as_array()
                .is_some_and(|v| v.is_empty())
    } else if t.provider=="direct_tci" {
        let address=t.tci.as_ref().ok_or("TCI PTT address unavailable")?;
        s["capabilities"]["supported_commands"].as_array().is_some_and(|v|v.contains(&json!("ptt")))
            && s["tci"]["trx"]==address.trx && s["tci"]["channel"]==address.channel
            && s["tci"]["transmit_enabled"]==true
            && recent(&s["tci"]["trx_observed_at"],1000.0)
    } else {
        s["capabilities"]["supported_commands"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("ptt")))
            && r["vfos"].as_array().is_some_and(|vfos| {
                if let Some(slice)=leased_slice {
                    matches!(slice,"A"|"B") && r["transmit_vfo"]==slice
                        && vfos.iter().filter(|v|v["letter"]==slice).count()==1
                        && vfos.iter().any(|v|v["letter"]==slice&&v["transmit"]==true)
                } else {
                    vfos.iter().any(|v|v["letter"]==r["receive_vfo"]&&v["receive"]==true)
                        && vfos.iter().any(|v|v["letter"]==r["transmit_vfo"]&&v["transmit"]==true)
                }
            })
    };
    // Digital FLEX routes use the same actual liveness evidence as their TX
    // context: an unchanged idle event can be kept current by a confirmed ping.
    // Manual/CW/TCI admission retains its existing observation-age policy.
    let observation_current = if t.provider == "direct_flex_lan" && leased_slice.is_some() {
        let live = |v: &Value| v.as_f64().is_some_and(|v| {
            v.is_finite() && v >= 0.0 && (0.0..=2500.0).contains(&(now as f64 - v * 1000.0))
        });
        live(&r["observed_at"]) || live(&s["heartbeat_at"])
    } else {
        recent(&r["observed_at"], 1000.0)
    };
    if !cap
        || radio["revision"] != t.observation_revision
        || radio["received_at_ms"] != t.received_at_ms
        || radio["available"] != true
        || !recent(&radio["received_at_ms"], 1.0)
        || !recent(&s["generated_at"], 1000.0)
        || !observation_current
        || r["connected"] != true
        || r["observed"] != true
        || r["transmitting"] != false
        || (s["allow_transmit"] != true && leased_slice.is_none())
        || s["carrier_tune"]["pending"] != false
        || s["carrier_tune"]["release_required"] != false
        || s["ptt"]["release_required"] != false
        || cw["armed"] != false
        || cw["pending"] != false
        || !cw["owner"].is_null()
        || !cw["lease"].is_null()
    {
        return Err("PTT on requires fresh idle RX, consent, capability, station readiness and no Tune/CW ownership".into());
    }
    Ok(())
}
fn cw_for_acquisition(
    on: bool,
    read: impl FnOnce() -> Result<Value, String>,
) -> Result<Value, String> {
    if on {
        read()
    } else {
        Ok(json!({"armed":false,"pending":false,"owner":null,"lease":null}))
    }
}
pub fn preflight(app: &tauri::AppHandle, t: &Target, on: bool, intent: &str) -> Result<(), String> {
    crate::worker::require_existing_generation(t.worker_generation)?;
    if !on{return Ok(());}
    if t.provider=="direct_tci"{crate::station_selection::owner_for(app,t.radio_id.as_deref().ok_or("TCI slot unavailable")?)?.require_tci_transmit_enabled(t.worker_generation,t.connection_generation)?;}
    if on {
        crate::carrier_tune::require_output_intent(intent)?;
        crate::carrier_tune::require_cw_ready(app)?;
    }
    let selected = crate::station_selection::owner(app)?.snapshot()?;
    let radio = crate::station_telemetry::owner(app)?.snapshot()?;
    let cw = cw_for_acquisition(on, || {
        crate::cw_session_backend::existing().map_or_else(
            || Ok(json!({"armed":false,"pending":false,"owner":null,"lease":null})),
            |s| s.snapshot(),
        )
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "PTT clock unavailable")?
        .as_millis() as u64;
    admission(&selected, &radio, t, on, now, &cw)
}
pub(crate) fn preflight_leased(app:&tauri::AppHandle,t:&Target,intent:&str,slice:&str)->Result<(),String> {
    crate::worker::require_existing_generation(t.worker_generation)?;
    crate::carrier_tune::require_output_intent(intent)?;crate::carrier_tune::require_cw_ready(app)?;
    let slot=t.radio_id.as_deref().ok_or("Digital PTT slot unavailable")?;
    let selected=crate::station_selection::owner_for(app,slot)?.snapshot()?;
    let radio=crate::station_telemetry::owner_for(app,slot)?.snapshot()?;
    let cw=cw_for_acquisition(true,||crate::cw_session_backend::existing().map_or_else(||Ok(json!({"armed":false,"pending":false,"owner":null,"lease":null})),|s|s.snapshot()))?;
    let now=SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_|"PTT clock unavailable")?.as_millis() as u64;
    admission_for(&selected,&radio,t,true,now,&cw,Some(slice))
}
pub fn context(app: &tauri::AppHandle) -> Result<Value, String> {
    context_for(app,&crate::radio_slots::active(app)?)
}
pub(crate) fn context_for(app:&tauri::AppHandle,slot:&str)->Result<Value,String> {
    let selection=crate::station_selection::owner_for(app,slot)?.snapshot()?;
    let radio=crate::station_telemetry::owner_for(app,slot)?.snapshot()?;
    let mut value=json!({"provider":selection["provider"],"selection_generation":selection["generation"],"selection_id":selection["selection_id"],"control_revision":selection["control_revision"],
        "worker_generation":radio["worker_generation"],"connection_generation":radio["snapshot"]["generation"],"observation_revision":radio["revision"],"received_at_ms":radio["received_at_ms"],
        "radio_id":slot});
    if selection["provider"]=="direct_tci"{value["tci"]=json!({"trx":radio["snapshot"]["tci"]["trx"],"channel":radio["snapshot"]["tci"]["channel"]});}
    let target=serde_json::from_value::<Target>(value).ok().filter(|t|t.validate().is_ok());Ok(json!({"target":target}))
}
fn recovery_proof(
    old: &Target,
    current: &Target,
    selected: &Value,
    radio: &Value,
    now: u64,
    retired: impl FnOnce(u64, u64) -> Result<(), String>,
) -> Result<(), String> {
    // Recovery is not a handover or permission to release an unrelated station.
    if old.provider != current.provider
        || old.selection_id != current.selection_id
        || old.selection_generation != current.selection_generation
        || old.tci!=current.tci
        || old.radio_id.as_ref().is_some_and(|id|current.radio_id.as_ref()!=Some(id))
    {
        return Err("Select the original PTT station before retired-owner recovery".into());
    }
    retired(old.worker_generation, current.worker_generation)?;
    admission(selected, radio, current, false, now, &Value::Null)?;
    let s = &radio["snapshot"];
    let r = &s["radio"];
    let recent = |v: &Value, k: f64| {
        v.as_f64()
            .is_some_and(|v| v.is_finite() && (-1000.0..=3000.0).contains(&(now as f64 - v * k)))
    };
    if radio["available"] != true
        || radio["revision"] != current.observation_revision
        || radio["received_at_ms"] != current.received_at_ms
        || !recent(&radio["received_at_ms"], 1.0)
        || !recent(&s["generated_at"], 1000.0)
        || !recent(&r["observed_at"], 1000.0)
        || r["connected"] != true
        || r["observed"] != true
        || r["transmitting"] != false
        || s["ptt"]["pending"] != false
        || s["ptt"]["release_required"] != false
        || s["carrier_tune"]["pending"] != false
        || s["carrier_tune"]["release_required"] != false
        || current.provider == "direct_agent"
            && (s["station"]["radio_known"] != true || s["station"]["radio_stale"] != false)
    {
        return Err(
            "Retired PTT recovery requires fresh authoritative RX and no current TX obligations"
                .into(),
        );
    }
    Ok(())
}
pub fn recover(
    app: &tauri::AppHandle,
    session: &Session,
    id: &str,
    owner_id: &str,
    target: Target,
) -> Result<Value, String> {
    let selection = crate::station_selection::owner(app)?;
    let expected = json!({"provider":target.provider,"generation":target.selection_generation,"selection_id":target.selection_id,"control_revision":target.control_revision});
    let _selection = selection.fence_radio_read_epoch(&expected)?;
    session.recover(id, owner_id, target, |old, t| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "PTT clock unavailable")?
            .as_millis() as u64;
        recovery_proof(
            old,
            t,
            &expected,
            &crate::station_telemetry::owner(app)?.snapshot()?,
            now,
            crate::worker::require_retired_generation,
        )
    })
}
pub fn execute(
    app: &tauri::AppHandle,
    mut request: Value,
    t: &Target,
    intent: &str,
) -> Result<Value, String> {
    let on = request["params"]["enabled"] == true;
    if on&&t.provider=="direct_tci"{crate::station_selection::owner_for(app,t.radio_id.as_deref().ok_or("TCI slot unavailable")?)?.require_tci_transmit_enabled(t.worker_generation,t.connection_generation)?;}
    if !on {
        // Release the retained native route even after visible radio selection
        // changes. The worker checks the original owner and connection generation.
        // Never start/reconnect a worker or substitute a replacement connection.
        crate::worker::require_existing_generation(t.worker_generation)?;
        let radio_id=if let Some(id)=&t.radio_id{id.clone()}else{
            // Old journals did not retain a slot. Resolve its existing native
            // selection identity; never guess Radio 1 from a missing field.
            let mut matches=Vec::new();
            for id in ["radio1","radio2"]{
                let selected=crate::station_selection::owner_for(app,id)?.snapshot()?;
                if selected["provider"]==t.provider && selected["generation"]==t.selection_generation && selected["selection_id"]==t.selection_id{matches.push(id.to_owned());}
            }
            if matches.len()!=1{return Err("The legacy PTT original slot cannot be identified; retain its obligation for explicit recovery".into());}
            matches.remove(0)
        };
        let native_intent=crate::station_selection::owner_for(app,&radio_id)?.retained_release_intent()?;
        request["params"]["radio_id"]=json!(radio_id);request["params"]["expected_native_intent"]=json!(native_intent);
        let response=crate::worker::request_existing_generation(request,t.worker_generation)?;
        crate::worker::require_existing_generation(t.worker_generation)?;
        return Ok(response);
    }
    if on {
        crate::carrier_tune::require_output_intent(intent)?;
        crate::carrier_tune::require_cw_ready(app)?;
    }
    crate::worker::require_existing_generation(t.worker_generation)?;
    let leased_slice=request["params"]["digital_lease"]["transmit_slice"].as_str().map(str::to_owned);
    let slot=if leased_slice.is_some(){t.radio_id.clone().ok_or("Digital PTT slot unavailable")?}else{crate::radio_slots::active(app)?};
    let selection = crate::station_selection::owner_for(app,&slot)?;
    let telemetry = crate::station_telemetry::owner_for(app,&slot)?;
    let preserve_receive = crate::companion_host::is_receive_preserving_ptt(&request);
    let expected = json!({"provider":t.provider,"generation":t.selection_generation,"selection_id":t.selection_id,"control_revision":t.control_revision});
    selection.ptt_control(
        request,
        &expected,
        || {
            crate::worker::require_existing_generation(t.worker_generation)?;
            if on {
                crate::carrier_tune::require_output_intent(intent)?;
            }
            let cw = cw_for_acquisition(on, || {
                crate::cw_session_backend::existing().map_or_else(
                    || Ok(json!({"armed":false,"pending":false,"owner":null,"lease":null})),
                    |s| s.snapshot(),
                )
            })?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "PTT clock unavailable")?
                .as_millis() as u64;
            admission_for(&expected, &telemetry.snapshot()?, t, on, now, &cw,leased_slice.as_deref())?;
            // Exact companion PTT changes only transmit state, not the sampled
            // receive route. Keep its telemetry live while retaining all
            // selection/output/PTT admission above and the exact reply below.
            if preserve_receive { Ok(None) } else { telemetry.control().map(Some) }
        },
        |q| {
            if on {
                crate::carrier_tune::require_output_intent(intent)?;
            }
            let v = crate::worker::request_existing_generation(q, t.worker_generation)?;
            crate::worker::require_existing_generation(t.worker_generation)?;
            let current = selection.snapshot()?;
            if current["provider"] != t.provider
                || current["generation"] != t.selection_generation
                || current["selection_id"] != t.selection_id
                || on && current["control_revision"].as_u64() != t.control_revision.checked_add(1)
            {
                return Err("PTT action superseded; inspect its unknown receipt".into());
            }
            Ok(v)
        },
    )
}
static OWNER: OnceLock<Mutex<Option<Arc<Session>>>> = OnceLock::new();
pub fn require_idle(app: &tauri::AppHandle) -> Result<(), String> {
    let owner = owner(app)?;
    owner.reconcile(&crate::station_telemetry::owner(app)?.snapshot()?)?;
    owner.require_idle()
}
pub(crate) fn require_idle_for(app:&tauri::AppHandle,slot:&str)->Result<(),String> {
    let owner=owner(app)?;owner.reconcile(&crate::station_telemetry::owner_for(app,slot)?.snapshot()?)?;owner.require_idle()
}
pub fn owner(app: &tauri::AppHandle) -> Result<Arc<Session>, String> {
    let mut slot = OWNER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "PTT owner unavailable")?;
    if let Some(s) = slot.as_ref() {
        return Ok(s.clone());
    }
    let s = Arc::new(Session::new(
        app.path()
            .app_config_dir()
            .map_err(|_| "PTT storage unavailable")?
            .join("ptt-session.json"),
    )?);
    *slot = Some(s.clone());
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ptt_known_readiness_rejection_has_no_durable_release_duty_or_dispatch() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(s
            .apply_admitted(
                &id,
                true,
                target(),
                &crate::carrier_tune::output_intent().unwrap(),
                |_| {
                    assert!(crate::carrier_tune::reserve_output().is_err());
                    Err("CW is armed".into())
                },
                |_, _| panic!("Rejected readiness dispatched")
            )
            .is_err());
        assert!(s.receipt(&id).unwrap().is_null());
        assert!(s.require_idle().is_ok());
        assert!(!s.path.exists());
    }
    fn target() -> Target {
        Target {
            provider: "direct_agent".into(),
            selection_generation: 2,
            selection_id: uuid::Uuid::new_v4().to_string(),
            worker_generation: 4,
            connection_generation: 7,
            control_revision: 3,
            observation_revision: 8,
            received_at_ms: 10000,
            radio_id:None,tci:None,
        }
    }
    fn fixture() -> (tempfile::TempDir, Session) {
        let d = tempfile::tempdir().unwrap();
        let s = Session::new(d.path().join("ptt.json")).unwrap();
        (d, s)
    }
    #[test]
    fn digital_exact_ptt_uses_existing_owner_and_stop_bypasses_acquisition_lane() {
        let _serial=crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_dir,s)=fixture();let mut t=target();t.provider="direct_flex_lan".into();
        let authority=crate::output_authority::shared();let operation=uuid::Uuid::new_v4().to_string();
        let ticket=authority.prepare(&operation,&authority.intent().unwrap()).unwrap();
        let on=s.apply_exact_admitted(&operation,true,t.clone(),&ticket,|_|Ok(()),|q,target|{
            assert!(authority.reserve_ordinary().is_err());
            let off=s.apply_exact_admitted(&uuid::Uuid::new_v4().to_string(),false,target.clone(),&ticket,|_|Ok(()),|q,t|Ok(reply(&q,t,true))).unwrap();
            assert_eq!(off["status"],"confirmed");
            assert!(authority.require_exact(&ticket).is_err());
            assert!(s.require_idle().is_err(),"on response still pending; release proof cannot retire a later possible effect");
            Ok(reply(&q,target,true))
        }).unwrap();
        assert_eq!(on["status"],"confirmed");
        assert!(s.apply_exact_admitted(&uuid::Uuid::new_v4().to_string(),true,t.clone(),&ticket,|_|Ok(()),|_,_|panic!("new operation reused ticket")).is_err());
        assert!(authority.reserve_ordinary().is_err(),"revoked obligation remains owned");
        authority.retire_exact(&ticket).unwrap();
    }
    #[test]
    fn digital_exact_release_never_releases_an_unrelated_ptt_owner_or_replays_on() {
        let _serial=crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_dir,s)=fixture();let mut t=target();t.provider="direct_flex_lan".into();
        let authority=crate::output_authority::shared();let operation=uuid::Uuid::new_v4().to_string();
        let ticket=authority.prepare(&operation,&authority.intent().unwrap()).unwrap();
        s.apply_exact_admitted(&operation,true,t.clone(),&ticket,|_|Ok(()),|q,t|Ok(reply(&q,t,true))).unwrap();
        let duplicate=s.apply_exact_admitted(&operation,true,t.clone(),&ticket,|_|panic!("duplicate admission"),|_,_|panic!("duplicate dispatch")).unwrap();
        assert_eq!(duplicate["status"],"confirmed");
        authority.retire_exact(&ticket).unwrap();
        let foreign=authority.prepare(&uuid::Uuid::new_v4().to_string(),&authority.intent().unwrap()).unwrap();
        assert!(s.apply_exact_admitted(&uuid::Uuid::new_v4().to_string(),false,t.clone(),&foreign,|_|Ok(()),|_,_|panic!("foreign release")).is_err());
        assert_eq!(s.retained_target().unwrap(),Some(t));
        authority.retire_exact(&foreign).unwrap();
    }
    #[test]
    fn a_previous_app_duty_retires_only_on_fresh_idle_radio_of_the_same_station() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let t = target();
        let owner = uuid::Uuid::new_v4().to_string();
        s.apply(&owner, true, t.clone(), &crate::carrier_tune::output_intent().unwrap(), |_, _| Err("lost".into()))
            .unwrap();
        let radio = |at: u64, transmitting: bool, selection: &str, ptt_owned: bool| json!({
            "available":true,"provider":t.provider,"selection_id":selection,"received_at_ms":at,
            "snapshot":{"source":t.provider,"radio":{"connected":true,"transmitting":transmitting},
                "ptt":{"release_required":ptt_owned},"carrier_tune":{"release_required":false}}});
        let now = 50_000;
        // A duty of THIS worker epoch is never retired here; ordinary release owns it.
        assert!(!s.retire_prior_app_duty(&owner, &radio(now, false, &t.selection_id, false), now).unwrap());
        // Make it a previous app's duty, as after a restart.
        {
            let mut saved = s.state.lock().unwrap();
            saved.release_required.as_mut().unwrap().worker_epoch = Some(uuid::Uuid::new_v4().to_string());
            s.persist(&saved).unwrap();
        }
        let s = Session::new(s.path.clone()).unwrap();
        assert!(s.require_idle().is_err());
        for (why, r) in [
            ("transmitting", radio(now, true, &t.selection_id, false)),
            ("stale", radio(now - 1_001, false, &t.selection_id, false)),
            ("future", radio(now + 1, false, &t.selection_id, false)),
            ("other station", radio(now, false, &uuid::Uuid::new_v4().to_string(), false)),
            ("current worker owns PTT", radio(now, false, &t.selection_id, true)),
        ] {
            assert!(!s.retire_prior_app_duty(&owner, &r, now).unwrap(), "{why} must not retire");
        }
        assert!(!s.retire_prior_app_duty(&uuid::Uuid::new_v4().to_string(), &radio(now, false, &t.selection_id, false), now).unwrap());
        assert!(s.require_idle().is_err());
        assert!(!s.owner_released(&owner).unwrap(), "a retained duty is not released");
        assert!(s.retire_prior_app_duty(&owner, &radio(now - 1_000, false, &t.selection_id, false), now).unwrap());
        assert!(s.require_idle().is_ok(), "the next acquisition is admitted");
        assert!(s.owner_released(&owner).unwrap(), "the recovery settles this owner's release");
        assert!(!s.owner_released(&uuid::Uuid::new_v4().to_string()).unwrap(), "an unknown owner has no release proof");
        // A second retirement finds no duty; the TX record closes through `owner_released`.
        assert!(!s.retire_prior_app_duty(&owner, &radio(now, false, &t.selection_id, false), now).unwrap());
        // Durable across restart, with an auditable recovery decision.
        let reloaded = Session::new(s.path.clone()).unwrap();
        assert!(reloaded.require_idle().is_ok());
        let saved = reloaded.state.lock().unwrap();
        assert!(saved.recoveries.iter().any(|r| r.owner_id == owner && r.status == "recovered"));
        assert!(saved.receipts.iter().filter(|r| r.owner_id == owner).all(|r| r.status != "unknown"));
    }
    #[test]
    fn ptt_previous_app_worker_one_cannot_alias_new_app_retired_worker_one() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let mut old = target();
        old.worker_generation = 1;
        let owner = uuid::Uuid::new_v4().to_string();
        s.apply(
            &owner,
            true,
            old.clone(),
            &crate::carrier_tune::output_intent().unwrap(),
            |_, _| Err("lost".into()),
        )
        .unwrap();
        let mut current = old.clone();
        current.worker_generation = 2;
        // This app may really have reaped its unrelated child 1. Even a
        // successful generation-only proof must not be consulted for old-app 1.
        for epoch in [Some(uuid::Uuid::new_v4().to_string()), None] {
            {
                let mut saved = s.state.lock().unwrap();
                saved.release_required.as_mut().unwrap().worker_epoch = epoch;
                s.persist(&saved).unwrap();
            }
            let reloaded = Session::new(s.path.clone()).unwrap();
            assert!(reloaded
                .recover(
                    &uuid::Uuid::new_v4().to_string(),
                    &owner,
                    current.clone(),
                    |_, _| panic!(
                        "Unrelated current-app retired generation 1 used as old-app proof"
                    )
                )
                .is_err());
            assert!(reloaded
                .apply(
                    &uuid::Uuid::new_v4().to_string(),
                    false,
                    old.clone(),
                    "",
                    |_, _| panic!("Old-app duty dispatched Off")
                )
                .is_err());
            let before = reloaded.snapshot().unwrap();
            reloaded.reconcile(&proof(&old, &owner, false)).unwrap();
            assert_eq!(reloaded.snapshot().unwrap(), before);
            assert!(reloaded.require_idle().is_err());
        }
    }
    #[test]
    fn ptt_retired_worker_recovery_is_explicit_durable_exact_and_never_dispatches_off() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (d, s) = fixture();
        let old = target();
        let owner = uuid::Uuid::new_v4().to_string();
        s.apply(
            &owner,
            true,
            old.clone(),
            &crate::carrier_tune::output_intent().unwrap(),
            |_, _| Err("lost".into()),
        )
        .unwrap();
        drop(s);
        let s = Session::new(d.path().join("ptt.json")).unwrap();
        let mut current = old.clone();
        current.worker_generation += 1;
        current.connection_generation += 1;
        let id = uuid::Uuid::new_v4().to_string();
        assert!(s.require_idle().is_err());
        assert!(s
            .apply(
                &uuid::Uuid::new_v4().to_string(),
                false,
                current.clone(),
                "",
                |_, _| panic!("Generic replacement Off dispatched")
            )
            .is_err());
        assert!(s
            .recover(
                &id,
                &owner,
                current.clone(),
                |_, _| Err("Not reaped".into())
            )
            .is_err());
        assert!(s.snapshot().unwrap()["release_required"].is_object());
        let blocked_path = d.path().join("blocked");
        std::fs::write(&blocked_path, b"fixture").unwrap();
        let blocked = Session {
            path: blocked_path.join("ptt.json"),
            state: Mutex::new(s.state.lock().unwrap().clone()),
            on: Mutex::new(()),
            off: Mutex::new(()),
        };
        assert!(blocked
            .recover(
                &uuid::Uuid::new_v4().to_string(),
                &owner,
                current.clone(),
                |_, _| Ok(())
            )
            .is_err());
        assert!(blocked.snapshot().unwrap()["release_required"].is_object());
        assert!(blocked.require_idle().is_err());
        let selected = json!({"provider":current.provider,"generation":current.selection_generation,"selection_id":current.selection_id,"control_revision":current.control_revision});
        let radio = json!({"available":true,"provider":current.provider,"selection_generation":current.selection_generation,"selection_id":current.selection_id,"worker_generation":current.worker_generation,"revision":current.observation_revision,"received_at_ms":10000,"snapshot":{"source":current.provider,"generation":current.connection_generation,"generated_at":10,"radio":{"observed":true,"connected":true,"transmitting":false,"observed_at":10},"station":{"radio_known":true,"radio_stale":false},"ptt":{"pending":false,"release_required":false},"carrier_tune":{"pending":false,"release_required":false}}});
        let proof = |o: &Target, t: &Target| {
            recovery_proof(o, t, &selected, &radio, 10000, |a, b| {
                if a == old.worker_generation && b == current.worker_generation {
                    Ok(())
                } else {
                    Err("Not retired".into())
                }
            })
        };
        let mut stale = radio.clone();
        stale["snapshot"]["radio"]["transmitting"] = json!(true);
        assert!(recovery_proof(&old, &current, &selected, &stale, 10000, |_, _| Ok(())).is_err());
        assert!(recovery_proof(&old, &current, &selected, &radio, 20000, |_, _| Ok(())).is_err());
        let mut unrelated = current.clone();
        unrelated.selection_id = uuid::Uuid::new_v4().to_string();
        assert!(recovery_proof(&old, &unrelated, &selected, &radio, 10000, |_, _| Ok(())).is_err());
        let decision = s.recover(&id, &owner, current.clone(), proof).unwrap();
        assert_eq!(decision["status"], "recovered");
        assert!(s.require_idle().is_ok());
        assert_eq!(
            s.recover(&id, &owner, current.clone(), |_, _| panic!(
                "Duplicate reproved"
            ))
            .unwrap(),
            decision
        );
        assert!(s
            .recover(
                &id,
                &uuid::Uuid::new_v4().to_string(),
                current.clone(),
                |_, _| Ok(())
            )
            .is_err());
        drop(s);
        let s = Session::new(d.path().join("ptt.json")).unwrap();
        assert_eq!(s.receipt(&id).unwrap(), decision);
        assert!(s.require_idle().is_ok());
        s.apply(
            &uuid::Uuid::new_v4().to_string(),
            true,
            current,
            &crate::carrier_tune::output_intent().unwrap(),
            |q, t| Ok(reply(&q, t, false)),
        )
        .unwrap();
    }
    fn reply(q: &Value, t: &Target, confirmed: bool) -> Value {
        json!({"id":q["id"],"ok":true,"result":{"accepted":true,"confirmed":confirmed,"control":"ptt","enabled":q["params"]["enabled"],"owner_id":q["params"]["ptt_owner_id"],"state":{"source":t.provider,"generation":t.connection_generation,"radio":{"connected":true,"observed":true,"transmitting":q["params"]["enabled"]},"ptt":{"source":"gui","owner_id":q["params"]["ptt_owner_id"],"confirmed":confirmed,"pending":false,"release_required":q["params"]["enabled"],"transmitting":q["params"]["enabled"]}}}})
    }
    fn proof(t: &Target, id: &str, on: bool) -> Value {
        json!({"available":true,"provider":t.provider,"selection_generation":t.selection_generation,"selection_id":t.selection_id,"worker_generation":t.worker_generation,"snapshot":{"source":t.provider,"generation":t.connection_generation,"ptt":{"owner_id":id,"source":"gui","confirmed":true,"pending":false,"release_required":on,"transmitting":on}}})
    }
    #[test]
    fn ptt_ack_requires_newer_backend_proof_and_review_never_clears_duty() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let t = target();
        let id = uuid::Uuid::new_v4().to_string();
        let intent = crate::carrier_tune::output_intent().unwrap();
        let r = s
            .apply(&id, true, t.clone(), &intent, |q, t| {
                let saved: Saved =
                    serde_json::from_slice(&std::fs::read(&s.path).unwrap()).unwrap();
                assert_eq!(saved.receipts[0].status, "pending");
                assert!(saved.release_required.is_some());
                Ok(reply(&q, t, false))
            })
            .unwrap();
        assert_eq!(r["status"], "acknowledged");
        assert!(s.require_idle().is_err());
        s.reconcile(&proof(&t, &id, true)).unwrap();
        assert_eq!(s.receipt(&id).unwrap()["status"], "confirmed");
        assert!(s.require_idle().is_err());
        let off = uuid::Uuid::new_v4().to_string();
        let r = s
            .apply(&off, false, t.clone(), "", |q, t| Ok(reply(&q, t, false)))
            .unwrap();
        assert_eq!(r["status"], "acknowledged");
        assert!(s.require_idle().is_err());
        let mut wrong = proof(&t, &id, false);
        wrong["snapshot"]["ptt"]["source"] = json!("embedded_cat");
        s.reconcile(&wrong).unwrap();
        assert!(s.require_idle().is_err());
        s.reconcile(&proof(&t, &id, false)).unwrap();
        assert!(s.require_idle().is_ok());
        assert_eq!(s.receipt(&off).unwrap()["status"], "confirmed");
        let next = uuid::Uuid::new_v4().to_string();
        s.apply(
            &next,
            true,
            t.clone(),
            &crate::carrier_tune::output_intent().unwrap(),
            |_, _| Err("lost".into()),
        )
        .unwrap();
        s.review(&next).unwrap();
        assert!(s.require_idle().is_err());
        assert!(s.snapshot().unwrap()["release_required"].is_object());
    }
    #[test]
    fn ptt_restart_dedupe_collision_no_replay_and_retained_owner_only_off() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let t = target();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(s
            .apply(
                &uuid::Uuid::new_v4().to_string(),
                false,
                t.clone(),
                "",
                |_, _| panic!()
            )
            .is_err());
        let intent = crate::carrier_tune::output_intent().unwrap();
        s.apply(&id, true, t.clone(), &intent, |_, _| Err("lost".into()))
            .unwrap();
        assert_eq!(
            s.apply(&id, true, t.clone(), &intent, |_, _| panic!("replay"))
                .unwrap()["status"],
            "unknown"
        );
        let mut other = t.clone();
        other.selection_id = uuid::Uuid::new_v4().to_string();
        assert!(s
            .apply(&id, true, other.clone(), &intent, |_, _| panic!())
            .is_err());
        assert!(s
            .apply(
                &uuid::Uuid::new_v4().to_string(),
                false,
                other,
                "",
                |_, _| panic!("unrelated release")
            )
            .is_err());
        let cold = Session::new(s.path.clone()).unwrap();
        assert!(cold.require_idle().is_err());
        assert_eq!(cold.receipt(&id).unwrap()["status"], "unknown");
        assert!(cold
            .apply(
                &uuid::Uuid::new_v4().to_string(),
                true,
                t.clone(),
                &intent,
                |_, _| panic!()
            )
            .is_err());
        let mut saved = s.state.lock().unwrap().clone();
        saved.receipts[0].status = "pending".into();
        s.persist(&saved).unwrap();
        assert_eq!(
            Session::new(s.path.clone()).unwrap().receipt(&id).unwrap()["status"],
            "unknown"
        );
    }
    #[test]
    fn ptt_off_and_cancel_retire_old_preparations_and_share_atomic_cw_tune_gate() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let mut t = target();
        t.provider = "direct_flex_lan".into();
        let old = crate::carrier_tune::output_intent().unwrap();
        crate::carrier_tune::revoke_output_intent().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(s
            .apply(&id, true, t.clone(), &old, |_, _| panic!())
            .is_err());
        assert!(s.receipt(&id).unwrap().is_null());
        let guard = crate::carrier_tune::reserve_output().unwrap();
        let current = crate::carrier_tune::output_intent().unwrap();
        assert!(s
            .apply(&id, true, t.clone(), &current, |_, _| panic!())
            .is_err());
        assert!(s.snapshot().unwrap()["release_required"].is_null());
        drop(guard);
        s.apply(&id, true, t.clone(), &current, |q, t| {
            assert!(crate::carrier_tune::reserve_output().is_err());
            let off = uuid::Uuid::new_v4().to_string();
            let r = s
                .apply(&off, false, t.clone(), "", |q, t| Ok(reply(&q, t, true)))
                .unwrap();
            assert_eq!(r["status"], "confirmed");
            assert!(
                s.require_idle().is_err(),
                "An older in-flight acquisition retains its duty even after RX proof"
            );
            Ok(reply(&q, t, false))
        })
        .unwrap();
    }
    #[test]
    fn ptt_storage_failure_malformed_reply_and_terminal_retention() {
        let _serial = crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d, s) = fixture();
        let t = target();
        let intent = crate::carrier_tune::output_intent().unwrap();
        std::fs::create_dir(&s.path).unwrap();
        assert!(s
            .apply(
                &uuid::Uuid::new_v4().to_string(),
                true,
                t.clone(),
                &intent,
                |_, _| panic!()
            )
            .is_err());
        std::fs::remove_dir(&s.path).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let r = s
            .apply(&id, true, t.clone(), &intent, |q, t| {
                let mut v = reply(&q, t, false);
                v["result"]["owner_id"] = json!(uuid::Uuid::new_v4().to_string());
                Ok(v)
            })
            .unwrap();
        assert_eq!(r["status"], "unknown");
        s.review(&id).unwrap();
        assert!(s.snapshot().unwrap()["receipts"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(s.snapshot().unwrap()["release_required"].is_object());
        assert_eq!(
            s.apply(&id, true, t, &intent, |_, _| panic!()).unwrap()["status"],
            "reviewed"
        );
        let (_dir, failed) = fixture();
        let lost = uuid::Uuid::new_v4().to_string();
        assert!(failed
            .apply(&lost, true, target(), &intent, |q, t| {
                std::fs::remove_file(&failed.path).unwrap();
                std::fs::create_dir(&failed.path).unwrap();
                Ok(reply(&q, t, false))
            })
            .is_err());
        assert_eq!(failed.receipt(&lost).unwrap()["status"], "unknown");
        assert!(failed.snapshot().unwrap()["release_required"].is_object());
        assert!(failed.review(&lost).is_err());
        let q = json!({"id":lost,"params":{"enabled":true,"ptt_owner_id":lost}});
        assert_eq!(
            outcome(
                &lost,
                true,
                &lost,
                &target(),
                &Ok(reply(&q, &target(), true))
            ),
            "unknown",
            "Agent immediate confirmation is malformed; only later proof may promote an ACK"
        );
    }
    #[test]
    fn saved_audio_leased_admission_uses_exact_tx_slice_without_changing_manual_receive_gate() {
        let mut t=target();t.provider="direct_flex_lan".into();
        let sel=json!({"provider":t.provider,"generation":t.selection_generation,"selection_id":t.selection_id,"control_revision":t.control_revision});
        let cw=json!({"armed":false,"pending":false,"owner":null,"lease":null});
        let mut r=json!({"provider":t.provider,"selection_generation":t.selection_generation,"selection_id":t.selection_id,"worker_generation":t.worker_generation,"revision":t.observation_revision,"received_at_ms":t.received_at_ms,"available":true,"snapshot":{"source":t.provider,"generation":t.connection_generation,"generated_at":10.0,"allow_transmit":false,"capabilities":{"supported_commands":["ptt"]},"radio":{"connected":true,"observed":true,"observed_at":10.0,"transmitting":false,"receive_vfo":null,"transmit_vfo":"B","vfos":[{"letter":"B","receive":false,"transmit":true}]},"carrier_tune":{"pending":false,"release_required":false},"ptt":{"release_required":false}}});
        assert!(admission_for(&sel,&r,&t,true,10000,&cw,Some("B")).is_ok());
        assert!(admission(&sel,&r,&t,true,10000,&cw).is_err());
        assert!(admission_for(&sel,&r,&t,true,10000,&cw,Some("A")).is_err());
        r["snapshot"]["radio"]["transmitting"]=json!(true);assert!(admission_for(&sel,&r,&t,true,10000,&cw,Some("B")).is_err());
        assert!(admission(&sel,&r,&t,false,999999,&cw).is_ok());
    }
    #[test]
    fn ptt_native_on_admission_requires_exact_fresh_policy_off_retains_stale_authority() {
        assert!(
            cw_for_acquisition(false, || panic!("Urgent PTT off queried a busy CW owner")).is_ok()
        );
        let t = target();
        let sel = json!({"provider":t.provider,"generation":t.selection_generation,"selection_id":t.selection_id,"control_revision":t.control_revision});
        let cw = json!({"armed":false,"pending":false,"owner":null,"lease":null});
        let r = json!({"provider":t.provider,"selection_generation":t.selection_generation,"selection_id":t.selection_id,"worker_generation":t.worker_generation,"revision":t.observation_revision,"received_at_ms":t.received_at_ms,"available":true,"snapshot":{"source":t.provider,"generation":t.connection_generation,"generated_at":10.0,"allow_transmit":true,"radio":{"connected":true,"observed":true,"observed_at":10.0,"transmitting":false},"carrier_tune":{"pending":false,"release_required":false},"ptt":{"release_required":false},"station":{"supported_commands":["set_ptt"],"commissioned":true,"radio_known":true,"radio_stale":false,"inhibitors":[]}}});
        assert!(admission(&sel, &r, &t, true, 10000, &cw).is_ok());
        for path in [
            ["snapshot", "allow_transmit"],
            ["snapshot", "radio"],
            ["snapshot", "station"],
            ["snapshot", "carrier_tune"],
            ["snapshot", "ptt"],
        ] {
            let mut bad = r.clone();
            bad[path[0]][path[1]] = Value::Null;
            assert!(admission(&sel, &bad, &t, true, 10000, &cw).is_err());
            assert!(admission(&sel, &bad, &t, false, 999999, &cw).is_ok());
        }
        let mut old = r.clone();
        old["worker_generation"] = json!(5);
        assert!(admission(&sel, &old, &t, false, 10000, &cw).is_err());
        assert!(admission(&sel, &r, &t, true, 999999, &cw).is_err());
        let mut active = cw.clone();
        active["armed"] = json!(true);
        assert!(admission(&sel, &r, &t, true, 10000, &active).is_err());
        for label in ["monitor", "station-control", "cw", "unknown"] {
            assert!(validate(label, false, false, "on").is_err());
            assert!(validate(label, false, false, "off").is_err());
        }
    }
    #[test]
    fn tci_ptt_uses_addressed_route_evidence_without_fabricated_vfo_or_global_frequency(){
        let mut t=target();t.provider="direct_tci".into();t.radio_id=Some("radio2".into());t.tci=Some(TCITarget{trx:1,channel:1});t.validate().unwrap();
        let selected=json!({"provider":t.provider,"generation":t.selection_generation,"selection_id":t.selection_id,"control_revision":t.control_revision});
        let cw=json!({"armed":false,"pending":false,"owner":null,"lease":null});
        let radio=json!({"provider":t.provider,"selection_generation":t.selection_generation,"selection_id":t.selection_id,"worker_generation":t.worker_generation,"revision":t.observation_revision,"received_at_ms":t.received_at_ms,"available":true,
            "snapshot":{"source":t.provider,"radio_id":"radio2","generation":t.connection_generation,"generated_at":10.0,"allow_transmit":true,
                "radio":{"connected":true,"observed":true,"observed_at":10.0,"transmitting":false,"transmit_vfo":null,"receive_vfo":null,"transmit_frequency_hz":null,"vfos":[]},
                "capabilities":{"supported_commands":["ptt"]},"tci":{"trx":1,"channel":1,"transmit_enabled":true,"trx_observed_at":10.0},
                "carrier_tune":{"pending":false,"release_required":false},"ptt":{"release_required":false}}});
        assert!(admission(&selected,&radio,&t,true,10000,&cw).is_ok());
        for (field,value) in [("trx",json!(0)),("channel",json!(0)),("transmit_enabled",json!(false)),("trx_observed_at",json!(1.0))]{
            let mut bad=radio.clone();bad["snapshot"]["tci"][field]=value;assert!(admission(&selected,&bad,&t,true,10000,&cw).is_err());
        }
        let mut bad=radio.clone();bad["snapshot"]["radio_id"]=json!("radio1");assert!(admission(&selected,&bad,&t,true,10000,&cw).is_err());
        let old=json!({"provider":"direct_agent","selection_generation":2,"selection_id":t.selection_id,"worker_generation":4,"connection_generation":7,"control_revision":3,"observation_revision":8,"received_at_ms":10000});
        let legacy:Target=serde_json::from_value(old).unwrap();legacy.validate().unwrap();assert!(legacy.tci.is_none());
    }
    #[test]
    fn tci_ptt_retains_exact_slot_and_address_for_release_and_never_replays(){
        let _serial=crate::carrier_tune::OUTPUT_TEST.lock().unwrap();let (_d,s)=fixture();let mut t=target();t.provider="direct_tci".into();t.radio_id=Some("radio2".into());t.tci=Some(TCITarget{trx:1,channel:0});
        let id=uuid::Uuid::new_v4().to_string();let intent=crate::carrier_tune::output_intent().unwrap();
        let on=s.apply(&id,true,t.clone(),&intent,|q,target|{assert_eq!(q["params"]["radio_id"],"radio2");Ok(reply(&q,target,true))}).unwrap();assert_eq!(on["status"],"confirmed");
        let retained=s.retained_target().unwrap().unwrap();assert_eq!(retained,t);
        let mut replacement=t.clone();replacement.connection_generation+=1;
        assert!(s.apply(&uuid::Uuid::new_v4().to_string(),false,replacement,"",|_,_|panic!("replacement received stale off")).is_err());
        let off_id=uuid::Uuid::new_v4().to_string();let off=s.apply(&off_id,false,retained.clone(),"",|q,target|{assert_eq!(q["params"]["radio_id"],"radio2");Ok(reply(&q,target,true))}).unwrap();
        assert_eq!(off["status"],"confirmed");assert!(s.require_idle().is_ok());
        s.apply(&off_id,false,retained,"",|_,_|panic!("duplicate off replayed")).unwrap();
    }
    #[test]
    fn leased_ptt_renewal_and_normal_release_preserve_only_exact_completion() {
        let _serial=crate::carrier_tune::OUTPUT_TEST.lock().unwrap();
        let (_d,s)=fixture();let mut t=target();t.provider="direct_flex_lan".into();t.radio_id=Some("radio1".into());
        let operation=uuid::Uuid::new_v4().to_string();
        let authority=crate::output_authority::shared();
        let ticket=authority.prepare(&operation,&authority.intent().unwrap()).unwrap();
        let until=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64+800;
        let on=s.apply_exact_leased(&operation,t.clone(),&ticket,until,"A",|_|Ok(()),|q,t|{
            assert_eq!(q["params"]["digital_lease"]["lease_id"],ticket.lease_id());
            assert_eq!(q["params"]["digital_lease"]["expires_at_unix_ms"],until);
            Ok(reply(&q,t,true))
        }).unwrap();assert_eq!(on["status"],"confirmed");
        s.renew_exact(&t,&ticket,until,"A",|q,_|{
            assert_eq!(q["params"]["control"],"ptt_lease");
            Ok(json!({"ok":true,"result":{"lease_renewed":true,"owner_id":operation,"lease_id":ticket.lease_id(),"expires_at_unix_ms":until}}))
        }).unwrap();
        assert!(s.renew_exact(&t,&ticket,until,"A",|_,_|Ok(json!({"ok":true,"result":{"lease_renewed":true,"owner_id":operation,"lease_id":uuid::Uuid::new_v4().to_string(),"expires_at_unix_ms":until}}))).is_err());
        let off=s.release_completed_exact(&uuid::Uuid::new_v4().to_string(),t.clone(),&ticket,|_|Ok(()),|q,t|Ok(reply(&q,t,true))).unwrap();
        assert_eq!(off["status"],"confirmed");assert!(s.require_idle().is_ok());
        assert!(authority.require_exact(&ticket).is_ok(),"normal OFF does not masquerade as user Stop");
        let successor=authority.complete_exact(&ticket).unwrap().unwrap();
        assert!(authority.require_intent(&successor).is_ok());
        assert!(s.renew_exact(&t,&ticket,until,"A",|_,_|panic!("retired lease must not renew")).is_err());
    }
}

// Kept self-contained so the exact production admission function can also be
// exercised by the small Windows source-bound regression harness.
#[cfg(test)]
mod digital_heartbeat_admission_tests {
    use super::*;

    fn fixture() -> (Target, Value, Value, Value) {
        let t = Target {
            provider: "direct_flex_lan".into(), selection_generation: 2,
            selection_id: "00000000-0000-4000-8000-000000000001".into(),
            worker_generation: 4, connection_generation: 7, control_revision: 3,
            observation_revision: 8, received_at_ms: 10000,
            radio_id: Some("radio1".into()), tci: None,
        };
        let selected = json!({"provider":t.provider,"generation":t.selection_generation,
            "selection_id":t.selection_id,"control_revision":t.control_revision});
        let cw = json!({"armed":false,"pending":false,"owner":null,"lease":null});
        let radio = json!({"provider":t.provider,"selection_generation":t.selection_generation,
            "selection_id":t.selection_id,"worker_generation":t.worker_generation,
            "revision":t.observation_revision,"received_at_ms":t.received_at_ms,"available":true,
            "snapshot":{"source":t.provider,"radio_id":"radio1","generation":t.connection_generation,
                "generated_at":10.0,"heartbeat_at":9.0,"allow_transmit":false,
                "capabilities":{"supported_commands":["ptt"]},
                "radio":{"connected":true,"observed":true,"observed_at":1.0,"transmitting":false,
                    "receive_vfo":null,"transmit_vfo":"A","vfos":[{"letter":"A","receive":false,"transmit":true}]},
                "carrier_tune":{"pending":false,"release_required":false},"ptt":{"release_required":false}}});
        (t, selected, radio, cw)
    }

    #[test]
    fn leased_flex_accepts_unchanged_idle_observation_with_confirmed_heartbeat() {
        let (t, selected, radio, cw) = fixture();
        t.validate().unwrap();
        admission_for(&selected, &radio, &t, true, 10000, &cw, Some("A"))
            .expect("fresh confirmed FLEX heartbeat must admit unchanged idle digital PTT observation");
    }

    #[test]
    fn leased_flex_rejects_stale_invalid_or_future_liveness() {
        let (t, selected, radio, cw) = fixture();
        for heartbeat in [Value::Null, json!(7.499), json!(-1), json!(10.001), json!("9.0")] {
            let mut changed = radio.clone();
            changed["snapshot"]["heartbeat_at"] = heartbeat;
            assert!(admission_for(&selected, &changed, &t, true, 10000, &cw, Some("A")).is_err());
        }
        let mut current = radio.clone();
        current["snapshot"]["heartbeat_at"] = Value::Null;
        current["snapshot"]["radio"]["observed_at"] = json!(7.5);
        assert!(admission_for(&selected, &current, &t, true, 10000, &cw, Some("A")).is_ok());
        current["snapshot"]["radio"]["observed_at"] = json!(7.499);
        assert!(admission_for(&selected, &current, &t, true, 10000, &cw, Some("A")).is_err());
    }

    #[test]
    fn leased_flex_retains_exact_session_route_and_idle_output_checks() {
        let (t, selected, radio, cw) = fixture();
        for (pointer, value) in [
            ("/available", json!(false)), ("/worker_generation", json!(5)),
            ("/selection_id", json!("00000000-0000-4000-8000-000000000002")),
            ("/revision", json!(9)), ("/received_at_ms", json!(9999)),
            ("/snapshot/generation", json!(8)), ("/snapshot/radio/connected", json!(false)),
            ("/snapshot/radio/observed", json!(false)), ("/snapshot/radio/transmitting", json!(true)),
            ("/snapshot/radio/transmit_vfo", json!("B")),
            ("/snapshot/carrier_tune/pending", json!(true)),
            ("/snapshot/carrier_tune/release_required", json!(true)),
            ("/snapshot/ptt/release_required", json!(true)),
        ] {
            let mut changed = radio.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(admission_for(&selected, &changed, &t, true, 10000, &cw, Some("A")).is_err(), "{pointer}");
        }
        let mut busy = cw.clone(); busy["armed"] = json!(true);
        assert!(admission_for(&selected, &radio, &t, true, 10000, &busy, Some("A")).is_err());
        let mut changed = selected.clone(); changed["control_revision"] = json!(4);
        assert!(admission_for(&changed, &radio, &t, true, 10000, &cw, Some("A")).is_err());
    }

    #[test]
    fn manual_flex_and_tci_do_not_gain_heartbeat_admission() {
        let (mut t, mut selected, mut radio, cw) = fixture();
        radio["snapshot"]["allow_transmit"] = json!(true);
        radio["snapshot"]["radio"]["receive_vfo"] = json!("A");
        radio["snapshot"]["radio"]["vfos"][0]["receive"] = json!(true);
        assert!(admission(&selected, &radio, &t, true, 10000, &cw).is_err());
        radio["snapshot"]["radio"]["observed_at"] = json!(10.0);
        assert!(admission(&selected, &radio, &t, true, 10000, &cw).is_ok());
        t.provider = "direct_tci".into(); t.tci = Some(TCITarget { trx: 1, channel: 1 });
        selected["provider"] = json!(t.provider);
        radio["provider"] = json!(t.provider); radio["snapshot"]["source"] = json!(t.provider);
        radio["snapshot"]["tci"] = json!({"trx":1,"channel":1,"transmit_enabled":true,"trx_observed_at":10.0});
        assert!(admission(&selected, &radio, &t, true, 10000, &cw).is_ok());
        radio["snapshot"]["radio"]["observed_at"] = json!(1.0);
        assert!(admission(&selected, &radio, &t, true, 10000, &cw).is_err());
    }

    #[test]
    fn exact_release_remains_independent_of_liveness() {
        let (t, selected, mut radio, cw) = fixture();
        radio["snapshot"]["heartbeat_at"] = Value::Null;
        radio["snapshot"]["radio"]["connected"] = json!(false);
        assert!(admission_for(&selected, &radio, &t, false, 999999, &cw, Some("A")).is_ok());
        radio["worker_generation"] = json!(5);
        assert!(admission_for(&selected, &radio, &t, false, 999999, &cw, Some("A")).is_err());
    }
}
