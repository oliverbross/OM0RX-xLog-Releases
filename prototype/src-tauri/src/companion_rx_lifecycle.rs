//! One native receive thread remains charged until its device calls return.
//! Revoking logical authority never releases this resource reservation early.
use std::sync::atomic::{AtomicBool, Ordering};

pub struct NativePermit<'a>(&'a AtomicBool);
impl<'a> NativePermit<'a> {
    pub fn acquire(owner: &'a AtomicBool) -> Result<Self, String> {
        owner.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "The previous receive owner is still running or stopping. Stop it and wait for device shutdown before restarting.".to_owned())?;
        Ok(Self(owner))
    }
}
impl Drop for NativePermit<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// Called while holding the owner's CONTROL mutex, before any completion
/// effect (Stop, persistence or reporting success).
pub fn require_current(expected: u64, current: u64) -> Result<(), String> {
    if expected == current {
        Ok(())
    } else {
        Err("Receive startup was superseded; its result was discarded.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_stop_cannot_admit_another_hung_native_open() {
        let owner = AtomicBool::new(false);
        let first = NativePermit::acquire(&owner).unwrap();
        for _ in 0..100 {
            assert!(NativePermit::acquire(&owner).is_err());
        }
        drop(first);
        assert!(NativePermit::acquire(&owner).is_ok());
    }
    #[test]
    fn stale_startup_cannot_stop_save_or_report_new_session() {
        let mut effects = vec![];
        for completion in ["error", "timeout", "save", "success"] {
            if require_current(4, 5).is_ok() {
                effects.push(completion);
            }
        }
        assert!(effects.is_empty());
        assert!(require_current(5, 5).is_ok());
    }
}
