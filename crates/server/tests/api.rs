//! API tests against an in-memory fake scanner, so they run anywhere.

mod common;

use std::sync::mpsc::Sender;
use std::time::Duration;

use axum::Router;
use axum::http::{Method, StatusCode};
use common::{Events, call, json_call, kinds, scan_body};
use image::{DynamicImage, Rgb, RgbImage};
use janus_scan_server::{AppState, Config, router};
use scan_core::{
    Backend, CancelToken, Capabilities, ColorMode, Connection, Device, DeviceError, DeviceId, DeviceInfo, Page,
    ScanEvent, ScanRequest, Source,
};
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Fake scanner

#[derive(Clone, Copy)]
struct Fake {
    pages: u32,
    /// Keep the pass running until it's cancelled.
    hold: bool,
    present: bool,
}

fn info() -> DeviceInfo {
    DeviceInfo {
        id: DeviceId("fake:0".into()),
        name: "Fake ADS-1000W".into(),
        serial: None,
        connection: Connection::Network,
        backend: "fake".into(),
    }
}

struct FakeDevice {
    cfg: Fake,
    info: DeviceInfo,
    caps: Capabilities,
}

impl Backend for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn devices(&self) -> scan_core::Result<Vec<DeviceInfo>> {
        Ok(if self.present { vec![info()] } else { vec![] })
    }
    fn open(&self, _id: &DeviceId) -> scan_core::Result<Box<dyn Device>> {
        if !self.present {
            return Err(DeviceError::NotFound);
        }
        Ok(Box::new(FakeDevice {
            cfg: *self,
            info: info(),
            caps: Capabilities {
                sources: vec![Source::Feeder],
                duplex: true,
                resolutions: vec![150, 300],
                color_modes: vec![ColorMode::Color, ColorMode::Gray],
                max_size_mm: (215.9, 812.8),
            },
        }))
    }
}

impl Device for FakeDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn scan(&mut self, req: &ScanRequest, events: Sender<ScanEvent>, cancel: CancelToken) {
        for index in 0..self.cfg.pages {
            let _ = events.send(ScanEvent::PageStarted(index));
            // Dark marks on white so the page isn't blank.
            let mut img = RgbImage::from_pixel(60, 80, Rgb([255, 255, 255]));
            for x in 10..50 {
                img.put_pixel(x, 20 + index, Rgb([0, 0, 0]));
            }
            let _ = events.send(ScanEvent::Page(Page { index, dpi: req.dpi, image: DynamicImage::ImageRgb8(img) }));
        }
        while self.cfg.hold && !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = events.send(if cancel.is_cancelled() {
            ScanEvent::Error(DeviceError::Cancelled)
        } else {
            ScanEvent::FeederEmpty
        });
    }
}

fn app(cfg: Fake) -> Router {
    router(AppState::new(Box::new(cfg), None, Default::default()).unwrap())
}

fn app_with_config(yaml: &str) -> Router {
    router(AppState::new(Box::new(NORMAL), None, Config::parse(yaml).unwrap()).unwrap())
}

/// Scan one pass of the fake scanner and wait until it's stored.
async fn scan_once(app: &Router) {
    let mut ev = Events::open(app).await;
    assert_eq!(call(app, Method::POST, "/api/scan", Some(scan_body())).await.0, StatusCode::ACCEPTED);
    ev.scan_pass().await;
    ev.idle().await;
}

