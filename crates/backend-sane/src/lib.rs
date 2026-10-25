//! SANE backend for Linux (and anywhere libsane is installed).
//!
//! libsane is loaded at runtime, so this crate builds everywhere and
//! [`SaneBackend::new`] simply returns an error when SANE isn't installed.

mod ffi;
mod frame;
mod options;

use std::ffi::{CString, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::mpsc::Sender;

use scan_core::{
    Backend, CancelToken, Capabilities, Connection, Device, DeviceError, DeviceId, DeviceInfo, Page,
    Result, ScanEvent, ScanRequest, Source,
};

use ffi::{Handle, Sane, Status, Word};
use frame::{Format, Frame};
use options::{Constraint, OptDesc, Options, Unit, ValueType};

pub use frame::assemble as assemble_frames;

pub struct SaneBackend {
    sane: &'static Sane,
    /// Last device list, so `open` doesn't have to re-probe (which can take
    /// seconds with network backends) just to fill in the display name.
    known: Mutex<Vec<DeviceInfo>>,
}

impl SaneBackend {
    /// Load libsane and initialise it (once per process).
    pub fn new() -> Result<Self> {
        ffi::sane().map(|sane| Self { sane, known: Mutex::default() }).map_err(DeviceError::Other)
    }

    /// Like [`Backend::open`] but returns the concrete device, which allows
    /// setting arbitrary SANE options.
    pub fn open_device(&self, id: &DeviceId) -> Result<SaneDevice> {
        let name = CString::new(id.0.as_str()).map_err(|_| DeviceError::NotFound)?;
        let mut handle: Handle = ptr::null_mut();
        let status = {
            let _g = self.sane.global.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: valid C string and out-pointer.
            unsafe { (self.sane.open)(name.as_ptr(), &mut handle) }
        };
        if status == ffi::STATUS_INVAL {
            return Err(DeviceError::NotFound);
        }
        check(self.sane, status)?;
        let info = self
            .known
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|d| d.id == *id)
            .cloned()
            .unwrap_or_else(|| device_info(&id.0, None, None));
        let mut dev = SaneDevice {
            sane: self.sane,
            handle,
            info,
            options: Options::default(),
            caps: Capabilities {
                sources: vec![],
                duplex: false,
                resolutions: vec![],
                color_modes: vec![],
                max_size_mm: (0.0, 0.0),
            },
        };
        dev.reload_options()?;
        dev.caps = options::capabilities(&dev.options, dev.current_dpi().unwrap_or(300.0));
        Ok(dev)
    }
}

impl Backend for SaneBackend {
    fn name(&self) -> &'static str {
        "sane"
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let _g = self.sane.global.lock().unwrap_or_else(|e| e.into_inner());
        let mut list: *const *const ffi::SaneDevice = ptr::null();
        // SAFETY: out-pointer is valid; the list stays valid until the next
        // sane_get_devices call, which the global lock prevents meanwhile.
        check(self.sane, unsafe { (self.sane.get_devices)(&mut list, 0) })?;
        let mut out = Vec::new();
        if list.is_null() {
            return Ok(out);
        }
        let mut i = 0;
        loop {
            // SAFETY: null-terminated array of device pointers.
            let d = unsafe { *list.add(i) };
            if d.is_null() {
                break;
            }
            let d = unsafe { &*d };
            if let Some(name) = unsafe { ffi::cstr(d.name) } {
                let vendor = unsafe { ffi::cstr(d.vendor) };
                let model = unsafe { ffi::cstr(d.model) };
                out.push(device_info(&name, vendor.as_deref(), model.as_deref()));
            }
            i += 1;
        }
        *self.known.lock().unwrap_or_else(|e| e.into_inner()) = out.clone();
        Ok(out)
    }

    fn open(&self, id: &DeviceId) -> Result<Box<dyn Device>> {
        Ok(Box::new(self.open_device(id)?))
    }
}

fn device_info(name: &str, vendor: Option<&str>, model: Option<&str>) -> DeviceInfo {
    let label = [vendor, model]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    DeviceInfo {
        id: DeviceId(name.to_string()),
        name: if label.is_empty() { name.to_string() } else { label },
        serial: None,
        connection: connection_for(name),
        backend: "sane".into(),
    }
}

