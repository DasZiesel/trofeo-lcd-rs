//! Synthetic buffers only. No producer, operating-system API or device initialization.
use trofeo_lcd::{
    hwinfo_sm2::*,
    sensor_policy::{validate_text, HwinfoSelector, Source, Status},
};

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn get32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}
fn text(b: &mut [u8], at: usize, value: &[u8]) {
    b[at..at + value.len()].copy_from_slice(value);
    b[at + value.len()] = 0;
}
fn number(b: &mut [u8], at: usize, v: f64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn fixture(version: u32, revision: u32) -> Vec<u8> {
    let h = if revision == 0 { 44 } else { 48 };
    let s = if version == 1 { 264 } else { 392 };
    let r = if version == 1 { 316 } else { 460 };
    let so = h + 3; // Deliberately unaligned sections with gaps.
    let ro = so + 2 * s + 7;
    let mut b = vec![0; ro + 2 * r];
    b[..4].copy_from_slice(&[0x53, 0x69, 0x57, 0x48]);
    put32(&mut b, 4, version);
    put32(&mut b, 8, revision);
    b[12..20].copy_from_slice(&(-1234567i64).to_le_bytes());
    for (at, n) in [(20, so), (24, s), (28, 2), (32, ro), (36, r), (40, 2)] {
        put32(&mut b, at, n as u32);
    }
    if revision == 1 {
        put32(&mut b, 44, 2000);
    }
    for i in 0..2 {
        let a = so + i * s;
        put32(&mut b, a, 0x1234);
        put32(&mut b, a + 4, i as u32);
        text(&mut b, a + 8, b"Synthetic sensor");
        text(&mut b, a + 136, b"User sensor");
        if version == 2 {
            text(&mut b, a + 264, "Sensor ä".as_bytes());
        }
        let a = ro + i * r;
        put32(&mut b, a, if i == 0 { 1 } else { 3 });
        put32(&mut b, a + 4, i as u32);
        put32(&mut b, a + 8, 0x42);
        text(&mut b, a + 12, b"Original");
        text(&mut b, a + 140, b"User");
        text(&mut b, a + 268, b"RPM");
        for (offset, n) in [(284, 0.0), (292, -2.5), (300, 45.25), (308, 17.125)] {
            number(&mut b, a + offset, n);
        }
        if version == 2 {
            text(&mut b, a + 316, "Reading 温".as_bytes());
            text(&mut b, a + 444, "°C".as_bytes());
        }
    }
    b
}

#[test]
fn documented_layouts_and_unaligned_little_endian_fields() {
    for (v, r) in [(1, 0), (1, 1), (2, 1)] {
        let bytes = fixture(v, r);
        let s = decode(&bytes).unwrap();
        assert_eq!((s.header.version, s.header.revision), (v, r));
        assert_eq!(s.header.poll_time, -1234567);
        assert_eq!(
            s.header.polling_period,
            if r == 0 { None } else { Some(2000) }
        );
        assert_eq!(s.sensors.len(), 2);
        assert_eq!(s.readings.len(), 2);
        assert_eq!(
            s.readings[0].identity,
            "hwinfo:0x1234/0x0/0x42".parse::<HwinfoSelector>().unwrap()
        );
        assert_eq!(s.readings[1].identity.sensor_instance, 1);
        assert_eq!(s.readings[0].values.minimum, -2.5);
        assert_eq!(s.readings[0].values.maximum, 45.25);
        assert_eq!(s.readings[0].values.average, 17.125);
        assert_eq!(s.source(), Source::Hwinfo);
        assert_eq!(s.status(), Status::Unverified);
        assert_eq!(s.readings[0].status(), Status::Unverified); // A raw zero is not confirmed.
    }
}
#[test]
fn signature_is_numeric_not_ascii_and_inactive_never_accepted() {
    for signature in [
        0,
        0x44454144,
        0x44414544,
        u32::from_le_bytes(*b"HWiS"),
        u32::MAX,
    ] {
        let mut b = fixture(2, 1);
        put32(&mut b, 0, signature);
        assert_eq!(decode(&b), Err(DecodeError::Signature(signature)));
    }
    assert_eq!(ACTIVE_SIGNATURE, 0x48576953);
}
#[test]
fn unknown_versions_and_revisions_rejected() {
    for (v, r) in [(0, 0), (2, 0), (3, 1), (1, 2), (2, 2), (u32::MAX, u32::MAX)] {
        let mut b = fixture(2, 1);
        put32(&mut b, 4, v);
        put32(&mut b, 8, r);
        assert_eq!(
            decode(&b),
            Err(DecodeError::UnsupportedLayout {
                version: v,
                revision: r
            })
        );
    }
}
#[test]
fn every_truncation_of_each_layout_returns_error_without_panic() {
    for (v, r) in [(1, 0), (1, 1), (2, 1)] {
        let b = fixture(v, r);
        for n in 0..b.len() {
            let result = std::panic::catch_unwind(|| decode(&b[..n]));
            assert!(result.is_ok(), "panic at {v}/{r}/{n}");
            assert!(result.unwrap().is_err(), "accepted truncation {v}/{r}/{n}");
        }
    }
}
#[test]
fn exact_strides_required_even_for_empty_sections() {
    for field in [24, 36] {
        for size in [0, 1, 263, 265, 315, 317, 391, 393, 459, 461, u32::MAX] {
            let mut b = fixture(2, 1);
            put32(&mut b, field, size);
            assert_eq!(decode(&b), Err(DecodeError::ElementSize));
        }
    }
}
#[test]
fn section_bounds_overlap_and_dword_addition_overflow() {
    for field in [20, 32] {
        for offset in [0, 12, 47] {
            let mut b = fixture(2, 1);
            put32(&mut b, field, offset);
            assert_eq!(decode(&b), Err(DecodeError::InvalidSection));
        }
        let mut b = fixture(2, 1);
        let end = b.len() as u32;
        put32(&mut b, field, end);
        assert_eq!(decode(&b), Err(DecodeError::Truncated));
        put32(&mut b, field, u32::MAX - 10);
        assert_eq!(decode(&b), Err(DecodeError::ArithmeticOverflow));
    }
    let mut b = fixture(2, 1);
    let so = get32(&b, 20) as u32;
    put32(&mut b, 32, so + 10);
    assert_eq!(decode(&b), Err(DecodeError::OverlappingSections));
}
#[test]
fn excessive_counts_and_input_size_are_bounded_before_allocation() {
    for (field, max) in [(28, MAX_SENSORS), (40, MAX_READINGS)] {
        for count in [max + 1, u32::MAX] {
            let mut b = fixture(2, 1);
            put32(&mut b, field, count);
            assert_eq!(decode(&b), Err(DecodeError::ResourceLimit));
        }
    }
    assert_eq!(
        decode(&vec![0; MAX_BUFFER_BYTES + 1]),
        Err(DecodeError::InputTooLarge)
    );
}
#[test]
fn empty_sections_and_trailing_padding() {
    let mut b = fixture(1, 0);
    put32(&mut b, 28, 0);
    put32(&mut b, 40, 0);
    put32(&mut b, 20, 44);
    put32(&mut b, 32, 44);
    b.truncate(44);
    assert!(decode(&b).unwrap().readings.is_empty());
    b.extend([0xff; 17]);
    assert!(decode(&b).is_ok());
}
#[test]
fn reversed_disjoint_sections_use_offsets_not_assumed_order() {
    let old = fixture(2, 1);
    let so = get32(&old, 20);
    let ro = get32(&old, 32);
    let mut b = old.clone();
    b[48..].fill(0);
    let new_ro = 51;
    let new_so = new_ro + 920 + 7;
    b.resize(new_so + 784, 0);
    b[new_ro..new_ro + 920].copy_from_slice(&old[ro..ro + 920]);
    b[new_so..new_so + 784].copy_from_slice(&old[so..so + 784]);
    put32(&mut b, 20, new_so as u32);
    put32(&mut b, 32, new_ro as u32);
    assert_eq!(decode(&b).unwrap().readings, decode(&old).unwrap().readings);
}
#[test]
fn invalid_sensor_index_and_no_sensor_for_reading_rejected() {
    for index in [2, u32::MAX] {
        let mut b = fixture(1, 1);
        let ro = get32(&b, 32);
        put32(&mut b, ro + 4, index);
        assert_eq!(decode(&b), Err(DecodeError::InvalidSensorIndex(index)));
    }
    let mut b = fixture(1, 1);
    put32(&mut b, 28, 0);
    assert_eq!(decode(&b), Err(DecodeError::InvalidSensorIndex(0)));
}
#[test]
fn duplicate_identities_are_rejected_but_reading_ids_are_sensor_local() {
    let mut b = fixture(2, 1);
    let so = get32(&b, 20);
    put32(&mut b, so + 392 + 4, 0);
    assert_eq!(decode(&b), Err(DecodeError::DuplicateSensor));
    let mut b = fixture(2, 1);
    let ro = get32(&b, 32);
    put32(&mut b, ro + 460 + 4, 0);
    assert_eq!(decode(&b), Err(DecodeError::DuplicateReading));
    let b = fixture(2, 1);
    assert_eq!(decode(&b).unwrap().readings.len(), 2);
}
#[test]
fn identities_survive_reordering_and_allow_full_u32_range() {
    let mut b = fixture(2, 1);
    let so = get32(&b, 20);
    let ro = get32(&b, 32);
    put32(&mut b, so, u32::MAX);
    put32(&mut b, so + 4, u32::MAX);
    put32(&mut b, ro + 8, u32::MAX);
    let id = decode(&b).unwrap().readings[0].identity;
    let first = b[so..so + 392].to_vec();
    let second = b[so + 392..so + 784].to_vec();
    b[so..so + 392].copy_from_slice(&second);
    b[so + 392..so + 784].copy_from_slice(&first);
    put32(&mut b, ro + 4, 1);
    put32(&mut b, ro + 460 + 4, 0);
    assert_eq!(decode(&b).unwrap().readings[0].identity, id);
    assert_eq!(id.to_string(), "hwinfo:0xffffffff/0xffffffff/0xffffffff");
    put32(&mut b, ro + 8, 0);
    assert_eq!(decode(&b).unwrap().readings[0].identity.reading_id, 0);
}
#[test]
fn ansi_is_lossless_without_guessing_a_codepage() {
    for v in [1, 2] {
        let mut b = fixture(v, 1);
        let so = get32(&b, 20);
        let ro = get32(&b, 32);
        text(&mut b, so + 8, &[0x80, 0xe9, 0xff]);
        text(&mut b, ro + 268, &[0xb0, b'C']);
        let s = decode(&b).unwrap();
        assert_eq!(s.sensors[0].original_name.bytes(), [0x80, 0xe9, 0xff]);
        assert_eq!(s.sensors[0].original_name.ascii(), None);
        assert_eq!(s.sensors[0].user_name.ascii(), Some("User sensor"));
        assert_eq!(s.readings[0].unit.bytes(), [0xb0, b'C']);
        assert_eq!(s.readings[0].status(), Status::Unverified);
    }
}
#[test]
fn utf8_fields_follow_version_and_do_not_replace_original_bytes() {
    let b = fixture(2, 1);
    let s = decode(&b).unwrap();
    assert_eq!(s.sensors[0].user_name_utf8, Some("Sensor ä"));
    assert_eq!(s.readings[0].user_label_utf8, Some("Reading 温"));
    assert_eq!(s.readings[0].unit_utf8, Some("°C"));
    assert_eq!(s.readings[0].unit.ascii(), Some("RPM")); // Metadata mismatch does not become a valid role sample.
    let b = fixture(1, 0);
    let s = decode(&b).unwrap();
    assert_eq!(s.sensors[0].user_name_utf8, None);
    assert_eq!(s.readings[0].unit_utf8, None);
}
#[test]
fn malformed_utf8_and_missing_terminators_rejected() {
    let base = fixture(2, 1);
    let so = get32(&base, 20);
    let ro = get32(&base, 32);
    for (at, len) in [
        (so + 8, 128),
        (so + 136, 128),
        (so + 264, 128),
        (ro + 12, 128),
        (ro + 140, 128),
        (ro + 268, 16),
        (ro + 316, 128),
        (ro + 444, 16),
    ] {
        let mut b = base.clone();
        b[at..at + len].fill(b'x');
        assert_eq!(decode(&b), Err(DecodeError::UnterminatedText));
        b[at + len - 1] = 0;
        assert!(decode(&b).is_ok());
    }
    for at in [so + 264, ro + 316, ro + 444] {
        for bad in [
            &[0xff][..],
            &[0xc0, 0xaf],
            &[0xed, 0xa0, 0x80],
            &[0xe2, 0x82],
        ] {
            let mut b = base.clone();
            text(&mut b, at, bad);
            assert_eq!(decode(&b), Err(DecodeError::InvalidUtf8));
        }
        let mut b = base.clone();
        b[at] = 0;
        b[at + 1] = 0xff;
        assert!(decode(&b).is_ok());
    }
}
#[test]
fn all_wire_types_and_unknown_values_are_safe_and_unverified() {
    for n in 0..=9 {
        let mut b = fixture(2, 1);
        let ro = get32(&b, 32);
        put32(&mut b, ro, n);
        let s = decode(&b).unwrap();
        assert_eq!(s.readings[0].reading_type, ReadingType::from(n));
        assert_eq!(s.readings[0].status(), Status::Unverified);
    }
    let mut b = fixture(2, 1);
    let ro = get32(&b, 32);
    put32(&mut b, ro, u32::MAX);
    assert_eq!(
        decode(&b).unwrap().readings[0].reading_type,
        ReadingType::Unknown(u32::MAX)
    );
}
#[test]
fn units_are_raw_metadata_not_inferred_from_type_or_role() {
    for unit in ["", "RPM", "V", "%", "unexpected"] {
        let mut b = fixture(2, 1);
        let ro = get32(&b, 32);
        text(&mut b, ro + 444, unit.as_bytes());
        let s = decode(&b).unwrap();
        assert_eq!(s.readings[0].unit_utf8, Some(unit));
        assert_eq!(s.readings[0].status(), Status::Unverified);
    }
}
#[test]
fn nonfinite_values_preserve_bits_without_quality_promotion() {
    for at in [284, 292, 300, 308] {
        for bits in [
            0x7ff8000000000042u64,
            0x7ff0000000000000,
            0xfff0000000000000,
        ] {
            let mut b = fixture(2, 1);
            let ro = get32(&b, 32);
            b[ro + at..ro + at + 8].copy_from_slice(&bits.to_le_bytes());
            let s = decode(&b).unwrap();
            let v = s.readings[0].values;
            let decoded = match at {
                284 => v.current,
                292 => v.minimum,
                300 => v.maximum,
                _ => v.average,
            };
            assert_eq!(decoded.to_bits(), bits);
            assert_eq!(s.readings[0].status(), Status::Invalid);
            assert_eq!(s.readings[1].status(), Status::Unverified);
        }
    }
    let mut b = fixture(2, 1);
    let ro = get32(&b, 32);
    number(&mut b, ro + 284, -0.0);
    assert_eq!(
        decode(&b).unwrap().readings[0].values.current.to_bits(),
        (-0.0f64).to_bits()
    );
}
#[test]
fn time_metadata_never_confirms_live_data_or_activates_provider() {
    let mut b = fixture(2, 1);
    b[12..20].copy_from_slice(&i64::MAX.to_le_bytes());
    put32(&mut b, 44, 0);
    assert_eq!(decode(&b).unwrap().status(), Status::Unverified);
    assert!(validate_text("sensor_source=hwinfo")
        .unwrap()
        .ensure_available()
        .is_err());
}
#[test]
fn deterministic_mutation_and_random_corpus_never_panics() {
    for (v, r) in [(1, 0), (1, 1), (2, 1)] {
        let base = fixture(v, r);
        for i in 0..base.len() {
            for byte in [0, 0x80, 0xff] {
                let mut b = base.clone();
                b[i] = byte;
                assert!(
                    std::panic::catch_unwind(|| decode(&b)).is_ok(),
                    "mutation {v}/{r}/{i}/{byte}"
                );
            }
        }
    }
    let mut seed = 0x123456789abcdef0u64;
    for n in 0..5000 {
        let mut b = vec![0; n % 2048];
        for byte in &mut b {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (seed >> 32) as u8;
        }
        if b.len() >= 12 && n % 2 == 0 {
            put32(&mut b, 0, 0x48576953);
            put32(&mut b, 4, 2);
            put32(&mut b, 8, 1);
        }
        assert!(
            std::panic::catch_unwind(|| decode(&b)).is_ok(),
            "random {n}"
        );
    }
}
