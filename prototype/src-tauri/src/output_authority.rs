//! Shared output admission and revocable intent. No transport or hardware I/O.
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard, OnceLock, atomic::{AtomicU8, Ordering}};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reservation {
    operation_id: String,
    nonce: String,
    intent: String,
}
impl Reservation {
    pub(crate) fn operation_id(&self) -> &str { &self.operation_id }
    pub(crate) fn intent_id(&self) -> &str { &self.intent }
    pub(crate) fn lease_id(&self) -> &str { &self.nonce }
    pub(crate) fn validate(&self) -> Result<(), String> {
        for value in [&self.operation_id, &self.nonce, &self.intent] {
            let id = uuid::Uuid::parse_str(value).map_err(|_| "Invalid output reservation")?;
            if id.is_nil() || id.to_string() != *value { return Err("Invalid output reservation".into()); }
        }
        Ok(())
    }
}
struct State {
    intent: String,
    reservation: Option<Reservation>,
}
pub(crate) struct Authority {
    acquisition: Mutex<()>,
    state: Mutex<State>,
}
impl Default for Authority {
    fn default() -> Self {
        Self { acquisition: Mutex::new(()), state: Mutex::new(State {
            intent: uuid::Uuid::new_v4().to_string(), reservation: None,
        }) }
    }
}
impl Authority {
    fn state(&self) -> Result<MutexGuard<'_, State>, String> {
        self.state.lock().map_err(|_| "Output intent unavailable".into())
    }
    fn acquire(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.acquisition.try_lock().map_err(|_| {
            "A radio/CW acquisition is in progress. Stop and Cancel remain available.".into()
        })
    }
    pub(crate) fn intent(&self) -> Result<String, String> { Ok(self.state()?.intent.clone()) }
    pub(crate) fn revoke(&self) -> Result<(), String> {
        // Keep an unresolved reservation visible to every competing owner.
        self.state()?.intent = uuid::Uuid::new_v4().to_string();
        Ok(())
    }
    /// Release-only digital Stop must use this exact comparison, never the
    /// unconditional global user-Stop operation. Repeated Stop retains the duty.
    pub(crate) fn revoke_exact(&self, ticket: &Reservation) -> Result<bool, String> {
        let mut state = self.state()?;
        if state.reservation.as_ref() != Some(ticket) { return Ok(false); }
        if state.intent == ticket.intent { state.intent = uuid::Uuid::new_v4().to_string(); }
        Ok(true)
    }
    pub(crate) fn require_intent(&self, expected: &str) -> Result<(), String> {
        if self.intent()? != expected {
            return Err("A later PTT off/Cancel retired this acquisition preparation".into());
        }
        Ok(())
    }
    pub(crate) fn reserve_ordinary(&self) -> Result<MutexGuard<'_, ()>, String> {
        let guard = self.acquire()?;
        if self.state()?.reservation.is_some() {
            return Err("A retained output operation owns admission; its exact release remains available.".into());
        }
        Ok(guard)
    }
    pub(crate) fn prepare(&self, operation_id: &str, expected_intent: &str) -> Result<Reservation, String> {
        let id = uuid::Uuid::parse_str(operation_id).map_err(|_| "Invalid output operation identity")?;
        if id.is_nil() { return Err("Invalid output operation identity".into()); }
        let _guard = self.acquire()?;
        let mut state = self.state()?;
        if state.intent != expected_intent || state.reservation.is_some() {
            return Err("Output preparation is retired or another operation owns admission.".into());
        }
        let ticket = Reservation { operation_id: id.to_string(), nonce: uuid::Uuid::new_v4().to_string(), intent: state.intent.clone() };
        state.reservation = Some(ticket.clone());
        Ok(ticket)
    }
    pub(crate) fn require_exact(&self, ticket: &Reservation) -> Result<(), String> {
        let state = self.state()?;
        if state.reservation.as_ref() != Some(ticket) || state.intent != ticket.intent {
            return Err("Exact output preparation was retired.".into());
        }
        Ok(())
    }
    /// Linearize a simulated effect transition with global/exact revocation.
    /// Only an atomic state transition runs under this short lock; no callback,
    /// journal, transport or audio operation can hold up Stop here.
    pub(crate) fn compare_exchange_exact(&self, ticket: &Reservation, phase: &AtomicU8,
        expected: u8, next: u8) -> Result<bool, String> {
        let state = self.state()?;
        if state.reservation.as_ref() != Some(ticket) || state.intent != ticket.intent {
            return Err("Exact output preparation was retired.".into());
        }
        Ok(phase.compare_exchange(expected, next, Ordering::SeqCst, Ordering::SeqCst).is_ok())
    }
    pub(crate) fn reserve_exact(&self, ticket: &Reservation) -> Result<MutexGuard<'_, ()>, String> {
        let guard = self.acquire()?;
        self.require_exact(ticket)?;
        Ok(guard)
    }
    /// Restore a durable release duty, never permission to start again.
    pub(crate) fn restore_unresolved(&self, ticket: &Reservation) -> Result<(), String> {
        ticket.validate()?;
        let _guard = self.acquire()?;
        let mut state = self.state()?;
        if state.reservation.as_ref().is_some_and(|current| current != ticket) {
            return Err("Another exact output reservation is retained.".into());
        }
        state.intent = uuid::Uuid::new_v4().to_string();
        state.reservation = Some(ticket.clone());
        Ok(())
    }
    /// Call only after durable no-start or exact observed-release proof. This
    /// retires bookkeeping; it does not command or prove physical release.
    pub(crate) fn retire_exact(&self, ticket: &Reservation) -> Result<bool, String> {
        let mut state = self.state()?;
        if state.reservation.as_ref() != Some(ticket) { return Ok(false); }
        if state.intent == ticket.intent { state.intent = uuid::Uuid::new_v4().to_string(); }
        state.reservation = None;
        Ok(true)
    }
    /// Only after durable exact observed OFF and native output closure. A
    /// preceding Stop cannot obtain a continuation token; a following Stop
    /// invalidates the returned token normally. Never accepts an arbitrary
    /// current intent as evidence of this operation's normal completion.
    pub(crate) fn complete_exact(&self, ticket: &Reservation) -> Result<Option<String>, String> {
        let mut state = self.state()?;
        if state.reservation.as_ref() != Some(ticket) || state.intent != ticket.intent {
            return Ok(None);
        }
        state.intent = uuid::Uuid::new_v4().to_string();
        state.reservation = None;
        Ok(Some(state.intent.clone()))
    }
}
static AUTHORITY: OnceLock<Authority> = OnceLock::new();
pub(crate) fn shared() -> &'static Authority { AUTHORITY.get_or_init(Authority::default) }

