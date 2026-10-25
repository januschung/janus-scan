//! ICA objects and their delegate callbacks live on the main thread. Every
//! public entry point runs on a background thread, posts its work to the main
//! dispatch queue and waits for the reply or for delegate messages. The main
//! thread must therefore be running its run loop: AppKit does this in a GUI app,
//! and [`run_with_main_loop`] does it for command-line tools.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, Message, DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSError, NSIndexSet, NSPoint, NSRect, NSRunLoop, NSSize,
    NSString, NSURL,
};
use objc2_image_capture_core::{
    ICDevice, ICDeviceBrowser, ICDeviceBrowserDelegate, ICDeviceDelegate,
    ICDeviceLocationTypeMask, ICDeviceTypeMask, ICScannerBitDepth, ICScannerDevice,
    ICScannerDeviceDelegate, ICScannerDocumentType, ICScannerFunctionalUnit,
    ICScannerFunctionalUnitDocumentFeeder, ICScannerFunctionalUnitType, ICScannerMeasurementUnit,
    ICScannerPixelDataType, ICScannerTransferMode, ICTransportTypeUSB,
};
use scan_core::{
    Backend, CancelToken, Capabilities, ColorMode, Connection, Device, DeviceError, DeviceId,
    DeviceInfo, Page, PaperSize, Result, ScanEvent, ScanRequest, Source,
};

const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const ENUMERATE_TIMEOUT: Duration = Duration::from_secs(3);
/// Resolutions offered when the driver reports a dense range.
const COMMON_DPI: [u32; 8] = [100, 150, 200, 240, 300, 400, 600, 1200];
const MM_PER_INCH: f64 = 25.4;

/// Delegate callbacks forwarded to the thread waiting on an open device.
enum Msg {
    Ready,
    Opened,
    OpenFailed(DeviceError),
    Selected(Option<DeviceError>),
    Scanned(PathBuf),
    Completed(Option<DeviceError>),
    Removed,
}

// ---------------------------------------------------------------------------
// Main-thread state

struct Hub {
    _browser: Retained<ICDeviceBrowser>,
    _delegate: Retained<Delegate>,
}

thread_local! {
    static HUB: RefCell<Option<Hub>> = const { RefCell::new(None) };
    static ENUMERATED: Cell<bool> = const { Cell::new(false) };
    static SCANNERS: RefCell<Vec<Retained<ICScannerDevice>>> = const { RefCell::new(Vec::new()) };
    /// Per-device delegates, keyed by device id. ICA holds delegates weakly.
    static OPEN: RefCell<HashMap<String, Retained<Delegate>>> = RefCell::new(HashMap::new());
}

/// Run `f` on the main thread and wait for its result.
fn on_main<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    assert!(
        MainThreadMarker::new().is_none(),
        "backend-ica must be called from a background thread; the main thread runs ICA callbacks"
    );
    let (tx, rx) = mpsc::channel();
    DispatchQueue::main().exec_async(move || {
        let _ = tx.send(f());
    });
    rx.recv().expect("main queue dropped the reply; is the main run loop running?")
}

