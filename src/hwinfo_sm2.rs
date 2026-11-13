//! Pure decoder for a caller-owned, consistent copy of the HWiNFO SM2 mapping.
//! No acquisition, synchronization, role selection or freshness confirmation.
#![forbid(unsafe_code)]

use crate::sensor_policy::{HwinfoSelector, Source, Status};
use std::{collections::HashSet, fmt, ops::Range};

/// Active wire bytes are ASCII `HWiS` (48 57 69 53).
/// Interpret those bytes as little endian, not as a C multi-character literal.
pub const ACTIVE_SIGNATURE: u32 = u32::from_le_bytes(*b"HWiS");
// Application resource limits, not limits promised by HWiNFO.
pub const MAX_BUFFER_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SENSORS: u32 = 4096;
pub const MAX_READINGS: u32 = 16_384;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    InputTooLarge,
    Truncated,
    Signature(u32),
    UnsupportedLayout { version: u32, revision: u32 },
    ElementSize,
    ResourceLimit,
    ArithmeticOverflow,
    InvalidSection,
    OverlappingSections,
    InvalidSensorIndex(u32),
    DuplicateSensor,
    DuplicateReading,
    UnterminatedText,
    InvalidUtf8,
    Allocation,
}
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid HWiNFO SM2 data: {self:?}")
    }
}
impl std::error::Error for DecodeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub offset: u32,
    pub element_size: u32,
    pub count: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub revision: u32,
    /// Raw publisher time; no conversion to age or confirmation of freshness.
    pub poll_time: i64,
    /// Raw interval; its unit is not specified by the supplied interface header.
    pub polling_period: Option<u32>,
    pub sensors: Section,
    pub readings: Section,
}

