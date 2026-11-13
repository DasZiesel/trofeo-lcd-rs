//! Owned inventory, conservative publisher progress and cancellable acquisition.
//! Publisher progress is not evidence of per-reading freshness.
use crate::{
    hwinfo_sm2::{self, DecodeError, ReadingType},
    sensor_policy::{HwinfoSelector, Role, Sample, Source, Status, Unit},
};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub identity: HwinfoSelector,
    pub sensor_name: String,
    pub label: String,
    pub reading_type: ReadingType,
    pub unit: String,
    pub current: f64,
    pub numeric_status: Status,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub poll_time_raw: i64,
    pub polling_period_raw: Option<u32>,
    pub entries: Vec<Entry>,
}
fn display_text(
    utf8: Option<&str>,
    user: hwinfo_sm2::AnsiText<'_>,
    original: hwinfo_sm2::AnsiText<'_>,
) -> String {
    if let Some(text) = utf8.filter(|s| !s.is_empty()) {
        return text.escape_debug().to_string();
    }
    let text = if user.bytes().is_empty() {
        original
    } else {
        user
    };
    match text.ascii() {
        Some(s) => s.escape_debug().to_string(),
        None => format!("ANSI {:?}", text.bytes()), // no guessed locale/codepage
    }
}
impl Frame {
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let snapshot = hwinfo_sm2::decode(bytes).map_err(Failure::Decode)?;
        let sensors: std::collections::HashMap<_, _> = snapshot
            .sensors
            .iter()
            .map(|s| ((s.identity.sensor_id, s.identity.instance), s))
            .collect();
        let entries = snapshot
            .readings
            .iter()
            .map(|r| {
                let sensor = sensors[&(r.identity.sensor_id, r.identity.sensor_instance)];
                Entry {
                    identity: r.identity,
                    sensor_name: display_text(
                        sensor.user_name_utf8,
                        sensor.user_name,
                        sensor.original_name,
                    ),
                    label: display_text(r.user_label_utf8, r.user_label, r.original_label),
                    reading_type: r.reading_type,
                    // Units are exact validation input, not escaped display labels.
                    unit: r
                        .unit_utf8
                        .map(str::to_owned)
                        .or_else(|| r.unit.ascii().map(str::to_owned))
                        .unwrap_or_else(|| format!("ANSI {:?}", r.unit.bytes())),
                    current: r.values.current,
                    numeric_status: r.status(),
                }
            })
            .collect();
        Ok(Self {
            poll_time_raw: snapshot.header.poll_time,
            polling_period_raw: snapshot.header.polling_period,
            entries,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Decode(DecodeError),
    #[cfg(all(windows, target_pointer_width = "64"))]
    Transport(crate::hwinfo_transport::TransportError),
    Unavailable,
    DuplicateIdentity,
    InventoryLimit,
    WorkerPanicked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    None,
    Missing,
    Duplicate,
    TypeMismatch,
    UnitMismatch,
    Range,
    PublisherUnverified,
    PublisherStale,
    ReadingUnverified,
    ProviderUnavailable,
    Closed,
}
#[derive(Clone, Debug)]
pub struct Resolution {
    pub sample: Sample,
    pub diagnostic: Diagnostic,
}
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub poll_interval: Duration,
    pub stale_after: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            stale_after: Duration::from_secs(5),
        }
    }
}
impl Options {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.poll_interval < Duration::from_millis(100)
            || self.poll_interval > Duration::from_secs(10)
            || self.stale_after < self.poll_interval * 2
            || self.stale_after > Duration::from_secs(60)
        {
            return Err("invalid bounded polling/freshness options");
        }
        Ok(self)
    }
}
#[derive(Clone, Debug)]
pub struct State {
    pub generation: u64,
    pub frame: Option<Arc<Frame>>,
    pub failure: Option<Failure>,
    pub last_progress_at: Option<Duration>,
    last_seen: Option<Duration>,
    progressed: bool,
    closed: bool,
    options: Options,
}
impl State {
    pub fn new(options: Options) -> Result<Self, &'static str> {
        Ok(Self {
            generation: 0,
            frame: None,
            failure: None,
            last_progress_at: None,
            last_seen: None,
            progressed: false,
            closed: false,
            options: options.validate()?,
        })
    }
    pub fn snapshot_status(&self, now: Duration) -> Status {
        if self.closed {
            return Status::Closed;
        }
        if self.failure.is_some() || self.frame.is_none() {
            return Status::Unavailable;
        }
        match self.last_progress_at.and_then(|t| now.checked_sub(t)) {
            Some(age) if age >= self.options.stale_after => Status::Stale,
            Some(_) if self.progressed => Status::Ok, // publisher only, never a reading
            _ => Status::Unverified,
        }
    }
    pub fn update(&mut self, result: Result<Frame, Failure>, now: Duration) {
        if self.closed {
            return;
        }
        let result = result.and_then(|frame| {
            if frame.entries.len() > hwinfo_sm2::MAX_READINGS as usize {
                return Err(Failure::InventoryLimit);
            }
            let mut seen = HashSet::new();
            for r in &frame.entries {
                if !seen.insert((
                    r.identity.sensor_id,
                    r.identity.sensor_instance,
                    r.identity.reading_id,
                )) {
                    return Err(Failure::DuplicateIdentity);
                }
            }
            Ok(frame)
        });
        let frame = match result {
            Ok(frame) => frame,
            Err(error) => {
                self.frame = None;
                self.failure = Some(error);
                self.progressed = false;
                self.last_progress_at = None;
                self.last_seen = Some(now);
                return;
            }
        };
        let advancing = self
            .frame
            .as_ref()
            .is_some_and(|f| frame.poll_time_raw > f.poll_time_raw);
        let reset = self.frame.is_none()
            || self.last_seen.is_some_and(|t| now < t)
            || (advancing
                && (self.snapshot_status(now) == Status::Stale
                    || self
                        .last_seen
                        .is_some_and(|t| now.saturating_sub(t) >= self.options.stale_after)))
            || self
                .frame
                .as_ref()
                .is_some_and(|f| frame.poll_time_raw < f.poll_time_raw);
        if reset {
            self.generation = self.generation.saturating_add(1);
            self.last_progress_at = Some(now);
            self.progressed = false;
            self.frame = Some(Arc::new(frame));
        } else if self
            .frame
            .as_ref()
            .is_some_and(|f| frame.poll_time_raw > f.poll_time_raw)
        {
            self.last_progress_at = Some(now);
            self.progressed = true;
            self.frame = Some(Arc::new(frame));
        }
        // Equal raw poll times never refresh data or the progress deadline.
        self.failure = None;
        self.last_seen = Some(now);
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.frame = None;
        self.last_progress_at = None;
    }
    pub fn resolve(&self, role: Role, id: HwinfoSelector, now: Duration) -> Resolution {
        let result = |status, diagnostic| Resolution {
            sample: Sample::new(Source::Hwinfo, role, None, status),
            diagnostic,
        };
        if self.closed {
            return result(Status::Closed, Diagnostic::Closed);
        }
        if matches!(
            self.failure,
            Some(
                Failure::DuplicateIdentity
                    | Failure::Decode(DecodeError::DuplicateSensor | DecodeError::DuplicateReading)
            )
        ) {
            return result(Status::Invalid, Diagnostic::Duplicate);
        }
        let Some(frame) = &self.frame else {
            return result(Status::Unavailable, Diagnostic::ProviderUnavailable);
        };
        let Some(entry) = frame.entries.iter().find(|r| r.identity == id) else {
            return result(Status::NotFound, Diagnostic::Missing);
        };
        let expected = if role.unit() == Unit::Celsius {
            ReadingType::Temperature
        } else {
            ReadingType::Fan
        };
        if entry.reading_type != expected {
            return result(Status::Invalid, Diagnostic::TypeMismatch);
        }
        let unit_ok = match role.unit() {
            Unit::Celsius => matches!(entry.unit.as_str(), "°C" | "C"),
            Unit::Rpm => entry.unit == "RPM",
        };
        if !unit_ok {
            return result(Status::Invalid, Diagnostic::UnitMismatch);
        }
        let range_ok = match role.unit() {
            Unit::Celsius => (-273.15..=1000.0).contains(&entry.current),
            Unit::Rpm => (0.0..=u32::MAX as f64).contains(&entry.current),
        };
        if !entry.current.is_finite() || entry.numeric_status == Status::Invalid || !range_ok {
            return result(Status::Invalid, Diagnostic::Range);
        }
        match self.snapshot_status(now) {
            Status::Stale => result(Status::Stale, Diagnostic::PublisherStale),
            Status::Ok => result(Status::Unverified, Diagnostic::ReadingUnverified),
            _ => result(Status::Unverified, Diagnostic::PublisherUnverified),
        }
    }
}

