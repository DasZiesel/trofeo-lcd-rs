//! Bounded diagnostic state, using production decoding, selection and projection.
//! Selects the lowest compatible temperature ID solely as a test slot, not a CPU claim.
use crate::{
    conf_edit::ConfDoc,
    hwinfo_provider::{Frame, Options, State, View},
    sensor_display::{self, Projection},
    sensor_policy::{self, Role, Status},
};
use std::{sync::Arc, time::Duration};
pub struct Probe {
    state: State,
    saved: Option<String>,
}
pub struct Report {
    pub publisher: Status,
    pub display: Status,
    pub roundtrip: bool,
}
impl Probe {
    pub fn new() -> Self {
        Self {
            state: State::new(Options::default()).expect("default options"),
            saved: None,
        }
    }
    pub fn observe(&mut self, bytes: &[u8], now: Duration) -> Result<Report, String> {
        self.state.update(Frame::decode(bytes), now);
        if self.state.failure.is_some() {
            return Err("provider acquisition or decode error".into());
        }
        let view = View {
            state: Arc::new(self.state.clone()),
            now,
        };
        if self.saved.is_none() {
            let id = view
                .state
                .frame
                .as_ref()
                .ok_or("missing frame")?
                .entries
                .iter()
                .filter(|e| sensor_display::compatible(e, Role::CpuTemp))
                .map(|e| e.identity)
                .min_by_key(|id| (id.sensor_id, id.sensor_instance, id.reading_id))
                .ok_or("no compatible temperature reading")?;
            let doc = sensor_display::select(
                &ConfDoc::parse("sensor_source=hwinfo\n"),
                &view,
                Role::CpuTemp,
                id,
            )?;
            self.saved = Some(doc.to_text());
        }
        // Same saved text and parser as the editor; no file or inventory output.
        let saved = self.saved.as_ref().expect("selected");
        let reloaded = ConfDoc::parse(saved);
        let settings = sensor_policy::validate_text(&reloaded.to_text())?;
        let p = Projection::new(&settings, Some(&view));
        let sample = p.samples[0].ok_or("missing projection")?;
        if !matches!(sample.status, Status::Ok | Status::Unverified) {
            return Err(format!("display={:?}", sample.status));
        }
        Ok(Report {
            publisher: self.state.snapshot_status(now),
            display: sample.status,
            roundtrip: reloaded.to_text() == *saved,
        })
    }
}
impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}