fn find_scanner(id: &str) -> Option<Retained<ICScannerDevice>> {
    SCANNERS.with_borrow(|list| list.iter().find(|s| device_key(s) == id).cloned())
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// Serial number when ICA knows it (USB), otherwise ICA's own persistent id.
fn device_key(dev: &ICDevice) -> String {
    unsafe {
        dev.serialNumberString()
            .filter(|s| s.length() > 0)
            .or_else(|| dev.persistentIDString())
            .or_else(|| dev.UUIDString())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

fn device_info(dev: &ICScannerDevice) -> DeviceInfo {
    unsafe {
        let usb = dev
            .transportType()
            .is_some_and(|t| t.isEqualToString(ICTransportTypeUSB));
        let serial = dev.serialNumberString().map(|s| s.to_string()).filter(|s| !s.is_empty());
        DeviceInfo {
            id: DeviceId(device_key(dev)),
            name: dev.name().map(|s| s.to_string()).unwrap_or_else(|| "Scanner".into()),
            serial,
            connection: if usb { Connection::Usb } else { Connection::Network },
            backend: "ica".into(),
        }
    }
}

fn map_error(err: &NSError) -> DeviceError {
    let text = err.localizedDescription().to_string();
    let lower = text.to_lowercase();
    if lower.contains("busy") || lower.contains("in use") {
        DeviceError::Busy
    } else if lower.contains("jam") {
        DeviceError::Jam
    } else if lower.contains("cover") {
        DeviceError::CoverOpen
    } else if lower.contains("no paper") || lower.contains("no document") {
        DeviceError::NoPaper
    } else {
        DeviceError::Other(format!("{text} (code {})", err.code()))
    }
}

// ---------------------------------------------------------------------------
// Delegate: one class serves the browser and each open device.

struct Ivars {
    tx: Option<Sender<Msg>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "JanusScanIcaDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl ICDeviceBrowserDelegate for Delegate {
        #[unsafe(method(deviceBrowser:didAddDevice:moreComing:))]
        fn did_add(&self, _browser: &ICDeviceBrowser, device: &ICDevice, _more: bool) {
            if let Ok(scanner) = device.retain().downcast::<ICScannerDevice>() {
                SCANNERS.with_borrow_mut(|list| list.push(scanner));
            }
        }

        #[unsafe(method(deviceBrowser:didRemoveDevice:moreGoing:))]
        fn did_remove_from_browser(&self, _browser: &ICDeviceBrowser, device: &ICDevice, _more: bool) {
            let key = device_key(device);
            SCANNERS.with_borrow_mut(|list| list.retain(|s| device_key(s) != key));
        }

        #[unsafe(method(deviceBrowserDidEnumerateLocalDevices:))]
        fn did_enumerate(&self, _browser: &ICDeviceBrowser) {
            ENUMERATED.set(true);
        }
    }

    unsafe impl ICDeviceDelegate for Delegate {
        #[unsafe(method(device:didOpenSessionWithError:))]
        fn did_open(&self, _device: &ICDevice, error: Option<&NSError>) {
            self.send(match error {
                Some(e) => Msg::OpenFailed(map_error(e)),
                None => Msg::Opened,
            });
        }

        #[unsafe(method(device:didCloseSessionWithError:))]
        fn did_close(&self, _device: &ICDevice, _error: Option<&NSError>) {}

        #[unsafe(method(didRemoveDevice:))]
        fn did_remove(&self, _device: &ICDevice) {
            self.send(Msg::Removed);
        }
    }

    unsafe impl ICScannerDeviceDelegate for Delegate {
        #[unsafe(method(scannerDeviceDidBecomeAvailable:))]
        fn did_become_available(&self, _scanner: &ICScannerDevice) {
            self.send(Msg::Ready);
        }

        #[unsafe(method(scannerDevice:didSelectFunctionalUnit:error:))]
        fn did_select(&self, _scanner: &ICScannerDevice, _unit: &ICScannerFunctionalUnit, error: Option<&NSError>) {
            self.send(Msg::Selected(error.map(map_error)));
        }

        #[unsafe(method(scannerDevice:didScanToURL:))]
        fn did_scan_to_url(&self, _scanner: &ICScannerDevice, url: &NSURL) {
            if let Some(path) = url.path() {
                self.send(Msg::Scanned(PathBuf::from(path.to_string())));
            }
        }

        #[unsafe(method(scannerDevice:didCompleteScanWithError:))]
        fn did_complete(&self, _scanner: &ICScannerDevice, error: Option<&NSError>) {
            self.send(Msg::Completed(error.map(map_error)));
        }
    }
);

impl Delegate {
    fn new(tx: Option<Sender<Msg>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { tx });
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, msg: Msg) {
        if std::env::var_os("JB_DEBUG").is_some() { eprintln!("[ica] {}", match &msg { Msg::Ready=>"ready", Msg::Opened=>"opened", Msg::OpenFailed(_)=>"open-failed", Msg::Selected(_)=>"selected", Msg::Scanned(_)=>"scanned", Msg::Completed(_)=>"completed", Msg::Removed=>"removed" }); }
        if let Some(tx) = &self.ivars().tx {
            let _ = tx.send(msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Backend

pub struct IcaBackend;

impl IcaBackend {
    /// Start browsing for local (USB) and Bonjour scanners.
    pub fn new() -> Self {
        on_main(|| {
            if HUB.with_borrow(Option::is_some) {
                return;
            }
            let delegate = Delegate::new(None);
            let browser = unsafe { ICDeviceBrowser::new() };
            let mask = ICDeviceTypeMask::Scanner.0
                | ICDeviceLocationTypeMask::Local.0
                | ICDeviceLocationTypeMask::Shared.0
                | ICDeviceLocationTypeMask::Bonjour.0;
            unsafe {
                browser.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                browser.setBrowsedDeviceTypeMask(ICDeviceTypeMask(mask));
            }
            HUB.set(Some(Hub { _browser: browser.clone(), _delegate: delegate }));
            unsafe { browser.start() };
        });
        Self
    }
}

impl Default for IcaBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for IcaBackend {
    fn name(&self) -> &'static str {
        "ica"
    }

    /// Waits briefly for the first local enumeration; Bonjour devices keep
    /// arriving after that, so callers poll this.
    fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let deadline = Instant::now() + ENUMERATE_TIMEOUT;
        while !on_main(|| ENUMERATED.get()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(on_main(|| {
            SCANNERS.with_borrow(|list| list.iter().map(|s| device_info(s)).collect())
        }))
    }

    fn open(&self, id: &DeviceId) -> Result<Box<dyn Device>> {
        IcaDevice::open(id.0.clone()).map(|d| Box::new(d) as Box<dyn Device>)
    }
}

// ---------------------------------------------------------------------------
// Device

struct IcaDevice {
    key: String,
    info: DeviceInfo,
    caps: Capabilities,
    selected: Source,
    rx: Receiver<Msg>,
}

fn wait<T>(rx: &Receiver<Msg>, timeout: Duration, mut pick: impl FnMut(Msg) -> Option<Result<T>>) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Removed) => return Err(DeviceError::Disconnected),
            Ok(Msg::OpenFailed(e)) => return Err(e),
            Ok(msg) => {
                if let Some(r) = pick(msg) {
                    return r;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(DeviceError::Other("The scanner didn't respond in time.".into()));
            }
            Err(RecvTimeoutError::Disconnected) => return Err(DeviceError::Disconnected),
        }
    }
}

fn unit_type(source: Source) -> ICScannerFunctionalUnitType {
    match source {
        Source::Feeder => ICScannerFunctionalUnitType::DocumentFeeder,
        Source::Flatbed => ICScannerFunctionalUnitType::Flatbed,
    }
}

fn index_set(set: &NSIndexSet) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = set.firstIndex();
    while i != usize::MAX && i as isize != isize::MAX {
        out.push(i as u32);
        i = set.indexGreaterThanIndex(i);
    }
    out
}

