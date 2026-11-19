//! SOURCE-ONLY REVIEW DRAFT: not registered in main or any endpoint/owner.
//! Byte validation is not waveform/message certification, timing or TX authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_MANIFEST_BYTES: usize = 4096;
const MAX_PCM_BYTES: usize = 720_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Mode { #[serde(rename = "FT8")] Ft8, #[serde(rename = "FT4")] Ft4 }
impl Mode {
    fn geometry(self) -> Geometry {
        match self {
            Self::Ft8 => Geometry { tag: 8, frames: 180_000, period_us: 15_000_000, lead: 6000, signal: 79 * 1920, tone_max: 7, samples_per_symbol: 1920 },
            Self::Ft4 => Geometry { tag: 4, frames: 90_000, period_us: 7_500_000, lead: 3600, signal: 105 * 576, tone_max: 3, samples_per_symbol: 576 },
        }
    }
}
struct Geometry { tag: u8, frames: u32, period_us: u64, lead: u32, signal: u32, tone_max: u32, samples_per_symbol: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Encoding { #[serde(rename = "f32le")] F32Le }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MessageForm { #[serde(rename = "standard_type1")] StandardType1 }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Format { sample_rate_hz: u32, channels: u16, encoding: Encoding }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    schema_version: u32,
    mode: Mode,
    message_form: MessageForm,
    message: String,
    format: Format,
    /// IEEE-754 binary32 bits as a JSON u32, never a rounded decimal frequency.
    offset_hz_f32_bits: u32,
    frames: u32,
    waveform_frames: u32,
    pcm_byte_count: u32,
    period_duration_us: u64,
    pcm_sha256: String,
    content_sha256: String,
}
impl Manifest {
    pub(crate) fn mode(&self) -> Mode { self.mode }
    pub(crate) fn period_duration_us(&self) -> u64 { self.period_duration_us }
}

/// Explicit owner-supplied admission bounds. No defaults or device policy.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_manifest_bytes: usize,
    pub max_message_bytes: usize,
    pub max_frames: u32,
    pub max_pcm_bytes: usize,
    pub min_offset_hz: f32,
    pub max_offset_hz: f32,
    pub max_abs_sample: f32,
}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_manifest_bytes == 0 || self.max_message_bytes == 0 || self.max_frames == 0 || self.max_pcm_bytes == 0
            || !self.min_offset_hz.is_finite() || self.min_offset_hz < 0.0
            || !self.max_offset_hz.is_finite() || self.max_offset_hz < self.min_offset_hz
            || !self.max_abs_sample.is_finite() || self.max_abs_sample <= 0.0 || self.max_abs_sample > 1.0 {
            return Err(Error::InvalidLimits);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    InvalidLimits, SizeLimit, InvalidManifest, UnsupportedSchema, UnsupportedFormat,
    InvalidMessage, UnsupportedMessageShape, InvalidGeometry, InvalidOffset,
    InvalidSample, AmplitudeLimit, InvalidSilence, EmptySignal, InvalidDigest, DigestMismatch,
}

/// Owns the exact validated descriptor and raw bytes. No mutable accessors,
/// operation identity, route, lease, timestamp or on-air receipt are attached.
pub(crate) struct ValidatedPeriod { manifest: Manifest, raw_manifest: Box<[u8]>, pcm: Box<[u8]>, peak_abs_sample: f32 }
impl ValidatedPeriod {
    pub(crate) fn manifest(&self) -> &Manifest { &self.manifest }
    pub(crate) fn raw_manifest(&self) -> &[u8] { &self.raw_manifest }
    pub(crate) fn pcm(&self) -> &[u8] { &self.pcm }
    pub(crate) fn peak_abs_sample(&self) -> f32 { self.peak_abs_sample }
    /// Whole-body identity includes every original manifest byte, not only the
    /// fields covered by the producer's waveform content digest.
    pub(crate) fn body_sha256(&self) -> String {
        let mut body = Sha256::new();
        body.update(b"xnexus-prepared-corpus-body-v1\0");
        body.update((self.raw_manifest.len() as u64).to_le_bytes());
        body.update(&self.raw_manifest);
        body.update((self.pcm.len() as u64).to_le_bytes());
        body.update(&self.pcm);
        format!("{:x}", body.finalize())
    }
}

/// Binary payload is separate from JSON; no base64 or sample JSON array.
/// A future adapter MUST cap incoming streams before allocating this Box.
/// No content is trimmed, normalized, clamped, resampled or regenerated.
pub(crate) fn validate(json: &[u8], pcm: Box<[u8]>, limits: Limits) -> Result<ValidatedPeriod, Error> {
    limits.validate()?;
    if json.is_empty() || json.len() > MAX_MANIFEST_BYTES || json.len() > limits.max_manifest_bytes
        || pcm.len() > MAX_PCM_BYTES || pcm.len() > limits.max_pcm_bytes {
        return Err(Error::SizeLimit);
    }
    // Typed derive rejects unknown and duplicate fields, including nested format.
    let manifest: Manifest = serde_json::from_slice(json).map_err(|_| Error::InvalidManifest)?;
    if manifest.schema_version != 1 { return Err(Error::UnsupportedSchema); }
    if manifest.format.sample_rate_hz != 12_000 || manifest.format.channels != 1 {
        return Err(Error::UnsupportedFormat);
    }
    let geometry = manifest.mode.geometry();
    if manifest.frames != geometry.frames || manifest.period_duration_us != geometry.period_us
        || manifest.frames > limits.max_frames || manifest.waveform_frames < geometry.lead + geometry.signal
        || manifest.waveform_frames > manifest.frames
        || manifest.frames.checked_mul(4) != Some(manifest.pcm_byte_count)
        || manifest.pcm_byte_count as usize != pcm.len()
        || u64::from(manifest.frames) * 1_000_000 != u64::from(manifest.format.sample_rate_hz) * manifest.period_duration_us {
        return Err(Error::InvalidGeometry);
    }
    validate_message(&manifest.message, limits.max_message_bytes)?;
    let offset = f32::from_bits(manifest.offset_hz_f32_bits);
    let spacing = 12_000.0f32 / geometry.samples_per_symbol as f32;
    if !offset.is_finite() || offset < 0.0 || offset < limits.min_offset_hz || offset > limits.max_offset_hz
        || offset + geometry.tone_max as f32 * spacing >= 6000.0 {
        return Err(Error::InvalidOffset);
    }
    let pcm_digest = decode_digest(&manifest.pcm_sha256)?;
    let content_digest = decode_digest(&manifest.content_sha256)?;
    let mut peak = 0.0f32;
    let mut nonzero = false;
    for (index, raw) in pcm.chunks_exact(4).enumerate() {
        let bytes: [u8; 4] = raw.try_into().map_err(|_| Error::InvalidGeometry)?;
        let sample = f32::from_le_bytes(bytes);
        if !sample.is_finite() { return Err(Error::InvalidSample); }
        if sample.abs() > limits.max_abs_sample { return Err(Error::AmplitudeLimit); }
        // Producer permits numeric zero in the lead; retain signed zero bits.
        if index < geometry.lead as usize && sample != 0.0 { return Err(Error::InvalidSilence); }
        // Producer-added padding is exactly +0 f32 bytes, not arbitrary silence.
        if index >= manifest.waveform_frames as usize && bytes != [0; 4] { return Err(Error::InvalidSilence); }
        nonzero |= sample != 0.0;
        peak = peak.max(sample.abs());
    }
    if !nonzero { return Err(Error::EmptySignal); }
    let actual_pcm: [u8; 32] = Sha256::digest(&pcm).into();
    if pcm_digest != actual_pcm { return Err(Error::DigestMismatch); }
    let mut content = Sha256::new();
    content.update(b"xnexus-local-period-v1\0");
    content.update([geometry.tag]);
    content.update(manifest.format.sample_rate_hz.to_le_bytes());
    content.update(manifest.format.channels.to_le_bytes());
    content.update(b"f32le\0");
    content.update(manifest.offset_hz_f32_bits.to_le_bytes());
    content.update((manifest.message.len() as u32).to_le_bytes());
    content.update(manifest.message.as_bytes());
    content.update((pcm.len() as u64).to_le_bytes());
    content.update(&pcm);
    let actual_content: [u8; 32] = content.finalize().into();
    if content_digest != actual_content { return Err(Error::DigestMismatch); }
    Ok(ValidatedPeriod { manifest, raw_manifest: json.into(), pcm, peak_abs_sample: peak })
}

fn decode_digest(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(Error::InvalidDigest);
    }
    let nibble = |b: u8| if b.is_ascii_digit() { b - b'0' } else { b - b'a' + 10 };
    let mut digest = [0; 32];
    for (output, pair) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) { *output = nibble(pair[0]) * 16 + nibble(pair[1]); }
    Ok(digest)
}

