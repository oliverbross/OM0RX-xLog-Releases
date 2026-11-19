//! Exact-rate final PCM output. No resampling, device fallback, gain or PTT.
use crate::{companion_tx_contract::OutputRoute, output_authority::Reservation};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct Signals {
    pub cancelled: AtomicBool,
    pub enabled: AtomicBool,
    pub ready: AtomicBool,
    pub failed: AtomicBool,
    pub done: AtomicBool,
    pub output_closed: AtomicBool,
    pub started_unix_us: AtomicU64,
    pub presented_unix_us: AtomicU64,
    pub callback_ns: AtomicU64,
    pub heartbeat_deadline_ns: AtomicU64,
    pub origin: Instant,
}
impl Signals {
    pub fn new(origin: Instant) -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            output_closed: AtomicBool::new(true),
            started_unix_us: AtomicU64::new(0),
            presented_unix_us: AtomicU64::new(0),
            callback_ns: AtomicU64::new(0),
            heartbeat_deadline_ns: AtomicU64::new(0),
            origin,
        })
    }
    pub fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.enabled.store(false, Ordering::Release);
    }
    pub fn fault(&self) {
        self.failed.store(true, Ordering::Release);
        self.stop();
    }
}

/// Sample scheduling uses the native predicted presentation position. Enqueue
/// of the final frame is NOT completion: callback clock must pass its end.
#[derive(Default)]
pub struct Render {
    cursor: usize,
    previous_end_us: Option<u64>,
    presentation_end_us: Option<u64>,
    pub started_us: Option<u64>,
}
impl Render {
    pub fn fill(
        &mut self,
        out: &mut [f32],
        samples: &[f32],
        map: &[u16],
        callback_us: u64,
        playback_us: u64,
        slot_us: u64,
        latest_us: u64,
        enabled: bool,
        cancelled: bool,
    ) -> Result<bool, &'static str> {
        out.fill(0.0);
        if cancelled || !enabled {
            return Ok(false);
        }
        if out.is_empty()
            || out.len() % 2 != 0
            || out.len() > 96_000
            || map.is_empty()
            || map.iter().any(|c| *c >= 2)
            || playback_us < callback_us
            || playback_us - callback_us > 500_000
        {
            return Err("Invalid native output clock or stereo buffer");
        }
        if self
            .presentation_end_us
            .is_some_and(|end| callback_us >= end)
        {
            return Ok(true);
        }
        let count = out.len() / 2;
        // WASAPI's predicted presentation time comes from the device clock position, which
        // virtual endpoints such as DAX TX advance in coarse steps: measured on DAX TX it
        // wobbles ±2.4 ms between consecutive 25 ms callbacks while no sample is lost. The PCM
        // cursor only advances by frames actually written, so that wobble never skips or
        // replays audio. A real dropout moves the estimate by a whole buffer, so only a step
        // beyond half a buffer (at least 5 ms) is a discontinuity, and only once PCM has begun.
        let tolerance_us = ((count as u64 * 1_000_000).div_ceil(48_000) / 2).max(5_000);
        if self.started_us.is_some()
            && self
                .previous_end_us
                .is_some_and(|end| playback_us.abs_diff(end) > tolerance_us)
        {
            return Err("Native output presentation discontinuity");
        }
        self.previous_end_us = Some(
            playback_us
                .checked_add((count as u64 * 1_000_000).div_ceil(48_000))
                .ok_or("Output clock overflow")?,
        );
        let first = if self.started_us.is_none() {
            if playback_us > latest_us {
                return Err("Prepared slot start missed");
            }
            ((slot_us.saturating_sub(playback_us) * 48_000).div_ceil(1_000_000) as usize).min(count)
        } else {
            0
        };
        for (index, frame) in out.chunks_exact_mut(2).enumerate().skip(first) {
            if self.cursor == samples.len() {
                break;
            }
            let presentation = playback_us + (index as u64 * 1_000_000).div_ceil(48_000);
            if self.started_us.is_none() {
                if presentation < slot_us || presentation > latest_us {
                    return Err("Prepared slot presentation missed");
                }
                self.started_us = Some(presentation);
            }
            for &channel in map {
                frame[channel as usize] = samples[self.cursor];
            }
            self.cursor += 1;
            if self.cursor == samples.len() {
                self.presentation_end_us =
                    Some(playback_us + ((index as u64 + 1) * 1_000_000).div_ceil(48_000));
            }
        }
        Ok(false)
    }
}