fn read_capabilities(scanner: &ICScannerDevice) -> Capabilities {
    unsafe {
        let types: Vec<usize> = scanner
            .availableFunctionalUnitTypes()
            .iter()
            .map(|n| n.unsignedIntegerValue())
            .collect();
        let mut sources = Vec::new();
        if types.contains(&ICScannerFunctionalUnitType::DocumentFeeder.0) {
            sources.push(Source::Feeder);
        }
        if types.contains(&ICScannerFunctionalUnitType::Flatbed.0) {
            sources.push(Source::Flatbed);
        }

        let unit = scanner.selectedFunctionalUnit();
        unit.setMeasurementUnit(ICScannerMeasurementUnit::Inches);
        let duplex = unit
            .clone()
            .downcast::<ICScannerFunctionalUnitDocumentFeeder>()
            .is_ok_and(|f| f.supportsDuplexScanning());

        let mut resolutions = index_set(&unit.preferredResolutions());
        if resolutions.is_empty() {
            resolutions = index_set(&unit.supportedResolutions());
        }
        if resolutions.len() > COMMON_DPI.len() {
            resolutions.retain(|r| COMMON_DPI.contains(r));
        }

        let size = unit.physicalSize();
        Capabilities {
            sources,
            duplex,
            resolutions,
            color_modes: vec![ColorMode::Color, ColorMode::Gray, ColorMode::Bw],
            max_size_mm: (
                (size.width * MM_PER_INCH) as f32,
                (size.height * MM_PER_INCH) as f32,
            ),
        }
    }
}

fn document_type(paper: PaperSize) -> ICScannerDocumentType {
    match paper {
        PaperSize::Letter => ICScannerDocumentType::TypeUSLetter,
        PaperSize::Legal => ICScannerDocumentType::TypeUSLegal,
        PaperSize::A4 => ICScannerDocumentType::TypeA4,
        PaperSize::A5 => ICScannerDocumentType::TypeA5,
        PaperSize::BusinessCard => ICScannerDocumentType::TypeBusinessCard,
        _ => ICScannerDocumentType::TypeDefault,
    }
}

fn nearest(values: &[u32], want: u32) -> u32 {
    values.iter().copied().min_by_key(|v| v.abs_diff(want)).unwrap_or(want)
}

