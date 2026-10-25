//! End to end through SANE's virtual `test` scanner (`test:0`): the same path
//! the real ADS-1000W takes, minus Brother's driver.
//!
//! Skipped (with a printed note) when libsane can't be loaded or `test:0`
//! isn't listed. Unless `SANE_CONFIG_DIR` is already set, SANE is pointed at a
//! temporary config that enables only the `test` backend, so real USB or
//! network scanners are never probed. The test feeder gives 10 pages per pass.

mod common;

use axum::http::{Method, StatusCode};
use backend_sane::SaneBackend;
use common::{Events, call, json_call, kinds};
use janus_scan_server::{AppState, router};
use scan_core::{Backend, DeviceId};
use serde_json::json;

#[tokio::test]
async fn feeder_passes_through_the_api() {
    if std::env::var_os("SANE_CONFIG_DIR").is_none() {
        let dir = std::env::temp_dir().join(format!("janus-scan-sane-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dll.conf"), "test\n").unwrap();
        // Trailing ':' keeps the default config dirs as a fallback.
        // SAFETY: the only test in this binary; libsane isn't loaded yet.
        unsafe { std::env::set_var("SANE_CONFIG_DIR", format!("{}:", dir.display())) };
    }
    let backend = match SaneBackend::new() {
        Ok(b) => b,
        Err(e) => return println!("SKIP: libsane not available: {e}"),
    };
    match backend.devices() {
        Ok(list) if list.iter().any(|d| d.id == DeviceId("test:0".into())) => {}
        other => return println!("SKIP: SANE test:0 not listed ({other:?})"),
    }

    let app = router(AppState::new(Box::new(backend), Some("test:0".into()), Default::default()).unwrap());
    let (status, v) = json_call(&app, Method::GET, "/api/scanner", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["device"]["backend"], "sane");
    assert!(v["capabilities"]["sources"].as_array().unwrap().contains(&json!("feeder")));

    let mut ev = Events::open(&app).await;
    let body = json!({ "request": { "source": "feeder", "duplex": false, "dpi": 75, "color": "gray", "paper": { "kind": "a5" } } });
    for pass in 1..=2 {
        // The second pass is "Scan more": pages go onto the end.
        assert_eq!(call(&app, Method::POST, "/api/scan", Some(body.clone())).await.0, StatusCode::ACCEPTED);
        let events = ev.scan_pass().await;
        assert_eq!(kinds(&events).last(), Some(&"feeder-empty"), "{events:?}");
        assert_eq!(events.iter().filter(|e| e["kind"] == "page").count(), 10);
        ev.idle().await;
        let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
        assert_eq!(pages.as_array().unwrap().len(), 10 * pass);
    }

    let (_, pages) = json_call(&app, Method::GET, "/api/pages", None).await;
    let (status, jpeg) = call(&app, Method::GET, pages[0]["thumb"].as_str().unwrap(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(image::load_from_memory(&jpeg).is_ok());
}
