//! Offline timing arithmetic only. No clock reads, timers, threads, audio or PTT.
//! A plan and its synthetic span are not waveform, output or RF certification.
const SECOND: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding { F32Le, I16Le }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Format { pub rate: u32, pub channels: u16, pub encoding: Encoding }
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub calibration: u128, pub clock: u128, pub device: u128, pub route: u128,
    pub policy: u128, pub reservation: u128, pub owner_context_sha256: [u8;32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Calibration {
    pub binding: Binding, pub format: Format,
    /// Exact output-channel routing covered by this calibration and route epoch.
    pub channel_map: Vec<u16>,
    pub utc_anchor_ns: u64, pub mono_anchor_ns: u64,
    pub output_anchor_frame: u64, pub output_anchor_mono_ns: u64,
    pub measured_at_mono_ns: u64, pub valid_until_mono_ns: u64,
    /// Aggregate UTC-to-monotonic AND presentation-anchor error at measurement.
    /// Not just the audio driver's timestamp error. Drift expands this bound
    /// through the latest possible presentation, including the bound itself.
    pub uncertainty_ns: u64, pub drift_ppb: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub epoch: u128, pub max_calibration_age_ns: u64, pub max_uncertainty_ns: u64,
    pub max_clock_step_ns: u64, pub max_horizon_ns: u64, pub max_alignment_ns: u64,
    pub required_enqueue_lead_ns: u64, pub ptt_setup_ns: u64, pub ptt_release_ns: u64,
    pub ptt_window_open_ns: u64, pub ptt_window_close_ns: u64, pub max_ptt_duration_ns: u64,
}
/// An explicit fixture certificate, not a claim derived from waveform_frames.
/// A future commissioned producer needs a separately qualified certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpanSource { SyntheticFixture }
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub source: SpanSource, pub body_sha256: [u8;32], pub format: Format,
    pub period_ns: u64, pub full_frames: u64, pub signal_first: u64, pub signal_end: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub operation: u128, pub binding: Binding, pub body_sha256: [u8;32],
    pub format: Format, pub channel_map: Vec<u16>, pub slot_utc_ns: u64, pub span: Span,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClockSample { pub epoch: u128, pub utc_ns: u64, pub mono_ns: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error { Invalid, Binding, Format, Span, ClockChanged, Expired, Uncertain,
    Overflow, Late, Alignment, PttWindow }
type Result<T> = std::result::Result<T,Error>;

/// Only plan() constructs this immutable snapshot. Admission revalidation never
/// rebases it to a replacement calibration, route, policy, clock or body.
/// This is NOT an active-playback or first-frame validation API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    request: Request, calibration: Calibration, policy: Policy,
    slot_mono_ns: u64, first_frame: u64, end_frame: u64,
    signal_first_frame: u64, signal_end_frame: u64,
    first_presentation_ns: u64, end_presentation_ns: u64,
    signal_first_ns: u64, signal_end_ns: u64,
    commit_deadline_ns: u64, ptt_on_ns: u64, ptt_off_ns: u64, uncertainty_ns: u64,
}
impl Plan {
    pub(crate) fn revalidate_admission(&self, current: &Request, calibration: &Calibration,
        policy: &Policy, now: ClockSample) -> Result<()> {
        if current!=&self.request || calibration!=&self.calibration || policy!=&self.policy {
            return Err(Error::Binding);
        }
        if plan(current,calibration,policy,now)?!=*self {return Err(Error::Binding);}
        Ok(())
    }
}
fn add(a:u64,b:u64)->Result<u64>{a.checked_add(b).ok_or(Error::Overflow)}
fn sub(a:u64,b:u64)->Result<u64>{a.checked_sub(b).ok_or(Error::Overflow)}
fn ceil_product(a:u64,b:u64,denominator:u64)->Result<u64>{
    if denominator==0{return Err(Error::Invalid);}
    let value=(u128::from(a)*u128::from(b)).div_ceil(u128::from(denominator));
    u64::try_from(value).map_err(|_|Error::Overflow)
}
fn presentation(c:&Calibration,frame:u64)->Result<u64>{
    add(c.output_anchor_mono_ns,ceil_product(sub(frame,c.output_anchor_frame)?,SECOND,u64::from(c.format.rate))?)
}
fn utc_to_mono(c:&Calibration,utc:u64)->Result<u64>{
    let value=i128::from(c.mono_anchor_ns)+i128::from(utc)-i128::from(c.utc_anchor_ns);
    u64::try_from(value).map_err(|_|Error::Overflow)
}
fn presentation_uncertainty(c:&Calibration,nominal_end:u64)->Result<u64>{
    // Solve U >= base + ceil((elapsed + U) * drift / SECOND).
    // Expanding only through nominal_end underestimates the latest endpoint.
    if c.drift_ppb>=SECOND {return Err(Error::Invalid);}
    let elapsed=sub(nominal_end,c.measured_at_mono_ns)?;
    let numerator=u128::from(c.uncertainty_ns)*u128::from(SECOND)
        +u128::from(elapsed)*u128::from(c.drift_ppb);
    u64::try_from(numerator.div_ceil(u128::from(SECOND-c.drift_ppb)))
        .map_err(|_|Error::Overflow)
}

pub(crate) fn plan(r:&Request,c:&Calibration,p:&Policy,now:ClockSample)->Result<Plan>{
    if r.operation==0 || [r.binding.calibration,r.binding.clock,r.binding.device,r.binding.route,
        r.binding.policy,r.binding.reservation].contains(&0) || p.epoch==0 {return Err(Error::Invalid);}
    if r.binding!=c.binding || r.binding.policy!=p.epoch || now.epoch!=r.binding.clock {return Err(Error::Binding);}
    if r.format!=c.format || r.span.format!=r.format || r.channel_map!=c.channel_map
        || !(8_000..=192_000).contains(&r.format.rate)
        || r.format.channels==0 || r.format.channels>64 || r.channel_map.is_empty()
        || r.channel_map.len()>usize::from(r.format.channels)
        || r.channel_map.iter().enumerate().any(|(i,ch)|*ch>=r.format.channels || r.channel_map[..i].contains(ch)) {
        return Err(Error::Format);
    }
    let s=&r.span;
    if s.body_sha256!=r.body_sha256 || !matches!(s.source,SpanSource::SyntheticFixture)
        || ![7_500_000_000,15_000_000_000].contains(&s.period_ns) || r.slot_utc_ns%s.period_ns!=0
        || s.signal_first>=s.signal_end || s.signal_end>s.full_frames
        || u128::from(s.full_frames)*u128::from(SECOND)!=u128::from(s.period_ns)*u128::from(r.format.rate) {
        return Err(Error::Span);
    }
    if p.max_calibration_age_ns==0 || p.max_horizon_ns==0 || p.required_enqueue_lead_ns==0
        || p.max_ptt_duration_ns==0 || p.ptt_window_open_ns>=p.ptt_window_close_ns
        || p.ptt_window_close_ns>s.period_ns || c.measured_at_mono_ns<c.mono_anchor_ns
        || c.output_anchor_mono_ns<c.mono_anchor_ns || c.valid_until_mono_ns<=c.measured_at_mono_ns {
        return Err(Error::Invalid);
    }
    if now.mono_ns<c.measured_at_mono_ns || now.mono_ns>=c.valid_until_mono_ns
        || sub(now.mono_ns,c.measured_at_mono_ns)?>p.max_calibration_age_ns {return Err(Error::Expired);}
    if utc_to_mono(c,now.utc_ns)?.abs_diff(now.mono_ns)>p.max_clock_step_ns {return Err(Error::ClockChanged);}
    let slot=utc_to_mono(c,r.slot_utc_ns)?;
    if slot<now.mono_ns || slot<c.output_anchor_mono_ns {return Err(Error::Late);}
    let end_slot=add(slot,s.period_ns)?;
    if sub(end_slot,now.mono_ns)?>p.max_horizon_ns {return Err(Error::Expired);}
    let first_frame=add(c.output_anchor_frame,ceil_product(sub(slot,c.output_anchor_mono_ns)?,u64::from(r.format.rate),SECOND)?)?;
    let end_frame=add(first_frame,s.full_frames)?;
    let signal_first_frame=add(first_frame,s.signal_first)?;
    let signal_end_frame=add(first_frame,s.signal_end)?;
    let first_presentation_ns=presentation(c,first_frame)?;
    let end_presentation_ns=presentation(c,end_frame)?;
    let signal_first_ns=presentation(c,signal_first_frame)?;
    let signal_end_ns=presentation(c,signal_end_frame)?;
    let uncertainty=presentation_uncertainty(c,end_presentation_ns)?;
    if uncertainty>p.max_uncertainty_ns {return Err(Error::Uncertain);}
    if add(sub(first_presentation_ns,slot)?,uncertainty)?>p.max_alignment_ns {return Err(Error::Alignment);}
    let latest_presentation=add(end_presentation_ns,uncertainty)?;
    if latest_presentation>c.valid_until_mono_ns || sub(latest_presentation,now.mono_ns)?>p.max_horizon_ns {
        return Err(Error::Expired);
    }
    let commit_deadline_ns=sub(sub(first_presentation_ns,uncertainty)?,p.required_enqueue_lead_ns)?;
    if now.mono_ns>commit_deadline_ns {return Err(Error::Late);}
    let ptt_on_ns=sub(signal_first_ns,p.ptt_setup_ns)?;
    let ptt_off_ns=add(signal_end_ns,p.ptt_release_ns)?;
    let earliest_ptt_on=sub(ptt_on_ns,uncertainty)?;
    let latest_ptt_off=add(ptt_off_ns,uncertainty)?;
    if earliest_ptt_on<add(slot,p.ptt_window_open_ns)? || latest_ptt_off>add(slot,p.ptt_window_close_ns)?
        || latest_ptt_off>end_slot || ptt_off_ns<=ptt_on_ns || sub(latest_ptt_off,earliest_ptt_on)?>p.max_ptt_duration_ns {
        return Err(Error::PttWindow);
    }
    // Admission must precede BOTH enqueue and PTT setup; no catch-up keying.
    let commit_deadline_ns=commit_deadline_ns.min(earliest_ptt_on).min(slot);
    if now.mono_ns>commit_deadline_ns {return Err(Error::Late);}
    Ok(Plan {request:r.clone(),calibration:c.clone(),policy:p.clone(),slot_mono_ns:slot,
        first_frame,end_frame,signal_first_frame,signal_end_frame,first_presentation_ns,end_presentation_ns,
        signal_first_ns,signal_end_ns,commit_deadline_ns,ptt_on_ns,ptt_off_ns,uncertainty_ns:uncertainty})
}

#[cfg(test)]
#[path="output_timing_tests.rs"]
mod tests;
