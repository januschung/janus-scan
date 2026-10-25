//! Saving the shared document: as a download to the browser that asked, or
//! into one of the NAS folders named in the config file. The browser only
//! ever sees destination names, never paths.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use export::{ExportOptions, ExportPage, OutputFormat};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ApiResult, Shared, blocking};

/// The server's config file (`JANUS_CONFIG`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub destinations: Vec<Destination>,
}

/// A folder pages can be saved to, usually a mounted NAS share.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub name: String,
    pub path: PathBuf,
    /// File name preset for this folder; the browser's own otherwise.
    #[serde(default)]
    pub name_template: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
        let mut seen = std::collections::HashSet::new();
        for d in &config.destinations {
            if d.name.trim().is_empty() {
                return Err("a destination has an empty name".into());
            }
            if !seen.insert(d.name.as_str()) {
                return Err(format!("two destinations are called \"{}\"", d.name));
            }
            if !d.path.is_absolute() {
                return Err(format!("the path of \"{}\" must be absolute: {}", d.name, d.path.display()));
            }
        }
        Ok(config)
    }
}

#[derive(Serialize)]
pub struct DestinationView {
    name: String,
    /// Set when the folder has its own file name preset.
    name_template: Option<String>,
}

pub async fn list(State(s): State<Shared>) -> Json<Vec<DestinationView>> {
    Json(
        s.destinations
            .iter()
            .map(|d| DestinationView { name: d.name.clone(), name_template: d.name_template.clone() })
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct SaveRequest {
    /// A destination's name, or `None` to download.
    #[serde(default)]
    destination: Option<String>,
    name_template: String,
    #[serde(flatten)]
    export: ExportOptions,
}

#[derive(Serialize)]
struct Saved {
    /// File names only.
    files: Vec<String>,
}

fn load_pages(s: &Shared) -> ApiResult<Vec<ExportPage>> {
    let doc = s.doc.lock().unwrap();
    let pages = doc
        .ids()
        .into_iter()
        .map(|id| doc.load(id).map(|(image, dpi)| ExportPage { image, dpi }))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, e))?;
    if pages.is_empty() {
        return Err(ApiError::new(StatusCode::CONFLICT, "There are no pages to save."));
    }
    Ok(pages)
}

fn export_error(e: export::ExportError) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't save: {e}"))
}

pub async fn save(State(s): State<Shared>, Json(req): Json<SaveRequest>) -> ApiResult<Response> {
    s.touch();
    let st = s.clone();
    blocking(move || match &req.destination {
        Some(name) => to_destination(&st, name, &req),
        None => download(&st, &req),
    })
    .await?
}

fn to_destination(s: &Shared, name: &str, req: &SaveRequest) -> ApiResult<Response> {
    let dest = s
        .destinations
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("There's no folder called \"{name}\".")))?;
    if !dest.path.is_dir() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("The \"{}\" folder isn't available. Check that the NAS share is mounted.", dest.name),
        ));
    }
    let pages = load_pages(s)?;
    let template = dest.name_template.as_deref().unwrap_or(&req.name_template);
    let paths = export::export(&pages, &dest.path, template, &req.export).map_err(export_error)?;
    let files = paths.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
    Ok(Json(Saved { files }).into_response())
}

/// A private folder for one download, removed when dropped. Named like the
/// page store's folders, so a crash leaves nothing behind after the next start.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "{}{}-download-{}",
            crate::TEMP_PREFIX,
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn download(s: &Shared, req: &SaveRequest) -> ApiResult<Response> {
    let pages = load_pages(s)?;
    let io = |e: std::io::Error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't save: {e}"));
    let scratch = Scratch::new().map_err(io)?;
    let paths = export::export(&pages, &scratch.0, &req.name_template, &req.export).map_err(export_error)?;
    let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let (name, content_type, body) = if let [one] = paths.as_slice() {
        (name_of(one), mime(req.export.format), std::fs::read(one).map_err(io)?)
    } else {
        // One file per page (JPEG, PNG): bundle them, uncompressed since
        // they already are.
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for p in &paths {
            zip.start_file(name_of(p), opts).map_err(|e| io(e.into()))?;
            zip.write_all(&std::fs::read(p).map_err(io)?).map_err(io)?;
        }
        let bytes = zip.finish().map_err(|e| io(e.into()))?.into_inner();
        // "Scan …_001.jpg" -> "Scan ….zip"
        let first = name_of(&paths[0]);
        let stem = first.rsplit_once('.').map_or(first.as_str(), |(stem, _)| stem);
        (format!("{}.zip", stem.strip_suffix("_001").unwrap_or(stem)), "application/zip", bytes)
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, attachment(&name)),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}

fn mime(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Pdf => "application/pdf",
        OutputFormat::Jpeg => "image/jpeg",
        OutputFormat::Png => "image/png",
        OutputFormat::Tiff => "image/tiff",
    }
}

/// `Content-Disposition` with an ASCII fallback and the exact UTF-8 name.
fn attachment(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' }).collect();
    let ascii = ascii.replace(['"', '\\'], "_");
    let encoded: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Size hint for the Save section: one page encoded, times the page count.
pub async fn estimate(State(s): State<Shared>, Json(opts): Json<ExportOptions>) -> ApiResult<Json<u64>> {
    let st = s.clone();
    blocking(move || {
        let (first, count) = {
            let doc = st.doc.lock().unwrap();
            let first = doc.ids().first().map(|&id| doc.load(id)).transpose().map_err(|e| ApiError::new(StatusCode::CONFLICT, e))?;
            (first, doc.len())
        };
        let Some((image, dpi)) = first else { return Ok(Json(0)) };
        Ok(Json(export::estimate_size(&[ExportPage { image, dpi }], &opts) * count as u64))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_checks_names_and_paths() {
        let ok = Config::parse("destinations:\n  - name: Receipts\n    path: /nas/receipts\n    name_template: \"R {n}\"\n").unwrap();
        assert_eq!(ok.destinations[0].name_template.as_deref(), Some("R {n}"));
        assert!(Config::parse("").unwrap().destinations.is_empty());
        assert!(Config::parse("destinations:\n  - name: A\n    path: nas/a\n").unwrap_err().contains("absolute"));
        let dup = "destinations:\n  - name: A\n    path: /a\n  - name: A\n    path: /b\n";
        assert!(Config::parse(dup).unwrap_err().contains("two destinations"));
        assert!(Config::parse("destinations:\n  - name: A\n    path: /a\n    colour: red\n").is_err());
    }

    #[test]
    fn attachment_header_keeps_unicode_name() {
        assert_eq!(
            attachment("Scan 2026-10-18 é.pdf"),
            "attachment; filename=\"Scan 2026-10-18 _.pdf\"; filename*=UTF-8''Scan%202026-10-18%20%C3%A9.pdf"
        );
    }
}