/// SANE doesn't say how a device is attached; guess from its name.
pub fn connection_for(name: &str) -> Connection {
    let has_ip = name
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .any(|tok| {
            let parts: Vec<_> = tok.split('.').collect();
            parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.parse::<u8>().is_ok())
        });
    if has_ip || name.to_ascii_lowercase().contains("net") {
        Connection::Network
    } else {
        Connection::Usb
    }
}

/// Map a SANE status to a device error.
pub fn map_status(status: i32, text: impl FnOnce() -> String) -> DeviceError {
    match status {
        ffi::STATUS_JAMMED => DeviceError::Jam,
        ffi::STATUS_COVER_OPEN => DeviceError::CoverOpen,
        ffi::STATUS_DEVICE_BUSY => DeviceError::Busy,
        ffi::STATUS_CANCELLED => DeviceError::Cancelled,
        ffi::STATUS_IO_ERROR => DeviceError::Disconnected,
        ffi::STATUS_NO_DOCS => DeviceError::NoPaper,
        _ => DeviceError::Other(text()),
    }
}

/// With `JANUS_SCAN_DEBUG` set, log SANE call results to stderr (status codes
/// only, never page data), for diagnosing vendor drivers.
fn trace(call: &str, status: Status) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("JANUS_SCAN_DEBUG").is_some()) {
        eprintln!("[sane] {call} -> {status}");
    }
}

fn check(sane: &Sane, status: Status) -> Result<()> {
    if status == ffi::STATUS_GOOD {
        Ok(())
    } else {
        Err(map_status(status, || sane.strstatus(status)))
    }
}

/// A value for [`SaneDevice::set_option`].
#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Bool(bool),
    Number(f64),
    Text(String),
}

pub struct SaneDevice {
    sane: &'static Sane,
    handle: Handle,
    info: DeviceInfo,
    options: Options,
    caps: Capabilities,
}

// SAFETY: the handle is owned by this value and only used through `&mut self`
// (or `&self` for read-only queries); SANE handles may move between threads.
unsafe impl Send for SaneDevice {}

impl Drop for SaneDevice {
    fn drop(&mut self) {
        let _g = self.sane.global.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: handle came from sane_open and is closed exactly once.
        unsafe { (self.sane.close)(self.handle) };
    }
}

const READ_CHUNK: usize = 64 * 1024;

impl SaneDevice {
    fn reload_options(&mut self) -> Result<()> {
        let mut count: Word = 0;
        // SAFETY: option 0 is always the integer option count.
        check(self.sane, unsafe {
            (self.sane.control_option)(
                self.handle,
                0,
                ffi::ACTION_GET_VALUE,
                &mut count as *mut Word as *mut c_void,
                ptr::null_mut(),
            )
        })?;
        let mut all = Vec::new();
        for index in 1..count {
            // SAFETY: index within the reported option count.
            let d = unsafe { (self.sane.get_option_descriptor)(self.handle, index) };
            if d.is_null() {
                continue;
            }
            // SAFETY: descriptor stays valid while the handle is open.
            if let Some(opt) = unsafe { read_descriptor(index, &*d) } {
                all.push(opt);
            }
        }
        self.options = Options { all };
        Ok(())
    }

    fn get_number(&self, name: &str) -> Option<f64> {
        let opt = self.options.get(name)?;
        if !matches!(opt.ty, ValueType::Int | ValueType::Fixed) || !opt.active {
            return None;
        }
        let mut buf = vec![0 as Word; (opt.size.max(4) as usize).div_ceil(4)];
        // SAFETY: buffer holds at least `size` bytes.
        let status = unsafe {
            (self.sane.control_option)(
                self.handle,
                opt.index,
                ffi::ACTION_GET_VALUE,
                buf.as_mut_ptr() as *mut c_void,
                ptr::null_mut(),
            )
        };
        (status == ffi::STATUS_GOOD).then(|| match opt.ty {
            ValueType::Fixed => ffi::fixed_to_f64(buf[0]),
            _ => buf[0] as f64,
        })
    }

    fn current_dpi(&self) -> Option<f64> {
        self.get_number("resolution")
    }

