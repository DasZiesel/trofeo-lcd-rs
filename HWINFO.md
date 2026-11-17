# HWiNFO sensor setup

This feature reads local HWiNFO Shared Memory on Windows 10 or later (x64). Install
[HWiNFO](https://www.hwinfo.com/download/) separately and run its sensor monitoring.
This project does not include HWiNFO executables, SDK files, proprietary headers,
license keys or a HWiNFO installer. HWiNFO is an independent third-party product.
Linux, 32-bit Windows and ARM64 builds are not validated for this feature;
unsupported platforms reject the provider. Existing non-HWiNFO sources remain available.

## Enable and assign sensors

1. Open HWiNFO's settings and enable **Shared Memory Support**, then keep sensor
   monitoring running. Setting placement can vary between HWiNFO versions.
2. Copy `trofeo.conf.example` to your own `trofeo.conf` if you do not already have
   one. Preserve your existing display settings. Set `sensor_source = hwinfo`.
3. Open the configuration editor. Under the LHM/sensor section, expand
   **HWiNFO sensor assignments (restart required)** and click
   **Read HWiNFO inventory**. Inventory reading starts only on request.
4. Wait for publisher progress, search the available readings and select the
   correct sensor for each role. Compare labels and values with HWiNFO yourself:
   a connector name alone does not prove which fan or pump is attached.
5. Save, stop inventory reading and restart the LCD application to apply source
   or binding changes. Restart the editor as well after timing changes.

For editing without the normal preview/device controls, the editor supports
`trofeo_config.exe .\trofeo.conf --sensor-editor-only` with an existing
configuration file. This still opens a graphical window; explicitly starting
inventory reads the running HWiNFO publisher. Normal LCD startup can use USB,
graphics and other configured integrations; HWiNFO selection is not a simulation mode.

| Configuration role | Required reading |
| --- | --- |
| `sensor_cpu_temp`, `sensor_gpu_temp`, `sensor_mb_temp` | Temperature in `C` or `°C` |
| `sensor_cpu_fan`, `sensor_case_fan`, `sensor_pump` | Fan speed in `RPM` |

Bindings use `hwinfo:0xSSSSSSSS/0xIIIIIIII/0xRRRRRRRR`: sensor ID, sensor
instance and reading ID (eight hexadecimal digits each). Names and list positions
are not persistent identities. The following IDs are synthetic and will normally
resolve to `NotFound`; use the editor to select your own readings:

```ini
sensor_source = hwinfo
sensor_cpu_temp = hwinfo:0x00000001/0x00000000/0x00000001
sensor_cpu_fan = hwinfo:0x00000001/0x00000000/0x00000002
sensor_case_fan = hwinfo:0x00000001/0x00000000/0x00000003
sensor_pump = hwinfo:0x00000001/0x00000000/0x00000004
sensor_mb_temp = hwinfo:0x00000001/0x00000000/0x00000005
sensor_gpu_temp = native:gpu_temp
hwinfo_poll_interval_ms = 2000
hwinfo_stale_timeout_ms = 10000
```

Explicit role bindings override the default source. `auto` retains legacy
native/LHM behavior and does not automatically discover HWiNFO. With `hwinfo`,
unassigned CPU/fan/board roles stay missing; the GPU temperature keeps its native
source unless explicitly assigned to HWiNFO. There is no automatic fallback for
missing, invalid or stale HWiNFO bindings. CPU power, usage and clock are not
new HWiNFO roles. The reserved `bridge` provider is unavailable in this build.
Recheck assignments after hardware or publisher changes; never share your private
configuration, inventory output or screenshots containing sensor identities.

## Timing and availability

`hwinfo_poll_interval_ms` defaults to **2000 ms**, allowed range **100–10000 ms**.
`hwinfo_stale_timeout_ms` defaults to **10000 ms**, must be at least twice the
poll interval and at most **60000 ms**. Values must be unsigned decimal integers.
These settings control this reader, not HWiNFO's own sensor polling interval.
Choose a stale timeout that accommodates the publisher's update interval.

Command-line overrides `--hwinfo-poll-interval-ms` and
`--hwinfo-stale-timeout-ms` take priority over file settings, which override
defaults. Source, role and timing changes require an application restart.
The editor displays its active timing; saving, reloading the file or stopping
and starting inventory does not replace those active options until editor restart.
Invalid configurations are rejected rather than silently corrected.

| State | Meaning and action |
| --- | --- |
| `Unverified` | Initial/recovered snapshot has not yet demonstrated publisher progress; wait for a new poll. |
| `NotFound` | No matching identity or no assignment; select a valid reading. |
| `Invalid` | Duplicate identity, wrong type/unit or invalid numeric data; check the assignment and publisher. |
| `Stale` | Publisher progress exceeded the timeout; check that HWiNFO and Shared Memory remain active. |
| `Unavailable` | Mapping/mutex access, decoding or worker failed; check HWiNFO settings and access conditions. |
| `Closed` | Reader has stopped. |
| `Ok` with `ReadingUnverified` diagnostic | Eligible for display based on global publisher progress; individual measurement freshness is not independently proven. |

Unavailable states suppress the affected number instead of substituting zero or
another source. The display status footer distinguishes quality states. A valid
zero remains a possible reading. Access failures are retried at the configured
bounded interval; recovery must establish publisher progress again. This does not
enable or restart HWiNFO. Do not change security settings merely to hide an error.

## Free and Pro licensing

HWiNFO's [license comparison](https://www.hwinfo.com/licenses/) currently gives
the non-Pro 64-bit version a **12-hour Shared Memory limit**. Shared Memory
deactivates when that limit is reached. For continuous use, the regular **Pro**
option provides unlimited Shared Memory; select the license appropriate to your
use (Personal is non-commercial; Engineer/Corporate cover commercial use).
Consult the current [HWiNFO EULA](https://www.hwinfo.com/files/license.pdf).
This application never automatically re-enables Shared Memory or bypasses the
limit. Loss of Shared Memory is handled as unavailable/stale data.

## Known limits

The reader supports the validated SM2 layout/version-revision pairs (1,0), (1,1) and (2,1),
with strict size, identity and bounds checks; unknown layouts are rejected.
It reads current values, not min/max/average as a replacement for live data.
Old non-ASCII ANSI labels may appear as escaped bytes; labels do not define identity.
The interface has no independent per-reading timestamp or error status. Global
publisher progress cannot prove physical accuracy or identify every finite cached
value. This is a display integration, not a safety monitor or fan controller.
DeepCool forwarding is disabled for incompatible HWiNFO role configurations.
The existing native/GPU paths remain separate, and native module code remains in
the application binary even when HWiNFO is selected.
