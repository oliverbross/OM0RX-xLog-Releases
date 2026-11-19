//! Offline adapter only: no endpoint, live context builder, device, or audio I/O.
//! Preparing or committing its owner Body is not an actual-start/on-air receipt.
use crate::{digital_tx_owner::{Body, Observation}, prepared_period::{Mode, ValidatedPeriod}};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyntheticPowerPolicy {
    pub epoch: String,
    pub radio_milliwatts: u32,
    pub ceiling_milliwatts: u32,
    pub amplifier_enabled: bool,
    pub digital_allowed: bool,
}

/// Supplied explicitly by offline fixtures. No defaults, inference from a radio
/// identifier, or live construction path exists. Every field participates in
/// equality and in the canonical context carried by the owner journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyntheticContext {
    pub server_instance: String,
    pub workspace_generation: String,
    pub worker_generation: u64,
    pub settings_revision: u64,
    pub identity_epoch: String,
    pub identity_revision: u64,
    pub radio_session: String,
    pub selection_epoch: String,
    pub receive_slice: String,
    pub transmit_slice: String,
    pub dax_route_epoch: String,
    pub dax_device_id: String,
    pub mode: Mode,
    pub band: String,
    pub frequency_hz: u64,
    pub antenna: String,
    pub power_policy: SyntheticPowerPolicy,
    pub reservation_epoch: String,
    pub clock_epoch: String,
}
impl SyntheticContext {
    fn key(&self) -> Result<String, String> {
        for id in [&self.server_instance, &self.workspace_generation, &self.identity_epoch,
            &self.radio_session, &self.selection_epoch, &self.dax_route_epoch,
            &self.power_policy.epoch, &self.reservation_epoch, &self.clock_epoch] {
            let uuid = uuid::Uuid::parse_str(id).map_err(|_| "Invalid synthetic context epoch")?;
            if uuid.is_nil() || uuid.to_string() != *id { return Err("Invalid synthetic context epoch".into()); }
        }
        for value in [&self.receive_slice, &self.transmit_slice, &self.dax_device_id, &self.band, &self.antenna] {
            if value.is_empty() || value.len() > 128 || value.trim() != value.as_str()
                || !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                return Err("Invalid synthetic route field".into());
            }
        }
        if self.worker_generation == 0 || self.worker_generation > 9_007_199_254_740_990
            || self.settings_revision > 9_007_199_254_740_990 || self.identity_revision > 9_007_199_254_740_990
            || self.frequency_hz == 0 || self.frequency_hz > 6_000_000_000
            || self.power_policy.radio_milliwatts > self.power_policy.ceiling_milliwatts
            || self.power_policy.ceiling_milliwatts > 1_000_000 || !self.power_policy.digital_allowed {
            return Err("Invalid synthetic operating policy".into());
        }
        serde_json::to_string(self).map_err(|_| "Synthetic context serialization failed".into())
    }
    pub(crate) fn observe(&self, now_us: u64) -> Result<SyntheticObservation, String> {
        Ok(SyntheticObservation { key: self.key()?, clock_epoch: self.clock_epoch.clone(), now_us })
    }
}
pub(crate) struct SyntheticObservation { key: String, clock_epoch: String, now_us: u64 }
impl SyntheticObservation {
    pub(crate) fn owner(&self) -> Observation<'_> {
        Observation { context: &self.key, clock_epoch: &self.clock_epoch, now_us: self.now_us }
    }
}

pub(crate) struct PreparedOperation { body: Body, period: Arc<ValidatedPeriod> }
impl PreparedOperation {
    pub(crate) fn new(operation_id: String, period: ValidatedPeriod, context: &SyntheticContext,
        slot_start_us: u64, latest_start_us: u64) -> Result<Self, String> {
        let id = uuid::Uuid::parse_str(&operation_id).map_err(|_| "Invalid prepared operation identity")?;
        let duration = period.manifest().period_duration_us();
        if id.is_nil() || id.to_string() != operation_id || context.mode != period.manifest().mode()
            || slot_start_us % duration != 0 || latest_start_us < slot_start_us
            || slot_start_us.checked_add(duration).is_none_or(|end| latest_start_us >= end) {
            return Err("Invalid prepared operation slot or mode".into());
        }
        let body = Body { operation_id, manifest: format!("prepared-period-body-v1:{}", period.body_sha256()),
            context: context.key()?, clock_epoch: context.clock_epoch.clone(), slot_start_us,
            slot_duration_us: duration, latest_start_us };
        Ok(Self { body, period: Arc::new(period) })
    }
    pub(crate) fn body(&self) -> &Body { &self.body }
    pub(crate) fn period(&self) -> &ValidatedPeriod { &self.period }
}