    /// Set a SANE option by name. Numbers are brought inside the option's
    /// constraint first. Returns `Ok(false)` if the device has no such option
    /// or it is currently inactive (e.g. geometry on a hand scanner).
    pub fn set_option(&mut self, name: &str, value: OptionValue) -> Result<bool> {
        let Some(opt) = self.options.get(name).cloned() else { return Ok(false) };
        if !opt.active {
            return Ok(false);
        }
        let words = (opt.size.max(4) as usize).div_ceil(4);
        let mut buf: Vec<u8> = match (&value, opt.ty) {
            (OptionValue::Text(s), ValueType::String) => {
                let mut b = vec![0u8; opt.size.max(1) as usize];
                let n = s.len().min(b.len() - 1);
                b[..n].copy_from_slice(&s.as_bytes()[..n]);
                // Pad to a word multiple so the buffer is never short.
                b.resize(b.len().next_multiple_of(4), 0);
                b
            }
            (OptionValue::Bool(v), ValueType::Bool) => word_buf(*v as Word, words),
            (OptionValue::Number(v), ValueType::Int) => word_buf(opt.constrain(*v).round() as Word, words),
            (OptionValue::Number(v), ValueType::Fixed) => word_buf(ffi::f64_to_fixed(opt.constrain(*v)), words),
            (OptionValue::Number(v), ValueType::Bool) => word_buf((*v != 0.0) as Word, words),
            _ => return Err(DeviceError::Other(format!("option {name}: value {value:?} doesn't fit type {:?}", opt.ty))),
        };
        let mut info: Word = 0;
        // SAFETY: buffer is at least the option's size, properly typed.
        let status = unsafe {
            (self.sane.control_option)(
                self.handle,
                opt.index,
                ffi::ACTION_SET_VALUE,
                buf.as_mut_ptr() as *mut c_void,
                &mut info,
            )
        };
        trace(&format!("sane_control_option set {name}"), status);
        check(self.sane, status)?;
        // Always reload (cheap): ranges and active flags often depend on other
        // options, and not every backend sets SANE_INFO_RELOAD_OPTIONS.
        self.reload_options()?;
        Ok(true)
    }

    /// Set source, mode, resolution and scan area. Returns whether the chosen
    /// source is a feeder, plus the resolution actually in effect.
    fn apply(&mut self, req: &ScanRequest) -> Result<(bool, u32)> {
        let sources = self.options.get("source").map(|o| o.strings().to_vec()).unwrap_or_default();
        let mut feeder = false;
        if !sources.is_empty() {
            let pick = options::pick_source(&sources, req.source, req.duplex).ok_or_else(|| {
                DeviceError::Other(match (req.source, req.duplex) {
                    (Source::Feeder, true) => "This scanner can't scan two-sided.".into(),
                    (Source::Feeder, false) => "This scanner has no document feeder.".into(),
                    (Source::Flatbed, _) => "This scanner has no flatbed.".into(),
                })
            })?;
            feeder = options::is_feeder_source(&pick);
            self.set_option("source", OptionValue::Text(pick))?;
        }

        let modes = self.options.get("mode").map(|o| o.strings().to_vec()).unwrap_or_default();
        if let Some(mode) = options::pick_mode(&modes, req.color) {
            self.set_option("mode", OptionValue::Text(mode))?;
        }

        if let Some(res) = self.options.get("resolution").cloned() {
            self.set_option("resolution", OptionValue::Number(options::nearest_resolution(&res, req.dpi)))?;
        }
        let dpi = self.current_dpi().unwrap_or(req.dpi as f64);

        // Recompute limits for the selected source before clamping the area.
        self.caps.max_size_mm = options::capabilities(&self.options, dpi).max_size_mm;
        let (w, h) = req.paper.area_mm(&self.caps);
        for (tl, br, len) in [("tl-x", "br-x", w), ("tl-y", "br-y", h)] {
            let origin = match self.options.get(tl).cloned() {
                Some(o) => {
                    let v = o.range_min().unwrap_or(0.0);
                    self.set_option(tl, OptionValue::Number(v))?;
                    v
                }
                None => 0.0,
            };
            if let Some(b) = self.options.get(br).cloned() {
                let v = origin + options::mm_to_option(&b, len as f64, dpi);
                self.set_option(br, OptionValue::Number(v))?;
            }
        }
        Ok((feeder, dpi.round() as u32))
    }