/// Bytes before the first NUL, in the publisher's unspecified Windows ANSI codepage.
/// Non-ASCII bytes are valid and preserved; a later UI must supply a known codepage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnsiText<'a> {
    bytes: &'a [u8],
}
impl<'a> AnsiText<'a> {
    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
    /// No implicit UTF-8/Latin-1 conversion or replacement characters.
    pub fn ascii(self) -> Option<&'a str> {
        if self.bytes.is_ascii() {
            std::str::from_utf8(self.bytes).ok()
        } else {
            None
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SensorIdentity {
    pub sensor_id: u32,
    pub instance: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sensor<'a> {
    pub identity: SensorIdentity,
    pub original_name: AnsiText<'a>,
    pub user_name: AnsiText<'a>,
    pub user_name_utf8: Option<&'a str>,
}

/// Wire type, not an alternative source policy or quality model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingType {
    None,
    Temperature,
    Voltage,
    Fan,
    Current,
    Power,
    Clock,
    Usage,
    Other,
    Unknown(u32),
}
impl From<u32> for ReadingType {
    fn from(n: u32) -> Self {
        match n {
            0 => Self::None,
            1 => Self::Temperature,
            2 => Self::Voltage,
            3 => Self::Fan,
            4 => Self::Current,
            5 => Self::Power,
            6 => Self::Clock,
            7 => Self::Usage,
            8 => Self::Other,
            n => Self::Unknown(n),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawValues {
    pub current: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub average: f64,
}
impl RawValues {
    /// Pure numeric check, not a claim that a reading is observed or fresh.
    pub fn all_finite(self) -> bool {
        [self.current, self.minimum, self.maximum, self.average]
            .iter()
            .all(|v| v.is_finite())
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Reading<'a> {
    /// Existing configuration identity. The array index is never persisted as an ID.
    pub identity: HwinfoSelector,
    pub reading_type: ReadingType,
    pub original_label: AnsiText<'a>,
    pub user_label: AnsiText<'a>,
    pub unit: AnsiText<'a>,
    pub user_label_utf8: Option<&'a str>,
    pub unit_utf8: Option<&'a str>,
    pub values: RawValues,
}
impl Reading<'_> {
    /// Raw non-finite data is retained for inspection, but is never a valid sample.
    /// Even finite data stays Unverified, including zero and unknown types/units.
    pub fn status(&self) -> Status {
        if self.values.all_finite() {
            Status::Unverified
        } else {
            Status::Invalid
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot<'a> {
    pub header: Header,
    pub sensors: Vec<Sensor<'a>>,
    pub readings: Vec<Reading<'a>>,
}
impl Snapshot<'_> {
    pub fn source(&self) -> Source {
        Source::Hwinfo
    }
    pub fn status(&self) -> Status {
        Status::Unverified
    }
}

fn bytes<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N], DecodeError> {
    let end = offset
        .checked_add(N)
        .ok_or(DecodeError::ArithmeticOverflow)?;
    data.get(offset..end)
        .ok_or(DecodeError::Truncated)?
        .try_into()
        .map_err(|_| DecodeError::Truncated)
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32, DecodeError> {
    Ok(u32::from_le_bytes(bytes(data, offset)?))
}
fn f64_at(data: &[u8], offset: usize) -> Result<f64, DecodeError> {
    Ok(f64::from_le_bytes(bytes(data, offset)?))
}
fn c_string(data: &[u8], offset: usize, length: usize) -> Result<&[u8], DecodeError> {
    let end = offset
        .checked_add(length)
        .ok_or(DecodeError::ArithmeticOverflow)?;
    let field = data.get(offset..end).ok_or(DecodeError::Truncated)?;
    let nul = field
        .iter()
        .position(|b| *b == 0)
        .ok_or(DecodeError::UnterminatedText)?;
    // Bytes after the terminator are padding, not text.
    field.get(..nul).ok_or(DecodeError::Truncated)
}
fn ansi(data: &[u8], offset: usize, length: usize) -> Result<AnsiText<'_>, DecodeError> {
    Ok(AnsiText {
        bytes: c_string(data, offset, length)?,
    })
}
fn utf8(data: &[u8], offset: usize, length: usize) -> Result<&str, DecodeError> {
    std::str::from_utf8(c_string(data, offset, length)?).map_err(|_| DecodeError::InvalidUtf8)
}
fn section(data: &[u8], at: usize) -> Result<Section, DecodeError> {
    Ok(Section {
        offset: u32_at(data, at)?,
        element_size: u32_at(data, at + 4)?,
        count: u32_at(data, at + 8)?,
    })
}
fn extent(s: Section, header_size: usize, len: usize) -> Result<Range<usize>, DecodeError> {
    // Wire offsets remain in the DWORD address space even on a 64-bit host.
    let size = s
        .count
        .checked_mul(s.element_size)
        .ok_or(DecodeError::ArithmeticOverflow)?;
    let end = s
        .offset
        .checked_add(size)
        .ok_or(DecodeError::ArithmeticOverflow)?;
    let start = usize::try_from(s.offset).map_err(|_| DecodeError::ArithmeticOverflow)?;
    let end = usize::try_from(end).map_err(|_| DecodeError::ArithmeticOverflow)?;
    if start < header_size {
        return Err(DecodeError::InvalidSection);
    }
    if end > len {
        return Err(DecodeError::Truncated);
    }
    Ok(start..end)
}
fn reserve<T>(vec: &mut Vec<T>, count: u32) -> Result<(), DecodeError> {
    vec.try_reserve_exact(usize::try_from(count).map_err(|_| DecodeError::ArithmeticOverflow)?)
        .map_err(|_| DecodeError::Allocation)
}

// Shared structural preflight, also used before any transport payload allocation.
fn layout_spec(data: &[u8]) -> Result<(u32, u32, (usize, u32, u32)), DecodeError> {
    let signature = u32_at(data, 0)?;
    if signature != ACTIVE_SIGNATURE {
        return Err(DecodeError::Signature(signature));
    }
    let version = u32_at(data, 4)?;
    let revision = u32_at(data, 8)?;
    let sizes = match (version, revision) {
        (1, 0) => (44, 264, 316),
        (1, 1) => (48, 264, 316),
        (2, 1) => (48, 392, 460),
        _ => return Err(DecodeError::UnsupportedLayout { version, revision }),
    };
    Ok((version, revision, sizes))
}
pub(crate) fn header_size(data: &[u8]) -> Result<usize, DecodeError> {
    Ok(layout_spec(data)?.2 .0)
}
pub(crate) struct SnapshotLayout {
    pub header: Header,
    pub sensors: Range<usize>,
    pub readings: Range<usize>,
    pub required_len: usize,
}
pub(crate) fn validate_header(
    data: &[u8],
    available: usize,
) -> Result<SnapshotLayout, DecodeError> {
    let (version, revision, (header_size, sensor_size, reading_size)) = layout_spec(data)?;
    data.get(..header_size).ok_or(DecodeError::Truncated)?;
    let header = Header {
        version,
        revision,
        poll_time: i64::from_le_bytes(bytes(data, 12)?),
        polling_period: if revision == 1 {
            Some(u32_at(data, 44)?)
        } else {
            None
        },
        sensors: section(data, 20)?,
        readings: section(data, 32)?,
    };
    if header.sensors.element_size != sensor_size || header.readings.element_size != reading_size {
        return Err(DecodeError::ElementSize);
    }
    if header.sensors.count > MAX_SENSORS || header.readings.count > MAX_READINGS {
        return Err(DecodeError::ResourceLimit);
    }
    let sr = extent(header.sensors, header_size, available)?;
    let rr = extent(header.readings, header_size, available)?;
    if !sr.is_empty() && !rr.is_empty() && sr.start < rr.end && rr.start < sr.end {
        return Err(DecodeError::OverlappingSections);
    }
    let required_len = header_size.max(sr.end).max(rr.end);
    Ok(SnapshotLayout {
        header,
        sensors: sr,
        readings: rr,
        required_len,
    })
}

/// Decode one bounded snapshot. Success means structurally decoded, never live.
/// Supported layouts: (version, revision) = (1,0), (1,1), (2,1), exact strides.
/// (2,0) is rejected: the documented UTF-8 version postdates revision 1.
/// Gaps, reversed disjoint sections and trailing bytes are allowed; overlaps are not.
pub fn decode(data: &[u8]) -> Result<Snapshot<'_>, DecodeError> {
    if data.len() > MAX_BUFFER_BYTES {
        return Err(DecodeError::InputTooLarge);
    }
    let layout = validate_header(data, data.len())?;
    let header = layout.header;
    let version = header.version;
    let sensor_size = header.sensors.element_size;
    let reading_size = header.readings.element_size;
    let sr = layout.sensors;
    let rr = layout.readings;
    let mut sensors = Vec::new();
    let mut readings = Vec::new();
    reserve(&mut sensors, header.sensors.count)?;
    reserve(&mut readings, header.readings.count)?;
    let mut sensor_ids = HashSet::new();
    let mut reading_ids = HashSet::new();
    sensor_ids
        .try_reserve(header.sensors.count as usize)
        .map_err(|_| DecodeError::Allocation)?;
    reading_ids
        .try_reserve(header.readings.count as usize)
        .map_err(|_| DecodeError::Allocation)?;
    let sensor_bytes = data.get(sr).ok_or(DecodeError::Truncated)?;
    for record in sensor_bytes.chunks_exact(sensor_size as usize) {
        let identity = SensorIdentity {
            sensor_id: u32_at(record, 0)?,
            instance: u32_at(record, 4)?,
        };
        if !sensor_ids.insert(identity) {
            return Err(DecodeError::DuplicateSensor);
        }
        sensors.push(Sensor {
            identity,
            original_name: ansi(record, 8, 128)?,
            user_name: ansi(record, 136, 128)?,
            user_name_utf8: if version == 2 {
                Some(utf8(record, 264, 128)?)
            } else {
                None
            },
        });
    }
    let reading_bytes = data.get(rr).ok_or(DecodeError::Truncated)?;
    for record in reading_bytes.chunks_exact(reading_size as usize) {
        let index = u32_at(record, 4)?;
        let sensor = sensors
            .get(index as usize)
            .ok_or(DecodeError::InvalidSensorIndex(index))?;
        let id = u32_at(record, 8)?;
        let identity = HwinfoSelector {
            sensor_id: sensor.identity.sensor_id,
            sensor_instance: sensor.identity.instance,
            reading_id: id,
        };
        if !reading_ids.insert((identity.sensor_id, identity.sensor_instance, id)) {
            return Err(DecodeError::DuplicateReading);
        }
        readings.push(Reading {
            identity,
            reading_type: u32_at(record, 0)?.into(),
            original_label: ansi(record, 12, 128)?,
            user_label: ansi(record, 140, 128)?,
            unit: ansi(record, 268, 16)?,
            user_label_utf8: if version == 2 {
                Some(utf8(record, 316, 128)?)
            } else {
                None
            },
            unit_utf8: if version == 2 {
                Some(utf8(record, 444, 16)?)
            } else {
                None
            },
            values: RawValues {
                current: f64_at(record, 284)?,
                minimum: f64_at(record, 292)?,
                maximum: f64_at(record, 300)?,
                average: f64_at(record, 308)?,
            },
        });
    }
    Ok(Snapshot {
        header,
        sensors,
        readings,
    })
}
