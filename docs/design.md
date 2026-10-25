# Janus Scan: design notes

How Janus Scan is built and why, what we learned about Brother's drivers, and what's left to do. For using it, see the [README](../README.md).

## Scope

Built and tested on one scanner: the Brother ADS-1000W (sheet-fed, two-sided, USB and Wi-Fi). Other models are welcome through contributions tested on the real hardware (see [Other scanner models](#other-scanner-models)). Windows is out of scope; Brother's own software works there.

| Platform | Versions | Notes |
| --- | --- | --- |
| macOS | current | Main development machine (Apple Silicon) |
| Ubuntu | 24.04 and newer | |
| Linux Mint | 22 and newer | Built on Ubuntu 24.04 |
| Linux Lite | 7 and newer | Built on Ubuntu 24.04; the low-end test machine (old hardware) |
| CachyOS | rolling | Arch-based; always the newest libraries |
| Janus Scan Server | any x86-64 Linux host | Docker Compose, Kubernetes or LXC; phones and other computers use it through a browser |

The three Ubuntu-family distros share one package base (24.04); releases built on 22.04 or older aren't supported. The scanner's own Scan button isn't supported (see [Brother's drivers](#brothers-drivers)).

## How it works

```
janus-scan/
├── crates/
│   ├── scan-core/      # scanner model: devices, capabilities, scan requests, events, errors
│   ├── backend-ica/    # macOS: Apple's ImageCaptureCore through objc2 (USB and Wi-Fi)
│   ├── backend-sane/   # Linux and the server: libsane, loaded at runtime, with Brother's driver
│   ├── imaging/        # thumbnails, blank-page detection, deskew, crop, color conversion
│   ├── export/         # PDF, TIFF, JPEG and PNG writers, file naming
│   ├── document/       # the document being assembled, in a private temporary folder
│   ├── cli/            # the janus-scan command-line tool
│   └── server/         # Janus Scan Server: web API, serves the web front end
├── app/                # desktop app: Tauri 2 + Svelte (also built as the server's web front end)
├── deploy/             # server: Docker image, Compose file, Helm chart, LXC files
├── docker/             # Linux build and test images
├── docs/               # Wi-Fi guide, these notes, screenshots
├── packaging/          # Arch package recipe (CachyOS)
└── scripts/            # install, Linux packages, diagnostics, README recordings
```

### Scanner model

Everything above the backends talks to two traits in `scan-core`:

- `Backend` lists devices and opens one. `Device` reports its capabilities and runs one pass through the feeder, sending events on a channel: `PageStarted`, `Page`, then `FeederEmpty`, `Done` or `Error`. A `CancelToken` stops a pass.
- **Errors** are one `DeviceError` type with a plain-language message for each case (no paper, jam, double feed, cover open, busy, disconnected, didn't start), so the UI handles errors once for every backend.
- **One scanner, two connections:** when the same scanner shows up over USB and Wi-Fi, the list keeps one entry and prefers USB.
- **Feeder empty** ends the pass; the UI then asks "More pages to feed?" and the next pass appends to the same document.

Scanning runs on its own thread, and a second thread processes page N (blank check, deskew, crop, color) while the scanner feeds page N+1. Full-size pages (about 2500 × 3300 pixels at 300 dpi) stay in Rust and on disk; the UI only gets thumbnails and preview-size images.

### Desktop app

Tauri 2 with Svelte: a native window around the system web view (WebKit on macOS, WebKitGTK on Linux), so the app is small (a 2.8 MB `.dmg` or `.deb`). Visual effects stay light because WebKitGTK on old hardware is the slowest target. If Linux Lite hardware can't keep up, the fallback is a Slint UI; only the UI layer would change.

- **macOS** scans through ImageCaptureCore (Brother's ICA driver), which also finds Wi-Fi scanners through Bonjour. ICA calls back on the main run loop, so the backend bridges those callbacks to the scan thread.
- **Linux** scans through SANE with Brother's `brscan4` driver as the SANE backend. Packages are `.deb`, Arch and AppImage rather than Snap or Flatpak, whose sandboxes make USB and system SANE drivers hard to reach.

### Janus Scan Server

A small container next to the scanner on the network. Any phone, tablet or computer scans by opening a web page, with no driver on the device. It replaced the idea of a native Android app, which would have meant reverse-engineering Brother's network protocol.

```
 phone / tablet / laptop ──HTTPS──▶  janus-scan-server (x86-64)  ──Wi-Fi──▶  ADS-1000W
   (browser, installable)             Brother brscan4 · scan-core
                                      imaging · export · document
                                          ├──▶ download to the device that asked
                                          └──▶ NAS folder (NFS or SMB, mounted by the platform)
```

**Reuse.** `crates/server` adds only the HTTP layer (`axum`) and job handling on top of `scan-core`, `backend-sane`, `imaging`, `export` and `document`. The web front end is the desktop app's Svelte UI built a second time (`npm run build:web`): `app/src/lib/api.ts` has two transports, Tauri commands for the desktop and HTTP plus Server-Sent Events for the server, behind the same interface.

**One shared document.** The server holds a single document in progress; every browser sees the same pages, live, so a scan started on the phone can be finished on the laptop. No sessions or cookies. A lock allows one pass at a time (others get "in use"); only the browser that started a pass gets "More pages?".

| Endpoint | Does |
| --- | --- |
| `GET /api/scanner` | status and capabilities |
| `POST /api/scan`, `POST /api/scan/cancel` | start a pass (scan settings plus clean-up options; `409` while another runs), stop it |
| `GET /api/events` | Server-Sent Events: scan progress, the page list after edits, scanning state, idle expiry |
| `GET/PATCH/PUT/DELETE /api/pages…` | list, thumbnails, previews, rotate, reorder, remove one or all |
| `GET /api/destinations`, `POST /api/save`, `POST /api/estimate` | save to a NAS folder or download (one file, or a ZIP when there's one file per page); size estimate |

**Saving.** The server never speaks NFS or SMB: the platform mounts the share and `server.yaml` maps names to folders, each with an optional file name preset. Browsers see names only; file names are sanitized so they can't leave their folder, and an unmounted share is reported rather than created.

**Phones.** Below 700 px the sidebar becomes a bottom sheet, pages show in two columns, Scan is a large button and the preview opens full screen. Reordering uses pointer events (long-press, then drag; the list scrolls at the edges), because browsers don't fire drag-and-drop events for touch. A manifest and service worker make it installable, which browsers only allow over HTTPS.

**Housekeeping.** Unsaved pages are deleted after `IDLE_TIMEOUT_MINUTES` (default 60) without activity: scanning, editing, saving or viewing pages, not an open tab that only listens. Never during a pass; every open browser is told why. Stale temporary folders are removed at start-up (also after a container restart, where the server is always PID 1, so process IDs repeat) and pages at shutdown.

**Shutdown.** SANE backends can reset signal handlers process-wide during a pass (seen with SANE's test backend), and the default SIGTERM action does nothing to a container's PID 1. So SIGTERM and SIGINT are blocked in every thread and collected by one thread with `sigwait`, and SIGPIPE is set back to "ignore" after each pass. Event streams end when shutdown starts, so open browsers don't hold it up.

**Trust model: no login.** It's a home tool; anyone who can reach the scanner can already use it. LAN only: remote use goes through a VPN, never an open port. No telemetry.

#### Deployment

The server reads environment variables (`SCANNER_IP`, `PORT`, `TZ`, `IDLE_TIMEOUT_MINUTES`, …) and `server.yaml`, and treats NAS destinations as plain folders, so the same build runs everywhere:

| | Docker Compose | Kubernetes | LXC |
| --- | --- | --- | --- |
| Files | `deploy/docker-compose.yml` | `deploy/helm/janus-scan` | `deploy/lxc` (`install.sh`, systemd unit) |
| NAS folders | `local` volume driver, `type: nfs` or `cifs` | NFS mounted into the pod, or any existing claim | host mounts the share; bind mount into the container (unprivileged: UID 1000 inside is 101000 on the host) |
| Notes | | one replica, `Recreate` (the lock and the document are in memory); ingress-nginx needs buffering off for live updates | no image: binary from the Dockerfile's `dist` target |

**The image** (32 MB compressed) is `debian:stable-slim` with the server and the web files, and later Brother's driver, but **no `libsane1`**: Brother's backend exports the plain SANE API, so the server loads it directly (`JANUS_SCAN_LIBSANE`). Debian's `libsane1` would add about 300 MB (udev, systemd, gphoto2, poppler, net-snmp, Perl) for scanners we don't use. Debian slim is the lightest base that works: the driver is a glibc binary, so Alpine is out. The Rust stage cross-compiles to x86-64 (libsane isn't linked), so building on Apple Silicon is fast. It runs as user 1000, and its health check asks the server, not the scanner, so it can't disturb a pass. Brother's driver is never redistributed: the published image (`ghcr.io/januschung/janus-scan-server`) has everything else plus `janus-scan-install-driver`, and users add the driver on their own machine with `deploy/driver.Dockerfile`, a seconds-long build that Compose runs for them. Without the driver, the container stops with a message saying how to add it.

**Releases.** Pushing a version tag (`v0.1.0`, matching `Cargo.toml` and `tauri.conf.json`) runs `.github/workflows/release.yml`: a universal `.dmg` (unsigned), the `.deb`, AppImage and Arch package, the server image on `ghcr.io`, and the Compose/Helm and LXC bundles, all attached to a draft GitHub Release with checksums. Only CI builds AppImages: they can't be built under emulation on Apple Silicon.

## Brother's drivers

What we measured and ran into, mostly the hard way.

**On the network.** The scanner advertises itself over Bonjour as `_scanner._tcp` and uses Brother's own protocol on port 54921 (admin page on 80 and 443). It doesn't support eSCL/AirScan (`/eSCL/ScannerCapabilities` returns 404), so every platform needs Brother's driver.

**Speed** (color, 300 dpi, both sides):

| Path | Seconds per page side |
| --- | --- |
| macOS, USB (ICA) | 2.0, about the scanner's mechanical speed |
| macOS, Wi-Fi (ICA) | 10.1, Brother's macOS driver is the bottleneck |
| Linux desktop, Wi-Fi (`brscan4`) | 3.2 |
| Janus Scan Server, Wi-Fi (`brscan4`) | 2.2 |

**macOS (ICA).**
- SANE (`sane-backends` 1.4.0) sees the scanner but has no driver for it, so macOS uses ICA only.
- The driver selects the feeder on its own when a session opens, without reporting it.
- An empty-feeder scan ends with no error, so a pass with no pages counts as "no paper".

**Linux (`brscan4` 0.4.11).**
- **Hardware:** it lists the ADS-1000W (USB ID 0x60a6).
- **Capabilities it reports:** resolutions up to 9600 dpi, which are interpolated (the real maximum is 600), and a page length up to 812.8 mm.
- **Network registration:** a network scanner has to be registered by IP with `brsaneconfig4`. It exits 0 even when it can't write its config, so check the config file (`brsanenetdevice4.cfg`). Its `-q` listing needs Avahi.
- **Empty feeder over Wi-Fi:** `sane_start` succeeds, then the first `sane_read` fails with an I/O error. The backend reports that one case as "The scanner didn't start …" rather than "disconnected".
- **`netbase`:** without `netbase` (`/etc/services`, `/etc/protocols`), `sane_open` on a network scanner segfaults. Desktop installs always have it; slim container images don't.
- **`libjpeg`:** it loads `libjpeg` at runtime (`.so.62` or `.so.8`) to decode what the scanner sends over Wi-Fi.
- **Scan button:** it expects Brother's scan-key tool, but Brother publishes none for the ADS-1000W on Linux, so the scanner's own Scan button isn't supported. Pressing it makes the scanner beep three times, which is expected.
- **Debugging:** `JANUS_SCAN_DEBUG=1` logs SANE call results (status codes only) for diagnosing driver behavior.

## Testing

- **`cargo test --workspace`** covers the scanner model, imaging, export, the document store, the server API (with an in-memory test scanner) and the SANE backend end to end through SANE's virtual `test` scanner. On a Mac that needs `brew install sane-backends`; the SANE tests skip themselves when it's missing.
- **`docker/`** runs the same tests on Ubuntu 24.04, Linux Mint 22.3 and CachyOS (`matrix.sh`), and a real Wi-Fi scan from a container (`brother-wifi.sh`). Mint and CachyOS images are x86-64, so they run under emulation on Apple Silicon. Linux Lite has no image; Ubuntu 24.04 stands in for it.
- **What needs real machines:** USB scanning on Linux, real graphics (GPU, Wayland/X11, NVIDIA), speed on old hardware, and network discovery.
- **Checked by hand so far:** the desktop app on macOS (USB and Wi-Fi) and on Linux Mint and Linux Lite; the server under Docker Compose, in a kind cluster, in a Debian 13 container with the LXC install and on a k3s cluster over HTTPS, installed on a phone.
- **README recordings** come from `scripts/showcase`.

## Other scanner models

Only the ADS-1000W has been tested, but little of the code depends on it. From reading the code:

**Already generic.** Both backends take any device the system driver exposes: any SANE backend on Linux and the server, any ICA scanner on macOS. Nothing filters by model or USB ID. Sources, two-sided support, resolutions, color modes and maximum page size are read from the driver at runtime, and option names are matched loosely ("adf", "feeder", "duplex", "color", "gray") rather than as the ADS-1000W's exact strings. Errors come from standard SANE status codes.

**Likely to need changes:**

| Area | Today | For other models |
| --- | --- | --- |
| Server packaging | Wired to `brscan4`: `libsane-brother4.so.1` (`JANUS_SCAN_LIBSANE`), `brsaneconfig4` and its config folder in `deploy/register-scanner.sh`, model default `ADS-1000W` (`SCANNER_MODEL`) | Newer Brother models use `brscan5` (`libsane-brother5`, `brsaneconfig5`), very old ones `brscan3` or older. Make the driver family a build setting. Non-Brother scanners need `libsane1` back in the image. The desktop app is unaffected: it uses the system's SANE. |
| Resolution | Capped at 600 dpi (`MAX_DPI` in `backend-sane/src/options.rs`): the ADS-1000W's optical maximum, since Brother's driver advertises up to 9600 (interpolated) | Scanners with 1200 dpi or more lose their higher settings; make the cap per model or configurable. |
| Source | The UI always uses the feeder when there is one | Printer-scanner combos with a feeder and a flatbed need a feeder/flatbed choice; both backends already support flatbed. |
| Driver quirks | Tuned on one model: the empty-feeder I/O error, the "More pages?" flow, the macOS ICA behavior in [Brother's drivers](#brothers-drivers) | Probably similar across Brother's sheet-fed models; check on the real scanner. |

Sheet-fed `brscan4` models (the ADS-1000W's siblings) would very likely work in the desktop app as is, and on the server with `SCANNER_MODEL` changed.

## Not planned

Ideas from the original plan that aren't being built, and known limits. They're here so they aren't rediscovered as surprises.

- **Desktop extras:** a Wi-Fi setup wizard (the [Wi-Fi guide](wifi-setup.md) covers setup instead), registering the network scanner with `brsaneconfig4` from the app, finding the scanner again by serial number after an IP change, rescanning one page, inserting pages at a chosen spot, splitting a batch into several documents, auto-rotate, brightness and contrast controls, a custom paper size in the UI, OCR, PDF/A, adding pages to an existing PDF, presets, recent scans, translations.
- **Signed packages:** the `.dmg` isn't signed or notarized (that needs a paid Apple developer ID). People approve it once under Privacy & Security, or build it themselves. Tauri's own DMG step needs a logged-in GUI session, since it drives Finder.
- **No AppImage from a Mac:** AppImage tools find themselves through `/proc/self/exe`, which points at the emulator when building x86-64 under emulation on Apple Silicon, so `scripts/package-linux.sh` builds a `.deb` and an Arch package instead. On real x86-64 Linux, `scripts/install.sh` still builds an AppImage.
- **Ctrl-C after a scan on Linux:** SANE can reset signal handlers during a pass (the server works around it); whether that affects stopping the desktop app from a terminal hasn't been checked.
- **Untested setups:** saving to a real SMB share, and a real Proxmox LXC container.
- **Open questions, if anyone picks this up:** whether the driver can take Wi-Fi settings over USB, whether the scanner can send JPEG over Wi-Fi to cut transfer time, and whether ICA reports "feeder empty" during a session.