// Compatibility entry points used by current CW/voice/Tune/PTT callers.
pub fn output_intent() -> Result<String, String> { shared().intent() }
pub fn revoke_output_intent() -> Result<(), String> { shared().revoke() }
pub fn require_output_intent(expected: &str) -> Result<(), String> { shared().require_intent(expected) }
pub fn reserve_output() -> Result<MutexGuard<'static, ()>, String> { shared().reserve_ordinary() }

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    fn prepare(owner: &Authority) -> Reservation {
        owner.prepare(&uuid::Uuid::new_v4().to_string(), &owner.intent().unwrap()).unwrap()
    }
    #[test]
    fn normal_completion_continues_only_its_own_exact_epoch_and_stop_always_wins() {
        let owner=Authority::default();
        let first=prepare(&owner);
        let successor=owner.complete_exact(&first).unwrap().unwrap();
        assert!(owner.require_intent(&successor).is_ok());
        assert!(owner.require_exact(&first).is_err());
        let second=owner.prepare(&uuid::Uuid::new_v4().to_string(),&successor).unwrap();
        owner.revoke().unwrap();
        assert_eq!(owner.complete_exact(&second).unwrap(),None,"Stop before completion cannot re-arm");
        owner.retire_exact(&second).unwrap();
        let third=prepare(&owner);
        let successor=owner.complete_exact(&third).unwrap().unwrap();
        owner.revoke().unwrap();
        assert!(owner.require_intent(&successor).is_err(),"Stop after completion still retires next admission");
        assert_eq!(owner.complete_exact(&third).unwrap(),None,"duplicate completion cannot learn a newer intent");
    }
    #[test]
    fn effect_transition_rechecks_revocation_after_an_earlier_valid_ticket() {
        let owner = Authority::default();
        let ticket = prepare(&owner);
        let phase = AtomicU8::new(0);
        owner.require_exact(&ticket).unwrap();
        owner.revoke().unwrap();
        assert!(owner.compare_exchange_exact(&ticket, &phase, 0, 1).is_err());
        assert_eq!(phase.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn exact_transition_never_overwrites_stop_or_a_different_reservation() {
        let owner = Authority::default();
        let ticket = prepare(&owner);
        let phase = AtomicU8::new(2);
        assert!(!owner.compare_exchange_exact(&ticket, &phase, 0, 1).unwrap());
        assert_eq!(phase.load(Ordering::SeqCst), 2);
        let foreign = prepare(&Authority::default());
        assert!(owner.compare_exchange_exact(&foreign, &phase, 2, 1).is_err());
        let ready = AtomicU8::new(0);
        assert!(owner.compare_exchange_exact(&ticket, &ready, 0, 1).unwrap());
        assert!(!owner.compare_exchange_exact(&ticket, &ready, 0, 1).unwrap());
    }
    #[test]
    fn current_owners_keep_nonblocking_shared_acquisition_and_revocation() {
        let owner = Arc::new(Authority::default());
        for role in ["CW", "voice", "carrier", "PTT", "fldigi"] {
            let intent = owner.intent().unwrap();
            let guard = owner.reserve_ordinary().unwrap();
            let other = owner.clone();
            assert!(std::thread::spawn(move || other.reserve_ordinary().is_err()).join().unwrap(), "{role}");
            owner.revoke().unwrap();
            assert!(owner.require_intent(&intent).is_err());
            drop(guard);
            assert!(owner.reserve_ordinary().is_ok());
        }
    }
    #[test]
    fn prepared_and_revoked_operations_block_competing_owners_until_exact_retirement() {
        let owner = Authority::default();
        let ticket = prepare(&owner);
        assert!(owner.reserve_ordinary().is_err());
        assert!(owner.reserve_exact(&ticket).is_ok());
        owner.revoke().unwrap();
        assert!(owner.reserve_exact(&ticket).is_err());
        assert!(owner.reserve_ordinary().is_err());
        assert!(owner.retire_exact(&ticket).unwrap());
        assert!(owner.reserve_ordinary().is_ok());
    }
    #[test]
    fn stop_revocation_and_exact_retirement_bypass_held_acquisition() {
        let owner = Arc::new(Authority::default());
        let ticket = prepare(&owner);
        let guard = owner.reserve_exact(&ticket).unwrap();
        let other = owner.clone();
        let exact = ticket.clone();
        let (sent, received) = mpsc::channel();
        let stop = std::thread::spawn(move || {
            assert!(other.revoke_exact(&exact).unwrap());
            // Fixture supplies no-start proof; real retirement requires proof.
            sent.send(other.retire_exact(&exact).unwrap()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(owner.require_exact(&ticket).is_err(), "late callback must fail");
        drop(guard);
        stop.join().unwrap();
    }
    #[test]
    fn stale_ticket_cannot_clear_or_revoke_replacement_even_for_same_operation_id() {
        let owner = Authority::default();
        let old = prepare(&owner);
        owner.retire_exact(&old).unwrap();
        let current = owner.prepare(old.operation_id(), &owner.intent().unwrap()).unwrap();
        let current_intent = owner.intent().unwrap();
        assert!(!owner.revoke_exact(&old).unwrap());
        assert!(!owner.retire_exact(&old).unwrap());
        assert_eq!(owner.intent().unwrap(), current_intent);
        assert!(owner.reserve_exact(&current).is_ok());
        assert!(owner.reserve_ordinary().is_err());
        // The durable owner journal separately forbids consuming an ID twice.
    }
    #[test]
    fn stale_digital_stop_cannot_invalidate_a_later_ordinary_owner() {
        let owner = Authority::default();
        let old = prepare(&owner);
        owner.retire_exact(&old).unwrap();
        let ordinary_intent = owner.intent().unwrap();
        let guard = owner.reserve_ordinary().unwrap();
        assert!(!owner.revoke_exact(&old).unwrap());
        assert!(!owner.retire_exact(&old).unwrap());
        assert!(owner.require_intent(&ordinary_intent).is_ok());
        drop(guard);
    }
    #[test]
    fn recovery_restores_only_a_release_duty_and_never_rearms() {
        let prior = Authority::default();
        let ticket = prepare(&prior);
        let recovered = Authority::default();
        recovered.restore_unresolved(&ticket).unwrap();
        assert!(recovered.reserve_exact(&ticket).is_err());
        assert!(recovered.reserve_ordinary().is_err());
        assert!(recovered.restore_unresolved(&prepare(&Authority::default())).is_err());
        assert!(recovered.retire_exact(&ticket).unwrap());
        assert!(recovered.reserve_ordinary().is_ok());
    }
    #[test]
    fn invalid_or_retired_preparation_cannot_reserve_output() {
        let owner = Authority::default();
        for id in ["radio1", "", "00000000-0000-0000-0000-000000000000"] {
            assert!(owner.prepare(id, &owner.intent().unwrap()).is_err());
        }
        let expired = owner.intent().unwrap();
        owner.revoke().unwrap();
        assert!(owner.prepare(&uuid::Uuid::new_v4().to_string(), &expired).is_err());
        assert!(owner.reserve_ordinary().is_ok());
    }
}
