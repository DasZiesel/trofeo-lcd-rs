use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use trofeo_lcd::{
    hwinfo_provider::*,
    hwinfo_sm2::{DecodeError, ReadingType},
    sensor_policy::{validate_text, HwinfoSelector, Role, Status},
};
fn id(n: u32) -> HwinfoSelector {
    HwinfoSelector {
        sensor_id: 1,
        sensor_instance: 2,
        reading_id: n,
    }
}
fn entry(n: u32) -> Entry {
    Entry {
        identity: id(n),
        sensor_name: "Synthetic".into(),
        label: "Temperature".into(),
        reading_type: ReadingType::Temperature,
        unit: "°C".into(),
        current: 42.0,
        numeric_status: Status::Unverified,
    }
}
fn frame(time: i64) -> Frame {
    Frame {
        poll_time_raw: time,
        polling_period_raw: Some(2000),
        entries: vec![entry(3)],
    }
}
fn sec(n: u64) -> Duration {
    Duration::from_secs(n)
}
fn state() -> State {
    State::new(Options::default()).unwrap()
}
#[test]
fn progress_is_not_reading_quality_and_identical_snapshots_never_refresh() {
    let mut s = state();
    s.update(Ok(frame(10)), sec(0));
    assert_eq!(s.snapshot_status(sec(0)), Status::Unverified);
    s.update(Ok(frame(11)), sec(1));
    assert_eq!(s.snapshot_status(sec(1)), Status::Ok);
    let r = s.resolve(Role::CpuTemp, id(3), sec(1));
    assert_eq!(r.sample.status, Status::Ok);
    assert_eq!(r.sample.value, Some(42.));
    assert_eq!(r.diagnostic, Diagnostic::ReadingUnverified);
    let mut repeated = frame(11);
    repeated.entries[0].current = 99.;
    for n in 2..=10 {
        s.update(Ok(repeated.clone()), sec(n));
    }
    assert_eq!(s.last_progress_at, Some(sec(1)));
    assert_eq!(s.generation, 1);
    assert_eq!(s.frame.as_ref().unwrap().entries[0].current, 42.);
    assert_eq!(
        s.resolve(Role::CpuTemp, id(3), sec(10)).sample.status,
        Status::Stale
    );
}
#[test]
fn poll_rollback_clock_rollback_and_long_gap_reset_generation() {
    let mut s = state();
    s.update(Ok(frame(10)), sec(0));
    s.update(Ok(frame(11)), sec(1));
    s.update(Ok(frame(1)), sec(2));
    assert_eq!(s.generation, 2);
    assert_eq!(s.snapshot_status(sec(2)), Status::Unverified);
    s.update(Ok(frame(2)), sec(3));
    s.update(Ok(frame(3)), sec(1));
    assert_eq!(s.generation, 3);
    assert_eq!(s.snapshot_status(sec(1)), Status::Unverified);
    s.update(Ok(frame(4)), sec(20));
    assert_eq!(s.generation, 4);
    assert_eq!(s.snapshot_status(sec(20)), Status::Unverified);
}
#[test]
fn errors_discard_old_values_and_reconnect_requires_new_progress() {
    let mut s = state();
    s.update(Ok(frame(1)), sec(0));
    s.update(Ok(frame(2)), sec(1));
    s.update(Err(Failure::Unavailable), sec(2));
    assert!(s.frame.is_none());
    assert_eq!(s.snapshot_status(sec(2)), Status::Unavailable);
    s.update(Ok(frame(2)), sec(3));
    assert_eq!(s.generation, 2);
    assert_eq!(s.snapshot_status(sec(3)), Status::Unverified);
    s.update(Ok(frame(3)), sec(4));
    assert_eq!(s.snapshot_status(sec(4)), Status::Ok);
}
#[test]
fn identities_survive_order_and_rename_missing_and_duplicates_are_explicit() {
    let mut s = state();
    let mut f = frame(1);
    f.entries.push(entry(4));
    s.update(Ok(f.clone()), sec(0));
    f.poll_time_raw = 2;
    f.entries.reverse();
    f.entries[0].label = "Renamed".into();
    s.update(Ok(f), sec(1));
    assert_eq!(
        s.resolve(Role::CpuTemp, id(3), sec(1)).diagnostic,
        Diagnostic::ReadingUnverified
    );
    assert_eq!(
        s.resolve(Role::CpuTemp, id(99), sec(1)).diagnostic,
        Diagnostic::Missing
    );
    let mut f = frame(3);
    f.entries.push(entry(3));
    s.update(Ok(f), sec(2));
    assert_eq!(
        s.resolve(Role::CpuTemp, id(3), sec(2)).diagnostic,
        Diagnostic::Duplicate
    );
    s.update(Err(Failure::Decode(DecodeError::DuplicateReading)), sec(3));
    assert_eq!(
        s.resolve(Role::CpuTemp, id(3), sec(3)).sample.status,
        Status::Invalid
    );
}
#[test]
fn units_types_and_ranges_never_get_implicitly_converted() {
    for (kind, unit, value, role, reason) in [
        (
            ReadingType::Temperature,
            "°F",
            42.,
            Role::CpuTemp,
            Diagnostic::UnitMismatch,
        ),
        (
            ReadingType::Unknown(99),
            "°C",
            42.,
            Role::CpuTemp,
            Diagnostic::TypeMismatch,
        ),
        (
            ReadingType::Fan,
            "rpm",
            42.,
            Role::Pump,
            Diagnostic::UnitMismatch,
        ),
        (
            ReadingType::Fan,
            "RPM",
            -1.,
            Role::CpuFan,
            Diagnostic::Range,
        ),
        (
            ReadingType::Fan,
            "RPM",
            u32::MAX as f64 + 1.,
            Role::CaseFan,
            Diagnostic::Range,
        ),
        (
            ReadingType::Temperature,
            "°C",
            -274.,
            Role::MbTemp,
            Diagnostic::Range,
        ),
        (
            ReadingType::Temperature,
            "°C",
            1001.,
            Role::GpuTemp,
            Diagnostic::Range,
        ),
        (
            ReadingType::Temperature,
            "°C",
            f64::NAN,
            Role::CpuTemp,
            Diagnostic::Range,
        ),
        (
            ReadingType::Temperature,
            "°C",
            f64::INFINITY,
            Role::CpuTemp,
            Diagnostic::Range,
        ),
    ] {
        let mut s = state();
        let mut f = frame(1);
        f.entries[0].reading_type = kind;
        f.entries[0].unit = unit.into();
        f.entries[0].current = value;
        s.update(Ok(f), sec(0));
        let r = s.resolve(role, id(3), sec(0));
        assert_eq!(r.diagnostic, reason);
        assert_eq!(r.sample.value, None);
    }
    let mut f = frame(1);
    f.entries[0].reading_type = ReadingType::Fan;
    f.entries[0].unit = "RPM".into();
    f.entries[0].current = 0.;
    let mut s = state();
    s.update(Ok(f), sec(0));
    assert_eq!(
        s.resolve(Role::Pump, id(3), sec(0)).sample.status,
        Status::Unverified
    );
}
#[test]
fn mixed_policy_no_fallback_auto_does_not_request_hwinfo_and_reload_is_safe() {
    assert!(!validate_text("").unwrap().hwinfo());
    let settings = validate_text(
        "sensor_source=hwinfo\nsensor_cpu_temp=hwinfo:0x1/0x2/0x3\nsensor_case_fan=lhm:case_fan",
    )
    .unwrap();
    assert!(settings.hwinfo());
    assert!(!settings.native_cpu());
    assert!(settings.lhm());
    let mut s = state();
    s.update(Ok(frame(1)), sec(0));
    let view = View {
        state: Arc::new(s),
        now: sec(0),
    };
    let samples =
        settings.resolve_with_hwinfo([Some(77.); 6], [Some(123.); 6], [Some(88.); 6], Some(&view));
    assert_eq!(samples[0].status, Status::Unverified);
    assert_eq!(samples[0].value, None);
    assert_eq!(samples[2].value, Some(123.));
    assert_eq!(samples[5].value, Some(77.));
    assert_eq!(
        settings.resolve_with_hwinfo([Some(77.); 6], [Some(123.); 6], [Some(88.); 6], None)[0]
            .status,
        Status::Unavailable
    );
    assert!(settings.validate_reload(&settings).is_ok());
    assert!(settings
        .validate_reload(&validate_text("sensor_source=native").unwrap())
        .is_err());
}
fn wait_for(worker: &Worker, predicate: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + sec(3);
    loop {
        if worker.view().is_some_and(|v| predicate(&v.state)) {
            return;
        }
        assert!(Instant::now() < deadline, "worker deadline");
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn worker_recovers_and_shutdown_wakes_poll_sleep() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut worker = Worker::spawn_with(
        Options {
            poll_interval: Duration::from_millis(100),
            stale_after: sec(1),
        },
        move |_| {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(Failure::Unavailable)
            } else {
                Ok(frame(n as i64))
            }
        },
    )
    .unwrap();
    wait_for(&worker, |s| {
        s.snapshot_status(sec(0)) == Status::Unavailable
    });
    wait_for(&worker, |s| {
        s.frame.as_ref().is_some_and(|f| f.poll_time_raw >= 2)
    });
    assert!(calls.load(Ordering::SeqCst) <= 10);
    let start = Instant::now();
    worker.shutdown().unwrap();
    assert!(start.elapsed() < sec(1));
    assert_eq!(
        worker.view().unwrap().state.snapshot_status(sec(0)),
        Status::Closed
    );
    let n = calls.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(calls.load(Ordering::SeqCst), n);
    worker.shutdown().unwrap();
}
#[test]
fn shutdown_cancels_inflight_source_and_render_view_does_not_wait() {
    let entered = Arc::new(AtomicUsize::new(0));
    let flag = entered.clone();
    let mut worker = Worker::spawn_with(Options::default(), move |cancel| {
        flag.store(1, Ordering::Release);
        while !cancel.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(2));
        }
        Err(Failure::Unavailable)
    })
    .unwrap();
    let deadline = Instant::now() + sec(2);
    while entered.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let start = Instant::now();
    let _ = worker.view();
    assert!(start.elapsed() < Duration::from_millis(100));
    worker.shutdown().unwrap();
    assert!(start.elapsed() < sec(1));
}
#[test]
fn worker_panics_become_closed_diagnostics() {
    let worker =
        Worker::spawn_with(Options::default(), |_| panic!("synthetic source failure")).unwrap();
    wait_for(&worker, |s| s.snapshot_status(sec(0)) == Status::Closed);
    assert_eq!(
        worker.view().unwrap().state.failure,
        Some(Failure::WorkerPanicked)
    );
}
#[test]
fn bounded_options_and_stale_at_consumer_time() {
    assert!(State::new(Options {
        poll_interval: Duration::ZERO,
        stale_after: sec(5)
    })
    .is_err());
    let mut s = state();
    s.update(Ok(frame(1)), sec(0));
    s.update(Ok(frame(2)), sec(1));
    assert_eq!(s.snapshot_status(sec(6)), Status::Stale);
    s.close();
    s.update(Ok(frame(3)), sec(7));
    assert!(s.frame.is_none());
}
#[test]
fn owned_inventory_from_bytes_preserves_ids_utf8() {
    let mut b = vec![0u8; 900];
    b[..4].copy_from_slice(b"HWiS");
    for (p, n) in [
        (4, 2u32),
        (8, 1),
        (20, 48),
        (24, 392),
        (28, 1),
        (32, 440),
        (36, 460),
        (40, 1),
        (48, 1),
        (52, 2),
        (440, 1),
        (448, 3),
    ] {
        b[p..p + 4].copy_from_slice(&n.to_le_bytes());
    }
    b[312..317].copy_from_slice(b"Name!");
    b[756..760].copy_from_slice(b"Temp");
    b[884..887].copy_from_slice("°C".as_bytes());
    b[724..732].copy_from_slice(&42.5f64.to_le_bytes());
    let f = Frame::decode(&b).unwrap();
    drop(b);
    assert_eq!(f.entries[0].identity, id(3));
    assert_eq!(f.entries[0].sensor_name, "Name!");
    assert_eq!(f.entries[0].label, "Temp");
    assert_eq!(f.entries[0].unit, "°C");
    assert_eq!(f.entries[0].current, 42.5);
}

