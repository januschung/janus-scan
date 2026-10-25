//! Janus Scan Server: the scanner and one shared document behind a small HTTP
//! API, so any browser on the network can scan without a local driver.
//!
//! One scan pass runs at a time (the scanner lock); every browser sees the
//! same document. Live updates go out as Server-Sent Events: `scan` events
//! carry the same payloads as the desktop app's `scan` event, `pages` carries
//! the full page list after an edit, and `state` says whether a pass is
//! running.

use std::convert::Infallible;
use std::io::Cursor;
use std::path::Path as FsPath;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, patch, post, put};
use axum::{Json, Router};
use document::{PageInfo, PageStore};
use image::DynamicImage;
use scan_core::{
    Backend, CancelToken, Capabilities, Connection, Device, DeviceError, DeviceInfo, ScanEvent, ScanRequest,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::{BroadcastStream, WatchStream};
use tokio_stream::{Stream, StreamExt};
use tower_http::services::{ServeDir, ServeFile};

mod save;
pub use save::{Config, Destination};

const TEMP_PREFIX: &str = "janus-scan-server-";

// ---------------------------------------------------------------------------
// Wire types

/// Same shape as the desktop app's scan options, so the front end sends one
/// object to either transport.
#[derive(Deserialize, Clone)]
pub struct ScanOptions {
    pub request: ScanRequest,
    #[serde(default)]
    pub skip_blank: bool,
    #[serde(default = "default_sensitivity")]
    pub blank_sensitivity: f32,
    #[serde(default)]
    pub auto_crop: bool,
    #[serde(default)]
    pub deskew: bool,
}

fn default_sensitivity() -> f32 {
    0.5
}

/// A page as the front end sees it. `thumb` is a URL here rather than the
/// desktop app's data URL; both work as an `<img src>`.
#[derive(Serialize, Clone, Debug)]
pub struct PageView {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub thumb: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScanUpdate {
    Page { page: PageView },
    Skipped { index: u32 },
    FeederEmpty,
    Done,
    Error { error: DeviceError, message: String },
}

#[derive(Serialize)]
pub struct ScannerStatus {
    pub device: DeviceInfo,
    pub capabilities: Capabilities,
    pub scanning: bool,
}

#[derive(Clone, Debug)]
enum Update {
    Scan(ScanUpdate),
    Pages(Vec<PageView>),
    Scanning(bool),
    /// Unsaved pages were removed after this many minutes without activity.
    Expired(u64),
}

impl Update {
    fn event(&self) -> Event {
        let (name, data) = match self {
            Self::Scan(u) => ("scan", serde_json::to_string(u)),
            Self::Pages(p) => ("pages", serde_json::to_string(p)),
            Self::Scanning(s) => ("state", serde_json::to_string(&serde_json::json!({ "scanning": s }))),
            Self::Expired(m) => ("expired", serde_json::to_string(&serde_json::json!({ "minutes": m }))),
        };
        Event::default().event(name).data(data.unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Errors

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self(status, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

/// Page-store errors are all "no such page" or local I/O trouble.
fn page_error(msg: String) -> ApiError {
    let status = if msg.contains("no longer exists") { StatusCode::NOT_FOUND } else { StatusCode::CONFLICT };
    ApiError::new(status, msg)
}

type ApiResult<T> = Result<T, ApiError>;

// ---------------------------------------------------------------------------
// State

pub struct AppState {
    backend: Box<dyn Backend>,
    /// Device name to use; the first one the backend lists when `None`.
    device_name: Option<String>,
    device: Arc<Mutex<Option<Box<dyn Device>>>>,
    /// Kept outside `device` so status requests don't wait behind a scan,
    /// which holds the device for the whole pass.
    opened: Mutex<Option<(DeviceInfo, Capabilities)>>,
    doc: Arc<Mutex<PageStore>>,
    /// `Some` while a pass runs: the scanner lock.
    scanning: Arc<Mutex<Option<CancelToken>>>,
    updates: broadcast::Sender<Update>,
    destinations: Vec<Destination>,
    /// Set on shutdown: ends the event streams, which would otherwise keep
    /// their connections (and so the server) open forever.
    closing: watch::Sender<bool>,
    /// Last time someone scanned, edited, saved or looked at a page; passive
    /// polling and open event streams don't count.
    last_activity: Mutex<Instant>,
    /// Differs on every start, so thumbnail URLs from a previous run (page ids
    /// restart at 1) never hit a browser's cached image.
    epoch: u128,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(backend: Box<dyn Backend>, device_name: Option<String>, config: Config) -> std::io::Result<Shared> {
        Ok(Arc::new(Self {
            backend,
            device_name,
            device: Arc::default(),
            opened: Mutex::default(),
            doc: Arc::new(Mutex::new(PageStore::new(TEMP_PREFIX)?)),
            scanning: Arc::default(),
            updates: broadcast::channel(256).0,
            destinations: config.destinations,
            closing: watch::channel(false).0,
            last_activity: Mutex::new(Instant::now()),
            epoch: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
        }))
    }

    fn view(&self, info: PageInfo) -> PageView {
        PageView {
            id: info.id,
            width: info.width,
            height: info.height,
            dpi: info.dpi,
            thumb: format!("/api/pages/{}/thumb?v={}-{}", info.id, self.epoch, info.turns),
        }
    }

    fn views(&self) -> Vec<PageView> {
        let infos = self.doc.lock().unwrap().infos();
        infos.into_iter().map(|i| self.view(i)).collect()
    }

    /// Start shutting down: end the event streams and cancel a pass in progress.
    pub fn close(&self) {
        self.closing.send_replace(true);
        if let Some(c) = self.scanning.lock().unwrap().as_ref() {
            c.cancel();
        }
    }

    /// Delete the scanned pages now. The process may exit without dropping
    /// the state, and pages must never outlive the server.
    pub fn discard_pages(&self) {
        self.doc.lock().unwrap_or_else(|e| e.into_inner()).discard();
    }

    fn is_scanning(&self) -> bool {
        self.scanning.lock().unwrap().is_some()
    }

    fn send(&self, u: Update) {
        // No subscribers is fine: nobody has the page open.
        let _ = self.updates.send(u);
    }

    fn pages_changed(&self) {
        self.touch();
        self.send(Update::Pages(self.views()));
    }

    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// Delete unsaved pages once nobody has done anything for `timeout`, so
    /// an abandoned document doesn't stay on the server. Never during a pass.
    pub fn start_idle_timeout(self: &Arc<Self>, timeout: Duration) {
        let st = Arc::clone(self);
        let every = (timeout / 10).clamp(Duration::from_millis(10), Duration::from_secs(60));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            while !*st.closing.borrow() {
                tick.tick().await;
                st.expire_if_idle(timeout);
            }
        });
    }

    fn expire_if_idle(&self, timeout: Duration) {
        // Holding the scanner lock: a pass can't start halfway through.
        let scanning = self.scanning.lock().unwrap();
        if scanning.is_some() || self.last_activity.lock().unwrap().elapsed() < timeout {
            return;
        }
        {
            let mut doc = self.doc.lock().unwrap();
            if doc.is_empty() {
                return;
            }
            doc.clear();
        }
        drop(scanning);
        let minutes = timeout.as_secs() / 60;
        eprintln!("janus-scan-server: removed unsaved pages after {minutes} min without activity");
        self.send(Update::Pages(Vec::new()));
        self.send(Update::Expired(minutes));
    }

    /// Open the scanner if it isn't open yet. Blocking: probing SANE devices
    /// can take seconds.
    fn ensure_open(&self) -> Result<(DeviceInfo, Capabilities), DeviceError> {
        if let Some(open) = self.opened.lock().unwrap().clone() {
            return Ok(open);
        }
        let mut slot = self.device.lock().unwrap();
        // Listing also tells the backend each device's display name.
        let listed = self.backend.devices();
        let id = match &self.device_name {
            Some(name) => scan_core::DeviceId(name.clone()),
            None => listed?.into_iter().next().ok_or(DeviceError::NotFound)?.id,
        };
        let dev = self.backend.open(&id)?;
        let open = (dev.info().clone(), dev.capabilities().clone());
        *slot = Some(dev);
        *self.opened.lock().unwrap() = Some(open.clone());
        Ok(open)
    }

    /// Forget the open device so the next request finds the scanner again,
    /// for example after it dropped off Wi-Fi.
    fn forget_device(&self) {
        *self.opened.lock().unwrap() = None;
        *self.device.lock().unwrap() = None;
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/scanner", get(scanner))
        .route("/api/scan", post(start_scan))
        .route("/api/scan/cancel", post(cancel_scan))
        .route("/api/events", get(events))
        .route("/api/pages", get(list_pages).delete(clear_pages))
        .route("/api/pages/order", put(reorder_pages))
        .route("/api/pages/{id}", patch(edit_page).delete(delete_page))
        .route("/api/pages/{id}/thumb", get(thumb))
        .route("/api/pages/{id}/preview", get(preview))
        .route("/api/destinations", get(save::list))
        .route("/api/save", post(save::save))
        .route("/api/estimate", post(save::estimate))
        .route("/api/{*rest}", any(|| async { ApiError::new(StatusCode::NOT_FOUND, "No such API endpoint.") }))
        .with_state(state)
}

/// Adds the built web front end (`app/build-web`). Paths that aren't files get
/// `index.html`, so the app's own routes load too.
pub fn with_web(router: Router, dir: &FsPath) -> Router {
    router.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))))
}

// ---------------------------------------------------------------------------
// Scanner

async fn scanner(State(s): State<Shared>) -> ApiResult<Json<ScannerStatus>> {
    let st = s.clone();
    let (device, capabilities) = blocking(move || st.ensure_open())
        .await?
        .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    Ok(Json(ScannerStatus { device, capabilities, scanning: s.is_scanning() }))
}

fn processing(o: &ScanOptions) -> imaging::Processing {
    imaging::Processing { color: o.request.color, auto_crop: o.auto_crop, deskew: o.deskew, ..Default::default() }
}

async fn start_scan(State(s): State<Shared>, Json(options): Json<ScanOptions>) -> ApiResult<StatusCode> {
    let cancel = CancelToken::default();
    {
        let mut lock = s.scanning.lock().unwrap();
        if lock.is_some() {
            return Err(ApiError::new(StatusCode::CONFLICT, "The scanner is in use. Try again when the scan finishes."));
        }
        *lock = Some(cancel.clone());
    }
    s.touch();
    s.send(Update::Scanning(true));

    let st = s.clone();
    let opened = blocking(move || st.ensure_open()).await?;
    let (info, _) = match opened {
        Ok(o) => o,
        Err(e) => {
            finish(&s);
            return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e.to_string()));
        }
    };

    let (tx, rx) = mpsc::channel::<ScanEvent>();

    // Scanner thread: feeds pages into the channel.
    let device = s.device.clone();
    let req = options.request.clone();
    std::thread::spawn(move || match device.lock().unwrap().as_mut() {
        Some(dev) => dev.scan(&req, tx, cancel),
        None => {
            let _ = tx.send(ScanEvent::Error(DeviceError::NotFound));
        }
    });

    // Processing thread: works on page N while the scanner feeds page N+1.
    let st = s.clone();
    std::thread::spawn(move || {
        let proc = processing(&options);
        let emit = |u: ScanUpdate| st.send(Update::Scan(u));
        let started = Instant::now();
        let mut scanned = 0u32;
        for ev in rx {
            if matches!(ev, ScanEvent::FeederEmpty | ScanEvent::Done | ScanEvent::Error(_)) {
                log_timing(info.connection, &options.request, scanned, started.elapsed().as_secs_f32());
            }
            match ev {
                ScanEvent::PageStarted(_) => {}
                ScanEvent::Page(p) => {
                    scanned += 1;
                    if options.skip_blank && imaging::is_blank(&p.image, options.blank_sensitivity) {
                        emit(ScanUpdate::Skipped { index: p.index });
                        continue;
                    }
                    let img = imaging::apply(&p.image, &proc);
                    let added = st.doc.lock().unwrap().add(&img, p.dpi);
                    st.touch();
                    match added {
                        Ok(info) => emit(ScanUpdate::Page { page: st.view(info) }),
                        Err(e) => emit(error(DeviceError::Other(format!("Couldn't store page: {e}")))),
                    }
                }
                ScanEvent::FeederEmpty => emit(ScanUpdate::FeederEmpty),
                ScanEvent::Done => emit(ScanUpdate::Done),
                ScanEvent::Error(e) => {
                    if matches!(e, DeviceError::Disconnected | DeviceError::NotFound) {
                        // The scanner thread has returned by now; reopen next time.
                        st.forget_device();
                    }
                    emit(error(e))
                }
            }
        }
        finish(&st);
    });
    Ok(StatusCode::ACCEPTED)
}

