use std::{path::PathBuf, sync::Arc, time::{Duration, Instant}};
use trofeo_lcd::{alerts, config::ConfigFile, conf_edit::ConfDoc, hwinfo_provider::{Entry, Frame, Options, State, View}, hwinfo_sm2::ReadingType, profiles, schema, sensor_display::Projection, sensor_policy::{self, HwinfoSelector, Status}, share, themes};

fn directory() -> PathBuf {
    let dir = std::env::current_exe().unwrap().parent().unwrap().join(format!(
        "synthetic-upstream-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn profiles_themes_and_shared_designs_cannot_replace_sensor_bindings() {
    let dir = directory();
    let fields = schema::fields();
    for (key, value) in [
        ("sensor_source", "hwinfo"), ("sensor_cpu_temp", "hwinfo:0x1/0x2/0x3"),
        ("sensor_cpu_fan", "hwinfo:0x1/0x2/0x4"), ("sensor_case_fan", "hwinfo:0x1/0x2/0x5"),
        ("sensor_pump", "hwinfo:0x1/0x2/0x6"), ("sensor_mb_temp", "hwinfo:0x1/0x2/0x7"),
        ("sensor_gpu_temp", "hwinfo:0x1/0x2/0x8"),
        ("hwinfo_poll_interval_ms", "3000"), ("hwinfo_stale_timeout_ms", "12000"),
    ] {
        let text = format!("{key} = {value}\n");
        assert!(profiles::save_text(&dir, "blocked", &text).unwrap_err().contains("restart"));
        let field = fields.iter().find(|f| f.key == key).unwrap();
        assert!(!share::shareable(field));
        assert!(share::parse_import(&text, &fields).apply.is_empty());
        assert!(share::settings_text_in(&ConfDoc::parse(&text), &fields, None).is_empty());
        std::fs::write(dir.join("blocked.theme"), &text).unwrap();
        assert!(themes::load_user(&dir, "blocked").is_err());
    }
}

#[test]
fn profiles_and_themes_preserve_sensor_settings_and_cli_priority() {
    let dir = directory();
    profiles::save_text(&dir, "visual", "theme = mono\nbrightness = 40\n").unwrap();
    let mut file = ConfigFile::parse_raw_for_tests("profile = visual\nsensor_source = hwinfo\nsensor_cpu_temp = hwinfo:0x1/0x2/0x3\nhwinfo_poll_interval_ms = 3000\nhwinfo_stale_timeout_ms = 12000\n");
    file.apply_profile_in(Some(&dir)).unwrap();
    file.apply_theme_in(None).unwrap();
    let (file, settings) = sensor_policy::configured(file, &[
        ("brightness".into(), "65".into()),
        ("hwinfo_poll_interval_ms".into(), "2500".into()),
    ]).unwrap();
    assert_eq!(file.get("brightness"), Some("65"));
    assert_eq!(file.get("theme"), Some("mono"));
    assert_eq!(file.get("sensor_cpu_temp"), Some("hwinfo:0x1/0x2/0x3"));
    assert_eq!(settings.hwinfo_options.poll_interval, Duration::from_millis(2500));
    assert_eq!(settings.hwinfo_options.stale_after, Duration::from_millis(12000));
    let samples = settings.resolve_with_hwinfo([Some(99.); 6], [Some(98.); 6], [Some(97.); 6], None);
    assert_eq!(samples[0].value, None, "no fallback from an explicit missing binding");
}

#[test]
fn threshold_alerts_clear_when_projected_hwinfo_data_goes_stale() {
    let id = HwinfoSelector { sensor_id: 1, sensor_instance: 2, reading_id: 3 };
    let settings = sensor_policy::validate_text("sensor_source=hwinfo\nsensor_cpu_temp=hwinfo:0x1/0x2/0x3\n").unwrap();
    let mut state = State::new(Options::default()).unwrap();
    for poll in [1, 2] {
        state.update(Ok(Frame { poll_time_raw: poll, polling_period_raw: Some(2000), entries: vec![Entry {
            identity: id, sensor_name: "Synthetic".into(), label: "Temperature".into(),
            reading_type: ReadingType::Temperature, unit: "C".into(), current: 95., numeric_status: Status::Unverified,
        }] }), Duration::from_secs(poll as u64));
    }
    let state = Arc::new(state);
    let mut alert = alerts::State::new();
    let rules = alerts::Rules { cpu_temp: 90, delay_secs: 0, ..Default::default() };
    let now = Instant::now();
    for (secs, expected) in [(2, true), (30, false)] {
        let projection = Projection::new(&settings, Some(&View { state: state.clone(), now: Duration::from_secs(secs) }));
        let reading = alerts::Reading { cpu_temp: projection.samples[0].unwrap().value.map(|v| v as f32), ..Default::default() };
        assert_eq!(alert.update(&rules, now + Duration::from_secs(secs), &reading).is_some(), expected);
    }
}
