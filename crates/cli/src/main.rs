//! `janus-scan`: command-line front end over the same backends the app uses.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use scan_core::{
    Backend, CancelToken, ColorMode, DeviceInfo, Page, PaperSize, ScanEvent, ScanRequest, Source,
    dedupe_devices,
};

#[derive(Parser)]
#[command(name = "janus-scan", about = "Scan documents from the command line")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List scanners
    List,
    /// Show what a scanner supports
    Caps {
        /// Device id or name fragment; defaults to the first scanner
        #[arg(short, long)]
        device: Option<String>,
    },
    /// Scan pages to image files
    Scan {
        #[arg(short, long)]
        device: Option<String>,
        #[arg(long, default_value_t = 300)]
        dpi: u32,
        /// Scan both sides of each sheet
        #[arg(long)]
        duplex: bool,
        #[arg(long, value_enum, default_value_t = Color::Color)]
        color: Color,
        #[arg(long, value_enum, default_value_t = Paper::Letter)]
        paper: Paper,
        #[arg(long)]
        flatbed: bool,
        /// Ask for more pages each time the feeder runs empty
        #[arg(long)]
        multi: bool,
        /// Output folder
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Color {
    Color,
    Gray,
    Bw,
}

#[derive(Clone, Copy, ValueEnum)]
enum Paper {
    Letter,
    Legal,
    A4,
    A5,
    Card,
    Receipt,
    Auto,
}

fn backends() -> anyhow::Result<Vec<Box<dyn Backend>>> {
    #[allow(unused_mut)]
    let mut out: Vec<Box<dyn Backend>> = Vec::new();
    #[cfg(target_os = "macos")]
    out.push(Box::new(backend_ica::IcaBackend::new()));
    #[cfg(target_os = "linux")]
    match backend_sane::SaneBackend::new() {
        Ok(b) => out.push(Box::new(b)),
        Err(e) => eprintln!("SANE unavailable: {e}"),
    }
    if out.is_empty() {
        bail!("no scanner backend available on this system");
    }
    Ok(out)
}

fn all_devices(backends: &[Box<dyn Backend>]) -> Vec<(usize, DeviceInfo)> {
    let mut list = Vec::new();
    for (i, b) in backends.iter().enumerate() {
        match b.devices() {
            Ok(devs) => list.extend(devs.into_iter().map(|d| (i, d))),
            Err(e) => eprintln!("{}: {e}", b.name()),
        }
    }
    let kept = dedupe_devices(list.iter().map(|(_, d)| d.clone()).collect());
    list.retain(|(_, d)| kept.iter().any(|k| k.id == d.id && k.connection == d.connection));
    list
}

fn pick(backends: &[Box<dyn Backend>], query: Option<&str>) -> anyhow::Result<Box<dyn scan_core::Device>> {
    let devices = all_devices(backends);
    let found = devices
        .iter()
        .find(|(_, d)| query.is_none_or(|q| d.id.0 == q || d.name.to_lowercase().contains(&q.to_lowercase())))
        .context("no matching scanner found")?;
    eprintln!("Using {} ({:?})", found.1.name, found.1.connection);
    Ok(backends[found.0].open(&found.1.id)?)
}

fn save(page: &Page, out: &std::path::Path, n: usize) -> anyhow::Result<PathBuf> {
    let path = out.join(format!("scan_{n:03}.png"));
    page.image.save(&path).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn ask_more() -> bool {
    eprint!("Feeder empty. Load more pages and press Enter, or type 'd' + Enter to finish: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    !line.trim().eq_ignore_ascii_case("d")
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let backends = backends()?;
    match cli.cmd {
        Cmd::List => {
            let devices = all_devices(&backends);
            if devices.is_empty() {
                println!("No scanners found.");
            }
            for (_, d) in devices {
                println!("{:<24} {:<8?} {:<5} id={}", d.name, d.connection, d.backend, d.id.0);
            }
        }
        Cmd::Caps { device } => {
            let dev = pick(&backends, device.as_deref())?;
            println!("{:#?}", dev.capabilities());
        }
        Cmd::Scan { device, dpi, duplex, color, paper, flatbed, multi, out } => {
            std::fs::create_dir_all(&out)?;
            let mut dev = pick(&backends, device.as_deref())?;
            let req = ScanRequest {
                source: if flatbed { Source::Flatbed } else { Source::Feeder },
                duplex,
                dpi,
                color: match color {
                    Color::Color => ColorMode::Color,
                    Color::Gray => ColorMode::Gray,
                    Color::Bw => ColorMode::Bw,
                },
                paper: match paper {
                    Paper::Letter => PaperSize::Letter,
                    Paper::Legal => PaperSize::Legal,
                    Paper::A4 => PaperSize::A4,
                    Paper::A5 => PaperSize::A5,
                    Paper::Card => PaperSize::BusinessCard,
                    Paper::Receipt => PaperSize::Receipt,
                    Paper::Auto => PaperSize::Auto,
                },
            };
            let mut saved = 0;
            let started = Instant::now();
            loop {
                let (tx, rx) = mpsc::channel();
                let pass_start = Instant::now();
                dev.scan(&req, tx, CancelToken::default());
                let mut feeder_empty = false;
                for ev in rx.try_iter() {
                    match ev {
                        ScanEvent::Page(p) => {
                            saved += 1;
                            let path = save(&p, &out, saved)?;
                            eprintln!(
                                "page {saved}: {}x{} @ {} dpi -> {}",
                                p.image.width(),
                                p.image.height(),
                                p.dpi,
                                path.display()
                            );
                        }
                        ScanEvent::FeederEmpty => feeder_empty = true,
                        ScanEvent::Error(e) => bail!("{e}"),
                        ScanEvent::PageStarted(_) | ScanEvent::Done => {}
                    }
                }
                eprintln!("pass took {:.1}s", pass_start.elapsed().as_secs_f32());
                if !(multi && feeder_empty && ask_more()) {
                    break;
                }
            }
            eprintln!("{saved} page(s) in {:.1}s", started.elapsed().as_secs_f32());
        }
    }
    Ok(())
}

fn main() {
    let work = || match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    #[cfg(target_os = "macos")]
    backend_ica::run_with_main_loop(work);
    #[cfg(not(target_os = "macos"))]
    std::process::exit(work());
}
