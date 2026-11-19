//! Device-free RX route, sample-clock and bounded retention rules.
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
const MAX_EXACT: u64 = 9_007_199_254_740_990;
// The receiver reconstructs contiguous sample time and reports measured host
// mapping uncertainty. Match its 50 ms budget without rewriting packet times.
const MAX_HOST_MAPPING_US: u128 = 50_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub device_id: String,
    pub radio_id: String,
    pub receiver_id: String,
    pub dax_channel: u8,
    pub sample_rate: u32,
    pub source_channels: u16,
    pub selected_channel: u16,
    pub operator_confirmed: bool,
}
impl Binding {
    pub fn validate(&self) -> Result<(), String> {
        if !self.operator_confirmed
            || self.device_id.len() > 1024
            || self.device_id.is_empty()
            || !matches!(self.radio_id.as_str(), "radio1" | "radio2")
            || self.receiver_id.is_empty()
            || self.receiver_id.len() > 256
            || !(1..=8).contains(&self.dax_channel)
            || !(8000..=192000).contains(&self.sample_rate)
            || !(1..=16).contains(&self.source_channels)
            || self.selected_channel >= self.source_channels
        {
            return Err(
                "Confirm the exact FLEX slice A / DAX input association and native format.".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Route {
    pub slice: String,
    pub radio_id: String,
    pub receiver_id: String,
    pub connection_generation: u64,
    pub selection_generation: u64,
    pub dax_channel: u8,
    pub frequency_hz: u64,
    pub mode: String,
}
fn timely(v: &Value, now: u64, seconds: bool) -> bool {
    let t = if seconds {
        v.as_f64().map(|v| v * 1000.0)
    } else {
        v.as_u64().map(|v| v as f64)
    };
    t.is_some_and(|t| t.is_finite() && t >= 0.0 && t <= now as f64 && now as f64 - t <= 5000.0)
}
/// Reads only retained native subscription evidence; never asks a device.
pub fn route(p: &Value, now: u64) -> Option<Route> {
    route_for(p, now, "A")
}
pub fn route_for(p: &Value, now: u64, letter: &str) -> Option<Route> {
    if !matches!(letter, "A" | "B") {
        return None;
    }
    let s = &p["snapshot"];
    let r = &s["radio"];
    if p["provider"] != "direct_flex_lan"
        || p["available"] != true
        || !timely(&p["received_at_ms"], now, false)
        || !timely(&s["generated_at"], now, true)
        || !(timely(&r["observed_at"], now, true) || timely(&s["heartbeat_at"], now, true))
        || r["connected"] != true
        || r["observed"] != true
        // Passive capture may contain DAX silence during TX. A known PTT change
        // alone does not replace the receiver; unknown state still rejects.
        || !r["transmitting"].is_boolean()
    {
        return None;
    }
    let vfos = r["vfos"].as_array()?;
    let mut slices = vfos.iter().filter(|v| v["letter"] == letter);
    let a = slices.next()?;
    if slices.next().is_some() {
        return None;
    }
    let selection = p["selection_id"].as_str()?;
    uuid::Uuid::parse_str(selection).ok()?;
    let client = a["flex_client_handle"].as_str()?;
    if client.is_empty()
        || client.len() > 32
        || !client.bytes().all(|b| b.is_ascii_hexdigit() || b == b'x')
    {
        return None;
    }
    let slice = a["flex_slice_id"].as_u64().filter(|v| *v <= 255)?;
    let dax = a["dax_channel"].as_u64().filter(|v| (1..=8).contains(v))?;
    let mode = a["mode"].as_str()?.to_owned();
    if mode.is_empty() || mode == "?" || mode.len() > 32 {
        return None;
    }
    Some(Route {
        slice: letter.into(),
        radio_id: p["radio_id"].as_str()?.to_owned(),
        receiver_id: format!("{selection}/{client}/{slice}"),
        connection_generation: s["generation"].as_u64()?,
        selection_generation: p["selection_generation"].as_u64()?,
        dax_channel: dax as u8,
        frequency_hz: a["frequency_hz"]
            .as_u64()
            .filter(|v| (1..=1_000_000_000_000).contains(v))?,
        mode,
    })
}
pub fn device_name_channel(name: &str) -> Option<u8> {
    // Exact documented DAX RX names only, never reserved/monitor/TX endpoints.
    for n in 1..=8 {
        if name == format!("DAX Audio RX {n}")
            || name == format!("DAX RX {n}")
            || name == format!("DAX RX {n} (FlexRadio DAX)")
        {
            return Some(n);
        }
    }
    None
}
#[derive(Clone)]
struct Chunk {
    sequence: u64,
    first: u64,
    time: u64,
    uncertainty: u32,
    samples: Vec<i16>,
}
pub struct Buffer {
    pub descriptor: Option<Value>,
    pub reason: Option<String>,
    chunks: VecDeque<Chunk>,
    next: u64,
    frames: usize,
    rate: u32,
    last: Option<(u64, u64, u64)>, // next device frame, QPC, host UTC
    anchor: Option<(u64, u64)>,    // first frame, exported UTC microseconds
    timestamp_residual_range: Option<(i128, i128)>, // microseconds * sample rate
    max_calibration_us: u64,
    gap_times: VecDeque<u64>, // endpoint QPC, 100 ns; rolling recovery budget
    recovered_gaps: u64,
    recovered_timing_intervals: u64,
}
impl Default for Buffer {
    fn default() -> Self {
        Self {
            descriptor: None,
            reason: Some("RX sharing is stopped.".into()),
            chunks: VecDeque::new(),
            next: 1,
            frames: 0,
            rate: 0,
            last: None,
            anchor: None,
            timestamp_residual_range: None,
            max_calibration_us: 0,
            gap_times: VecDeque::new(),
            recovered_gaps: 0,
            recovered_timing_intervals: 0,
        }
    }
}
impl Buffer {
    pub fn start(
        &mut self,
        binding: &Binding,
        route: &Route,
        generation: u64,
    ) -> Result<(), String> {
        binding.validate()?;
        if generation > MAX_EXACT
            || route.radio_id != binding.radio_id
            || route.receiver_id != binding.receiver_id
            || route.dax_channel != binding.dax_channel
        {
            return Err("Receive binding does not match the current owner route.".into());
        }
        self.stop("Waiting for a valid capture timestamp.");
        self.gap_times.clear();
        self.recovered_gaps = 0;
        self.recovered_timing_intervals = 0;
        self.rate = binding.sample_rate;
        self.descriptor = Some(
            json!({"capture_id":uuid::Uuid::new_v4().to_string(),"epoch":uuid::Uuid::new_v4().to_string(),
            "radio_id":route.radio_id,"receiver_id":route.receiver_id,"mapping_generation":generation,
            "device_id":binding.device_id,"slice":route.slice,"dax_channel":route.dax_channel,"frequency_hz":route.frequency_hz,"mode":route.mode,
            "source_channels":binding.source_channels,"selected_channel":binding.selected_channel,"sample_rate":binding.sample_rate,
            "sample_format":"s16le","timestamp_basis":"windows_endpoint_capture","remote_rf_latency_us":null,"utc_sync_accuracy_us":null}),
        );
        Ok(())
    }
    pub fn stop(&mut self, reason: &str) {
        self.descriptor = None;
        self.reason = Some(reason.into());
        self.chunks.clear();
        self.frames = 0;
        self.last = None;
        self.anchor = None;
        self.timestamp_residual_range = None;
        self.max_calibration_us = 0;
    }
    pub fn push(
        &mut self,
        first: u64,
        qpc: u64,
        utc: u64,
        uncertainty: u64,
        samples: &[i16],
    ) -> Result<bool, String> {
        // true means PCM was published; false means an interval was discarded.
        // Only true may extend the capture loop's qualified-audio watchdog.
        if !self.valid_packet(first, qpc, utc, uncertainty, samples.len(), false) {
            self.stop("Capture counter or host clock discontinuity; restart RX sharing.");
            return Err(self.reason.clone().unwrap());
        }
        self.push_valid(first, qpc, utc, uncertainty, samples)
    }
    fn valid_packet(
        &self,
        first: u64,
        qpc: u64,
        utc: u64,
        uncertainty: u64,
        frames: usize,
        gap: bool,
    ) -> bool {
        self.descriptor.is_some()
            && frames > 0
            && frames <= 4096
            && qpc > 0
            && utc > 0
            && uncertainty <= 10100
            && utc / 10 <= MAX_EXACT
            && self.next < MAX_EXACT
            && first
                .checked_add(frames as u64)
                .is_some_and(|end| end <= MAX_EXACT)
            && self.last.is_none_or(|(next, old_qpc, old_utc)| {
                (first == next || (gap && first > next))
                    && qpc > old_qpc
                    && qpc.abs_diff(old_qpc).abs_diff(utc.abs_diff(old_utc)) <= 20000
                    && utc > old_utc
            })
    }
    /// Consume a declared WASAPI gap without exporting its samples. Keep the
    /// same endpoint/route and global cursor, but revoke the partial decoder
    /// period using a new descriptor epoch. Clock history survives this reset.
    pub fn discard_gap(
        &mut self,
        first: u64,
        qpc: u64,
        utc: u64,
        uncertainty: u64,
        frames: usize,
    ) -> Result<(), String> {
        self.discard_interval(first, qpc, utc, uncertainty, frames, true)
    }
    fn discard_interval(
        &mut self,
        first: u64,
        qpc: u64,
        utc: u64,
        uncertainty: u64,
        frames: usize,
        device_gap: bool,
    ) -> Result<(), String> {
        if !self.valid_packet(first, qpc, utc, uncertainty, frames, device_gap) {
            self.stop(
                "Receive gap contained invalid counters or clock timestamps; restart sharing.",
            );
            return Err(self.reason.clone().unwrap());
        }
        while self
            .gap_times
            .front()
            .is_some_and(|t| qpc.saturating_sub(*t) >= 600_000_000)
        {
            self.gap_times.pop_front();
        }
        if self.gap_times.len() >= 6 {
            self.stop("Receive input has repeated audio/timing gaps (more than six in 60 seconds); check DAX and restart sharing.");
            return Err(self.reason.clone().unwrap());
        }
        self.gap_times.push_back(qpc);
        if device_gap {
            self.recovered_gaps += 1;
        } else {
            self.recovered_timing_intervals += 1;
        }
        self.last = Some((first + frames as u64, qpc, utc));
        self.descriptor.as_mut().unwrap()["epoch"] = json!(uuid::Uuid::new_v4().to_string());
        self.chunks.clear();
        self.frames = 0;
        self.anchor = None;
        self.timestamp_residual_range = None;
        self.max_calibration_us = 0;
        self.reason =
            Some("Receive interval discarded; waiting for fresh timestamped samples.".into());
        Ok(())
    }
    fn push_valid(
        &mut self,
        first: u64,
        qpc: u64,
        utc: u64,
        uncertainty: u64,
        samples: &[i16],
    ) -> Result<bool, String> {
        // DAX delivers continuous frames in packets whose endpoint timestamps
        // have scheduling jitter. A 2 ms packet/nominal-rate equality test rejects
        // valid audio. Preserve those measured timestamps; bound their FULL
        // epoch range instead. For any subscriber's first packet j, the receiver
        // observes |R_i - R_j| / rate, so range + calibration bounds late joins
        // too. Calculate in the exact truncated microseconds exported on wire.
        // These extrema deliberately outlive the two-second PCM retention.
        let (frame, wall_us) = *self.anchor.get_or_insert((first, utc / 10));
        let residual = (i128::from(utc / 10) - i128::from(wall_us)) * i128::from(self.rate)
            - i128::from(first - frame) * 1_000_000;
        let (low, high) = self.timestamp_residual_range.unwrap_or((0, 0));
        let low = low.min(residual);
        let high = high.max(residual);
        let calibration_us = uncertainty.div_ceil(10);
        let max_calibration_us = self.max_calibration_us.max(calibration_us);
        let bound_us =
            ((high - low) as u128).div_ceil(u128::from(self.rate)) + u128::from(max_calibration_us);
        if bound_us > MAX_HOST_MAPPING_US {
            // Reject the entire partial decoding interval, not the endpoint.
            // No out-of-budget packet is exported or used as the next anchor.
            // Physical clock checks above and the shared recovery/watchdog
            // limits still apply; consumers reset on the explicit new epoch.
            return self
                .discard_interval(first, qpc, utc, uncertainty, samples.len(), false)
                .map(|()| false);
        }
        self.timestamp_residual_range = Some((low, high));
        self.max_calibration_us = max_calibration_us;
        self.last = Some((first + samples.len() as u64, qpc, utc));
        self.chunks.push_back(Chunk {
            sequence: self.next,
            first,
            time: utc / 10,
            uncertainty: ((uncertainty + 9) / 10) as u32,
            samples: samples.to_vec(),
        });
        self.next += 1;
        self.frames += samples.len();
        self.reason = None;
        while self.frames > self.rate as usize * 2 || self.chunks.len() > 512 {
            if let Some(c) = self.chunks.pop_front() {
                self.frames -= c.samples.len();
            }
        }
        Ok(true)
    }
    pub fn running(&self, now: u64) -> bool {
        self.descriptor.is_some()
            && self.reason.is_none()
            && self
                .chunks
                .back()
                .is_some_and(|c| now >= c.time / 1000 && now - c.time / 1000 <= 1000)
    }
    pub fn response(&self, after: u64, now: u64) -> Value {
        let running = self.running(now);
        let oldest = self.chunks.front().map(|c| c.sequence).unwrap_or(self.next);
        let chosen: Vec<_> = self
            .chunks
            .iter()
            .filter(|c| running && c.sequence > after)
            .take(32)
            .collect();
        let next = chosen
            .last()
            .map(|c| c.sequence + 1)
            .unwrap_or(after.saturating_add(1).min(MAX_EXACT));
        let chunks: Vec<_> = chosen.iter().map(|c| { let bytes: Vec<u8> = c.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            json!({"sequence":c.sequence,"first_frame":c.first,"frame_count":c.samples.len(),"sample0_unix_us":c.time,
                "calibration_uncertainty_us":c.uncertainty,"pcm_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}) }).collect();
        json!({"generated_at_unix_ms":now,"max_age_ms":1000,"state":if running {"running"} else {"stopped"},
            "diagnostics":{"recovered_audio_gaps":self.recovered_gaps,
                "discarded_timing_intervals":self.recovered_timing_intervals},
            "reason":if running {Value::Null} else {json!(self.reason.as_deref().unwrap_or("Capture timestamps expired."))},
            "descriptor":if running {self.descriptor.clone().unwrap()} else {Value::Null},
            "oldest_sequence":oldest,"next_sequence":next,"gap":after > 0 && (after < oldest.saturating_sub(1) || after >= self.next),"chunks":chunks})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Binding, Route) {
        let b = Binding {
            device_id: "wasapi:fixture".into(),
            radio_id: "radio1".into(),
            receiver_id: "receiver".into(),
            dax_channel: 2,
            sample_rate: 48000,
            source_channels: 2,
            selected_channel: 1,
            operator_confirmed: true,
        };
        let r = Route {
            slice: "A".into(),
            radio_id: b.radio_id.clone(),
            receiver_id: b.receiver_id.clone(),
            connection_generation: 1,
            selection_generation: 1,
            dax_channel: 2,
            frequency_hz: 14074000,
            mode: "DIGU".into(),
        };
        (b, r)
    }
    #[test]
    fn saved_audio_configured_b_descriptor_and_qsy_start_get_fresh_capture_without_old_pcm() {
        let (b, mut r) = fixture();
        r.slice = "B".into();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[7; 480]).unwrap();
        let first = q.response(0, 1000);
        assert_eq!(first["descriptor"]["slice"], "B");
        assert!(q.running(1000));
        r.frequency_hz += 1000;
        q.start(&b, &r, 2).unwrap();
        assert!(!q.running(1000));
        assert!(q.response(0, 1000)["chunks"].as_array().unwrap().is_empty());
        q.push(0, 20_000_000, 20_000_000, 100, &[8; 480]).unwrap();
        let next = q.response(0, 2000);
        assert_ne!(
            next["descriptor"]["capture_id"],
            first["descriptor"]["capture_id"]
        );
        assert_eq!(next["descriptor"]["frequency_hz"], r.frequency_hz);
    }
    #[test]
    fn channel_names_never_guess_reserved_or_tx() {
        assert_eq!(device_name_channel("DAX Audio RX 2"), Some(2));
        assert_eq!(device_name_channel("DAX RX 2"), Some(2));
        assert_eq!(device_name_channel("DAX RX 8 (FlexRadio DAX)"), Some(8));
        for n in [
            "DAX RESERVED AUDIO RX 1",
            "DAX TX 1",
            "Microphone",
            "DAX Audio RX 9",
        ] {
            assert_eq!(device_name_channel(n), None);
        }
    }
    #[test]
    fn no_audio_authority_before_first_valid_timestamp() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        assert_eq!(q.response(0, 1000)["descriptor"], Value::Null);
    }
    #[test]
    fn selected_mono_samples_and_counter_gap_flush() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[-32768, 32767])
            .unwrap();
        let v = q.response(0, 1000);
        assert_eq!(v["chunks"][0]["pcm_base64"], "AID/fw==");
        assert_eq!(v["next_sequence"], 2);
        assert!(q.push(3, 10_000_417, 10_000_417, 100, &[1]).is_err());
        assert_eq!(q.response(1, 1000)["descriptor"], Value::Null);
    }
    #[test]
    fn wall_clock_jump_flushes_and_sequence_does_not_reset() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[1; 480]).unwrap();
        assert!(q.push(480, 10_100_000, 11_100_000, 100, &[1; 480]).is_err());
        q.start(&b, &r, 2).unwrap();
        q.push(0, 20_000_000, 20_000_000, 100, &[1; 480]).unwrap();
        assert_eq!(q.response(1, 2000)["chunks"][0]["sequence"], 2);
    }
    #[test]
    fn retention_is_bounded_and_overrun_reports_gap() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        for n in 0..220 {
            q.push(
                n * 480,
                10_000_000 + n * 100000,
                10_000_000 + n * 100000,
                100,
                &[1; 480],
            )
            .unwrap();
        }
        let v = q.response(1, 3200);
        assert_eq!(v["gap"], true);
        assert_eq!(v["chunks"].as_array().unwrap().len(), 32);
        assert!(q.frames <= 96000);
    }
    #[test]
    fn route_requires_owned_slice_dax_identity_and_current_evidence() {
        let mut p = json!({"provider":"direct_flex_lan","available":true,"radio_id":"radio1","received_at_ms":10000,
            "selection_id":uuid::Uuid::new_v4().to_string(),"selection_generation":2,
            "snapshot":{"generation":3,"generated_at":10.0,"heartbeat_at":10.0,"radio":{"connected":true,"observed":true,"transmitting":false,
            "vfos":[{"letter":"A","flex_slice_id":0,"flex_client_handle":"0x000000ab","dax_channel":2,"frequency_hz":14074000,"mode":"DIGU"}]}}});
        let expected = route(&p, 10000).unwrap();
        assert_eq!(expected.dax_channel, 2);
        p["snapshot"]["radio"]["transmitting"] = json!(true);
        assert_eq!(route(&p, 10000).unwrap(), expected);
        p["snapshot"]["radio"]["transmitting"] = json!(false);
        for field in ["dax_channel", "flex_slice_id", "flex_client_handle"] {
            let mut missing = p.clone();
            missing["snapshot"]["radio"]["vfos"][0][field] = Value::Null;
            assert!(route(&missing, 10000).is_none());
        }
        p["snapshot"]["radio"]["vfos"][0]["dax_channel"] = json!(0);
        assert!(route(&p, 10000).is_none());
        p["snapshot"]["radio"]["vfos"][0]["dax_channel"] = json!(2);
        assert!(route(&p, 15001).is_none());
        p["snapshot"]["radio"]["transmitting"] = Value::Null;
        assert!(route(&p, 10000).is_none());
        p["snapshot"]["radio"]["transmitting"] = json!(false);
        p["snapshot"]["radio"]["vfos"][0]["frequency_hz"] = json!(14075000);
        assert_ne!(route(&p, 10000).unwrap(), expected);
        p["snapshot"]["generation"] = json!(4);
        assert_ne!(route(&p, 10000).unwrap(), expected);
    }
    #[test]
    fn expired_audio_has_no_descriptor_or_samples() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[1; 480]).unwrap();
        let v = q.response(0, 2001);
        assert!(v["descriptor"].is_null());
        assert_eq!(v["chunks"], json!([]));
    }
    #[test]
    fn cumulative_clock_drift_cannot_hide_in_small_per_packet_steps() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[1; 480]).unwrap();
        for n in 1..500 {
            q.push(
                n * 480,
                10_000_000 + n * 101_000,
                10_000_000 + n * 101_000,
                100,
                &[1; 480],
            )
            .unwrap();
        }
        assert!(!q
            .push(500 * 480, 60_500_000, 60_500_000, 100, &[1; 480])
            .unwrap());
        assert!(q.chunks.is_empty());
        assert_eq!(q.recovered_timing_intervals, 1);
    }
    #[test]
    fn observed_dax_rx1_packet_jitter_keeps_contiguous_audio() {
        // Metadata only: unchanged WASAPI owner, Windows 10 DAX RX 1, 30 s.
        // Timestamps are relative; no recorded audio or endpoint identifiers.
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        for line in include_str!("../tests/fixtures/dax-rx1-timing.csv")
            .lines()
            .skip(1)
        {
            let p: Vec<u64> = line.split(',').map(|v| v.parse().unwrap()).collect();
            q.push(
                p[0],
                10_000_000 + p[1],
                10_000_000 + p[2],
                p[3],
                &vec![0; p[4] as usize],
            )
            .unwrap_or_else(|error| panic!("frame {}: {error}", p[0]));
        }
        assert!(q.reason.is_none());
        assert_eq!(q.next, 1199);
    }
    #[test]
    fn full_epoch_uncertainty_accepts_boundary_then_discards_one_more_microsecond() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[0; 1200]).unwrap();
        q.push(1200, 10_749_900, 10_749_900, 100, &[0; 1200])
            .unwrap();
        assert!(!q
            .push(2400, 10_999_910, 10_999_910, 100, &[0; 1200])
            .unwrap());
        assert!(q.chunks.is_empty());
        assert_eq!(q.recovered_timing_intervals, 1);
    }
    #[test]
    fn opposite_residuals_bound_every_subscriber_origin() {
        let (mut b, r) = fixture();
        b.sample_rate = 8000;
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[0; 800]).unwrap();
        q.push(800, 11_300_000, 11_300_000, 100, &[0; 800]).unwrap();
        // Each is only 30 ms from the original anchor, but a late subscriber
        // starting at the second packet would see a 60 ms discrepancy.
        assert!(!q
            .push(1600, 11_700_000, 11_700_000, 100, &[0; 800])
            .unwrap());
        assert!(q.chunks.is_empty());
    }
    #[test]
    fn utc_residual_not_only_qpc_residual_uses_the_receiver_budget() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[0; 1200]).unwrap();
        // QPC residual 49.9 ms would fit, but UTC plus calibration exceeds 50 ms.
        assert!(!q
            .push(1200, 10_749_000, 10_750_000, 100, &[0; 1200])
            .unwrap());
        assert!(q.chunks.is_empty());
    }
    #[test]
    fn gradual_utc_slew_is_measured_but_does_not_bypass_the_epoch_budget() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        for n in 0..500 {
            q.push(
                n * 480,
                10_000_000 + n * 100_000,
                10_000_000 + n * 101_000,
                100,
                &[0; 480],
            )
            .unwrap();
        }
        assert!(!q
            .push(500 * 480, 60_000_000, 60_500_000, 100, &[0; 480])
            .unwrap());
        assert!(q.chunks.is_empty());
    }
    #[test]
    fn timestamp_extrema_survive_pcm_retention_eviction() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(0, 10_000_000, 10_000_000, 100, &[0; 1200]).unwrap();
        for n in 1..150 {
            q.push(
                n * 1200,
                10_000_000 + n * 250_000 + 499_900,
                10_000_000 + n * 250_000 + 499_900,
                100,
                &[0; 1200],
            )
            .unwrap();
        }
        assert!(q.chunks.front().unwrap().first > 0);
        assert!(!q
            .push(
                150 * 1200,
                10_000_000 + 150 * 250_000 + 499_910,
                10_000_000 + 150 * 250_000 + 499_910,
                100,
                &[0; 1200]
            )
            .unwrap());
        assert!(q.chunks.is_empty());
    }
    #[test]
    fn fractional_rate_budget_matches_every_receiver_origin_in_wire_units() {
        let (mut b, r) = fixture();
        b.sample_rate = 44100;
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        let mut observations = Vec::new();
        for n in 0..100u64 {
            let first = n * 440;
            let jitter = (n % 7) * 10_001;
            let timestamp = 10_000_007 + first * 10_000_000 / 44100 + jitter;
            q.push(first, timestamp, timestamp, 103, &[0; 440]).unwrap();
            observations.push((first, timestamp / 10));
            assert_eq!(q.chunks.back().unwrap().time, timestamp / 10);
        }
        let (low, high) = q.timestamp_residual_range.unwrap();
        let budget = (high - low).unsigned_abs().div_ceil(44100) + u128::from(q.max_calibration_us);
        for (origin_frame, origin_us) in &observations {
            for (frame, utc_us) in &observations {
                let residual = (i128::from(*utc_us) - i128::from(*origin_us)) * 44100
                    - (i128::from(*frame) - i128::from(*origin_frame)) * 1_000_000;
                assert!(residual.unsigned_abs().div_ceil(44100) + 11 <= budget);
            }
        }
    }
    #[test]
    fn invalid_rate_and_exhausted_json_counters_never_export() {
        let (mut b, r) = fixture();
        let mut q = Buffer::default();
        b.sample_rate = 0;
        assert!(q.start(&b, &r, 1).is_err());
        b.sample_rate = 48000;
        assert!(q.start(&b, &r, MAX_EXACT + 1).is_err());
        q.start(&b, &r, 1).unwrap();
        assert!(q.push(MAX_EXACT, 1, 1, 100, &[1]).is_err());
        q.start(&b, &r, 1).unwrap();
        q.next = MAX_EXACT;
        assert!(q.push(0, 1, 1, 100, &[1]).is_err());
        assert!(q.response(0, 0)["descriptor"].is_null());
    }
    #[test]
    fn declared_gap_revokes_partial_audio_preserves_capture_and_cursors() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        q.push(1200, 10_000_000, 10_000_000, 100, &[7; 1200])
            .unwrap();
        q.push(2400, 10_250_000, 10_250_000, 100, &[8; 1200])
            .unwrap();
        let before = q.descriptor.clone().unwrap();
        // One missing packet and one explicitly discarded packet.
        q.discard_gap(4800, 10_750_000, 10_750_000, 100, 1200)
            .unwrap();
        let waiting = q.response(1, 1075);
        assert_eq!(waiting["state"], "stopped");
        assert!(waiting["descriptor"].is_null());
        assert_eq!(waiting["chunks"], json!([]));
        assert_eq!(q.next, 3);
        q.push(6000, 11_000_000, 11_000_000, 100, &[9; 1200])
            .unwrap();
        let after = q.response(1, 1100);
        assert_eq!(after["state"], "running");
        assert_eq!(after["gap"], true); // unread sequence 2 was flushed
        assert_eq!(after["chunks"][0]["sequence"], 3);
        assert_eq!(after["chunks"][0]["first_frame"], 6000);
        assert_eq!(after["chunks"][0]["sample0_unix_us"], 1_100_000);
        assert_ne!(after["descriptor"]["epoch"], before["epoch"]);
        assert_eq!(after["descriptor"]["capture_id"], before["capture_id"]);
        assert_eq!(after["diagnostics"]["recovered_audio_gaps"], 1);
        assert_eq!(q.response(2, 1100)["gap"], false); // current cursor resets by epoch
        assert_eq!(q.response(0, 1100)["oldest_sequence"], 3); // late subscriber
    }
    #[test]
    fn gap_recovery_never_resets_physical_clock_or_counter_checks() {
        let (b, r) = fixture();
        for (first, qpc, utc) in [
            (0, 10_250_000, 10_250_000),
            (1200, 10_000_000, 10_250_000),
            (1200, 10_250_000, 10_300_000),
        ] {
            let mut q = Buffer::default();
            q.start(&b, &r, 1).unwrap();
            q.push(0, 10_000_000, 10_000_000, 100, &[0; 1200]).unwrap();
            assert!(q.discard_gap(first, qpc, utc, 100, 1200).is_err());
            assert!(q.descriptor.is_none());
        }
        for (first, qpc, utc) in [
            (3600, 10_500_000, 10_500_000),
            (2400, 10_250_000, 10_500_000),
            (2400, 10_500_000, 10_550_000),
        ] {
            let mut q = Buffer::default();
            q.start(&b, &r, 1).unwrap();
            q.push(0, 10_000_000, 10_000_000, 100, &[0; 1200]).unwrap();
            q.discard_gap(1200, 10_250_000, 10_250_000, 100, 1200)
                .unwrap();
            assert!(q.push(first, qpc, utc, 100, &[0; 1200]).is_err());
        }
    }
    #[test]
    fn repeated_gaps_are_bounded_and_user_stop_cannot_recover() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        for n in 0..6 {
            q.discard_gap(
                n * 1200,
                10_000_000 + n * 250_000,
                10_000_000 + n * 250_000,
                100,
                1200,
            )
            .unwrap();
        }
        assert!(q
            .discard_gap(7200, 11_500_000, 11_500_000, 100, 1200)
            .is_err());
        assert!(q.descriptor.is_none());
        assert!(q
            .push(8400, 11_750_000, 11_750_000, 100, &[0; 1200])
            .is_err());
        q.start(&b, &r, 1).unwrap();
        q.stop("Stopped by operator");
        assert!(q.discard_gap(0, 10_000_000, 10_000_000, 100, 1200).is_err());
        assert!(q.descriptor.is_none());
    }
    #[test]
    fn recovery_budget_expires_after_a_minute_without_reopening_device() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        for n in 0..20 {
            let t = 10_000_000 + n * 110_000_000;
            q.discard_gap(n * 528000, t, t, 100, 1200).unwrap();
            q.push(n * 528000 + 1200, t + 250_000, t + 250_000, 100, &[0; 1200])
                .unwrap();
        }
        assert_eq!(q.recovered_gaps, 20);
        assert!(q.descriptor.is_some());
    }
    #[test]
    fn observed_dax_outlier_discards_only_unqualified_intervals() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        let capture = q.descriptor.as_ref().unwrap()["capture_id"].clone();
        let mut exported = 0;
        let mut discarded = 0;
        for (i, line) in include_str!("../tests/fixtures/dax-rx1-startup-outlier.csv")
            .lines()
            .skip(1)
            .enumerate()
        {
            let p: Vec<u64> = line.split(',').map(|v| v.parse().unwrap()).collect();
            if i == 0 {
                assert_eq!(p[5], 1); // native startup result 2: do not anchor/export
                continue;
            }
            if p[5] == 1 {
                q.discard_gap(
                    p[0],
                    10_000_000 + p[1],
                    10_000_006 + p[2],
                    p[3],
                    p[4] as usize,
                )
                .unwrap();
                assert!(q.chunks.is_empty());
                continue;
            }
            assert_eq!(p[5], 0);
            let next = q.next;
            let old_epoch = q.descriptor.as_ref().unwrap()["epoch"].clone();
            let published = q
                .push(
                    p[0],
                    10_000_000 + p[1],
                    10_000_006 + p[2],
                    p[3],
                    &vec![0; p[4] as usize],
                )
                .unwrap();
            if published {
                exported += 1;
                let (low, high) = q.timestamp_residual_range.unwrap();
                // Extrema across ALL packets in this epoch bound every possible
                // late subscriber origin, even after retained PCM is evicted.
                assert!(
                    (high - low).unsigned_abs().div_ceil(48000) + u128::from(q.max_calibration_us)
                        <= 50_000
                );
                assert_eq!(q.chunks.back().unwrap().time, (10_000_006 + p[2]) / 10);
                assert_eq!(q.next, next + 1);
            } else {
                discarded += 1;
                assert!(q.chunks.is_empty());
                assert!(q.anchor.is_none());
                assert_eq!(q.next, next);
                assert_ne!(q.descriptor.as_ref().unwrap()["epoch"], old_epoch);
            }
            assert_eq!(q.descriptor.as_ref().unwrap()["capture_id"], capture);
        }
        assert_eq!(exported, 14393);
        assert_eq!(discarded, 1);
        assert_eq!(q.recovered_timing_intervals, 1);
        assert_eq!(q.recovered_gaps, 1);
        assert!(q.reason.is_none()); // fresh packets after the outlier resumed
    }
    #[test]
    fn timing_discards_and_device_gaps_share_the_same_recovery_budget() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        let (mut first, mut t) = (0, 10_000_000);
        for _ in 0..3 {
            assert!(q.push(first, t, t, 100, &[0; 1200]).unwrap());
            first += 1200;
            t += 850_000; // 60 ms timing discrepancy
            assert!(!q.push(first, t, t, 100, &[0; 1200]).unwrap());
            first += 1200;
            t += 250_000;
            q.discard_gap(first, t, t, 100, 1200).unwrap();
            first += 1200;
            t += 250_000;
        }
        assert_eq!(q.recovered_gaps, 3);
        assert_eq!(q.recovered_timing_intervals, 3);
        assert!(q.discard_gap(first, t, t, 100, 1200).is_err());
        assert!(q.descriptor.is_none());
    }
    #[test]
    fn recovery_budget_evicts_at_exact_sixty_second_boundary() {
        let (b, r) = fixture();
        let mut q = Buffer::default();
        q.start(&b, &r, 1).unwrap();
        for n in 0..6 {
            q.discard_gap(
                n * 1200,
                10_000_000 + n * 250_000,
                10_000_000 + n * 250_000,
                100,
                1200,
            )
            .unwrap();
        }
        q.discard_gap(2_880_000, 610_000_000, 610_000_000, 100, 1200)
            .unwrap();
        assert_eq!(q.gap_times.len(), 6);
        assert!(q
            .discard_gap(2_881_200, 610_100_000, 610_100_000, 100, 1200)
            .is_err());
    }
}
