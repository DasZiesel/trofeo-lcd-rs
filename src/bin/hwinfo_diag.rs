//! Explicit, bounded SM2 diagnostics. No application or hardware-provider startup.
use std::io::{self, Write};

const HELP: &str = "hwinfo_diag (--live | --synthetic TOKEN) [--snapshots 1|2|3] [--inventory --acknowledge-inventory]\nDefault: 3 attempts, 100 ms mutex wait each, 250 ms between successful attempts.\nStops on the first error. No file output. Live access requires prior user approval.\nInventory needs separate approval and both inventory flags; it may disclose private data.\n--provider-check --acknowledge-provider-read: selects the lowest compatible temperature ID as a diagnostic slot only, checks provider/editor/display in memory, prints status only, uses 3000 ms between at most 3 copies. No inventory output allowed.";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    synthetic: Option<String>,
    snapshots: u8,
    inventory: bool,
    provider_check: bool,
}
fn parse(args: &[String]) -> Result<Option<Options>, &'static str> {
    if args == ["--help"] {
        return Ok(None);
    }
    let mut live = false;
    let mut synthetic = None;
    let mut snapshots = None;
    let mut inventory = false;
    let mut acknowledge = false;
    let mut provider_check = false;
    let mut provider_ack = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--live" if !live => live = true,
            "--synthetic" if synthetic.is_none() => {
                let token = iter.next().ok_or("missing synthetic token")?;
                if token.is_empty()
                    || token.len() > 100
                    || !token
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err("invalid synthetic token");
                }
                synthetic = Some(token.clone());
            }
            "--snapshots" if snapshots.is_none() => {
                snapshots = Some(match iter.next().map(String::as_str) {
                    Some("1") => 1,
                    Some("2") => 2,
                    Some("3") => 3,
                    _ => return Err("snapshots must be 1, 2 or 3"),
                });
            }
            "--provider-check" if !provider_check => provider_check = true,
            "--acknowledge-provider-read" if !provider_ack => provider_ack = true,
            "--inventory" if !inventory => inventory = true,
            "--acknowledge-inventory" if !acknowledge => acknowledge = true,
            _ => return Err("unknown or repeated argument"),
        }
    }
    if live == synthetic.is_some() {
        return Err("select exactly one source explicitly");
    }
    if inventory != acknowledge {
        return Err("inventory requires both flags and separate approval");
    }
    if provider_check != provider_ack || (provider_check && inventory) {
        return Err("provider check needs acknowledgement and forbids inventory output");
    }
    Ok(Some(Options {
        synthetic,
        snapshots: snapshots.unwrap_or(3),
        inventory,
        provider_check,
    }))
}