/// A fresh empty folder standing in for a NAS share.
fn share(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("janus-scan-share-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

const NORMAL: Fake = Fake { pages: 3, hold: false, present: true };

// ---------------------------------------------------------------------------
// Tests

#[tokio::test]
async fn scanner_status_reports_device_and_capabilities() {
    let app = app(NORMAL);
    let (status, v) = json_call(&app, Method::GET, "/api/scanner", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["device"]["name"], "Fake ADS-1000W");
    assert_eq!(v["capabilities"]["resolutions"], json!([150, 300]));
    assert_eq!(v["scanning"], false);
}

#[tokio::test]
async fn new_browser_gets_current_state_first() {
    let app = app(NORMAL);
    let mut ev = Events::open(&app).await;
    assert_eq!(ev.next().await, ("state".into(), json!({ "scanning": false })));
    assert_eq!(ev.next().await, ("pages".into(), json!([])));
}

#[tokio::test]
async fn scan_adds_pages_to_the_shared_document() {
    let app = app(NORMAL);
    let mut ev = Events::open(&app).await;
    let mut other = Events::open(&app).await; // a second browser sees the same pass

    let (status, _) = call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pass = ev.scan_pass().await;
    assert_eq!(kinds(&pass), ["page", "page", "page", "feeder-empty"]);
    assert_eq!(kinds(&other.scan_pass().await), kinds(&pass));
    ev.idle().await;

    let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
    assert_eq!(pages.as_array().unwrap().len(), 3);
    assert_eq!(pages[0], pass[0]["page"]);

    let (status, jpeg) = call(&app, Method::GET, pages[0]["thumb"].as_str().unwrap(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(image::load_from_memory(&jpeg).unwrap().width(), 60);

    let (status, jpeg) = call(&app, Method::GET, &format!("/api/pages/{}/preview", pages[0]["id"]), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(image::load_from_memory(&jpeg).is_ok());

    // A second pass appends to the same document.
    call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    ev.scan_pass().await;
    ev.idle().await;
    let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
    assert_eq!(pages.as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn only_one_pass_at_a_time() {
    let app = app(Fake { hold: true, ..NORMAL });
    let mut ev = Events::open(&app).await;

    assert_eq!(call(&app, Method::POST, "/api/scan", Some(scan_body())).await.0, StatusCode::ACCEPTED);
    assert_eq!(ev.next_named("state").await, json!({ "scanning": false })); // the hello
    assert_eq!(ev.next_named("state").await, json!({ "scanning": true }));

    let (status, v) = json_call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(v["error"].as_str().unwrap().contains("in use"));

    // Status doesn't wait for the pass to finish.
    let (status, v) =
        tokio::time::timeout(Duration::from_secs(2), json_call(&app, Method::GET, "/api/scanner", None)).await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["scanning"], true);

    assert_eq!(call(&app, Method::POST, "/api/scan/cancel", None).await.0, StatusCode::NO_CONTENT);
    let pass = ev.scan_pass().await;
    assert_eq!(pass.last().unwrap()["error"]["kind"], "cancelled");
    ev.idle().await;

    // The lock is free again; pages scanned before the cancel are kept.
    let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
    assert_eq!(pages.as_array().unwrap().len(), 3);
    assert_eq!(call(&app, Method::POST, "/api/scan", Some(scan_body())).await.0, StatusCode::ACCEPTED);
    call(&app, Method::POST, "/api/scan/cancel", None).await;
}

#[tokio::test]
async fn page_edits_reach_every_browser() {
    let app = app(NORMAL);
    let mut ev = Events::open(&app).await;
    call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    ev.scan_pass().await;
    ev.idle().await;
    let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
    let ids: Vec<u64> = pages.as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()).collect();

    // Rotate: dimensions swap and the thumbnail URL changes.
    let (status, p) = json_call(&app, Method::PATCH, &format!("/api/pages/{}", ids[0]), Some(json!({ "rotate": "cw" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((p["width"].as_u64(), p["height"].as_u64()), (Some(80), Some(60)));
    assert_ne!(p["thumb"], pages[0]["thumb"]);
    let pushed = ev.next_named("pages").await;
    assert_eq!(pushed[0], p);

    // Reorder.
    let order = json!([ids[2], ids[0], ids[1]]);
    assert_eq!(call(&app, Method::PUT, "/api/pages/order", Some(order)).await.0, StatusCode::NO_CONTENT);
    let pushed = ev.next_named("pages").await;
    assert_eq!(pushed[0]["id"].as_u64(), Some(ids[2]));
    let stale = json!([ids[0], ids[1]]);
    assert_eq!(call(&app, Method::PUT, "/api/pages/order", Some(stale)).await.0, StatusCode::CONFLICT);

    // Delete one, then a missing one.
    let uri = format!("/api/pages/{}", ids[1]);
    assert_eq!(call(&app, Method::DELETE, &uri, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(ev.next_named("pages").await.as_array().unwrap().len(), 2);
    assert_eq!(call(&app, Method::DELETE, &uri, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, Method::GET, &format!("{uri}/thumb"), None).await.0, StatusCode::NOT_FOUND);

    // Clear.
    assert_eq!(call(&app, Method::DELETE, "/api/pages", None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(ev.next_named("pages").await, json!([]));
}

#[tokio::test]
async fn missing_scanner_is_reported_and_releases_the_lock() {
    let app = app(Fake { present: false, ..NORMAL });
    assert_eq!(call(&app, Method::GET, "/api/scanner", None).await.0, StatusCode::SERVICE_UNAVAILABLE);
    let (status, v) = json_call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"], "Scanner not found.");
    // Not stuck "in use".
    assert_eq!(call(&app, Method::POST, "/api/scan", Some(scan_body())).await.0, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn serves_the_web_front_end_but_not_for_api_typos() {
    let dir = std::env::temp_dir().join(format!("janus-scan-web-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html>app").unwrap();
    std::fs::write(dir.join("manifest.webmanifest"), "{}").unwrap();
    let app = janus_scan_server::with_web(app(NORMAL), &dir);

    for path in ["/", "/some/app/route"] {
        let (status, body) = call(&app, Method::GET, path, None).await;
        assert_eq!((status, body.as_slice()), (StatusCode::OK, b"<!doctype html>app".as_slice()), "{path}");
    }
    assert_eq!(call(&app, Method::GET, "/manifest.webmanifest", None).await, (StatusCode::OK, b"{}".to_vec()));
    let (status, v) = json_call(&app, Method::GET, "/api/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(v["error"].is_string());
    // The API still wins over the files.
    assert_eq!(json_call(&app, Method::GET, "/api/pages", None).await, (StatusCode::OK, json!([])));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Saving

async fn save_call(app: &Router, body: Value) -> axum::response::Response {
    let req = axum::http::Request::post("/api/save")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes().to_vec()
}

#[tokio::test]
async fn download_pdf() {
    let app = app(NORMAL);
    scan_once(&app).await;
    let resp = save_call(&app, json!({ "name_template": "Scan {n}", "format": "pdf" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/pdf");
    let disposition = resp.headers()["content-disposition"].to_str().unwrap().to_string();
    assert!(disposition.contains("filename=\"Scan 001.pdf\""), "{disposition}");
    assert!(body_bytes(resp).await.starts_with(b"%PDF"));
}

#[tokio::test]
async fn download_one_jpeg_per_page_as_zip() {
    let app = app(NORMAL);
    scan_once(&app).await;
    let resp = save_call(&app, json!({ "name_template": "Scan {n}", "format": "jpeg" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/zip");
    assert!(resp.headers()["content-disposition"].to_str().unwrap().contains("filename=\"Scan 001.zip\""));
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(body_bytes(resp).await)).unwrap();
    let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();
    assert_eq!(names, ["Scan 001_001.jpg", "Scan 001_002.jpg", "Scan 001_003.jpg"]);
}

#[tokio::test]
async fn save_to_nas_folders() {
    let receipts = share("receipts");
    let taxes = share("taxes");
    let yaml = format!(
        "destinations:\n  - name: Receipts\n    path: {}\n    name_template: \"Receipts {{n}}\"\n  - name: Taxes 2026\n    path: {}\n",
        receipts.display(),
        taxes.display()
    );
    let app = app_with_config(&yaml);

    // Names only, never paths.
    let (_, list) = json_call(&app, Method::GET, "/api/destinations", None).await;
    assert_eq!(list, json!([{ "name": "Receipts", "name_template": "Receipts {n}" }, { "name": "Taxes 2026", "name_template": null }]));

    let (status, v) = json_call(&app, Method::POST, "/api/save", Some(json!({ "destination": "Receipts", "name_template": "x", "format": "pdf" }))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"], "There are no pages to save.");

    scan_once(&app).await;
    // The folder's own preset wins over the browser's template; {n} counts up.
    for want in ["Receipts 001.pdf", "Receipts 002.pdf"] {
        let (status, v) = json_call(&app, Method::POST, "/api/save", Some(json!({ "destination": "Receipts", "name_template": "ignored", "format": "pdf" }))).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v, json!({ "files": [want] }));
    }
    assert_eq!(files_in(&receipts), ["Receipts 001.pdf", "Receipts 002.pdf"]);

    // A template can't climb out of the folder.
    let (status, v) = json_call(&app, Method::POST, "/api/save", Some(json!({ "destination": "Taxes 2026", "name_template": "../../escape", "format": "pdf" }))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(files_in(&taxes), [v["files"][0].as_str().unwrap()]);
    assert!(!taxes.parent().unwrap().join("escape.pdf").exists());

    let (status, v) = json_call(&app, Method::POST, "/api/save", Some(json!({ "destination": "Nope", "name_template": "x", "format": "pdf" }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");

    // An unmounted share is reported, not created.
    std::fs::remove_dir_all(&taxes).unwrap();
    let (status, v) = json_call(&app, Method::POST, "/api/save", Some(json!({ "destination": "Taxes 2026", "name_template": "x", "format": "pdf" }))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(v["error"].as_str().unwrap().contains("isn't available"), "{v}");
    assert!(!taxes.exists());

    let (status, size) = json_call(&app, Method::POST, "/api/estimate", Some(json!({ "format": "pdf" }))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(size.as_u64().unwrap() > 0);
    let _ = std::fs::remove_dir_all(&receipts);
}

#[tokio::test]
async fn shutting_down_ends_event_streams() {
    let state = AppState::new(Box::new(NORMAL), None, Default::default()).unwrap();
    let app = router(state.clone());
    let resp = app.oneshot(axum::http::Request::get("/api/events").body(axum::body::Body::empty()).unwrap()).await.unwrap();
    state.close();
    // The whole body completes (the hello events), instead of staying open.
    let body = tokio::time::timeout(Duration::from_secs(5), http_body_util::BodyExt::collect(resp.into_body()))
        .await
        .expect("event stream still open after close")
        .unwrap();
    assert!(String::from_utf8_lossy(&body.to_bytes()).contains("event: state"));
}

// ---------------------------------------------------------------------------
// Idle timeout

fn idle_app(cfg: Fake, timeout_ms: u64) -> Router {
    let state = AppState::new(Box::new(cfg), None, Default::default()).unwrap();
    state.start_idle_timeout(Duration::from_millis(timeout_ms));
    router(state)
}

async fn page_count(app: &Router) -> usize {
    json_call(app, Method::GET, "/api/pages", None).await.1.as_array().unwrap().len()
}

#[tokio::test]
async fn idle_pages_are_removed_and_everyone_is_told() {
    let app = idle_app(NORMAL, 300);
    let mut ev = Events::open(&app).await;
    call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    ev.scan_pass().await;
    ev.idle().await;
    assert_eq!(page_count(&app).await, 3);

    assert_eq!(ev.next_named("pages").await, json!([]));
    assert_eq!(ev.next_named("expired").await, json!({ "minutes": 0 }));
    assert_eq!(page_count(&app).await, 0);
}

#[tokio::test]
async fn activity_keeps_the_document() {
    let app = idle_app(NORMAL, 400);
    scan_once(&app).await;
    let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
    let first = format!("/api/pages/{}", pages[0]["id"]);
    // Well past the timeout in total, but never idle for that long.
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        call(&app, Method::PATCH, &first, Some(json!({ "rotate": "cw" }))).await;
    }
    assert_eq!(page_count(&app).await, 3);
    // Looking at the list or keeping the page open isn't activity.
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        page_count(&app).await;
    }
    assert_eq!(page_count(&app).await, 0);
}

#[tokio::test]
async fn never_removed_during_a_pass() {
    let app = idle_app(Fake { hold: true, ..NORMAL }, 100);
    let mut ev = Events::open(&app).await;
    call(&app, Method::POST, "/api/scan", Some(scan_body())).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(page_count(&app).await, 3, "pages removed while the pass was still running");
    call(&app, Method::POST, "/api/scan/cancel", None).await;
    ev.scan_pass().await;
    ev.idle().await;
    // Idle from the end of the pass on.
    assert_eq!(ev.next_named("expired").await, json!({ "minutes": 0 }));
}
