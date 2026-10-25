mod pages;

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use document::PageStore;
use pages::{PageView, data_url};
use scan_core::{
    Backend, CancelToken, Capabilities, Connection, Device, DeviceError, DeviceInfo, ScanEvent,
    ScanRequest, dedupe_devices,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

type CmdResult<T> = Result<T, String>;

#[derive(Default)]
struct AppState {
    backends: OnceLock<Vec<Box<dyn Backend>>>,
    /// Which backend each listed device came from.
    known: Mutex<Vec<(usize, DeviceInfo)>>,
    device: Arc<Mutex<Option<Box<dyn Device>>>>,
    pages: Arc<Mutex<Option<PageStore>>>,
    cancel: Mutex<Option<CancelToken>>,
}

fn backends(state: &AppState) -> &[Box<dyn Backend>] {
    state.backends.get_or_init(|| {
        #[allow(unused_mut)]
        let mut out: Vec<Box<dyn Backend>> = Vec::new();
        #[cfg(target_os = "macos")]
        out.push(Box::new(backend_ica::IcaBackend::new()));
        #[cfg(target_os = "linux")]
        if let Ok(b) = backend_sane::SaneBackend::new() {
            out.push(Box::new(b));
        }
        out
    })
}

/// Device and scanning calls block (ICA waits on the main thread), so they run
/// on the blocking pool, never on the main thread or an async worker.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

fn with_pages<T>(state: &AppState, f: impl FnOnce(&mut PageStore) -> CmdResult<T>) -> CmdResult<T> {
    let mut guard = state.pages.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = Some(pages::new_store().map_err(|e| format!("Can't create a temporary folder: {e}"))?);
    }
    f(guard.as_mut().expect("just created"))
}

// ---------------------------------------------------------------------------
// Devices

#[tauri::command]
async fn list_devices(app: AppHandle) -> CmdResult<Vec<DeviceInfo>> {
    blocking(move || {
        let state = app.state::<AppState>();
        let mut found = Vec::new();
        for (i, b) in backends(&state).iter().enumerate() {
            if let Ok(devs) = b.devices() {
                found.extend(devs.into_iter().map(|d| (i, d)));
            }
        }
        let kept = dedupe_devices(found.iter().map(|(_, d)| d.clone()).collect());
        found.retain(|(_, d)| kept.iter().any(|k| k.id == d.id && k.connection == d.connection));
        let list = found.iter().map(|(_, d)| d.clone()).collect();
        *state.known.lock().unwrap() = found;
        list
    })
    .await
}

#[tauri::command]
async fn open_device(app: AppHandle, id: String) -> CmdResult<Capabilities> {
    blocking(move || {
        let state = app.state::<AppState>();
        let (backend, info) = state
            .known
            .lock()
            .unwrap()
            .iter()
            .find(|(_, d)| d.id.0 == id)
            .cloned()
            .ok_or_else(|| DeviceError::NotFound.to_string())?;
        let mut slot = state.device.lock().unwrap();
        if slot.as_ref().is_some_and(|d| d.info().id == info.id) {
            return Ok(slot.as_ref().unwrap().capabilities().clone());
        }
        *slot = None; // close the previous device first
        let dev = backends(&state)[backend].open(&info.id).map_err(|e| e.to_string())?;
        let caps = dev.capabilities().clone();
        *slot = Some(dev);
        Ok(caps)
    })
    .await?
}

// ---------------------------------------------------------------------------
// Scanning

#[derive(Deserialize, Clone)]
struct ScanOptions {
    request: ScanRequest,
    skip_blank: bool,
    blank_sensitivity: f32,
    auto_crop: bool,
    deskew: bool,
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ScanUpdate {
    Page { page: PageView },
    Skipped { index: u32 },
    FeederEmpty,
    Done,
    Error { error: DeviceError, message: String },
}

fn processing(o: &ScanOptions) -> imaging::Processing {
    imaging::Processing {
        color: o.request.color,
        auto_crop: o.auto_crop,
        deskew: o.deskew,
        ..Default::default()
    }
}

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>, options: ScanOptions) -> CmdResult<()> {
    let cancel = CancelToken::default();
    *state.cancel.lock().unwrap() = Some(cancel.clone());
    let device = state.device.clone();
    let connection = device.lock().unwrap().as_ref().map(|d| d.info().connection);
    let pages = state.pages.clone();
    let (tx, rx) = mpsc::channel::<ScanEvent>();

    // Scanner thread: feeds pages into the channel.
    let scan_opts = options.clone();
    std::thread::spawn(move || {
        let mut guard = device.lock().unwrap();
        match guard.as_mut() {
            Some(dev) => dev.scan(&scan_opts.request, tx, cancel),
            None => {
                let _ = tx.send(ScanEvent::Error(DeviceError::NotFound));
            }
        }
    });

    // Processing thread: works on page N while the scanner feeds page N+1.
    std::thread::spawn(move || {
        let proc = processing(&options);
        let emit = |u: ScanUpdate| {
            let _ = app.emit("scan", u);
        };
        let started = Instant::now();
        let mut scanned = 0u32;
        for ev in rx {
            if matches!(ev, ScanEvent::FeederEmpty | ScanEvent::Done | ScanEvent::Error(_)) {
                log_timing(connection, &options.request, scanned, started.elapsed().as_secs_f32());
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
                    let mut guard = pages.lock().unwrap();
                    if guard.is_none() {
                        *guard = pages::new_store().ok();
                    }
                    match guard.as_mut().map(|s| s.add(&img, p.dpi).map(|info| pages::view(s, info))) {
                        Some(Ok(page)) => emit(ScanUpdate::Page { page }),
                        Some(Err(e)) => emit(error(DeviceError::Other(format!("Couldn't store page: {e}")))),
                        None => emit(error(DeviceError::Other("Can't create a temporary folder.".into()))),
                    }
                }
                ScanEvent::FeederEmpty => emit(ScanUpdate::FeederEmpty),
                ScanEvent::Done => emit(ScanUpdate::Done),
                ScanEvent::Error(e) => emit(error(e)),
            }
        }
    });
    Ok(())
}