/// Release the scanner lock.
fn finish(s: &AppState) {
    // SANE backends can reset SIGPIPE to its default during a pass, which
    // would let a write to a closed connection kill the process.
    #[cfg(unix)]
    // SAFETY: setting a signal disposition to "ignore" is always sound.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    *s.scanning.lock().unwrap() = None;
    s.touch();
    s.send(Update::Scanning(false));
}

/// One line per pass for comparing speeds. No page content.
fn log_timing(connection: Connection, req: &ScanRequest, pages: u32, secs: f32) {
    let per_page = if pages > 0 { secs / pages as f32 } else { 0.0 };
    eprintln!(
        "[timing] connection={connection:?} dpi={} color={:?} duplex={} pages={pages} total={secs:.1}s per_page={per_page:.1}s",
        req.dpi, req.color, req.duplex
    );
}

fn error(e: DeviceError) -> ScanUpdate {
    ScanUpdate::Error { message: e.to_string(), error: e }
}

async fn cancel_scan(State(s): State<Shared>) -> StatusCode {
    if let Some(c) = s.scanning.lock().unwrap().as_ref() {
        c.cancel();
    }
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------
// Events

async fn events(State(s): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Subscribe before taking the snapshot so nothing falls in between.
    let rx = s.updates.subscribe();
    let hello = [Update::Scanning(s.is_scanning()), Update::Pages(s.views())];
    let live = BroadcastStream::new(rx).filter_map(|u| u.ok().map(Some)); // a lagging browser just misses some
    let closing = WatchStream::new(s.closing.subscribe()).filter(|c| *c).map(|_| None);
    let stream = tokio_stream::iter(hello)
        .chain(live.merge(closing).take_while(Option::is_some).map(Option::unwrap))
        .map(|u| Ok(u.event()));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Pages

async fn list_pages(State(s): State<Shared>) -> Json<Vec<PageView>> {
    Json(s.views())
}

async fn clear_pages(State(s): State<Shared>) -> StatusCode {
    s.doc.lock().unwrap().clear();
    s.pages_changed();
    StatusCode::NO_CONTENT
}

async fn reorder_pages(State(s): State<Shared>, Json(order): Json<Vec<u64>>) -> ApiResult<StatusCode> {
    s.doc.lock().unwrap().reorder(&order).map_err(page_error)?;
    s.pages_changed();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Turn {
    Cw,
    Ccw,
}

#[derive(Deserialize)]
struct PageEdit {
    rotate: Turn,
}

async fn edit_page(State(s): State<Shared>, Path(id): Path<u64>, Json(edit): Json<PageEdit>) -> ApiResult<Json<PageView>> {
    let info = s.doc.lock().unwrap().rotate(id, matches!(edit.rotate, Turn::Cw)).map_err(page_error)?;
    s.pages_changed();
    Ok(Json(s.view(info)))
}

/// Removes the page from the document being assembled. Saved files are never
/// touched; only the temporary copy goes.
async fn delete_page(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<StatusCode> {
    s.doc.lock().unwrap().delete(id).map_err(page_error)?;
    s.pages_changed();
    Ok(StatusCode::NO_CONTENT)
}

fn jpeg(img: &DynamicImage, quality: u8) -> ApiResult<Response> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut buf), quality)
        .encode_image(&img.to_rgb8())
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=31536000, immutable")], buf)
        .into_response())
}

async fn thumb(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Response> {
    let img = s.doc.lock().unwrap().thumb(id).map_err(page_error)?;
    jpeg(&img, 80)
}

#[derive(Deserialize)]
struct PreviewQuery {
    max_side: Option<u32>,
}

async fn preview(State(s): State<Shared>, Path(id): Path<u64>, Query(q): Query<PreviewQuery>) -> ApiResult<Response> {
    let max_side = q.max_side.unwrap_or(1600).clamp(400, 4000);
    s.touch();
    let st = s.clone();
    blocking(move || {
        // Load under the lock, scale outside it.
        let (img, _) = st.doc.lock().unwrap().load(id).map_err(page_error)?;
        let mut resp = jpeg(&imaging::preview(&img, max_side), 88)?;
        // Previews aren't keyed by rotation in the URL, so always revalidate.
        resp.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
        Ok(resp)
    })
    .await?
}
