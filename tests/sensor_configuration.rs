//! Only pure configuration and in-memory samples; never start the application.
use trofeo_lcd::{conf_edit::ConfDoc, config::ConfigFile, sensor_policy::*};
fn settings(text: &str) -> Settings {
    validate_text(text).unwrap()
}

#[test]
fn selector_roundtrip_and_canonical_form() {
    for raw in [
        "hwinfo:0x1/0X0/0xABCDEF",
        "hwinfo:0xffffffff/0xFFFFFFFF/0x0",
    ] {
        let s: HwinfoSelector = raw.parse().unwrap();
        assert_eq!(s.to_string().parse::<HwinfoSelector>().unwrap(), s);
        assert_eq!(s.to_string().len(), 39);
    }
    assert_eq!(
        "hwinfo:0x1/0x0/0x2"
            .parse::<HwinfoSelector>()
            .unwrap()
            .to_string(),
        "hwinfo:0x00000001/0x00000000/0x00000002"
    );
}
#[test]
fn selector_rejects_malformed_and_overflow() {
    for s in [
        "hwinfo:",
        "hwinfo:1/0/2",
        "hwinfo:0x/0x0/0x1",
        "hwinfo:0x100000000/0x0/0x1",
        "hwinfo:0x000000000/0x0/0x1",
        "hwinfo:0x-1/0x0/0x2",
        "hwinfo:0x+1/0x0/0x2",
        "hwinfo:0xG/0x0/0x2",
        "hwinfo:0x1/0x0",
        "hwinfo:0x1/0x0/0x2/0x3",
        "hwinfo:0x1/ 0x0/0x2",
        "hwinfo:0x１/0x0/0x2",
        "native:0x1/0x0/0x2",
    ] {
        assert!(s.parse::<HwinfoSelector>().is_err(), "{s}");
    }
}
#[test]
fn defaults_and_legacy_preserved() {
    let a = settings("");
    assert_eq!(a.source, Source::Auto);
    assert!(a.legacy() && a.native_cpu() && a.lhm());
    assert_eq!(settings("sensor_source=existing"), a);
    let f =
        ConfigFile::parse("lhm=true\nlhm=false\nfuture_key=keep\nlhm_cpu_fan=\"Fan #2\"").unwrap();
    assert_eq!(f.get("lhm"), Some("false"));
    assert_eq!(f.get("lhm_cpu_fan"), Some("Fan #2"));
    assert_eq!(Settings::parse(&f).unwrap(), a);
}
#[test]
fn file_cli_priority_uses_shared_path() {
    let f = ConfigFile::parse("sensor_source=hwinfo\nsensor_cpu_temp=hwinfo:0x1/0x0/0x2").unwrap();
    let overrides = vec![
        (cli_key("--sensor-source").unwrap(), "native".into()),
        (
            cli_key("--sensor-cpu-temp").unwrap(),
            "native:cpu_temp".into(),
        ),
    ];
    let (_, c) = configured(f, &overrides).unwrap();
    assert_eq!(c.source, Source::Native);
    assert!(c.ensure_available().is_ok());
    assert_eq!(c.roles[0], Binding::Native(Role::CpuTemp));
    assert!(cli_key("--sensor-surprise").is_none());
    assert!(cli_key("--bridge-mode").is_some());
}
#[test]
fn duplicate_and_unknown_new_keys_rejected() {
    for text in [
        "sensor_other=1",
        "sensor_source=auto\nsensor_source=hwinfo",
        "bridge_mode=real\nbridge_mode=mock",
        "sensor_cpu_temp=native:cpu_temp\nSENSOR_CPU_TEMP=native:gpu_temp",
    ] {
        assert!(ConfigFile::parse(text).is_err(), "{text}");
    }
}
#[test]
fn configuration_errors_are_not_provider_errors() {
    for text in [
        "sensor_source=other",
        "sensor_cpu_temp=other:x",
        "sensor_cpu_temp=native:nope",
        "sensor_pump=native:cpu_temp",
        "sensor_cpu_temp=lhm:gpu_temp",
        "sensor_source=bridge",
        "bridge_mode=bad",
        "bridge_service=Other",
        "sensor_pump=lhm:pump\nlhm=false",
    ] {
        assert!(validate_text(text).is_err(), "{text}");
    }
    assert!(validate_text("sensor_source=hwinfo").is_ok());
}
#[test]
fn unavailable_provider_blocks_startup_even_with_native_overrides() {
    for text in [
        "sensor_source=bridge\nbridge_mode=mock",
        "sensor_source=auto\nsensor_pump=bridge:aio_pump\nbridge_mode=real",
    ] {
        let c = settings(text);
        let initialized = std::cell::Cell::new(false);
        let result = c.ensure_available().map(|_| initialized.set(true));
        assert!(result.unwrap_err().contains("unavailable"));
        assert!(!initialized.get());
    }
    assert!(!settings("sensor_source=hwinfo").native_cpu());
}
#[test]
fn explicit_bindings_never_fallback() {
    let c = settings("sensor_cpu_temp=hwinfo:0x1/0x0/0x2\nsensor_pump=lhm:case_fan");
    let r = c.resolve([Some(55.); 6], [None; 6], [Some(66.); 6]);
    assert_eq!(r[0].value, None);
    assert_eq!(r[0].status, Status::Unavailable);
    assert_eq!(r[3].value, None);
    assert_eq!(r[3].status, Status::NotFound);
    assert_eq!(r[5].value, Some(66.));
}
#[test]
fn native_and_lhm_strategies_and_aliases() {
    let native = [Some(50.), None, None, None, None, Some(40.)];
    let lhm = [
        Some(60.),
        Some(1000.),
        Some(900.),
        Some(0.),
        Some(30.),
        None,
    ];
    let r = settings("sensor_source=native").resolve(native, lhm, [None; 6]);
    assert_eq!(r[0].value, Some(50.));
    assert_eq!(r[1].value, None);
    let r = settings("sensor_source=lhm\nsensor_cpu_fan=lhm:pump").resolve(native, lhm, [None; 6]);
    assert_eq!(r[0].value, Some(60.));
    assert_eq!(r[1].value, Some(0.));
    assert_eq!(r[1].status, Status::Ok);
    assert_eq!(r[5].value, Some(40.));
}
#[test]
fn bridge_alias_contract_preserved_without_client() {
    let c = settings("sensor_source=bridge\nbridge_mode=mock");
    assert_eq!(c.roles[1], Binding::Bridge(BridgeAlias::RadiatorFan));
    assert_eq!(c.roles[2], Binding::Bridge(BridgeAlias::VrmFan));
    let explicit = settings(
        "bridge_mode=real\nsensor_cpu_fan=bridge:vrm_fan\nsensor_case_fan=bridge:radiator_fan",
    );
    assert_eq!(explicit.roles[1], Binding::Bridge(BridgeAlias::VrmFan));
    assert!(explicit.ensure_available().is_err());
}
#[test]
fn sample_validation_distinguishes_zero_missing_invalid() {
    let c = settings("sensor_source=lhm");
    for value in [f64::NAN, f64::INFINITY, -1., u32::MAX as f64 + 1.] {
        let r = c.resolve([None; 6], [Some(value); 6], [None; 6]);
        assert_eq!(r[1].status, Status::Invalid);
        assert_eq!(r[1].value, None);
    }
}
#[test]
fn reload_retains_old_settings_and_cli_overrides() {
    let old = settings("sensor_source=native");
    assert!(old.validate_reload(&settings("sensor_source=lhm")).is_err());
    assert!(old
        .validate_reload(&settings("sensor_source=hwinfo"))
        .is_err());
    assert!(validate_text("sensor_source=bad").is_err());
    let (_, next) = configured(
        ConfigFile::parse("sensor_source=lhm\nfuture_key=keep").unwrap(),
        &[("sensor_source".into(), "native".into())],
    )
    .unwrap();
    assert!(old.validate_reload(&next).is_ok());
    assert_eq!(old, settings("sensor_source=native"));
    let alias = settings("sensor_cpu_temp=native:gpu_temp");
    assert!(settings("")
        .validate_reload(&alias)
        .unwrap_err()
        .contains("restart"));
}
#[test]
fn editor_roundtrip_and_shared_validation() {
    let original = "# keep this\nfuture_key = preserve\nlhm_cpu_fan = \"Fan #2\"\n";
    let mut doc = ConfDoc::parse(original);
    doc.set("sensor_source", "hwinfo");
    doc.set(
        "sensor_cpu_temp",
        &"hwinfo:0x1/0x0/0x2"
            .parse::<HwinfoSelector>()
            .unwrap()
            .to_string(),
    );
    let text = doc.to_text();
    assert!(
        text.contains("# keep this")
            && text.contains("future_key = preserve")
            && text.contains("Fan #2")
    );
    let c = validate_text(&text).unwrap();
    assert_eq!(
        c.ensure_available().is_ok(),
        cfg!(all(windows, target_pointer_width = "64"))
    );
    doc.set("sensor_cpu_temp", "hwinfo:broken");
    assert!(validate_text(&doc.to_text()).is_err());
    let fields = trofeo_lcd::schema::fields();
    for key in KEYS
        .into_iter()
        .chain(["sensor_source", "bridge_mode", "bridge_service"])
    {
        assert!(
            fields
                .iter()
                .any(|f| f.key == key && f.restart && !f.help_en.is_empty()),
            "{key}"
        );
    }
}