/// Configure the selected unit for `req` and start scanning into `dir`.
fn start_scan(key: &str, req: &ScanRequest, caps: &Capabilities, dir: &Path) -> Result<u32> {
    let scanner = find_scanner(key).ok_or(DeviceError::Disconnected)?;
    unsafe {
        let unit = scanner.selectedFunctionalUnit();
        let dpi = nearest(&caps.resolutions, req.dpi);
        unit.setMeasurementUnit(ICScannerMeasurementUnit::Inches);
        unit.setResolution(dpi as usize);
        let (pixels, depth) = match req.color {
            ColorMode::Color => (ICScannerPixelDataType::RGB, ICScannerBitDepth::Depth8Bits),
            ColorMode::Gray => (ICScannerPixelDataType::Gray, ICScannerBitDepth::Depth8Bits),
            ColorMode::Bw => (ICScannerPixelDataType::BW, ICScannerBitDepth::Depth1Bit),
        };
        unit.setPixelDataType(pixels);
        unit.setBitDepth(depth);

        if let Ok(feeder) = unit.clone().downcast::<ICScannerFunctionalUnitDocumentFeeder>() {
            if !feeder.documentLoaded() {
                return Err(DeviceError::NoPaper);
            }
            feeder.setDocumentType(document_type(req.paper));
            feeder.setDuplexScanningEnabled(req.duplex && feeder.supportsDuplexScanning());
        }
        let (w, h) = req.paper.area_mm(caps);
        unit.setScanArea(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(w as f64 / MM_PER_INCH, h as f64 / MM_PER_INCH),
        ));

        scanner.setTransferMode(ICScannerTransferMode::FileBased);
        scanner.setDownloadsDirectory(&NSURL::fileURLWithPath(&ns(&dir.to_string_lossy())));
        scanner.setDocumentName(&ns("page"));
        scanner.setDocumentUTI(&ns("public.png"));
        scanner.requestScan();
        Ok(dpi)
    }
}

/// Private per-scan folder for the files ICA writes; removed after each pass.
fn scratch_dir() -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = std::env::temp_dir().join(format!(
        "janus-scan-{}-{}",
        std::process::id(),
        rand_seed()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| DeviceError::Other(format!("Can't create a temporary folder: {e}")))?;
    Ok(dir)
}

fn rand_seed() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