#[cfg(all(windows, target_pointer_width = "64"))]
fn run(options: Options, out: &mut impl Write) -> io::Result<u8> {
    use std::{sync::atomic::AtomicBool, thread, time::Duration};
    use trofeo_lcd::hwinfo_transport::Transport;
    let transport = match &options.synthetic {
        Some(token) => Transport::synthetic(token, 100),
        None => Transport::hwinfo(100),
    };
    let transport = match transport {
        Ok(value) => value,
        Err(error) => {
            writeln!(out, "transport=error error={error:?} decoder=not_run")?;
            return Ok(1);
        }
    };
    let cancel = AtomicBool::new(false);
    let mut probe = trofeo_lcd::hwinfo_probe::Probe::new();
    let mut display_ready = false;
    let origin = std::time::Instant::now();
    for attempt in 1..=options.snapshots {
        let copy = match transport.capture(&cancel) {
            Ok(value) => value,
            Err(error) => {
                writeln!(
                    out,
                    "attempt={attempt} transport=error error={error:?} decoder=not_run"
                )?;
                return Ok(1);
            }
        };
        let snapshot = match copy.decode() {
            Ok(value) => value,
            Err(error) => {
                writeln!(
                    out,
                    "attempt={attempt} transport=copied bytes={} decoder=error error={error:?}",
                    copy.bytes().len()
                )?;
                return Ok(1);
            }
        };
        writeln!(out, "attempt={attempt} transport=copied decoder=decoded status=Unverified bytes={} version={} revision={} sensors={} readings={} poll_time_raw={} polling_period_raw={:?}",
            copy.bytes().len(), snapshot.header.version, snapshot.header.revision,
            snapshot.sensors.len(), snapshot.readings.len(), snapshot.header.poll_time, snapshot.header.polling_period)?;
        if options.provider_check {
            match probe.observe(copy.bytes(), origin.elapsed()) {
                Ok(report) => {
                    display_ready |= report.display == trofeo_lcd::sensor_policy::Status::Ok;
                    writeln!(out,"provider publisher={:?} display={:?} editor_roundtrip={} individual_freshness=Unverified",report.publisher,report.display,report.roundtrip)?;
                }
                Err(error) => {
                    writeln!(out, "provider=error {error}")?;
                    return Ok(1);
                }
            }
        }
        if options.inventory {
            for reading in &snapshot.readings {
                // Debug escaping prevents text fields from injecting console control sequences.
                // ANSI remains raw bytes when no UTF-8 field is documented by this layout.
                let unit = match reading.unit_utf8 {
                    Some(text) => format!("utf8:{text:?}"),
                    None => format!("ansi_bytes:{:?}", reading.unit.bytes()),
                };
                writeln!(out, "inventory sensor_id=0x{:08x} instance=0x{:08x} reading_id=0x{:08x} type={:?} unit={} current={:?} status={:?}",
                    reading.identity.sensor_id, reading.identity.sensor_instance, reading.identity.reading_id,
                    reading.reading_type, unit, reading.values.current, reading.status())?;
            }
        }
        out.flush()?;
        // No mapping, mutex or decoded snapshot is retained across the delay.
        drop(snapshot);
        drop(copy);
        if attempt < options.snapshots {
            thread::sleep(Duration::from_millis(if options.provider_check {
                3000
            } else {
                250
            }));
        }
    }
    if options.provider_check && !display_ready {
        writeln!(
            out,
            "provider=incomplete reason=no_display_eligible_progress"
        )?;
        return Ok(2);
    }
    Ok(0)
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
fn run(_: Options, out: &mut impl Write) -> io::Result<u8> {
    writeln!(out, "error=unsupported_platform requires=Windows_x64")?;
    Ok(2)
}
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match parse(&args) {
        Ok(Some(options)) => run(options, &mut io::stdout().lock()),
        Ok(None) => writeln!(io::stdout().lock(), "{HELP}").map(|_| 0),
        Err(error) => writeln!(io::stderr().lock(), "argument_error={error}\n{HELP}").map(|_| 2),
    };
    // Output failure stops subsequent acquisition; never save a fallback dump.
    std::process::ExitCode::from(result.unwrap_or(3))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn requires_explicit_source_and_separate_inventory_ack() {
        for input in [
            vec![],
            vec!["--live", "--synthetic", "test"],
            vec!["--live", "--inventory"],
            vec!["--live", "--acknowledge-inventory"],
            vec!["--synthetic", "bad\\token"],
            vec!["--live", "--live"],
        ] {
            assert!(parse(&args(&input)).is_err());
        }
        assert_eq!(
            parse(&args(&["--live"])).unwrap(),
            Some(Options {
                synthetic: None,
                snapshots: 3,
                inventory: false,
                provider_check: false,
            })
        );
        assert!(
            parse(&args(&["--live", "--inventory", "--acknowledge-inventory"]))
                .unwrap()
                .unwrap()
                .inventory
        );
        assert!(parse(&args(&["--help"])).unwrap().is_none());
    }
    #[test]
    fn limits_are_not_overridable() {
        assert!(parse(&args(&["--live", "--provider-check"])).is_err());
        assert!(parse(&args(&["--live", "--acknowledge-provider-read"])).is_err());
        assert!(parse(&args(&[
            "--live",
            "--provider-check",
            "--acknowledge-provider-read",
            "--inventory",
            "--acknowledge-inventory"
        ]))
        .is_err());
        assert!(
            parse(&args(&[
                "--synthetic",
                "test",
                "--provider-check",
                "--acknowledge-provider-read"
            ]))
            .unwrap()
            .unwrap()
            .provider_check
        );
        for value in ["0", "4", "-1", "4294967295", "01", "x"] {
            assert!(parse(&args(&["--live", "--snapshots", value])).is_err());
        }
        for value in ["1", "2", "3"] {
            assert!(parse(&args(&["--synthetic", "test", "--snapshots", value])).is_ok());
        }
        assert!(parse(&args(&["--live", "--timeout", "1000"])).is_err());
        assert!(parse(&args(&["--live", "--output", "dump"])).is_err());
    }
}