    fn start(&self) -> Status {
        // SAFETY: valid open handle.
        let status = unsafe { (self.sane.start)(self.handle) };
        trace("sane_start", status);
        status
    }

    /// Read all frames of one page (one, or three for three-pass scanners).
    /// `sane_start` for the first frame has already succeeded.
    /// `first_from_feeder`: the first page of a feeder pass, where an I/O
    /// error before any data means the pass never started: Brother's network
    /// driver starts fine on an empty feeder and then fails the first read.
    fn read_page(&self, cancel: &CancelToken, first_from_feeder: bool) -> Result<Vec<Frame>> {
        let mut frames = Vec::new();
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            let mut p = ffi::SaneParameters::default();
            // SAFETY: valid handle and out-pointer.
            let status = unsafe { (self.sane.get_parameters)(self.handle, &mut p) };
            trace("sane_get_parameters", status);
            check(self.sane, status)?;
            let format = match p.format {
                ffi::FRAME_GRAY => Format::Gray,
                ffi::FRAME_RGB => Format::Rgb,
                ffi::FRAME_RED => Format::Red,
                ffi::FRAME_GREEN => Format::Green,
                ffi::FRAME_BLUE => Format::Blue,
                f => return Err(DeviceError::Other(format!("unsupported SANE frame format {f}"))),
            };
            let lines = (p.lines > 0).then_some(p.lines as usize);
            let bpl = p.bytes_per_line.max(0) as usize;
            let mut data = Vec::with_capacity(lines.map_or(bpl * 64, |l| l * bpl));
            loop {
                if cancel.is_cancelled() {
                    // SAFETY: valid handle; scan() calls it again at the end, which is harmless.
                    unsafe { (self.sane.cancel)(self.handle) };
                    return Err(DeviceError::Cancelled);
                }
                let mut len: Word = 0;
                // SAFETY: chunk has READ_CHUNK bytes.
                let status =
                    unsafe { (self.sane.read)(self.handle, chunk.as_mut_ptr(), READ_CHUNK as Word, &mut len) };
                if len > 0 {
                    data.extend_from_slice(&chunk[..len as usize]);
                }
                match status {
                    ffi::STATUS_GOOD => {}
                    ffi::STATUS_EOF => break,
                    s => {
                        trace(&format!("sane_read after {} bytes", data.len()), s);
                        if s == ffi::STATUS_IO_ERROR && first_from_feeder && frames.is_empty() && data.is_empty() {
                            return Err(DeviceError::NotStarted);
                        }
                        return Err(map_status(s, || self.sane.strstatus(s)));
                    }
                }
            }
            frames.push(Frame {
                format,
                pixels_per_line: p.pixels_per_line.max(0) as usize,
                bytes_per_line: bpl,
                lines,
                depth: p.depth.max(0) as u32,
                data,
            });
            if p.last_frame != 0 {
                return Ok(frames);
            }
            check(self.sane, self.start())?;
        }
    }

    fn run(&mut self, req: &ScanRequest, events: &Sender<ScanEvent>, cancel: &CancelToken) -> Result<ScanEvent> {
        let (feeder, dpi) = self.apply(req)?;
        let send = |ev: ScanEvent| events.send(ev).map_err(|_| DeviceError::Cancelled);
        let mut index = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(DeviceError::Cancelled);
            }
            let status = self.start();
            if status == ffi::STATUS_NO_DOCS && feeder {
                return if index == 0 { Err(DeviceError::NoPaper) } else { Ok(ScanEvent::FeederEmpty) };
            }
            check(self.sane, status)?;
            send(ScanEvent::PageStarted(index))?;
            let frames = self.read_page(cancel, feeder && index == 0)?;
            let image = frame::assemble(&frames).map_err(DeviceError::Other)?;
            send(ScanEvent::Page(Page { index, dpi, image }))?;
            index += 1;
            if !feeder {
                return Ok(ScanEvent::Done);
            }
        }
    }
}

fn word_buf(w: Word, words: usize) -> Vec<u8> {
    std::iter::repeat_n(w.to_ne_bytes(), words).flatten().collect()
}