impl IcaDevice {
    fn open(key: String) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let k = key.clone();
        let info = on_main(move || -> Result<DeviceInfo> {
            let scanner = find_scanner(&k).ok_or(DeviceError::NotFound)?;
            let delegate = Delegate::new(Some(tx.clone()));
            unsafe {
                scanner.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            }
            OPEN.with_borrow_mut(|m| m.insert(k, delegate));
            unsafe {
                if scanner.hasOpenSession() {
                    let _ = tx.send(Msg::Ready);
                } else {
                    scanner.requestOpenSession();
                }
            }
            Ok(device_info(&scanner))
        })?;
        // Drivers differ: some report "available", others just select a unit
        // on their own once the session opens. Accept any of them, then let
        // stragglers arrive so they don't satisfy a later wait.
        wait(&rx, OPEN_TIMEOUT, |m| {
            matches!(m, Msg::Ready | Msg::Opened | Msg::Selected(None)).then_some(Ok(()))
        })?;
        let settle = Instant::now() + Duration::from_secs(1);
        while let Some(left) = settle.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Msg::Removed) => return Err(DeviceError::Disconnected),
                Ok(Msg::OpenFailed(e)) => return Err(e),
                Ok(_) => {}
                Err(_) => break,
            }
        }

        let mut dev = Self {
            key,
            info,
            caps: Capabilities {
                sources: vec![],
                duplex: false,
                resolutions: vec![],
                color_modes: vec![],
                max_size_mm: (0.0, 0.0),
            },
            selected: Source::Flatbed,
            rx,
        };
        let k = dev.key.clone();
        let (types, current) = on_main(move || {
            find_scanner(&k).map(|s| unsafe {
                let types = s
                    .availableFunctionalUnitTypes()
                    .iter()
                    .map(|n| n.unsignedIntegerValue())
                    .collect::<Vec<_>>();
                (types, s.selectedFunctionalUnit().r#type().0)
            })
        })
        .ok_or(DeviceError::Disconnected)?;
        let feeder = ICScannerFunctionalUnitType::DocumentFeeder.0;
        let first = if types.contains(&feeder) { Source::Feeder } else { Source::Flatbed };
        if unit_type(first).0 == current {
            dev.selected = first;
        } else {
            dev.select(first)?;
        }
        let k = dev.key.clone();
        dev.caps = on_main(move || find_scanner(&k).map(|s| read_capabilities(&s)))
            .ok_or(DeviceError::Disconnected)?;
        Ok(dev)
    }

    fn select(&mut self, source: Source) -> Result<()> {
        let k = self.key.clone();
        on_main(move || -> Result<()> {
            let s = find_scanner(&k).ok_or(DeviceError::Disconnected)?;
            unsafe { s.requestSelectFunctionalUnit(unit_type(source)) };
            Ok(())
        })?;
        wait(&self.rx, OPEN_TIMEOUT, |m| match m {
            Msg::Selected(None) => Some(Ok(())),
            Msg::Selected(Some(e)) => Some(Err(e)),
            _ => None,
        })?;
        self.selected = source;
        Ok(())
    }

    fn run_pass(&mut self, req: &ScanRequest, events: &Sender<ScanEvent>, cancel: &CancelToken) -> Result<ScanEvent> {
        if !self.caps.sources.contains(&req.source) {
            return Err(DeviceError::Other("This scanner doesn't have that paper source.".into()));
        }
        if self.selected != req.source {
            self.select(req.source)?;
        }
        let dir = scratch_dir()?;
        let result = self.collect_pages(req, events, cancel, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn collect_pages(&mut self, req: &ScanRequest, events: &Sender<ScanEvent>, cancel: &CancelToken, dir: &Path) -> Result<ScanEvent> {
        let (k, r, caps, d) = (self.key.clone(), req.clone(), self.caps.clone(), dir.to_path_buf());
        let dpi = on_main(move || start_scan(&k, &r, &caps, &d))?;

        let mut index = 0;
        let mut cancel_sent = false;
        loop {
            if cancel.is_cancelled() && !cancel_sent {
                cancel_sent = true;
                let k = self.key.clone();
                on_main(move || {
                    if let Some(s) = find_scanner(&k) {
                        unsafe { s.cancelScan() };
                    }
                });
            }
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Msg::Scanned(path)) => {
                    let _ = events.send(ScanEvent::PageStarted(index));
                    let image = image::open(&path)
                        .map_err(|e| DeviceError::Other(format!("Couldn't read the scanned page: {e}")));
                    let _ = std::fs::remove_file(&path);
                    let _ = events.send(ScanEvent::Page(Page { index, dpi, image: image? }));
                    index += 1;
                }
                Ok(Msg::Completed(err)) => {
                    return match (cancel_sent, err) {
                        (true, _) => Err(DeviceError::Cancelled),
                        (false, Some(e)) => Err(e),
                        // Some drivers finish an empty-feeder pass without an error.
                        (false, None) if req.source == Source::Feeder && index == 0 => {
                            Err(DeviceError::NoPaper)
                        }
                        (false, None) if req.source == Source::Feeder => Ok(ScanEvent::FeederEmpty),
                        (false, None) => Ok(ScanEvent::Done),
                    };
                }
                Ok(Msg::Removed) => return Err(DeviceError::Disconnected),
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(DeviceError::Disconnected),
            }
        }
    }
}

impl Device for IcaDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn scan(&mut self, req: &ScanRequest, events: Sender<ScanEvent>, cancel: CancelToken) {
        let last = self.run_pass(req, &events, &cancel).unwrap_or_else(ScanEvent::Error);
        let _ = events.send(last);
    }
}

impl Drop for IcaDevice {
    fn drop(&mut self) {
        // Fire and forget: Drop may run anywhere, so never block on the main queue.
        let key = self.key.clone();
        DispatchQueue::main().exec_async(move || {
            if let Some(s) = find_scanner(&key) {
                unsafe { s.requestCloseSession() };
            }
            OPEN.with_borrow_mut(|m| m.remove(&key));
        });
    }
}

// ---------------------------------------------------------------------------

/// Run `work` on a background thread while the main thread services ICA
/// callbacks; exits the process with `work`'s return code. For CLIs only; GUI
/// apps already run the main loop.
pub fn run_with_main_loop(work: impl FnOnce() -> i32 + Send + 'static) -> ! {
    std::thread::spawn(move || std::process::exit(work()));
    let run_loop = NSRunLoop::mainRunLoop();
    loop {
        let ran = unsafe { run_loop.runMode_beforeDate(NSDefaultRunLoopMode, &NSDate::distantFuture()) };
        if !ran {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
