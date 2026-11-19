//! Immutable final-PCM transfer validation. Never opens audio or acquires PTT.
use crate::companion_tx_contract::{Mode, Prepare, MAX_DESCRIPTOR_BYTES, MAX_PREPARE_BYTES};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Clone)]
pub struct Prepared {
    pub descriptor: Prepare,
    pub body_sha256: String,
    pub samples: Arc<[f32]>,
}

pub fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}
pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate(
    body: &[u8],
    min_offset: u32,
    max_offset: u32,
    max_abs: f32,
) -> Result<Prepared, String> {
    if body.len() < 5
        || body.len() > MAX_PREPARE_BYTES
        || !max_abs.is_finite()
        || !(0.0..=1.0).contains(&max_abs)
        || max_abs == 0.0
    {
        return Err("Invalid bounded final PCM transfer".into());
    }
    let length = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
    if length == 0 || length > MAX_DESCRIPTOR_BYTES || 4 + length > body.len() {
        return Err("Invalid prepared descriptor size".into());
    }
    let descriptor: Prepare =
        serde_json::from_slice(&body[4..4 + length]).map_err(|_| "Invalid prepared descriptor")?;
    let d = &descriptor;
    if d.schema_version != 2
        || [
            &d.binding.server_instance,
            &d.binding.workspace_id,
            &d.binding.context_id,
            &d.binding.clock_epoch,
            &d.client_id,
            &d.operation_id,
            &d.event_id,
        ]
        .iter()
        .any(|v| !canonical_uuid(v))
    {
        return Err("Invalid prepared operation binding".into());
    }
    let m = &d.manifest;
    let end = d
        .slot_start_unix_us
        .checked_add(m.mode.period_us())
        .ok_or("Invalid slot")?;
    if d.slot_start_unix_us > 9_007_199_254_740_991
        || end > 9_007_199_254_740_991
        || d.slot_start_unix_us % m.mode.period_us() != 0
        || d.latest_start_unix_us < d.slot_start_unix_us
        || d.latest_start_unix_us >= end
        || d.latest_start_unix_us - d.slot_start_unix_us > 50_000
    {
        return Err("Invalid prepared slot deadline".into());
    }
    let pcm = &body[4 + length..];
    let offset = f32::from_bits(m.offset_hz_f32_bits);
    let gain = f32::from_bits(m.gain_f32_bits);
    if m.schema_version != 2
        || m.message_form != "wsjt77"
        || m.format.sample_rate_hz != 48_000
        || m.format.channels != 1
        || m.format.encoding != "f32le"
        || m.frames != m.mode.frames()
        || m.waveform_frames == 0
        || m.waveform_frames > m.frames
        || m.pcm_byte_count != m.frames * 4
        || m.support_start_frame >= m.waveform_frames
        || m.group_delay_frames != 256
        || m.filter_id != "kaiser-513-fc6000-beta10-x4-candidate1"
        || m.coefficients_sha256
            != "b3b266e2346da0272cdb1910943a6ba8020d17a56e55a7b0eb0f12cd2c73c764"
        || !gain.is_finite()
        || gain <= 0.0
        || gain > 0.8
        || max_abs > 0.95
        || pcm.len() != m.pcm_byte_count as usize
        || m.period_duration_us != m.mode.period_us()
        || m.message.is_empty()
        || m.message.len() > 37
        || m.message.trim() != m.message
        || !m.message.bytes().all(|b| (0x20..=0x7e).contains(&b))
        || m.message.contains("<...>")
        || m.message.starts_with('@')
        || !offset.is_finite()
        || offset < min_offset as f32
        || offset > max_offset as f32
        || !digest(&m.pcm_sha256)
        || !digest(&m.content_sha256)
    {
        return Err("Unsupported or invalid final PCM descriptor".into());
    }
    if format!("{:x}", Sha256::digest(pcm)) != m.pcm_sha256 {
        return Err("PCM digest mismatch".into());
    }
    let mut content = Sha256::new();
    content.update(b"xnexus-final-period-v2\0");
    content.update([match m.mode {
        Mode::FT8 => 8,
        Mode::FT4 => 4,
    }]);
    content.update(m.format.sample_rate_hz.to_le_bytes());
    content.update(m.format.channels.to_le_bytes());
    content.update(b"f32le\0");
    content.update(m.offset_hz_f32_bits.to_le_bytes());
    content.update((m.message.len() as u32).to_le_bytes());
    content.update(m.message.as_bytes());
    content.update((pcm.len() as u64).to_le_bytes());
    content.update(pcm);
    if format!("{:x}", content.finalize()) != m.content_sha256 {
        return Err("Content digest mismatch".into());
    }
    let mut samples = Vec::with_capacity(m.frames as usize);
    let mut nonzero = false;
    for (index, raw) in pcm.chunks_exact(4).enumerate() {
        let value = f32::from_le_bytes(raw.try_into().unwrap());
        if !value.is_finite() || value.abs() > max_abs {
            return Err("Invalid final PCM amplitude".into());
        }
        if index >= m.waveform_frames as usize && raw != [0, 0, 0, 0] {
            return Err("Invalid final PCM padding".into());
        }
        if index < m.support_start_frame as usize && raw != [0, 0, 0, 0] {
            return Err("Invalid final PCM support".into());
        }
        nonzero |= value != 0.0;
        samples.push(value);
    }
    if !nonzero {
        return Err("Empty final waveform".into());
    }
    Ok(Prepared {
        descriptor,
        body_sha256: format!("{:x}", Sha256::digest(body)),
        samples: samples.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion_tx_contract::{Binding, Format, Manifest};
    #[test]
    #[ignore = "requires the separately pinned actual Nexus final-output export"]
    fn actual_nexus_ft4_final_output_bytes_replay_without_transformation() {
        let root = std::path::PathBuf::from(
            std::env::var_os("XLOG_ACTUAL_FT4_FIXTURE")
                .expect("explicit actual producer fixture directory"),
        );
        assert!(root.is_absolute());
        let raw = std::fs::read(root.join("manifest.json")).unwrap();
        let pcm = std::fs::read(root.join("pcm.f32le")).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&raw)),
            "3f494e1ebd94ec717b93393934b537f85a75441f5fcf047374a6e8306f3d2bb8"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&pcm)),
            "91cfd59268bc6037ab32232970fed4ce20876d95b865de6f526bed0e44ccd7d8"
        );
        let (mut d, _) = fixture();
        d.manifest = serde_json::from_slice(&raw).unwrap();
        let bytes = body(&d, &pcm, false);
        let prepared = validate(&bytes, 100, 4500, 0.95).unwrap();
        assert_eq!(prepared.descriptor.manifest, d.manifest);
        assert_eq!(
            prepared.body_sha256,
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(
            prepared
                .samples
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect::<Vec<_>>(),
            pcm
        );
        assert_eq!(prepared.samples.len(), 360_000);
    }
    fn fixture() -> (Prepare, Vec<u8>) {
        let id = || uuid::Uuid::new_v4().to_string();
        let mut pcm = vec![0; 360_000 * 4];
        pcm[100 * 4..101 * 4].copy_from_slice(&0.25f32.to_le_bytes());
        let mut d = Prepare {
            schema_version: 2,
            binding: Binding {
                server_instance: id(),
                workspace_id: id(),
                context_id: id(),
                clock_epoch: id(),
            },
            client_id: id(),
            operation_id: id(),
            event_id: id(),
            slot_start_unix_us: 7_500_000,
            latest_start_unix_us: 7_550_000,
            manifest: Manifest {
                schema_version: 2,
                mode: Mode::FT4,
                message_form: "wsjt77".into(),
                message: "CQ K1ABC FN42".into(),
                format: Format {
                    sample_rate_hz: 48_000,
                    channels: 1,
                    encoding: "f32le".into(),
                },
                offset_hz_f32_bits: 1000f32.to_bits(),
                frames: 360_000,
                waveform_frames: 101,
                filter_id: "kaiser-513-fc6000-beta10-x4-candidate1".into(),
                coefficients_sha256:
                    "b3b266e2346da0272cdb1910943a6ba8020d17a56e55a7b0eb0f12cd2c73c764".into(),
                group_delay_frames: 256,
                support_start_frame: 100,
                gain_f32_bits: 0.5f32.to_bits(),
                pcm_byte_count: 1_440_000,
                period_duration_us: 7_500_000,
                pcm_sha256: String::new(),
                content_sha256: String::new(),
            },
        };
        seal(&mut d, &pcm);
        (d, pcm)
    }
    fn seal(d: &mut Prepare, pcm: &[u8]) {
        d.manifest.pcm_sha256 = format!("{:x}", Sha256::digest(pcm));
        let m = &d.manifest;
        let mut h = Sha256::new();
        h.update(b"xnexus-final-period-v2\0");
        h.update([if m.mode == Mode::FT4 { 4 } else { 8 }]);
        h.update(48000u32.to_le_bytes());
        h.update(1u16.to_le_bytes());
        h.update(b"f32le\0");
        h.update(m.offset_hz_f32_bits.to_le_bytes());
        h.update((m.message.len() as u32).to_le_bytes());
        h.update(m.message.as_bytes());
        h.update((pcm.len() as u64).to_le_bytes());
        h.update(pcm);
        d.manifest.content_sha256 = format!("{:x}", h.finalize());
    }
    fn body(d: &Prepare, pcm: &[u8], pretty: bool) -> Vec<u8> {
        let json = if pretty {
            serde_json::to_vec_pretty(d).unwrap()
        } else {
            serde_json::to_vec(d).unwrap()
        };
        let mut bytes = (json.len() as u32).to_le_bytes().to_vec();
        bytes.extend(json);
        bytes.extend(pcm);
        bytes
    }
    #[test]
    fn exact_final_bytes_are_retained_and_raw_json_is_part_of_operation_identity() {
        let (d, pcm) = fixture();
        let a = body(&d, &pcm, false);
        let b = body(&d, &pcm, true);
        let first = validate(&a, 100, 4500, 0.95).unwrap();
        let second = validate(&b, 100, 4500, 0.95).unwrap();
        assert_eq!(first.descriptor, second.descriptor);
        assert_ne!(first.body_sha256, second.body_sha256);
        assert_eq!(first.samples[100], 0.25);
        assert_eq!(
            first
                .samples
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect::<Vec<_>>(),
            pcm
        );
    }
    #[test]
    fn modified_samples_descriptor_support_and_unsafe_float_are_refused() {
        let (mut d, mut pcm) = fixture();
        let mut changed = body(&d, &pcm, false);
        *changed.last_mut().unwrap() = 1;
        assert!(validate(&changed, 100, 4500, 0.95).is_err());
        pcm[100 * 4..101 * 4].copy_from_slice(&f32::NAN.to_le_bytes());
        seal(&mut d, &pcm);
        assert!(validate(&body(&d, &pcm, false), 100, 4500, 0.95).is_err());
        let (mut d, pcm) = fixture();
        d.manifest.support_start_frame = 101;
        assert!(validate(&body(&d, &pcm, false), 100, 4500, 0.95).is_err());
        let (mut d, pcm) = fixture();
        d.manifest.gain_f32_bits = 0.81f32.to_bits();
        assert!(validate(&body(&d, &pcm, false), 100, 4500, 0.95).is_err());
        let (mut d, pcm) = fixture();
        d.latest_start_unix_us = d.slot_start_unix_us + 50_001;
        assert!(validate(&body(&d, &pcm, false), 100, 4500, 0.95).is_err());
    }
    #[test]
    fn bounded_rejection_precedes_json_or_sample_allocation() {
        assert!(validate(&[], 200, 3000, 0.9).is_err());
        assert!(validate(&[255; 5], 200, 3000, 0.9).is_err());
        assert!(validate(&vec![0; MAX_PREPARE_BYTES + 1], 200, 3000, 0.9).is_err());
    }
    #[test]
    fn canonical_ids_and_digests_are_not_normalized() {
        assert!(canonical_uuid("00000000-0000-4000-8000-000000000001"));
        assert!(!canonical_uuid("00000000-0000-4000-8000-00000000000A"));
        assert!(!canonical_uuid("00000000-0000-0000-0000-000000000000"));
        assert!(digest(&"a".repeat(64)));
        assert!(!digest(&"A".repeat(64)));
    }
}
