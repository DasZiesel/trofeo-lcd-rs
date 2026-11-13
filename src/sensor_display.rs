//! Display projection of existing policy samples; no acquisition or source selection.
use crate::{
    conf_edit::ConfDoc,
    hwinfo_provider::{Entry, View},
    sensor_policy::{
        self, Binding, HwinfoSelector, Role, Sample, Settings, Source, Status, Unit, KEYS, ROLES,
    },
    Framebuffer,
};

/// Provenance for history segmentation, not a per-reading freshness claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryKey {
    pub identity: HwinfoSelector,
    pub generation: u64,
    pub publisher_poll: Option<i64>,
    pub usable: bool,
}
impl HistoryKey {
    pub fn same_epoch(self, other: Self) -> bool {
        self.usable
            && other.usable
            && self.identity == other.identity
            && self.generation == other.generation
    }
}
pub struct Projection {
    pub samples: [Option<Sample>; 6],
    pub history: [Option<HistoryKey>; 6],
    pub diagnostics: [Option<crate::hwinfo_provider::Diagnostic>; 6],
    pub publisher_status: Option<Status>,
}
impl Projection {
    /// Call each render iteration: the immutable worker frame may have aged meanwhile.
    pub fn new(settings: &Settings, view: Option<&View>) -> Self {
        let mut out = Self {
            samples: [None; 6],
            history: [None; 6],
            diagnostics: [None; 6],
            publisher_status: view.map(|v| v.state.snapshot_status(v.now)),
        };
        let samples = settings.resolve_with_hwinfo([None; 6], [None; 6], [None; 6], view);
        for role in ROLES {
            let i = role.index();
            if let Binding::Hwinfo(id) = settings.roles[i] {
                let sample = samples[i];
                out.diagnostics[i] = view.map(|v| v.resolve(role, id).diagnostic);
                out.history[i] = Some(HistoryKey {
                    identity: id,
                    generation: view.map_or(0, |v| v.state.generation),
                    publisher_poll: view
                        .and_then(|v| v.state.frame.as_ref().map(|f| f.poll_time_raw)),
                    usable: sample.status == Status::Ok,
                });
                out.samples[i] = Some(sample);
            } else if settings.source == Source::Hwinfo && settings.roles[i] == Binding::Unbound {
                out.samples[i] = Some(samples[i]);
            }
        }
        out
    }
    /// Shared status footer for every layout, including layouts without all six slots.
    pub fn draw(&self, fb: &mut Framebuffer) {
        let rows: Vec<_> = self.samples.iter().flatten().collect();
        if rows.is_empty() {
            return;
        }
        let cols = if fb.width() >= 900 { 3 } else { 1 };
        let height = 12 + rows.len().div_ceil(cols) as u32 * 12;
        let y = fb.height().saturating_sub(height);
        fb.fill_rect(0, y, fb.width(), height, 12, 16, 22);
        fb.draw_text(
            4,
            y + 2,
            &format!(
                "HWiNFO publisher: {:?}; reading freshness unverified",
                self.publisher_status.unwrap_or(Status::Unavailable)
            ),
            170,
            180,
            190,
            1,
        );
        for (n, s) in rows.into_iter().enumerate() {
            fb.draw_text(
                4 + (n % cols) as u32 * (fb.width() / cols as u32),
                y + 14 + (n / cols) as u32 * 12,
                &match self.diagnostics[s.role.index()] {
                    Some(reason) if s.status == Status::Invalid => {
                        format!("{} {reason:?}", display_text(s))
                    }
                    _ => display_text(s),
                },
                225,
                225,
                225,
                1,
            );
        }
    }
}
/// Reserve a footer instead of covering a layout's ticker or controls.
pub fn footer_height(settings: &Settings, width: u32) -> u32 {
    let count = settings
        .roles
        .iter()
        .filter(|b| {
            matches!(b, Binding::Hwinfo(_))
                || (settings.source == Source::Hwinfo && **b == Binding::Unbound)
        })
        .count();
    if count == 0 {
        0
    } else {
        12 + count.div_ceil(if width >= 900 { 3 } else { 1 }) as u32 * 12
    }
}
pub fn display_text(s: &Sample) -> String {
    let role = &KEYS[s.role.index()][7..];
    match (s.status, s.value) {
        (Status::Ok, Some(v)) => format!(
            "{role}: {v:.1} {}",
            if s.unit == Unit::Celsius { "C" } else { "RPM" }
        ),
        _ => format!("{role}: -- ({:?})", s.status),
    }
}
/// Same strict type/unit contract used by runtime resolution. No name heuristics.
pub fn compatible(entry: &Entry, role: Role) -> bool {
    entry.compatibility(role).is_ok()
}
/// Transactional editor binding: preserve unknown entries, store only stable IDs.
pub fn select(
    doc: &ConfDoc,
    view: &View,
    role: Role,
    id: HwinfoSelector,
) -> Result<ConfDoc, String> {
    if !matches!(
        view.state.snapshot_status(view.now),
        Status::Ok | Status::Unverified
    ) {
        return Err("Inventory unavailable or stale; reconnect before selecting".into());
    }
    let entries = &view
        .state
        .frame
        .as_ref()
        .ok_or("Inventory unavailable")?
        .entries;
    let mut matching = entries.iter().filter(|e| e.identity == id);
    let entry = matching.next().ok_or("Sensor missing")?;
    if matching.next().is_some() || !compatible(entry, role) {
        return Err("Duplicate or incompatible sensor".into());
    }
    let mut next = doc.clone();
    next.set(KEYS[role.index()], &id.to_string());
    sensor_policy::validate_text(&next.to_text())?;
    Ok(next)
}

/// Case-insensitive name/ID filtering with the same type/unit gate as selection.
pub fn matches_search(entry: &Entry, role: Role, query: &str) -> bool {
    compatible(entry, role)
        && (query.is_empty()
            || format!("{} {} {}", entry.sensor_name, entry.label, entry.identity)
                .to_lowercase()
                .contains(&query.to_lowercase()))
}
