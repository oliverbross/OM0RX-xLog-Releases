//! Synthetic byte-integrity tests, never a substitute for actual producer/golden waves.
use super::*;
use serde_json::{json, Value};

fn limits() -> Limits {
    // Explicit disposable bounds, not shipping or commissioning defaults.
    Limits { max_manifest_bytes: 4096, max_message_bytes: 37, max_frames: 180_000,
        max_pcm_bytes: 720_000, min_offset_hz: 0.0, max_offset_hz: 6000.0, max_abs_sample: 1.0 }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn resign(manifest: &mut Value, pcm: &[u8]) {
    manifest["pcm_sha256"] = json!(hex(&Sha256::digest(pcm)));
    // Build the documented concatenation independently of validator helpers.
    let mut bytes = b"xnexus-local-period-v1\0".to_vec();
    bytes.push(if manifest["mode"] == "FT8" { 8 } else { 4 });
    bytes.extend_from_slice(&(manifest["format"]["sample_rate_hz"].as_u64().unwrap() as u32).to_le_bytes());
    bytes.extend_from_slice(&(manifest["format"]["channels"].as_u64().unwrap() as u16).to_le_bytes());
    bytes.extend_from_slice(b"f32le\0");
    bytes.extend_from_slice(&(manifest["offset_hz_f32_bits"].as_u64().unwrap() as u32).to_le_bytes());
    let message = manifest["message"].as_str().unwrap().as_bytes();
    bytes.extend_from_slice(&(message.len() as u32).to_le_bytes()); bytes.extend_from_slice(message);
    bytes.extend_from_slice(&(pcm.len() as u64).to_le_bytes()); bytes.extend_from_slice(pcm);
    manifest["content_sha256"] = json!(hex(&Sha256::digest(bytes)));
}
fn fixture(mode: &str) -> (Value, Vec<u8>) {
    let (frames, lead, waveform, duration) = match mode {
        "FT8" => (180_000, 6000, 157_680, 15_000_000),
        "FT4" => (90_000, 3600, 64_080, 7_500_000),
        _ => panic!("unsupported synthetic fixture"),
    };
    let mut pcm = vec![0; frames * 4];
    pcm[(lead + 1) * 4..(lead + 2) * 4].copy_from_slice(&0.25f32.to_le_bytes());
    let mut manifest = json!({"schema_version":1,"mode":mode,"message_form":"standard_type1",
        "message":"CQ K1ABC FN42","format":{"sample_rate_hz":12000,"channels":1,"encoding":"f32le"},
        "offset_hz_f32_bits":1500.0f32.to_bits(),"frames":frames,"waveform_frames":waveform,
        "pcm_byte_count":pcm.len(),"period_duration_us":duration,"pcm_sha256":"","content_sha256":""});
    resign(&mut manifest, &pcm); (manifest, pcm)
}
fn check(manifest: &Value, pcm: &[u8], policy: Limits) -> Result<ValidatedPeriod, Error> {
    validate(&serde_json::to_vec(manifest).unwrap(), pcm.to_vec().into_boxed_slice(), policy)
}
fn error(manifest: &Value, pcm: &[u8], policy: Limits) -> Error {
    check(manifest, pcm, policy).err().expect("must reject")
}

#[test]
fn full_periods_preserve_exact_manifest_and_pcm_for_both_modes() {
    for mode in ["FT8", "FT4"] {
        let (manifest, pcm) = fixture(mode); let result = check(&manifest, &pcm, limits()).unwrap();
        assert_eq!(serde_json::to_value(result.manifest()).unwrap(), manifest);
        assert_eq!(result.pcm(), pcm); assert_eq!(result.peak_abs_sample(), 0.25);
    }
}
#[test]
fn unknown_duplicate_fractional_and_overflow_fields_are_rejected() {
    let (manifest, pcm) = fixture("FT8");
    for extra in ["authority", "path", "url"] {
        let mut changed = manifest.clone(); changed[extra] = json!("not permitted");
        assert_eq!(error(&changed, &pcm, limits()), Error::InvalidManifest);
    }
    let mut nested = manifest.clone(); nested["format"]["byte_order"] = json!("little");
    assert_eq!(error(&nested, &pcm, limits()), Error::InvalidManifest);
    let original = serde_json::to_string(&manifest).unwrap();
    for text in [original.replacen('{', "{\"mode\":\"FT4\",", 1),
        original.replace("\"channels\":1", "\"channels\":1,\"channels\":1"),
        original.replace("\"schema_version\":1", "\"schema_version\":1.0"),
        original.replace("\"schema_version\":1", "\"schema_version\":4294967296")] {
        assert_eq!(validate(text.as_bytes(), pcm.clone().into_boxed_slice(), limits()).err(), Some(Error::InvalidManifest));
    }
    for bits in [json!(-1), json!(4294967296u64), json!(1.5), json!("1153138688")] {
        let mut changed = manifest.clone(); changed["offset_hz_f32_bits"] = bits;
        assert_eq!(error(&changed, &pcm, limits()), Error::InvalidManifest);
    }
}
#[test]
fn schema_format_geometry_and_exact_payload_length_must_agree() {
    for mode in ["FT8", "FT4"] {
        let (manifest, pcm) = fixture(mode);
        for field in ["frames", "waveform_frames", "pcm_byte_count", "period_duration_us"] {
            let mut changed = manifest.clone(); changed[field] = json!(0);
            assert_eq!(error(&changed, &pcm, limits()), Error::InvalidGeometry);
        }
        let mut changed = manifest.clone(); changed["waveform_frames"] = json!(manifest["frames"].as_u64().unwrap() + 1);
        assert_eq!(error(&changed, &pcm, limits()), Error::InvalidGeometry);
        for (field, value) in [("sample_rate_hz", 48000), ("channels", 2)] {
            let mut changed = manifest.clone(); changed["format"][field] = json!(value);
            assert_eq!(error(&changed, &pcm, limits()), Error::UnsupportedFormat);
        }
        let mut changed = manifest.clone(); changed["format"]["encoding"] = json!("s16le");
        assert_eq!(error(&changed, &pcm, limits()), Error::InvalidManifest);
        let mut changed = manifest.clone(); changed["schema_version"] = json!(2);
        assert_eq!(error(&changed, &pcm, limits()), Error::UnsupportedSchema);
        for cut in [1, 4] { assert_eq!(error(&manifest, &pcm[..pcm.len()-cut], limits()), Error::InvalidGeometry); }
        let mut extra = pcm.clone(); extra.push(0);
        assert!(matches!(error(&manifest, &extra, limits()), Error::SizeLimit | Error::InvalidGeometry));
    }
}
#[test]
fn each_digest_and_content_field_is_checked_independently() {
    let (manifest, pcm) = fixture("FT8");
    for field in ["pcm_sha256", "content_sha256"] {
        for bad in ["A".repeat(64), "g".repeat(64), "0".repeat(63), "0".repeat(65)] {
            let mut changed = manifest.clone(); changed[field] = json!(bad);
            assert_eq!(error(&changed, &pcm, limits()), Error::InvalidDigest);
        }
        let mut changed = manifest.clone(); changed[field] = json!("0".repeat(64));
        assert_eq!(error(&changed, &pcm, limits()), Error::DigestMismatch);
    }
    let mut changed_pcm = pcm.clone(); changed_pcm[6001*4..6002*4].copy_from_slice(&0.5f32.to_le_bytes());
    assert_eq!(error(&manifest, &changed_pcm, limits()), Error::DigestMismatch);
    let mut changed = manifest.clone(); changed["message"] = json!("CQ W9XYZ EN37");
    assert_eq!(error(&changed, &pcm, limits()), Error::DigestMismatch);
    let mut changed = manifest.clone(); changed["offset_hz_f32_bits"] = json!(1500.0f32.to_bits() + 1);
    assert_eq!(error(&changed, &pcm, limits()), Error::DigestMismatch);
}
#[test]
fn offset_bits_preserve_signed_zero_and_one_ulp_without_decimal_roundtrip() {
    let (manifest, pcm) = fixture("FT4"); let mut digests = Vec::new();
    for bits in [0.0f32.to_bits(), (-0.0f32).to_bits(), 1500.0f32.to_bits(), 1500.0f32.to_bits()+1] {
        let mut changed = manifest.clone(); changed["offset_hz_f32_bits"] = json!(bits); resign(&mut changed, &pcm);
        let result = check(&changed, &pcm, limits()).unwrap();
        assert_eq!(result.manifest().offset_hz_f32_bits, bits);
        assert_eq!(result.pcm(), pcm); digests.push(changed["content_sha256"].clone());
        assert_eq!(changed["pcm_sha256"], manifest["pcm_sha256"]);
    }
    for i in 0..digests.len() { for j in i+1..digests.len() { assert_ne!(digests[i], digests[j]); } }
}
#[test]
fn offsets_require_finiteness_owner_limits_and_strict_nyquist_margin() {
    for mode in ["FT8", "FT4"] {
        let (manifest, pcm) = fixture(mode);
        for offset in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 6000.0] {
            let mut changed = manifest.clone(); changed["offset_hz_f32_bits"] = json!(offset.to_bits());
            assert_eq!(error(&changed, &pcm, limits()), Error::InvalidOffset);
        }
        let boundary = if mode == "FT8" { 5956.25f32 } else { 5937.5f32 };
        let mut changed = manifest.clone(); changed["offset_hz_f32_bits"] = json!(boundary.to_bits());
        assert_eq!(error(&changed, &pcm, limits()), Error::InvalidOffset);
        changed["offset_hz_f32_bits"] = json!(boundary.to_bits()-1); resign(&mut changed, &pcm);
        assert!(check(&changed, &pcm, limits()).is_ok());
        let mut policy = limits(); policy.min_offset_hz = 1600.0;
        assert_eq!(error(&manifest, &pcm, policy), Error::InvalidOffset);
        policy = limits(); policy.max_offset_hz = 1400.0;
        assert_eq!(error(&manifest, &pcm, policy), Error::InvalidOffset);
    }
}
#[test]
fn every_sample_is_checked_for_nonfinite_and_amplitude_without_clamping() {
    for mode in ["FT8", "FT4"] {
        let (manifest, pcm) = fixture(mode); let sample_index = if mode == "FT8" { 6001 } else { 3601 };
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.01, -1.01] {
            let mut bad = pcm.clone(); bad[sample_index*4..(sample_index+1)*4].copy_from_slice(&value.to_le_bytes());
            assert_eq!(error(&manifest, &bad, limits()), if value.is_finite() { Error::AmplitudeLimit } else { Error::InvalidSample });
        }
        let mut policy = limits(); policy.max_abs_sample = 0.2;
        assert_eq!(error(&manifest, &pcm, policy), Error::AmplitudeLimit);
        let zeros = vec![0; pcm.len()]; assert_eq!(error(&manifest, &zeros, limits()), Error::EmptySignal);
    }
}
#[test]
fn negative_zero_lead_is_preserved_but_padding_must_be_positive_zero() {
    for mode in ["FT8", "FT4"] {
        let (mut manifest, mut pcm) = fixture(mode);
        pcm[..4].copy_from_slice(&(-0.0f32).to_le_bytes()); resign(&mut manifest, &pcm);
        assert_eq!(&check(&manifest, &pcm, limits()).unwrap().pcm()[..4], &(-0.0f32).to_le_bytes());
        let end = pcm.len(); pcm[end-4..].copy_from_slice(&(-0.0f32).to_le_bytes()); resign(&mut manifest, &pcm);
        assert_eq!(error(&manifest, &pcm, limits()), Error::InvalidSilence);
        pcm[end-4..].fill(0); pcm[..4].copy_from_slice(&f32::MIN_POSITIVE.to_le_bytes());
        assert_eq!(error(&manifest, &pcm, limits()), Error::InvalidSilence);
    }
}
#[test]
fn owner_limits_are_explicit_valid_and_narrower_than_format_caps() {
    let (manifest, pcm) = fixture("FT8");
    let mut bad = Vec::new();
    let mut p = limits(); p.max_manifest_bytes = 0; bad.push(p);
    p = limits(); p.max_message_bytes = 0; bad.push(p);
    p = limits(); p.max_frames = 0; bad.push(p);
    p = limits(); p.max_pcm_bytes = 0; bad.push(p);
    for v in [0.0, -1.0, 1.01, f32::NAN, f32::INFINITY] { p = limits(); p.max_abs_sample = v; bad.push(p); }
    for v in [-1.0, f32::NAN, f32::INFINITY, 7000.0] { p = limits(); p.min_offset_hz = v; bad.push(p); }
    for v in [-1.0, f32::NAN, f32::INFINITY] { p = limits(); p.max_offset_hz = v; bad.push(p); }
    for policy in bad { assert_eq!(error(&manifest, &pcm, policy), Error::InvalidLimits); }
    p = limits(); p.max_manifest_bytes = 100; assert_eq!(error(&manifest, &pcm, p), Error::SizeLimit);
    p = limits(); p.max_pcm_bytes = pcm.len()-1; assert_eq!(error(&manifest, &pcm, p), Error::SizeLimit);
    p = limits(); p.max_frames = 90_000; assert_eq!(error(&manifest, &pcm, p), Error::InvalidGeometry);
    p = limits(); p.max_message_bytes = 5; assert_eq!(error(&manifest, &pcm, p), Error::InvalidMessage);
}
#[test]
fn message_shape_does_not_normalize_or_accept_lossy_forms() {
    let (manifest, pcm) = fixture("FT8");
    for message in ["", "cq K1ABC FN42", " CQ K1ABC FN42", "CQ K1ABC FN42 ", "CQ  K1ABC FN42", "CQ K1ABC\0FN42", "CQ K1ABC ÉM12"] {
        let mut changed = manifest.clone(); changed["message"] = json!(message);
        assert_eq!(error(&changed, &pcm, limits()), Error::InvalidMessage);
    }
    for message in ["HELLO WORLD", "CQ DX K1ABC FN42", "CQ K1ABC -10", "CQ <K1ABC> FN42", "K1ABC/P W9XYZ -10", "K1ABC W9XYZ -00", "K1ABC W9XYZ -31", "CQ Q1ABC FN42"] {
        let mut changed = manifest.clone(); changed["message"] = json!(message);
        assert_eq!(error(&changed, &pcm, limits()), Error::UnsupportedMessageShape);
    }
    for message in ["CQ QU1RK FN42", "K1ABC W9XYZ EN37", "K1ABC W9XYZ -10", "K1ABC W9XYZ R-10", "K1ABC W9XYZ RR73"] {
        let mut changed = manifest.clone(); changed["message"] = json!(message); resign(&mut changed, &pcm);
        assert!(check(&changed, &pcm, limits()).is_ok());
    }
}
#[test]
fn bounded_parser_rejects_empty_truncated_and_oversized_input() {
    let (manifest, pcm) = fixture("FT8"); let encoded = serde_json::to_vec(&manifest).unwrap();
    assert_eq!(validate(&[], pcm.clone().into_boxed_slice(), limits()).err(), Some(Error::SizeLimit));
    assert_eq!(validate(&encoded[..encoded.len()-1], pcm.clone().into_boxed_slice(), limits()).err(), Some(Error::InvalidManifest));
    let oversized = vec![b' '; 4097]; let mut policy = limits(); policy.max_manifest_bytes = usize::MAX;
    assert_eq!(validate(&oversized, pcm.clone().into_boxed_slice(), policy).err(), Some(Error::SizeLimit));
    policy.max_pcm_bytes = usize::MAX;
    assert_eq!(validate(&encoded, vec![0;720001].into_boxed_slice(), policy).err(), Some(Error::SizeLimit));
}
#[test]
fn recomputed_non_message_waveform_and_unhashed_wave_length_show_certification_boundary() {
    // This synthetic one-sample pulse is not an FT8 or FT4 message waveform.
    // Digest-consistent validation MUST NOT be represented as encoded-message proof.
    for mode in ["FT8", "FT4"] {
        let (manifest, pcm) = fixture(mode);
        assert_eq!(pcm.chunks_exact(4).filter(|sample| *sample != [0;4]).count(), 1);
        assert!(check(&manifest, &pcm, limits()).is_ok());
        let mut changed = manifest.clone(); changed["waveform_frames"] = changed["frames"].clone();
        assert_eq!(changed["content_sha256"], manifest["content_sha256"]);
        assert!(check(&changed, &pcm, limits()).is_ok());
        assert_ne!(serde_json::to_vec(&manifest).unwrap(), serde_json::to_vec(&changed).unwrap());
        // Owner immutability must compare the whole typed manifest, not just hashes.
    }
}