#[test]
fn display_projection_expires_without_another_poll_and_recovers_conservatively() {
    use trofeo_lcd::sensor_display::{display_text, Projection};
    let settings =
        validate_text("sensor_source=hwinfo\nsensor_cpu_temp=hwinfo:0x1/0x2/0x3").unwrap();
    let mut s = state();
    s.update(Ok(frame(1)), sec(0));
    for (time, expected) in [(0, Status::Unverified)] {
        let view = View {
            state: Arc::new(s.clone()),
            now: sec(time),
        };
        let p = Projection::new(&settings, Some(&view));
        assert_eq!(p.samples[0].unwrap().status, expected);
        assert!(display_text(&p.samples[0].unwrap()).contains("--"));
    }
    s.update(Ok(frame(2)), sec(1));
    let view = View {
        state: Arc::new(s.clone()),
        now: sec(1),
    };
    let p = Projection::new(&settings, Some(&view));
    assert_eq!(p.samples[0].unwrap().value, Some(42.));
    assert!(display_text(&p.samples[0].unwrap()).contains("42.0 C"));
    assert_eq!(p.samples[1].unwrap().status, Status::NotFound);
    assert!(p.samples[5].is_none()); // native GPU is untouched
    let expired = View {
        state: view.state.clone(),
        now: sec(6),
    };
    let p2 = Projection::new(&settings, Some(&expired));
    assert_eq!(p2.samples[0].unwrap().status, Status::Stale);
    assert_eq!(p2.samples[0].unwrap().value, None);
    assert!(!p.history[0].unwrap().same_epoch(p2.history[0].unwrap()));
    s.update(Err(Failure::Unavailable), sec(7));
    assert_eq!(
        Projection::new(
            &settings,
            Some(&View {
                state: Arc::new(s.clone()),
                now: sec(7)
            })
        )
        .samples[0]
            .unwrap()
            .status,
        Status::Unavailable
    );
    s.update(Ok(frame(3)), sec(8));
    assert_eq!(
        s.resolve(Role::CpuTemp, id(3), sec(8)).sample.status,
        Status::Unverified
    );
    s.update(Ok(frame(4)), sec(9));
    assert_eq!(
        s.resolve(Role::CpuTemp, id(3), sec(9)).sample.value,
        Some(42.)
    );
    assert_ne!(s.generation, view.state.generation);
    for (w, h) in [(1920, 462), (480, 1280), (1280, 480)] {
        let mut fb = trofeo_lcd::Framebuffer::new(trofeo_lcd::Resolution::new(w, h));
        p.draw(&mut fb);
        assert!(fb.as_bytes().iter().any(|&v| v != 0));
        p2.draw(&mut fb);
        assert!(display_text(&p2.samples[0].unwrap()).contains("Stale"));
    }
}