/// Narrow textual shape checks only. There is deliberately no second WSJT
/// packer here and no claim that accepted PCM encodes these words or type bits.
fn validate_message(message: &str, max_bytes: usize) -> Result<(), Error> {
    if message.is_empty() || message.len() > 37 || message.len() > max_bytes || message.trim() != message
        || message.contains("  ") || !message.bytes().all(|b| (0x20..=0x7e).contains(&b) && !b.is_ascii_lowercase()) {
        return Err(Error::InvalidMessage);
    }
    let words: Vec<_> = message.split(' ').collect();
    let [first, second, third] = words.as_slice() else { return Err(Error::UnsupportedMessageShape); };
    let call = |word: &str| {
        let bytes = word.as_bytes();
        if !(3..=6).contains(&bytes.len()) || !bytes.iter().all(u8::is_ascii_alphanumeric)
            || (word.starts_with('Q') && !word.starts_with("QU1RK")) { return false; }
        let Some(area) = bytes.iter().rposition(u8::is_ascii_digit) else { return false; };
        (1..=2).contains(&area) && bytes[..area].iter().any(u8::is_ascii_uppercase)
            && (1..=3).contains(&(bytes.len() - area - 1)) && bytes[area + 1..].iter().all(u8::is_ascii_uppercase)
    };
    let grid = |word: &str| {
        let bytes = word.as_bytes(); bytes.len() == 4 && (b'A'..=b'R').contains(&bytes[0])
            && (b'A'..=b'R').contains(&bytes[1]) && bytes[2..].iter().all(u8::is_ascii_digit)
    };
    let report = |word: &str| {
        let bytes = word.strip_prefix('R').unwrap_or(word).as_bytes();
        if bytes.len() != 3 || !matches!(bytes[0], b'+' | b'-') || !bytes[1..].iter().all(u8::is_ascii_digit) { return false; }
        let value = (bytes[1] - b'0') * 10 + bytes[2] - b'0';
        if bytes[0] == b'-' { (1..=30).contains(&value) } else { value <= 49 }
    };
    if call(second) && (if *first == "CQ" { grid(third) }
        else { call(first) && (grid(third) || report(third) || matches!(*third, "RRR" | "RR73" | "73")) }) { Ok(()) }
    else { Err(Error::UnsupportedMessageShape) }
}

#[cfg(test)]
#[path = "prepared_period_tests.rs"]
mod tests;
