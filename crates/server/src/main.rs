//! `janus-scan-server`: serves the scan API on the LAN.
//! `janus-scan-server health` checks that a running server answers (for
//! container health checks) and exits 0 or 1.
//!
//! Settings come from the environment:
//! - `PORT` (default 8080) and `BIND` (default 0.0.0.0)
//! - `JANUS_DEVICE`: SANE device name to use; the first one listed otherwise
//! - `JANUS_CONFIG`: the config file with the NAS destinations (default
//!   /etc/janus-scan/server.yaml when it exists; download only otherwise)
//! - `IDLE_TIMEOUT_MINUTES`: delete unsaved pages after this long without
//!   activity (default 60; 0 keeps them until New or shutdown)
//! - `WEB_DIR`: the built web front end (`npm run build:web` in app/); API only when unset

use std::net::SocketAddr;
use std::path::PathBuf;

use backend_sane::SaneBackend;
use janus_scan_server::{AppState, Config, router, with_web};

const DEFAULT_CONFIG: &str = "/etc/janus-scan/server.yaml";

fn main() {
    if std::env::args().nth(1).as_deref() == Some("health") {
        std::process::exit(if health() { 0 } else { 1 });
    }
    // Before any thread exists, so every thread inherits the mask.
    #[cfg(unix)]
    let stop = signals::block();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("can't start the async runtime");
    #[cfg(unix)]
    let stop = signals::wait(stop);
    #[cfg(not(unix))]
    let stop = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = runtime.block_on(run(stop)) {
        eprintln!("janus-scan-server: {e}");
        std::process::exit(1);
    }
}

async fn run(stop: impl std::future::Future<Output = ()> + Send + 'static) -> Result<(), String> {
    let port: u16 = env("PORT").map(|p| p.parse().map_err(|_| format!("PORT is not a port number: {p}"))).transpose()?.unwrap_or(8080);
    let bind = env("BIND").unwrap_or_else(|| "0.0.0.0".into());
    let addr: SocketAddr = format!("{bind}:{port}").parse().map_err(|_| format!("BIND is not an address: {bind}"))?;

    let config = match env("JANUS_CONFIG") {
        Some(path) => Config::load(path.as_ref())?,
        None if std::path::Path::new(DEFAULT_CONFIG).exists() => Config::load(DEFAULT_CONFIG.as_ref())?,
        None => Config::default(),
    };
    for d in &config.destinations {
        if !d.path.is_dir() {
            // Not fatal: the share may be mounted later. Saving to it says so.
            eprintln!("janus-scan-server: warning: \"{}\" folder not found: {}", d.name, d.path.display());
        }
    }

    let backend = SaneBackend::new().map_err(|e| format!("SANE isn't available: {e}"))?;
    let state = AppState::new(Box::new(backend), env("JANUS_DEVICE"), config)
        .map_err(|e| format!("can't create a temporary folder: {e}"))?;

    let idle: u64 = env("IDLE_TIMEOUT_MINUTES")
        .map(|m| m.parse().map_err(|_| format!("IDLE_TIMEOUT_MINUTES is not a number of minutes: {m}")))
        .transpose()?
        .unwrap_or(60);
    if idle > 0 {
        state.start_idle_timeout(std::time::Duration::from_secs(idle * 60));
    }

    let mut app = router(state.clone());
    if let Some(dir) = env("WEB_DIR").map(PathBuf::from) {
        if !dir.join("index.html").is_file() {
            return Err(format!("WEB_DIR has no index.html: {}", dir.display()));
        }
        app = with_web(app, &dir);
    }

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("can't listen on {addr}: {e}"))?;
    eprintln!("janus-scan-server listening on http://{addr}");
    let closing = state.clone();
    let stop = async move {
        stop.await;
        eprintln!("janus-scan-server: shutting down");
        closing.close();
    };
    let served = axum::serve(listener, app).with_graceful_shutdown(stop).await.map_err(|e| e.to_string());
    // Exit without dropping the scanner: closing a SANE device can block
    // (seen with SANE's test backend after a pass), and the process is
    // ending anyway. The scanned pages are deleted first.
    state.discard_pages();
    eprintln!("janus-scan-server: stopped");
    if let Err(e) = served {
        eprintln!("janus-scan-server: {e}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// Asks the local server for its page list; any 200 means it's up.
fn health() -> bool {
    use std::io::{Read, Write};
    let port = env("PORT").and_then(|p| p.parse::<u16>().ok()).unwrap_or(8080);
    let timeout = Some(std::time::Duration::from_secs(3));
    let Ok(mut conn) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return false };
    let _ = conn.set_read_timeout(timeout);
    let _ = conn.set_write_timeout(timeout);
    let mut reply = [0u8; 12];
    conn.write_all(b"GET /api/pages HTTP/1.0\r\nHost: localhost\r\n\r\n").is_ok()
        && conn.read_exact(&mut reply).is_ok()
        && reply.starts_with(b"HTTP/1.") && &reply[9..12] == b"200"
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Ctrl-C, or SIGTERM from `docker stop` / Kubernetes / systemd.
///
/// Not a signal handler: SANE backends reset handlers process-wide during a
/// scan (SANE's test backend leaves SIGTERM at its default), and the default
/// action does nothing to a container's PID 1. Instead SIGTERM and SIGINT are
/// blocked in every thread and one thread collects them with `sigwait`; a
/// blocked signal is queued whatever its handler is.
#[cfg(unix)]
mod signals {
    use std::mem::MaybeUninit;

    pub fn block() -> libc::sigset_t {
        // SAFETY: plain libc calls on a local signal set.
        unsafe {
            let mut set = MaybeUninit::<libc::sigset_t>::zeroed().assume_init();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            set
        }
    }

    pub fn wait(set: libc::sigset_t) -> impl Future<Output = ()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let mut sig = 0;
            // SAFETY: `set` holds only SIGTERM and SIGINT, blocked in every thread.
            while unsafe { libc::sigwait(&set, &mut sig) } != 0 {}
            let _ = tx.send(());
        });
        async move {
            let _ = rx.await;
        }
    }
}