#[test]
fn editor_selection_preserves_unknowns_and_rejects_incompatible_stale_and_missing() {
    use trofeo_lcd::{
        conf_edit::ConfDoc,
        sensor_display::{compatible, select},
    };
    let doc = ConfDoc::parse("# keep\nunknown_option=untouched\nsensor_source=hwinfo\n");
    let mut s = state();
    s.update(Ok(frame(1)), sec(0));
    let view = View {
        state: Arc::new(s.clone()),
        now: sec(0),
    };
    let chosen = select(&doc, &view, Role::CpuTemp, id(3)).unwrap();
    assert_eq!(chosen.get("sensor_cpu_temp"), Some(id(3).to_string()));
    assert_eq!(chosen.get("unknown_option").as_deref(), Some("untouched"));
    assert!(chosen.to_text().contains("# keep"));
    assert!(!chosen.to_text().contains("Synthetic"));
    assert_eq!(
        validate_text(&chosen.to_text()).unwrap().roles[0],
        trofeo_lcd::sensor_policy::Binding::Hwinfo(id(3))
    );
    assert!(select(&doc, &view, Role::CpuFan, id(3)).is_err());
    assert!(select(&doc, &view, Role::CpuTemp, id(99)).is_err());
    assert!(select(
        &doc,
        &View {
            state: view.state,
            now: sec(5)
        },
        Role::CpuTemp,
        id(3)
    )
    .is_err());
    let mut e = entry(3);
    e.unit = "F".into();
    assert!(!compatible(&e, Role::CpuTemp));
    assert!(doc.get("sensor_cpu_temp").is_none());
}