/// One line per pass for comparing USB and Wi-Fi speed. No page content.
fn log_timing(connection: Option<Connection>, req: &ScanRequest, pages: u32, secs: f32) {
    let per_page = if pages > 0 { secs / pages as f32 } else { 0.0 };
    eprintln!(
        "[timing] connection={connection:?} dpi={} color={:?} duplex={} pages={pages} total={secs:.1}s per_page={per_page:.1}s",
        req.dpi, req.color, req.duplex
    );
}

fn error(e: DeviceError) -> ScanUpdate {
    ScanUpdate::Error { message: e.to_string(), error: e }
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    if let Some(c) = state.cancel.lock().unwrap().as_ref() {
        c.cancel();
    }
}

// ---------------------------------------------------------------------------
// Pages

#[tauri::command]
fn list_pages(state: State<AppState>) -> CmdResult<Vec<PageView>> {
    with_pages(&state, |s| Ok(pages::views(s)))
}

#[tauri::command]
async fn page_preview(app: AppHandle, id: u64, max_side: u32) -> CmdResult<String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let (img, _) = with_pages(&state, |s| s.load(id))?;
        Ok(data_url(&imaging::preview(&img, max_side.clamp(400, 4000)), 88))
    })
    .await?
}

#[tauri::command]
fn rotate_page(state: State<AppState>, id: u64, clockwise: bool) -> CmdResult<PageView> {
    with_pages(&state, |s| s.rotate(id, clockwise).map(|info| pages::view(s, info)))
}

/// Removes pages from the document being assembled. Files already saved are
/// never touched; only the temporary copies go.
#[tauri::command]
fn remove_pages(state: State<AppState>, ids: Vec<u64>) -> CmdResult<()> {
    with_pages(&state, |s| ids.iter().try_for_each(|&id| s.delete(id)))
}

#[tauri::command]
fn reorder_pages(state: State<AppState>, order: Vec<u64>) -> CmdResult<()> {
    with_pages(&state, |s| s.reorder(&order))
}

#[tauri::command]
fn clear_pages(state: State<AppState>) -> CmdResult<()> {
    with_pages(&state, |s| {
        s.clear();
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Saving

#[derive(Deserialize)]
struct SaveOptions {
    dir: PathBuf,
    name_template: String,
    #[serde(flatten)]
    export: export::ExportOptions,
}

fn load_all(state: &AppState) -> CmdResult<Vec<export::ExportPage>> {
    with_pages(state, |s| {
        s.ids()
            .into_iter()
            .map(|id| s.load(id).map(|(image, dpi)| export::ExportPage { image, dpi }))
            .collect()
    })
}

#[tauri::command]
async fn save_document(app: AppHandle, options: SaveOptions) -> CmdResult<Vec<PathBuf>> {
    blocking(move || {
        let state = app.state::<AppState>();
        let pages = load_all(&state)?;
        if pages.is_empty() {
            return Err("There are no pages to save.".into());
        }
        export::export(&pages, &options.dir, &options.name_template, &options.export).map_err(|e| e.to_string())
    })
    .await?
}

#[tauri::command]
async fn estimate_size(app: AppHandle, options: export::ExportOptions) -> CmdResult<u64> {
    blocking(move || {
        let state = app.state::<AppState>();
        // One page is enough to extrapolate from.
        let (first, count) = with_pages(&state, |s| {
            let first = s.ids().first().map(|&id| s.load(id)).transpose()?;
            Ok((first, s.len()))
        })?;
        let Some((image, dpi)) = first else { return Ok(0) };
        let one = export::estimate_size(&[export::ExportPage { image, dpi }], &options);
        Ok(one * count as u64)
    })
    .await?
}

#[tauri::command]
fn default_save_dir(app: AppHandle) -> Option<PathBuf> {
    app.path().document_dir().ok().or_else(|| app.path().home_dir().ok())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            open_device,
            start_scan,
            cancel_scan,
            list_pages,
            page_preview,
            rotate_page,
            remove_pages,
            reorder_pages,
            clear_pages,
            save_document,
            estimate_size,
            default_save_dir,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