/// # Safety
/// `d` must be a descriptor returned by `sane_get_option_descriptor`.
unsafe fn read_descriptor(index: Word, d: &ffi::SaneOptionDescriptor) -> Option<OptDesc> {
    let name = unsafe { ffi::cstr(d.name) }?;
    let ty = match d.kind {
        ffi::TYPE_BOOL => ValueType::Bool,
        ffi::TYPE_INT => ValueType::Int,
        ffi::TYPE_FIXED => ValueType::Fixed,
        ffi::TYPE_STRING => ValueType::String,
        _ => ValueType::Other,
    };
    let unit = match d.unit {
        ffi::UNIT_PIXEL => Unit::Pixel,
        ffi::UNIT_MM => Unit::Mm,
        _ => Unit::Other,
    };
    let num = |w: Word| if ty == ValueType::Fixed { ffi::fixed_to_f64(w) } else { w as f64 };
    let constraint = unsafe {
        match d.constraint_type {
            ffi::CONSTRAINT_RANGE if !d.constraint.range.is_null() => {
                let r = &*d.constraint.range;
                Constraint::Range { min: num(r.min), max: num(r.max), quant: num(r.quant) }
            }
            ffi::CONSTRAINT_WORD_LIST if !d.constraint.word_list.is_null() => {
                // First word is the list length.
                let wl = d.constraint.word_list;
                let n = (*wl).max(0) as usize;
                Constraint::WordList((1..=n).map(|i| num(*wl.add(i))).collect())
            }
            ffi::CONSTRAINT_STRING_LIST if !d.constraint.string_list.is_null() => {
                let mut v = Vec::new();
                let mut p = d.constraint.string_list;
                while !(*p).is_null() {
                    v.extend(ffi::cstr(*p));
                    p = p.add(1);
                }
                Constraint::StringList(v)
            }
            _ => Constraint::None,
        }
    };
    Some(OptDesc {
        index,
        name,
        ty,
        unit,
        size: d.size,
        active: d.cap & ffi::CAP_INACTIVE == 0,
        constraint,
    })
}

impl Device for SaneDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn scan(&mut self, req: &ScanRequest, events: Sender<ScanEvent>, cancel: CancelToken) {
        let result = self.run(req, &events, &cancel);
        // Always end the pass, whatever happened.
        // SAFETY: valid open handle.
        unsafe { (self.sane.cancel)(self.handle) };
        let _ = events.send(match result {
            Ok(ev) => ev,
            Err(e) => ScanEvent::Error(e),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_guess() {
        assert_eq!(connection_for("brother4:net1;dev0"), Connection::Network);
        assert_eq!(connection_for("escl:http://192.168.1.20:80"), Connection::Network);
        assert_eq!(connection_for("brother4:bus2;dev1"), Connection::Usb);
        assert_eq!(connection_for("test:0"), Connection::Usb);
        assert_eq!(connection_for("genesys:libusb:001:004"), Connection::Usb);
        assert_eq!(connection_for("x:1.2.3.4.5"), Connection::Usb);
    }

    #[test]
    fn status_mapping() {
        let t = || "boom".to_string();
        assert_eq!(map_status(ffi::STATUS_JAMMED, t), DeviceError::Jam);
        assert_eq!(map_status(ffi::STATUS_COVER_OPEN, t), DeviceError::CoverOpen);
        assert_eq!(map_status(ffi::STATUS_DEVICE_BUSY, t), DeviceError::Busy);
        assert_eq!(map_status(ffi::STATUS_CANCELLED, t), DeviceError::Cancelled);
        assert_eq!(map_status(ffi::STATUS_IO_ERROR, t), DeviceError::Disconnected);
        assert_eq!(map_status(1 /* UNSUPPORTED */, t), DeviceError::Other("boom".into()));
    }

    #[test]
    fn device_label() {
        let d = device_info("test:0", Some("Noname"), Some("frontend-tester"));
        assert_eq!(d.name, "Noname frontend-tester");
        assert_eq!(d.id, DeviceId("test:0".into()));
        assert_eq!(d.backend, "sane");
        assert_eq!(device_info("x:1", None, Some(" ")).name, "x:1");
    }

    #[test]
    fn new_never_panics() {
        // Either loads or reports an error; both are fine.
        let _ = SaneBackend::new();
    }
}
