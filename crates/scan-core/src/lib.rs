//! Backend-neutral scanner model: devices, capabilities, scan requests and the
//! event stream a scan produces. Platform backends implement [`Backend`] and
//! [`Device`]; the UI and CLI only ever talk to these traits.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

pub use image::DynamicImage;

/// Stable identity for a physical scanner. Built from the serial number when the
/// backend knows it, so the same scanner seen over USB and Wi-Fi is one device.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Connection {
    Usb,
    Network,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub serial: Option<String>,
    pub connection: Connection,
    pub backend: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Feeder,
    Flatbed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Color,
    Gray,
    /// One bit per pixel. Backends that can't scan line-art scan gray and the
    /// imaging crate thresholds it.
    Bw,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum PaperSize {
    Letter,
    Legal,
    A4,
    A5,
    BusinessCard,
    Receipt,
    /// The whole scan area; auto-crop trims it afterwards.
    Auto,
    Custom { width_mm: f32, height_mm: f32 },
}

impl PaperSize {
    /// Width and height in millimetres, or `None` for [`PaperSize::Auto`].
    pub fn size_mm(self) -> Option<(f32, f32)> {
        Some(match self {
            Self::Letter => (215.9, 279.4),
            Self::Legal => (215.9, 355.6),
            Self::A4 => (210.0, 297.0),
            Self::A5 => (148.0, 210.0),
            Self::BusinessCard => (88.9, 50.8),
            Self::Receipt => (80.0, 297.0),
            Self::Auto => return None,
            Self::Custom { width_mm, height_mm } => (width_mm, height_mm),
        })
    }

    /// Clamp to the device's maximum scan area.
    pub fn area_mm(self, caps: &Capabilities) -> (f32, f32) {
        let (max_w, max_h) = caps.max_size_mm;
        match self.size_mm() {
            Some((w, h)) => (w.min(max_w), h.min(max_h)),
            None => (max_w, max_h),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub sources: Vec<Source>,
    pub duplex: bool,
    pub resolutions: Vec<u32>,
    pub color_modes: Vec<ColorMode>,
    pub max_size_mm: (f32, f32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRequest {
    pub source: Source,
    pub duplex: bool,
    pub dpi: u32,
    pub color: ColorMode,
    pub paper: PaperSize,
}

impl Default for ScanRequest {
    fn default() -> Self {
        Self {
            source: Source::Feeder,
            duplex: false,
            dpi: 300,
            color: ColorMode::Color,
            paper: PaperSize::Letter,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    /// 0-based position within this scan pass.
    pub index: u32,
    pub dpi: u32,
    pub image: DynamicImage,
}

#[derive(Debug, Clone)]
pub enum ScanEvent {
    PageStarted(u32),
    Page(Page),
    /// The feeder ran out of paper. In multi-page mode the UI asks whether more
    /// pages are coming; the scan itself has ended.
    FeederEmpty,
    Error(DeviceError),
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "detail")]
pub enum DeviceError {
    #[error("No paper in the feeder. Load the pages face down and try again.")]
    NoPaper,
    /// The first page of a feeder pass failed to start with an error that
    /// can mean either an empty feeder or an unreachable scanner (Brother's
    /// network driver reports an empty feeder this way).
    #[error("The scanner didn't start. Load the pages face down in the feeder, make sure the scanner is on, and try again.")]
    NotStarted,
    #[error("Paper jam. Open the cover, remove the jammed sheet and try again.")]
    Jam,
    #[error("Two sheets went through at once. Fan the stack and reload it.")]
    DoubleFeed,
    #[error("The scanner cover is open. Close it and try again.")]
    CoverOpen,
    #[error("The scanner is busy with another app or computer.")]
    Busy,
    #[error("The scanner was disconnected.")]
    Disconnected,
    #[error("Scanner not found.")]
    NotFound,
    #[error("Scan cancelled.")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = DeviceError> = std::result::Result<T, E>;

/// Shared flag a caller sets to stop a scan in progress.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    fn devices(&self) -> Result<Vec<DeviceInfo>>;
    fn open(&self, id: &DeviceId) -> Result<Box<dyn Device>>;
}

pub trait Device: Send {
    fn info(&self) -> &DeviceInfo;
    fn capabilities(&self) -> &Capabilities;

    /// Run one pass through the feeder (or one flatbed scan), sending events to
    /// `events` as pages arrive. Blocks until the pass ends; the final event is
    /// always `Done`, `FeederEmpty` or `Error`.
    fn scan(&mut self, req: &ScanRequest, events: Sender<ScanEvent>, cancel: CancelToken);
}

/// Merge device lists from several backends, keeping one entry per physical
/// scanner and preferring USB when it's reachable both ways.
pub fn dedupe_devices(mut all: Vec<DeviceInfo>) -> Vec<DeviceInfo> {
    all.sort_by_key(|d| (d.id.0.clone(), d.connection != Connection::Usb));
    all.dedup_by(|a, b| a.id == b.id);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, connection: Connection) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId(id.into()),
            name: id.into(),
            serial: None,
            connection,
            backend: "test".into(),
        }
    }

    #[test]
    fn dedupe_prefers_usb() {
        let out = dedupe_devices(vec![
            dev("a", Connection::Network),
            dev("a", Connection::Usb),
            dev("b", Connection::Network),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].connection, Connection::Usb);
    }

    #[test]
    fn paper_clamps_to_device() {
        let caps = Capabilities {
            sources: vec![Source::Feeder],
            duplex: true,
            resolutions: vec![300],
            color_modes: vec![ColorMode::Color],
            max_size_mm: (215.9, 863.0),
        };
        assert_eq!(PaperSize::A4.area_mm(&caps), (210.0, 297.0));
        assert_eq!(PaperSize::Auto.area_mm(&caps), (215.9, 863.0));
    }
}