#[cfg(windows)]
pub struct Output {
    stream: Option<cpal::Stream>,
    signals: Arc<Signals>,
}
#[cfg(windows)]
impl Drop for Output {
    fn drop(&mut self) {
        self.signals.stop();
        drop(self.stream.take());
        self.signals.output_closed.store(true, Ordering::Release);
    }
}

#[cfg(windows)]
pub fn open(
    route: &OutputRoute,
    samples: Arc<[f32]>,
    slot_us: u64,
    latest_us: u64,
    exact: Reservation,
    signals: Arc<Signals>,
) -> Result<Output, String> {
    use cpal::traits::{DeviceTrait, StreamTrait};
    crate::companion_tx_timing::admit_output(&signals.cancelled, || {
        crate::output_authority::shared().require_exact(&exact)
    })?;
    let play_exact = exact.clone();
    let device = crate::contest_recorder_audio::device(&route.device_id)?;
    let format = device.default_output_config().map_err(|e| e.to_string())?;
    let name = device.description().map_err(|e| e.to_string())?;
    if !matches!(
        name.name(),
        "DAX Audio TX" | "DAX TX" | "DAX TX (FlexRadio DAX)"
    ) || route.sample_rate_hz != 48_000
        || route.channels != 2
        || format.sample_rate() != 48_000
        || format.channels() != 2
        || format.sample_format() != cpal::SampleFormat::F32
        || route.selected_channels.is_empty()
        || route.selected_channels.len() > 2
        || route.selected_channels.iter().any(|c| *c > 1)
        || route.selected_channels.len() == 2
            && route.selected_channels[0] == route.selected_channels[1]
    {
        return Err("Confirmed DAX TX endpoint must retain its exact 48 kHz stereo float format and channel map".into());
    }
    let map = route.selected_channels.clone();
    let failure = signals.clone();
    let retained = signals.clone();
    let mut render = Render::default();
    let mut anchor = None;
    let mut last_wall: Option<(u64, u64)> = None;
    let stream = device
        .build_output_stream::<f32, _, _>(
            crate::contest_recorder_audio::stream_config(2, 48_000),
            move |out, info| {
                out.fill(0.0);
                let mono = signals.origin.elapsed().as_nanos() as u64;
                if signals.cancelled.load(Ordering::Acquire)
                    || signals.failed.load(Ordering::Acquire)
                {
                    return;
                }
                if crate::output_authority::shared()
                    .require_exact(&exact)
                    .is_err()
                    || mono >= signals.heartbeat_deadline_ns.load(Ordering::Acquire)
                {
                    signals.fault();
                    return;
                }
                let stamp = info.timestamp();
                let Ok(wall) = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_micros() as u64)
                else {
                    signals.fault();
                    return;
                };
                if last_wall.is_some_and(|(last_mono, last_utc)| {
                    mono < last_mono
                        || mono - last_mono > 250_000_000
                        || wall.abs_diff(last_utc.saturating_add((mono - last_mono) / 1000))
                            > 50_000
                }) {
                    signals.fault();
                    return;
                }
                last_wall = Some((mono, wall));
                let (native_epoch, utc_epoch) = *anchor.get_or_insert((stamp.callback, wall));
                let Some(callback) = stamp.callback.checked_duration_since(native_epoch) else {
                    signals.fault();
                    return;
                };
                let Some(playback) = stamp.playback.checked_duration_since(native_epoch) else {
                    signals.fault();
                    return;
                };
                let callback_us = utc_epoch.saturating_add(callback.as_micros() as u64);
                let playback_us = utc_epoch.saturating_add(playback.as_micros() as u64);
                if callback_us.abs_diff(wall) > 50_000
                    || playback_us < callback_us
                    || playback_us - callback_us > 500_000
                {
                    signals.fault();
                    return;
                }
                signals.callback_ns.store(mono, Ordering::Release);
                signals
                    .presented_unix_us
                    .store(callback_us, Ordering::Release);
                signals.ready.store(true, Ordering::Release);
                match render.fill(
                    out,
                    &samples,
                    &map,
                    callback_us,
                    playback_us,
                    slot_us,
                    latest_us,
                    signals.enabled.load(Ordering::Acquire),
                    signals.cancelled.load(Ordering::Acquire),
                ) {
                    Ok(done) => {
                        if let Some(start) = render.started_us {
                            signals.started_unix_us.store(start, Ordering::Release);
                        }
                        if done {
                            signals.done.store(true, Ordering::Release);
                        }
                    }
                    Err(_) => {
                        out.fill(0.0);
                        signals.fault();
                    }
                }
            },
            move |_| failure.fault(),
            None,
        )
        .map_err(|e| e.to_string())?;
    retained.output_closed.store(false, Ordering::Release);
    let output = Output {
        stream: Some(stream),
        signals: retained,
    };
    crate::companion_tx_timing::admit_output(&output.signals.cancelled, || {
        crate::output_authority::shared().require_exact(&play_exact)
    })?;
    output
        .stream
        .as_ref()
        .unwrap()
        .play()
        .map_err(|e| e.to_string())?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silent_until_exact_slot_maps_only_selected_channels_and_waits_for_presentation_end() {
        let mut r = Render::default();
        let mut out = [9.0; 96];
        assert!(!r
            .fill(&mut out, &[0.25; 48], &[1], 0, 0, 1000, 1100, true, false)
            .unwrap());
        assert_eq!(out, [0.0; 96]);
        assert!(!r
            .fill(
                &mut out,
                &[0.25; 48],
                &[1],
                1000,
                1000,
                1000,
                1100,
                true,
                false
            )
            .unwrap());
        assert_eq!(r.started_us, Some(1000));
        assert!(out.chunks_exact(2).all(|s| s == [0.0, 0.25]));
        assert!(r
            .fill(
                &mut out,
                &[0.25; 48],
                &[1],
                2000,
                2000,
                1000,
                1100,
                true,
                false
            )
            .unwrap());
    }
    #[test]
    fn stop_and_missed_deadline_never_emit_or_restart() {
        let mut r = Render::default();
        let mut out = [1.0; 96];
        assert!(!r
            .fill(
                &mut out,
                &[0.25; 48],
                &[0, 1],
                1000,
                1000,
                1000,
                1100,
                true,
                true
            )
            .unwrap());
        assert_eq!(out, [0.0; 96]);
        assert_eq!(r.started_us, None);
        assert!(r
            .fill(
                &mut out,
                &[0.25; 48],
                &[0, 1],
                1200,
                1200,
                1000,
                1100,
                true,
                false
            )
            .is_err());
        assert_eq!(out, [0.0; 96]);
    }
    #[test]
    fn clock_gap_fails_instead_of_skipping_or_replaying_pcm() {
        let mut r = Render::default();
        let mut out = [0.0; 96];
        r.fill(&mut out, &[0.25; 480], &[0], 0, 0, 0, 100, true, false)
            .unwrap();
        // 48 frames end at 1 ms; a 6 ms jump exceeds the 5 ms floor of a tiny buffer.
        assert!(r
            .fill(
                &mut out,
                &[0.25; 480],
                &[0],
                7000,
                7000,
                0,
                100,
                true,
                false
            )
            .is_err());
        assert_eq!(out, [0.0; 96]);
    }
    /// Timestamps as cpal reports them for the FlexRadio DAX TX endpoint (1200-frame
    /// callbacks): a 24.5 ms latency ramp before the slot, then ±2.4 ms wobble per callback.
    #[test]
    fn dax_presentation_wobble_plays_every_sample_once_but_a_dropped_buffer_still_fails() {
        let samples: Vec<f32> = (0..12_000).map(|i| i as f32).collect();
        let slot = 200_000;
        let mut r = Render::default();
        let mut out = vec![0.0; 2400];
        let mut played = vec![];
        let mut callback = 0u64;
        let mut playback = 55_000u64;
        // Pre-roll ramp (as observed: +24.5 ms, +24.8 ms) before any sample is due.
        for step in [0u64, 24_467, 24_780] {
            playback += step;
            assert!(!r
                .fill(&mut out, &samples, &[0], callback, playback, slot, slot + 50_000, true, false)
                .unwrap());
            callback += 25_000;
            playback += 25_000;
        }
        let mut done = false;
        for wobble in [146i64, 2_099, -1_266, 2_379, -1_564, 1_607, -1_418, 2_025, 11, -15].iter().cycle() {
            playback = playback.checked_add_signed(*wobble).unwrap();
            done = r
                .fill(&mut out, &samples, &[0], callback, playback, slot, slot + 50_000, true, false)
                .unwrap();
            played.extend(out.chunks_exact(2).map(|f| f[0]).filter(|s| *s != 0.0));
            callback += 25_000;
            playback += 25_000;
            if done {
                break;
            }
        }
        assert!(done);
        assert_eq!(played, samples[1..]);
        assert!(r.started_us.is_some_and(|s| s >= slot && s <= slot + 50_000));

        // One whole lost buffer (25 ms) is still a discontinuity.
        let mut r = Render::default();
        r.fill(&mut out, &samples, &[0], 90_000, slot, slot, slot + 50_000, true, false)
            .unwrap();
        assert!(r
            .fill(&mut out, &samples, &[0], 140_000, slot + 50_000, slot, slot + 50_000, true, false)
            .is_err());
    }
}
