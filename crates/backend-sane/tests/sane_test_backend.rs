//! Integration tests against SANE's virtual `test` backend (`test:0`).
//!
//! Skipped (with a printed note) when libsane can't be loaded or `test:0`
//! isn't listed. Unless `SANE_CONFIG_DIR` is already set, the tests point SANE
//! at a temporary config that enables only the `test` backend, so they also
//! run where `test` is commented out in the system `dll.conf` (and don't probe
//! real USB or network scanners).
//!
//! The test backend's feeder returns SANE_STATUS_NO_DOCS on every 11th
//! `sane_start` after opening, so a fresh device yields 10 pages per pass.

use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, Once};
use std::thread;

use backend_sane::{OptionValue, SaneBackend, SaneDevice};
use scan_core::{
    Backend, CancelToken, ColorMode, Device, DeviceError, DeviceId, PaperSize, ScanEvent, ScanRequest,
    Source,
};

/// `test:0` can only be opened once at a time, so tests run one by one.
static SERIAL: Mutex<()> = Mutex::new(());

fn setup() -> Option<(MutexGuard<'static, ()>, SaneDevice)> {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    static CONFIG: Once = Once::new();
    CONFIG.call_once(|| {
        if std::env::var_os("SANE_CONFIG_DIR").is_none() {
            let dir = std::env::temp_dir().join(format!("janus-scan-sane-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("dll.conf"), "test\n").unwrap();
            // Trailing ':' keeps the default config dirs as a fallback.
            // SAFETY: runs once, under SERIAL, before libsane is initialised.
            unsafe { std::env::set_var("SANE_CONFIG_DIR", format!("{}:", dir.display())) };
        }
    });
    let backend = match SaneBackend::new() {
        Ok(b) => b,
        Err(e) => {
            println!("SKIP: libsane not available: {e}");
            return None;
        }
    };
    let id = DeviceId("test:0".into());
    match backend.devices() {
        Ok(list) if list.iter().any(|d| d.id == id) => {}
        other => {
            println!("SKIP: SANE test:0 not listed ({other:?})");
            return None;
        }
    }
    let mut dev = backend.open_device(&id).expect("open test:0");
    // The test backend keeps option values across close/open within a
    // process, so undo anything another test may have changed.
    for (name, v) in [
        ("hand-scanner", OptionValue::Bool(false)),
        ("read-delay", OptionValue::Bool(false)),
        ("mode", OptionValue::Text("Color".into())),
        ("three-pass", OptionValue::Bool(false)),
        ("mode", OptionValue::Text("Gray".into())),
        ("depth", OptionValue::Number(8.0)),
    ] {
        dev.set_option(name, v).expect("reset option");
    }
    Some((guard, dev))
}

/// Run a scan on a worker thread and collect every event.
fn scan_all(dev: &mut SaneDevice, req: &ScanRequest, on_event: impl Fn(&ScanEvent, &CancelToken)) -> Vec<ScanEvent> {
    let (tx, rx) = mpsc::channel();
    let cancel = CancelToken::default();
    thread::scope(|s| {
        let c = cancel.clone();
        s.spawn(move || dev.scan(req, tx, c));
        let mut out = Vec::new();
        for ev in rx {
            on_event(&ev, &cancel);
            out.push(ev);
        }
        out
    })
}

fn pages(events: &[ScanEvent]) -> Vec<&scan_core::Page> {
    events
        .iter()
        .filter_map(|e| match e {
            ScanEvent::Page(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn req(source: Source, color: ColorMode, dpi: u32, paper: PaperSize) -> ScanRequest {
    ScanRequest { source, duplex: false, dpi, color, paper }
}

#[test]
fn capabilities_from_test_backend() {
    let Some((_g, dev)) = setup() else { return };
    let caps = dev.capabilities();
    println!("test:0 capabilities: {caps:?}");
    assert!(caps.sources.contains(&Source::Feeder));
    assert!(caps.sources.contains(&Source::Flatbed));
    assert!(!caps.duplex);
    assert!(caps.resolutions.contains(&300));
    assert!(caps.color_modes.contains(&ColorMode::Color));
    assert!(caps.color_modes.contains(&ColorMode::Gray));
    assert!(caps.max_size_mm.0 > 100.0 && caps.max_size_mm.1 > 100.0);
    assert_eq!(dev.info().backend, "sane");
}

#[test]
fn feeder_pass_ends_with_feeder_empty() {
    let Some((_g, mut dev)) = setup() else { return };
    let events = scan_all(&mut dev, &req(Source::Feeder, ColorMode::Gray, 75, PaperSize::A5), |_, _| {});

    // PageStarted(i), Page(i) for each of the 10 pages, then FeederEmpty.
    assert_eq!(events.len(), 21, "{events:?}");
    for i in 0..10u32 {
        assert!(matches!(events[2 * i as usize], ScanEvent::PageStarted(n) if n == i));
        match &events[2 * i as usize + 1] {
            ScanEvent::Page(p) => {
                assert_eq!(p.index, i);
                assert_eq!(p.dpi, 75);
                let img = p.image.as_luma8().expect("gray page");
                // A5 is 148 x 210 mm; the test device's 200 mm limit clamps the height.
                let (w, h) = img.dimensions();
                assert!((w as i32 - 437).abs() <= 2 && (h as i32 - 590).abs() <= 2, "{w}x{h}");
            }
            other => panic!("expected page {i}, got {other:?}"),
        }
    }
    assert!(matches!(events[20], ScanEvent::FeederEmpty));

    // "Scan more": the next pass on the same device works again.
    let again = scan_all(&mut dev, &req(Source::Feeder, ColorMode::Gray, 75, PaperSize::BusinessCard), |_, _| {});
    assert_eq!(pages(&again).len(), 10);
    assert!(matches!(again.last(), Some(ScanEvent::FeederEmpty)));
}

#[test]
fn cancel_stops_the_pass() {
    let Some((_g, mut dev)) = setup() else { return };
    // Slow reads down so cancel lands mid-page.
    dev.set_option("read-delay", OptionValue::Bool(true)).unwrap();
    dev.set_option("read-delay-duration", OptionValue::Number(20_000.0)).unwrap();
    let events = scan_all(&mut dev, &req(Source::Feeder, ColorMode::Gray, 150, PaperSize::A4), |ev, cancel| {
        if matches!(ev, ScanEvent::PageStarted(1)) {
            cancel.cancel();
        }
    });
    assert!(pages(&events).len() < 10, "{} pages", pages(&events).len());
    assert!(
        matches!(events.last(), Some(ScanEvent::Error(DeviceError::Cancelled))),
        "{:?}",
        events.last()
    );

    // The device is usable afterwards.
    dev.set_option("read-delay", OptionValue::Bool(false)).unwrap();
    let after = scan_all(&mut dev, &req(Source::Flatbed, ColorMode::Gray, 75, PaperSize::A5), |_, _| {});
    assert_eq!(pages(&after).len(), 1);
    assert!(matches!(after.last(), Some(ScanEvent::Done)));
}

#[test]
fn flatbed_single_page_color() {
    let Some((_g, mut dev)) = setup() else { return };
    let events = scan_all(&mut dev, &req(Source::Flatbed, ColorMode::Color, 100, PaperSize::Letter), |_, _| {});
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(events[0], ScanEvent::PageStarted(0)));
    let p = pages(&events)[0];
    let img = p.image.as_rgb8().expect("RGB page");
    // Letter width 215.9 mm, clamped to the device's maximum.
    let max_w = dev.capabilities().max_size_mm.0.min(215.9);
    let want = (max_w / 25.4 * 100.0) as i32;
    assert!((img.width() as i32 - want).abs() <= 2, "{} vs {want}", img.width());
    assert!(matches!(events[2], ScanEvent::Done));
}

#[test]
fn bw_without_lineart_scans_gray() {
    let Some((_g, mut dev)) = setup() else { return };
    let events = scan_all(&mut dev, &req(Source::Flatbed, ColorMode::Bw, 75, PaperSize::BusinessCard), |_, _| {});
    assert!(pages(&events)[0].image.as_luma8().is_some());
    assert!(matches!(events.last(), Some(ScanEvent::Done)));
}

#[test]
fn gray_depths_and_unknown_length() {
    let Some((_g, mut dev)) = setup() else { return };
    let r = req(Source::Flatbed, ColorMode::Gray, 75, PaperSize::BusinessCard);
    for depth in [1.0, 8.0, 16.0] {
        dev.set_option("mode", OptionValue::Text("Gray".into())).unwrap();
        assert!(dev.set_option("depth", OptionValue::Number(depth)).unwrap());
        let events = scan_all(&mut dev, &r, |_, _| {});
        let img = pages(&events).first().unwrap_or_else(|| panic!("depth {depth}: {events:?}")).image.as_luma8().expect("gray").clone();
        // The default test picture is solid black; at 1 bit SANE sends 1 = black.
        assert!(img.pixels().all(|p| p.0[0] == 0), "depth {depth}: not all black");
        assert!(matches!(events.last(), Some(ScanEvent::Done)), "depth {depth}");
    }
    dev.set_option("depth", OptionValue::Number(8.0)).unwrap();

    // Hand-scanner mode reports lines == -1; read until EOF.
    assert!(dev.set_option("hand-scanner", OptionValue::Bool(true)).unwrap());
    let events = scan_all(&mut dev, &r, |_, _| {});
    let img = pages(&events).first().unwrap_or_else(|| panic!("{events:?}")).image.as_luma8().expect("gray");
    assert!(img.height() > 0 && img.width() > 0, "{:?}", img.dimensions());
    assert!(matches!(events.last(), Some(ScanEvent::Done)));
}

#[test]
fn three_pass_color() {
    let Some((_g, mut dev)) = setup() else { return };
    dev.set_option("mode", OptionValue::Text("Color".into())).unwrap();
    if !dev.set_option("three-pass", OptionValue::Bool(true)).unwrap() {
        println!("SKIP: test backend has no three-pass option");
        return;
    }
    let events = scan_all(&mut dev, &req(Source::Flatbed, ColorMode::Color, 75, PaperSize::BusinessCard), |_, _| {});
    let img = pages(&events).first().unwrap_or_else(|| panic!("{events:?}")).image.as_rgb8().expect("RGB page");
    assert!(img.width() > 0 && img.height() > 0);
    assert!(matches!(events.last(), Some(ScanEvent::Done)), "{:?}", events.last());
}