pub struct View {
    pub state: Arc<State>,
    pub now: Duration,
}
impl View {
    pub fn resolve(&self, role: Role, id: HwinfoSelector) -> Resolution {
        self.state.resolve(role, id, self.now)
    }
}
pub struct Worker {
    state: Arc<Mutex<Arc<State>>>,
    cancel: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    join: Option<JoinHandle<()>>,
    origin: Instant,
}
impl Worker {
    /// Source must honor cancellation and return in finite time. Never called on render thread.
    pub fn spawn_with<F>(options: Options, mut source: F) -> Result<Self, String>
    where
        F: FnMut(&AtomicBool) -> Result<Frame, Failure> + Send + 'static,
    {
        let mut current = State::new(options).map_err(str::to_owned)?;
        let state = Arc::new(Mutex::new(Arc::new(current.clone())));
        let shared = state.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (wake, receiver) = mpsc::channel();
        let origin = Instant::now();
        let join = thread::Builder::new()
            .name("hwinfo-reader".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let read =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source(&stop)));
                    let panicked = read.is_err();
                    current.update(
                        read.unwrap_or(Err(Failure::WorkerPanicked)),
                        origin.elapsed(),
                    );
                    if let Ok(mut slot) = shared.lock() {
                        *slot = Arc::new(current.clone());
                    }
                    if panicked || stop.load(Ordering::Acquire) {
                        break;
                    }
                    // No catch-up bursts after slow IO. Errors retry at the same bounded interval.
                    if receiver.recv_timeout(options.poll_interval).is_ok() {
                        break;
                    }
                }
                current.close();
                if let Ok(mut slot) = shared.lock() {
                    *slot = Arc::new(current);
                }
            })
            .map_err(|e| format!("cannot start HWiNFO worker: {e}"))?;
        Ok(Self {
            state,
            cancel,
            wake,
            join: Some(join),
            origin,
        })
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn start(options: Options) -> Result<Self, String> {
        let transport =
            crate::hwinfo_transport::Transport::hwinfo(100).map_err(|e| e.to_string())?;
        Self::spawn_with(options, move |cancel| {
            let copy = transport.capture(cancel).map_err(Failure::Transport)?;
            Frame::decode(copy.bytes())
        })
    }
    /// Only a try_lock and Arc clone; no Windows calls and no IO/decoder work.
    pub fn view(&self) -> Option<View> {
        let state = self.state.try_lock().ok()?.clone();
        Some(View {
            state,
            now: self.origin.elapsed(),
        })
    }
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Release);
        let _ = self.wake.send(());
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| "HWiNFO worker panicked".to_string())?;
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