#[test]
fn all_roles_allow_persisted_id_triplets_without_discovery() {
    let text = KEYS
        .map(|key| format!("{key}=hwinfo:0x1/0x0/0x2"))
        .join("\n");
    let c = settings(&text);
    let id: HwinfoSelector = "hwinfo:0x1/0x0/0x2".parse().unwrap();
    assert!(c.roles.iter().all(|b| *b == Binding::Hwinfo(id)));
    assert!(!c.native_cpu());
    assert_eq!(
        c.ensure_available().is_ok(),
        cfg!(all(windows, target_pointer_width = "64"))
    );
}


#[test]
fn hwinfo_timing_defaults_bounds_and_strict_numbers() {
    use std::time::Duration;
    let d=settings("").hwinfo_options;
    assert_eq!(d.poll_interval,Duration::from_millis(2000));
    assert_eq!(d.stale_after,Duration::from_millis(10000));
    for (poll,stale) in [(100,200),(2000,4000),(10000,20000),(10000,60000)] {
        assert!(validate_text(&format!("hwinfo_poll_interval_ms={poll}\nhwinfo_stale_timeout_ms={stale}")).is_ok());
    }
    for text in ["hwinfo_poll_interval_ms=99", "hwinfo_poll_interval_ms=10001", "hwinfo_poll_interval_ms=6000", "hwinfo_stale_timeout_ms=3999", "hwinfo_stale_timeout_ms=60001", "hwinfo_stale_timeout_ms=0", "hwinfo_poll_interval_ms=2000\nhwinfo_poll_interval_ms=2000"] {
        assert!(validate_text(text).is_err(),"{text}");
    }
    for key in ["hwinfo_poll_interval_ms","hwinfo_stale_timeout_ms"] {
        for bad in ["", "-1", "+2000", "2.0", "1e3", "NaN", "inf", "0x800", "18446744073709551616"] {
            assert!(validate_text(&format!("{key}={bad}")).is_err(),"{key}={bad}");
        }
    }
}
#[test]
fn timing_schema_roundtrip_cli_precedence_and_restart() {
    let mut doc=ConfDoc::parse("future_setting=keep\nhwinfo_poll_interval_ms=2000\nhwinfo_stale_timeout_ms=10000\n");
    doc.set("hwinfo_poll_interval_ms","3000");
    doc.set("hwinfo_stale_timeout_ms","12000");
    let file=ConfigFile::parse(&doc.to_text()).unwrap();
    let before=Settings::parse(&file).unwrap();
    assert_eq!(file.get("future_setting"),Some("keep"));
    let (_,after)=configured(file,&[(cli_key("--hwinfo-poll-interval-ms").unwrap(),"4000".into())]).unwrap();
    assert_eq!(after.hwinfo_options.poll_interval.as_millis(),4000);
    assert!(before.validate_reload(&after).unwrap_err().contains("restart"));
    assert!(before.validate_reload(&before).is_ok());
    let only_stale=settings("hwinfo_poll_interval_ms=3000\nhwinfo_stale_timeout_ms=13000");
    assert!(before.validate_reload(&only_stale).is_err());
    for f in trofeo_lcd::schema::fields().into_iter().filter(|f| f.key.starts_with("hwinfo_")) {
        assert!(f.restart);
        assert_eq!(settings(&format!("{}={}",f.key,f.default)).hwinfo_options,settings("").hwinfo_options);
    }
    for source in ["auto","native","lhm","bridge"] {
        let base=settings(&format!("sensor_source={source}\nbridge_mode=mock"));
        let changed=settings(&format!("sensor_source={source}\nbridge_mode=mock\nhwinfo_poll_interval_ms=1000"));
        assert_eq!(base.roles,changed.roles);assert!(!changed.hwinfo());
    }
}