#[test]
fn display_invalid_missing_unavailable_and_identity_changes_never_keep_numbers() {
    use trofeo_lcd::sensor_display::{display_text, Projection};
    let settings =
        validate_text("sensor_source=hwinfo\nsensor_cpu_temp=hwinfo:0x1/0x2/0x3").unwrap();
    let mut s = state();
    s.update(Ok(frame(1)), sec(0));
    let mut f = frame(2);
    f.entries[0].unit = "F".into();
    s.update(Ok(f), sec(1));
    for (time, expected) in [(1, Status::Invalid), (2, Status::NotFound)] {
        if time == 2 {
            let mut f = frame(3);
            f.entries[0].identity = id(4);
            s.update(Ok(f), sec(2));
        }
        let p = Projection::new(
            &settings,
            Some(&View {
                state: Arc::new(s.clone()),
                now: sec(time),
            }),
        );
        let sample = p.samples[0].unwrap();
        assert_eq!(sample.status, expected);
        assert_eq!(sample.value, None);
        assert!(display_text(&sample).contains("--"));
    }
    assert_eq!(
        Projection::new(&settings, None).samples[0].unwrap().status,
        Status::Unavailable
    );
    assert!(Projection::new(&validate_text("").unwrap(), None)
        .samples
        .iter()
        .all(Option::is_none));
}

