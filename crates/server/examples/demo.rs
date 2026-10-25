//! Demo server for screenshots and recordings: the real server and web front
//! end with a pretend scanner that "scans" image files instead of paper.
//!
//!   cargo run --release -p janus-scan-server --example demo -- <pages-dir> [port]
//!
//! `<pages-dir>` holds one folder per pass (`pass-1`, `pass-2`, ...), each
//! with that pass's page images in name order; passes repeat after the last.
//! `scripts/showcase/pages.mjs` makes a set of made-up sample documents.
//! Pages arrive at about the real scanner's speed (`DEMO_PAGE_MS`, default
//! 900). Saving offers two demo folders in a temporary directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use janus_scan_server::{AppState, Config, router, with_web};
use scan_core::{
    Backend, CancelToken, Capabilities, ColorMode, Connection, Device, DeviceError, DeviceId, DeviceInfo, Page,
    ScanEvent, ScanRequest, Source,
};

struct Demo {
    passes: Vec<Vec<PathBuf>>,
    next: Arc<AtomicUsize>,
    page_ms: u64,
}

fn info() -> DeviceInfo {
    DeviceInfo {
        id: DeviceId("demo".into()),
        name: "Brother ADS-1000W".into(),
        serial: None,
        connection: Connection::Network,
        backend: "demo".into(),
    }
}

impl Backend for Demo {
    fn name(&self) -> &'static str {
        "demo"
    }
    fn devices(&self) -> scan_core::Result<Vec<DeviceInfo>> {
        Ok(vec![info()])
    }
    fn open(&self, _: &DeviceId) -> scan_core::Result<Box<dyn Device>> {
        Ok(Box::new(DemoDevice {
            passes: self.passes.clone(),
            next: self.next.clone(),
            page_ms: self.page_ms,
            info: info(),
            caps: Capabilities {
                sources: vec![Source::Feeder],
                duplex: true,
                resolutions: vec![100, 150, 200, 300, 400, 600],
                color_modes: vec![ColorMode::Color, ColorMode::Gray, ColorMode::Bw],
                max_size_mm: (215.9, 812.8),
            },
        }))
    }
}

struct DemoDevice {
    passes: Vec<Vec<PathBuf>>,
    next: Arc<AtomicUsize>,
    page_ms: u64,
    info: DeviceInfo,
    caps: Capabilities,
}

impl Device for DemoDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn scan(&mut self, req: &ScanRequest, events: Sender<ScanEvent>, cancel: CancelToken) {
        let pass = &self.passes[self.next.fetch_add(1, Ordering::Relaxed) % self.passes.len()];
        for (index, path) in pass.iter().enumerate() {
            let index = index as u32;
            let _ = events.send(ScanEvent::PageStarted(index));
            std::thread::sleep(Duration::from_millis(self.page_ms));
            if cancel.is_cancelled() {
                let _ = events.send(ScanEvent::Error(DeviceError::Cancelled));
                return;
            }
            match image::open(path) {
                Ok(image) => {
                    let _ = events.send(ScanEvent::Page(Page { index, dpi: req.dpi, image }));
                }
                Err(e) => {
                    let _ = events.send(ScanEvent::Error(DeviceError::Other(format!("{}: {e}", path.display()))));
                    return;
                }
            }
        }
        let _ = events.send(ScanEvent::FeederEmpty);
    }
}

fn images_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("png" | "jpg" | "jpeg")));
    files.sort();
    files
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: demo <pages-dir> [port]"));
    let port: u16 = args.next().map(|p| p.parse().expect("port")).unwrap_or(8090);
    let mut pass_dirs: Vec<PathBuf> =
        std::fs::read_dir(&dir).expect("pages dir").flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    pass_dirs.sort();
    let passes: Vec<Vec<PathBuf>> = pass_dirs.iter().map(|d| images_in(d)).filter(|p| !p.is_empty()).collect();
    assert!(!passes.is_empty(), "no pass-* folders with images in {}", dir.display());
    let page_ms = std::env::var("DEMO_PAGE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(900);

    let nas = std::env::temp_dir().join("janus-scan-demo-nas");
    for f in ["receipts", "taxes"] {
        std::fs::create_dir_all(nas.join(f)).unwrap();
    }
    let config = Config::parse(&format!(
        "destinations:\n  - name: Receipts\n    path: {0}/receipts\n    name_template: \"Receipts {{date}} {{n}}\"\n  - name: Taxes 2026\n    path: {0}/taxes\n",
        nas.display()
    ))
    .unwrap();

    let backend = Demo { passes, next: Arc::default(), page_ms };
    let state = AppState::new(Box::new(backend), None, config).unwrap();
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/build-web");
    let app = with_web(router(state), &web);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    eprintln!("demo: http://127.0.0.1:{port} (saves to {})", nas.display());
    axum::serve(listener, app).await.unwrap();
}