#[test]
fn all_six_roles_share_runtime_and_editor_contract_zero_is_a_value() {
    use trofeo_lcd::{
        conf_edit::ConfDoc,
        sensor_display::{footer_height, select, Projection},
        sensor_policy::{Unit, ROLES},
    };
    let mut f = frame(1);
    f.entries.clear();
    for (i, role) in ROLES.iter().enumerate() {
        let mut e = entry(i as u32);
        if role.unit() == Unit::Rpm {
            e.reading_type = ReadingType::Fan;
            e.unit = "RPM".into();
            e.current = 0.;
        }
        f.entries.push(e);
    }
    let mut s = state();
    s.update(Ok(f.clone()), sec(0));
    f.poll_time_raw = 2;
    s.update(Ok(f), sec(1));
    let view = View {
        state: Arc::new(s),
        now: sec(1),
    };
    let mut doc = ConfDoc::parse("sensor_source=hwinfo\n");
    for (i, role) in ROLES.iter().enumerate() {
        doc = select(&doc, &view, *role, id(i as u32)).unwrap();
    }
    let settings = validate_text(&doc.to_text()).unwrap();
    let p = Projection::new(&settings, Some(&view));
    for (i, role) in ROLES.iter().enumerate() {
        let sample = p.samples[i].unwrap();
        assert_eq!(sample.role, *role);
        assert_eq!(sample.status, Status::Ok);
        assert_eq!(
            sample.value,
            Some(if role.unit() == Unit::Rpm { 0. } else { 42. })
        );
        assert_eq!(p.diagnostics[i], Some(Diagnostic::ReadingUnverified));
    }
    assert_eq!(footer_height(&settings, 1920), 36);
    assert_eq!(footer_height(&settings, 480), 84);
    assert_eq!(footer_height(&validate_text("").unwrap(), 1920), 0);
}

#[test]
fn editor_search_respects_type_unit_ids_case_and_renames() {
    use trofeo_lcd::sensor_display::matches_search;
    let mut e = entry(3);
    assert!(matches_search(&e, Role::CpuTemp, "SYNTHETIC"));
    assert!(matches_search(&e, Role::CpuTemp, "0x00000003"));
    assert!(!matches_search(&e, Role::CpuFan, ""));
    e.label = "Renamed reading".into();
    assert!(matches_search(&e, Role::CpuTemp, "renamed"));
    assert!(!matches_search(&e, Role::CpuTemp, "Temperature"));
    e.unit = "F".into();
    assert!(!matches_search(&e, Role::CpuTemp, "renamed"));
}
