//! Writing scanned pages to PDF, JPEG, PNG and TIFF files.
//!
//! - PDF is written directly with `lopdf`: color/gray pages as JPEG
//!   (DCTDecode), pure black/white pages as 1-bit Flate. Page size comes from
//!   pixels and dpi. The only metadata is Producer and an optional Title.
//! - JPEG/PNG get one file per page (`name_001.jpg`, ...); a single page has
//!   no suffix. TIFF can hold all pages in one file.
//! - Files are written to a hidden temp file in the target folder and then
//!   moved into place, never overwriting an existing file.

use std::fs::{self, File};
use std::io::{self, BufWriter, Cursor, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::Compression as FlateLevel;
use flate2::write::ZlibEncoder;
use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GrayImage, ImageEncoder, ImageError, RgbImage};
use lopdf::{Dictionary, Document, Object, Stream, StringFormat, dictionary};
use serde::{Deserialize, Serialize};
use tiff::encoder::colortype::{self, ColorType};
use tiff::encoder::compression::DeflateLevel;
use tiff::encoder::{Compression as TiffCompression, Rational, TiffEncoder};
use tiff::tags::ResolutionUnit;

pub use chrono::NaiveDateTime;

/// Value written to the PDF `Producer` field.
pub const PRODUCER: &str = "Janus Scan";

/// Default when a page reports no dpi.
const FALLBACK_DPI: u32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Pdf,
    Jpeg,
    Png,
    Tiff,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Tiff => "tif",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportOptions {
    pub format: OutputFormat,
    /// 1..=100; used for JPEG files and JPEG-compressed PDF pages.
    pub jpeg_quality: u8,
    /// TIFF only: all pages in one file instead of one file per page.
    pub multi_page_tiff: bool,
    pub pdf_title: Option<String>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { format: OutputFormat::Pdf, jpeg_quality: 85, multi_page_tiff: true, pdf_title: None }
    }
}

/// One page to write. Pure black/white pages should be Luma8 holding only
/// 0 and 255 (what `imaging::apply` produces); PDF stores those as 1-bit.
#[derive(Debug, Clone)]
pub struct ExportPage {
    pub image: DynamicImage,
    pub dpi: u32,
}

impl From<scan_core::Page> for ExportPage {
    fn from(p: scan_core::Page) -> Self {
        Self { image: p.image, dpi: p.dpi }
    }
}

impl From<&scan_core::Page> for ExportPage {
    fn from(p: &scan_core::Page) -> Self {
        Self { image: p.image.clone(), dpi: p.dpi }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("there are no pages to save")]
    NoPages,
    #[error("could not write the file: {0}")]
    Io(#[from] io::Error),
    #[error("could not encode the image: {0}")]
    Image(#[from] ImageError),
    #[error("could not build the PDF: {0}")]
    Pdf(#[from] lopdf::Error),
    #[error("could not write the TIFF: {0}")]
    Tiff(#[from] tiff::TiffError),
}

pub type Result<T, E = ExportError> = std::result::Result<T, E>;

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// Expand `{date}` (YYYY-MM-DD), `{time}` (HHMMSS) and `{n}` (3-digit
/// counter) and make the result safe as a file stem.
pub fn render_name(template: &str, now: NaiveDateTime, n: u32) -> String {
    let s = template
        .replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{time}", &now.format("%H%M%S").to_string())
        .replace("{n}", &format!("{n:03}"));
    sanitize_filename(&s)
}

/// Replace path separators, characters other systems reject and control
/// characters with `_`; trim leading/trailing dots and spaces (no hidden or
/// relative names). Falls back to "scan" when nothing is left.
pub fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    // Leave room for " (999)", "_001" and the extension within 255 bytes.
    const MAX: usize = 200;
    if s.len() > MAX {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s = s.trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    }
    if s.is_empty() { "scan".to_string() } else { s }
}

/// `dir/stem.ext`, or `dir/stem (2).ext`, `dir/stem (3).ext`, ... if taken.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let make = |suffix: String| dir.join(format!("{stem}{suffix}.{ext}"));
    let first = make(String::new());
    if !exists(&first) {
        return first;
    }
    (2u32..)
        .map(|i| make(format!(" ({i})")))
        .find(|p| !exists(p))
        .expect("some suffix is free")
}

fn exists(p: &Path) -> bool {
    // symlink_metadata so a dangling symlink also counts as taken.
    fs::symlink_metadata(p).is_ok()
}

fn per_page_files(count: usize, opts: &ExportOptions) -> bool {
    match opts.format {
        OutputFormat::Pdf => false,
        OutputFormat::Tiff if opts.multi_page_tiff => false,
        _ => count > 1,
    }
}

fn page_stem(stem: &str, index: usize, per_page: bool) -> String {
    if per_page { format!("{stem}_{:03}", index + 1) } else { stem.to_string() }
}

// ---------------------------------------------------------------------------
// Export entry points
// ---------------------------------------------------------------------------

/// Write `pages` into `dir` using the local time for `{date}`/`{time}`.
/// `{n}` becomes the lowest counter (from 1) whose file doesn't exist yet,
/// so repeated exports with `scan_{n}` give `scan_001`, `scan_002`, ...
/// Returns the paths written, in page order.
pub fn export(
    pages: &[ExportPage],
    dir: &Path,
    name_template: &str,
    opts: &ExportOptions,
) -> Result<Vec<PathBuf>> {
    let now = chrono::Local::now().naive_local();
    let mut n = 1;
    if name_template.contains("{n}") {
        let per_page = per_page_files(pages.len(), opts);
        let ext = opts.format.extension();
        while n < 100_000 {
            let stem = page_stem(&render_name(name_template, now, n), 0, per_page);
            if !exists(&dir.join(format!("{stem}.{ext}"))) {
                break;
            }
            n += 1;
        }
    }
    export_at(pages, dir, name_template, now, n, opts)
}

/// Like [`export`] with an explicit time and counter.
pub fn export_at(
    pages: &[ExportPage],
    dir: &Path,
    name_template: &str,
    now: NaiveDateTime,
    n: u32,
    opts: &ExportOptions,
) -> Result<Vec<PathBuf>> {
    if pages.is_empty() {
        return Err(ExportError::NoPages);
    }
    let stem = render_name(name_template, now, n);
    let ext = opts.format.extension();
    let quality = opts.jpeg_quality.clamp(1, 100);
    match opts.format {
        OutputFormat::Pdf => {
            let path = write_atomic(dir, &stem, ext, |w| {
                let mut doc = build_pdf(pages, opts)?;
                doc.save_to(w)?;
                Ok(())
            })?;
            Ok(vec![path])
        }
        OutputFormat::Tiff if opts.multi_page_tiff => {
            let path = write_atomic(dir, &stem, ext, |w| write_tiff(w, pages))?;
            Ok(vec![path])
        }
        format => {
            let per_page = per_page_files(pages.len(), opts);
            let mut out = Vec::with_capacity(pages.len());
            for (i, page) in pages.iter().enumerate() {
                let bytes = encode_page(page, format, quality)?;
                let path = write_atomic(dir, &page_stem(&stem, i, per_page), ext, |w| {
                    w.write_all(&bytes)?;
                    Ok(())
                })?;
                out.push(path);
            }
            Ok(out)
        }
    }
}

/// Encode one page as a standalone JPEG, PNG or single-page TIFF (a PDF
/// with just this page for [`OutputFormat::Pdf`]).
pub fn encode_page(page: &ExportPage, format: OutputFormat, jpeg_quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match format {
        OutputFormat::Jpeg => {
            let mut enc = JpegEncoder::new_with_quality(&mut buf, jpeg_quality.clamp(1, 100));
            enc.set_pixel_density(PixelDensity::dpi(dpi_of(page).min(u16::MAX as u32) as u16));
            match classify(&page.image) {
                Kind::Rgb => enc.encode_image(&DynamicImage::ImageRgb8(rgb8(&page.image)))?,
                _ => enc.encode_image(&DynamicImage::ImageLuma8(gray8(&page.image)))?,
            }
        }
        OutputFormat::Png => {
            let enc = PngEncoder::new(&mut buf);
            match classify(&page.image) {
                Kind::Rgb => {
                    let img = rgb8(&page.image);
                    enc.write_image(&img, img.width(), img.height(), image::ExtendedColorType::Rgb8)?
                }
                _ => {
                    let img = gray8(&page.image);
                    enc.write_image(&img, img.width(), img.height(), image::ExtendedColorType::L8)?
                }
            }
        }
        OutputFormat::Tiff => write_tiff(&mut Cursor::new(&mut buf), std::slice::from_ref(page))?,
        OutputFormat::Pdf => {
            let opts = ExportOptions { jpeg_quality, ..Default::default() };
            build_pdf(std::slice::from_ref(page), &opts)?.save_to(&mut buf)?;
        }
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Page classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bw,
    Gray,
    Rgb,
}

fn classify(img: &DynamicImage) -> Kind {
    match img {
        DynamicImage::ImageLuma8(g) => {
            if g.as_raw().iter().all(|&p| p == 0 || p == 255) {
                Kind::Bw
            } else {
                Kind::Gray
            }
        }
        DynamicImage::ImageLuma16(_) | DynamicImage::ImageLumaA8(_) | DynamicImage::ImageLumaA16(_) => {
            Kind::Gray
        }
        _ => Kind::Rgb,
    }
}

fn gray8(img: &DynamicImage) -> GrayImage {
    match img {
        DynamicImage::ImageLuma8(g) => g.clone(),
        o => o.to_luma8(),
    }
}

fn rgb8(img: &DynamicImage) -> RgbImage {
    match img {
        DynamicImage::ImageRgb8(r) => r.clone(),
        o => o.to_rgb8(),
    }
}

fn dpi_of(page: &ExportPage) -> u32 {
    if page.dpi == 0 { FALLBACK_DPI } else { page.dpi }
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

/// Pack a 0/255 gray image into 1 bit per pixel rows, MSB first, with
/// 1 = white as DeviceGray expects.
fn pack_bits(g: &GrayImage) -> Vec<u8> {
    let (w, h) = (g.width() as usize, g.height() as usize);
    let row_bytes = w.div_ceil(8);
    let mut out = vec![0u8; row_bytes * h];
    for (row, dst) in g.as_raw().chunks_exact(w.max(1)).zip(out.chunks_exact_mut(row_bytes.max(1))) {
        for (x, &p) in row.iter().enumerate() {
            if p >= 128 {
                dst[x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    out
}

fn flate(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::with_capacity(data.len() / 8), FlateLevel::default());
    enc.write_all(data)?;
    enc.finish()
}

fn jpeg_bytes(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100)).encode_image(img)?;
    Ok(buf)
}

/// Image XObject for a page: (dictionary, encoded data).
fn pdf_image(img: &DynamicImage, quality: u8) -> Result<(Dictionary, Vec<u8>)> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => w,
        "Height" => h,
    };
    let data = match classify(img) {
        Kind::Bw => {
            dict.set("ColorSpace", "DeviceGray");
            dict.set("BitsPerComponent", 1);
            dict.set("Filter", "FlateDecode");
            flate(&pack_bits(&gray8(img)))?
        }
        Kind::Gray => {
            dict.set("ColorSpace", "DeviceGray");
            dict.set("BitsPerComponent", 8);
            dict.set("Filter", "DCTDecode");
            jpeg_bytes(&DynamicImage::ImageLuma8(gray8(img)), quality)?
        }
        Kind::Rgb => {
            dict.set("ColorSpace", "DeviceRGB");
            dict.set("BitsPerComponent", 8);
            dict.set("Filter", "DCTDecode");
            jpeg_bytes(&DynamicImage::ImageRgb8(rgb8(img)), quality)?
        }
    };
    Ok((dict, data))
}

fn pdf_text(s: &str) -> Object {
    if s.is_ascii() {
        Object::string_literal(s)
    } else {
        // UTF-16BE with a byte order mark, as PDF text strings require.
        let mut bytes = vec![0xFE, 0xFF];
        bytes.extend(s.encode_utf16().flat_map(|u| u.to_be_bytes()));
        Object::String(bytes, StringFormat::Hexadecimal)
    }
}

/// Page size in PDF points for a page.
pub fn page_size_points(page: &ExportPage) -> (f32, f32) {
    let dpi = dpi_of(page) as f32;
    (page.image.width() as f32 * 72.0 / dpi, page.image.height() as f32 * 72.0 / dpi)
}

fn build_pdf(pages: &[ExportPage], opts: &ExportOptions) -> Result<Document> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::with_capacity(pages.len());
    for page in pages {
        let (wpt, hpt) = page_size_points(page);
        let (dict, data) = pdf_image(&page.image, opts.jpeg_quality)?;
        let img_id = doc.add_object(Stream::new(dict, data).with_compression(false));
        let content = format!("q {wpt:.4} 0 0 {hpt:.4} 0 0 cm /Im0 Do Q").into_bytes();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), wpt.into(), hpt.into()],
            "Contents" => content_id,
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => img_id },
            },
        });
        kids.push(Object::Reference(page_id));
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    let mut info = dictionary! { "Producer" => Object::string_literal(PRODUCER) };
    if let Some(title) = opts.pdf_title.as_deref().filter(|t| !t.trim().is_empty()) {
        info.set("Title", pdf_text(title));
    }
    let info_id = doc.add_object(info);
    doc.trailer.set("Root", catalog_id);
    doc.trailer.set("Info", info_id);
    Ok(doc)
}

// ---------------------------------------------------------------------------
// TIFF
// ---------------------------------------------------------------------------

fn tiff_page<W: Write + Seek, C: ColorType<Inner = u8>>(
    enc: &mut TiffEncoder<W>,
    w: u32,
    h: u32,
    dpi: u32,
    data: &[u8],
) -> Result<()> {
    let mut img = enc.new_image::<C>(w, h)?;
    img.resolution(ResolutionUnit::Inch, Rational { n: dpi, d: 1 });
    img.write_data(data)?;
    Ok(())
}

/// All pages into one TIFF (Deflate). Black/white pages are stored as 8-bit
/// gray, which Deflate shrinks well.
fn write_tiff<W: Write + Seek>(w: &mut W, pages: &[ExportPage]) -> Result<()> {
    let mut enc = TiffEncoder::new(w)?.with_compression(TiffCompression::Deflate(DeflateLevel::Balanced));
    for page in pages {
        let dpi = dpi_of(page);
        match classify(&page.image) {
            Kind::Rgb => {
                let img = rgb8(&page.image);
                tiff_page::<_, colortype::RGB8>(&mut enc, img.width(), img.height(), dpi, &img)?;
            }
            _ => {
                let img = gray8(&page.image);
                tiff_page::<_, colortype::Gray8>(&mut enc, img.width(), img.height(), dpi, &img)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Atomic writes
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write through `fill` into a hidden temp file in `dir`, then move it to a
/// free name from [`unique_path`]. Never replaces an existing file.
fn write_atomic(
    dir: &Path,
    stem: &str,
    ext: &str,
    fill: impl FnOnce(&mut BufWriter<File>) -> Result<()>,
) -> Result<PathBuf> {
    let tmp = loop {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = dir.join(format!(".{stem}.{}.{n}.tmp", std::process::id()));
        if !exists(&p) {
            break p;
        }
    };
    let result = (|| -> Result<PathBuf> {
        let file = File::options().write(true).create_new(true).open(&tmp)?;
        let mut w = BufWriter::new(file);
        fill(&mut w)?;
        let file = w.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        for _ in 0..1000 {
            let dest = unique_path(dir, stem, ext);
            // A hard link fails instead of overwriting if another writer took
            // the name in the meantime. Some file systems (FAT/exFAT sticks)
            // have no hard links; fall back to rename there.
            match fs::hard_link(&tmp, &dest) {
                Ok(()) => {
                    fs::remove_file(&tmp)?;
                    return Ok(dest);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    fs::rename(&tmp, &dest)?;
                    return Ok(dest);
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free file name").into())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

// ---------------------------------------------------------------------------
// Size estimate
// ---------------------------------------------------------------------------

/// Rough output size in bytes: encodes downscaled copies of up to three
/// sample pages and extrapolates by pixel count and page count.
pub fn estimate_size(pages: &[ExportPage], opts: &ExportOptions) -> u64 {
    if pages.is_empty() {
        return 0;
    }
    const MAX_SIDE: u32 = 1000;
    let samples: Vec<&ExportPage> = match pages.len() {
        1..=3 => pages.iter().collect(),
        n => vec![&pages[0], &pages[n / 2], &pages[n - 1]],
    };
    let quality = opts.jpeg_quality.clamp(1, 100);
    let mut per_page = 0f64;
    for page in &samples {
        let img = &page.image;
        let full_px = img.width() as f64 * img.height() as f64;
        let small = if img.width().max(img.height()) > MAX_SIDE {
            let kind = classify(img);
            let s = img.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle);
            if kind == Kind::Bw {
                let mut g = s.to_luma8();
                g.iter_mut().for_each(|p| *p = if *p >= 128 { 255 } else { 0 });
                DynamicImage::ImageLuma8(g)
            } else {
                s
            }
        } else {
            img.clone()
        };
        let small_px = (small.width() as f64 * small.height() as f64).max(1.0);
        let bytes = match opts.format {
            OutputFormat::Pdf => pdf_image(&small, quality).map(|(_, d)| d.len()),
            f => encode_page(&ExportPage { image: small, dpi: page.dpi }, f, quality).map(|b| b.len()),
        }
        .unwrap_or(0) as f64;
        per_page += bytes * full_px / small_px;
    }
    per_page /= samples.len() as f64;
    let overhead = match opts.format {
        OutputFormat::Pdf => 600.0 + 250.0 * pages.len() as f64,
        _ => 0.0,
    };
    (per_page * pages.len() as f64 + overhead).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb};
    use std::io::Read;

    fn when() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 17).unwrap().and_hms_opt(9, 30, 5).unwrap()
    }

    fn color_page(w: u32, h: u32, dpi: u32) -> ExportPage {
        let img = RgbImage::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        ExportPage { image: DynamicImage::ImageRgb8(img), dpi }
    }

    fn gray_page(w: u32, h: u32, dpi: u32) -> ExportPage {
        let img = GrayImage::from_fn(w, h, |x, y| Luma([((x + y) % 200 + 20) as u8]));
        ExportPage { image: DynamicImage::ImageLuma8(img), dpi }
    }

    /// Black left half, white right half, plus one black pixel at (w-1, 0).
    fn bw_page(w: u32, h: u32, dpi: u32) -> ExportPage {
        let mut img = GrayImage::from_fn(w, h, |x, _| Luma([if x < w / 2 { 0 } else { 255 }]));
        img.put_pixel(w - 1, 0, Luma([0]));
        ExportPage { image: DynamicImage::ImageLuma8(img), dpi }
    }

    fn mixed() -> Vec<ExportPage> {
        vec![color_page(850, 1100, 100), gray_page(600, 600, 200), bw_page(301, 150, 300)]
    }

    fn name_of(o: &Object) -> String {
        String::from_utf8(o.as_name().unwrap().to_vec()).unwrap()
    }

    fn num(o: &Object) -> f32 {
        o.as_float().or_else(|_| o.as_i64().map(|i| i as f32)).unwrap()
    }

    fn no_temp_files(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let name = e.unwrap().file_name().into_string().unwrap();
            assert!(!name.ends_with(".tmp"), "leftover temp file {name}");
        }
    }

    #[test]
    fn pdf_mixed_pages() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ExportOptions { pdf_title: Some("Tax papers".into()), ..Default::default() };
        let paths = export_at(&mixed(), dir.path(), "{date}_doc", when(), 1, &opts).unwrap();
        assert_eq!(paths, vec![dir.path().join("2026-10-17_doc.pdf")]);
        no_temp_files(dir.path());

        let doc = Document::load(&paths[0]).unwrap();
        let pages = doc.get_pages();
        assert_eq!(pages.len(), 3);

        let expect = [
            ((612.0, 792.0), "DCTDecode", "DeviceRGB", 8),
            ((216.0, 216.0), "DCTDecode", "DeviceGray", 8),
            ((72.24, 36.0), "FlateDecode", "DeviceGray", 1),
        ];
        for ((_, &page_id), (size, filter, cs, bpc)) in pages.iter().zip(expect) {
            let page = doc.get_dictionary(page_id).unwrap();
            let mb = page.get(b"MediaBox").unwrap().as_array().unwrap();
            assert!((num(&mb[2]) - size.0).abs() < 0.01, "width {}", num(&mb[2]));
            assert!((num(&mb[3]) - size.1).abs() < 0.01, "height {}", num(&mb[3]));

            let res = page.get(b"Resources").unwrap().as_dict().unwrap();
            let xobj = res.get(b"XObject").unwrap().as_dict().unwrap();
            let img_id = xobj.get(b"Im0").unwrap().as_reference().unwrap();
            let stream = doc.get_object(img_id).unwrap().as_stream().unwrap();
            assert_eq!(name_of(stream.dict.get(b"Filter").unwrap()), filter);
            assert_eq!(name_of(stream.dict.get(b"ColorSpace").unwrap()), cs);
            assert_eq!(stream.dict.get(b"BitsPerComponent").unwrap().as_i64().unwrap(), bpc);

            if bpc == 1 {
                // 301 px wide -> 38 bytes per row. Row 0: 150 black pixels
                // (bits 0), white (bits 1), last pixel black.
                let mut raw = Vec::new();
                flate2::read::ZlibDecoder::new(&stream.content[..]).read_to_end(&mut raw).unwrap();
                assert_eq!(raw.len(), 38 * 150);
                assert_eq!(raw[0], 0x00);
                assert_eq!(raw[19], 0xFF);
                // Byte 37 holds pixels 296..=300 plus 3 zero padding bits.
                assert_eq!(raw[37], 0b1111_0000);
                assert_eq!(raw[38 + 37], 0b1111_1000);
            } else {
                assert_eq!(&stream.content[..2], &[0xFF, 0xD8], "JPEG data");
            }
        }

        let info_id = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info = doc.get_dictionary(info_id).unwrap();
        let keys: Vec<_> = info.iter().map(|(k, _)| String::from_utf8(k.clone()).unwrap()).collect();
        assert_eq!(keys.len(), 2, "only Producer and Title: {keys:?}");
        assert_eq!(info.get(b"Producer").unwrap().as_str().unwrap(), b"Janus Scan");
        assert_eq!(info.get(b"Title").unwrap().as_str().unwrap(), b"Tax papers");
    }

    #[test]
    fn pdf_without_title_has_only_producer() {
        let bytes = encode_page(&bw_page(64, 64, 200), OutputFormat::Pdf, 85).unwrap();
        let doc = Document::load_mem(&bytes).unwrap();
        let info_id = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        assert_eq!(doc.get_dictionary(info_id).unwrap().len(), 1);
    }

    #[test]
    fn pdf_unicode_title() {
        let opts = ExportOptions { pdf_title: Some("Квитанция".into()), ..Default::default() };
        let mut buf = Vec::new();
        build_pdf(&[gray_page(10, 10, 72)], &opts).unwrap().save_to(&mut buf).unwrap();
        let doc = Document::load_mem(&buf).unwrap();
        let info_id = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let title = doc.get_dictionary(info_id).unwrap().get(b"Title").unwrap().as_str().unwrap().to_vec();
        assert_eq!(&title[..2], &[0xFE, 0xFF]);
    }

    fn tiff_page_sizes(path: &Path) -> Vec<(u32, u32)> {
        let mut dec = tiff::decoder::Decoder::new(File::open(path).unwrap()).unwrap();
        let mut sizes = vec![dec.dimensions().unwrap()];
        while dec.more_images() {
            dec.next_image().unwrap();
            sizes.push(dec.dimensions().unwrap());
        }
        sizes
    }

    #[test]
    fn tiff_multi_page_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ExportOptions { format: OutputFormat::Tiff, ..Default::default() };
        let paths = export_at(&mixed(), dir.path(), "scan", when(), 1, &opts).unwrap();
        assert_eq!(paths, vec![dir.path().join("scan.tif")]);
        assert_eq!(tiff_page_sizes(&paths[0]), vec![(850, 1100), (600, 600), (301, 150)]);

        // Pixels survive: decode the last page and compare.
        let mut dec = tiff::decoder::Decoder::new(File::open(&paths[0]).unwrap()).unwrap();
        dec.next_image().unwrap();
        dec.next_image().unwrap();
        let mut data = tiff::decoder::DecodingResult::U8(vec![]);
        dec.read_image_to_buffer(&mut data).unwrap();
        let tiff::decoder::DecodingResult::U8(px) = data else { panic!("8-bit expected") };
        assert_eq!(px, gray8(&bw_page(301, 150, 300).image).into_raw());
        no_temp_files(dir.path());
    }

    #[test]
    fn tiff_one_file_per_page() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ExportOptions { format: OutputFormat::Tiff, multi_page_tiff: false, ..Default::default() };
        let paths = export_at(&mixed(), dir.path(), "scan", when(), 1, &opts).unwrap();
        let names: Vec<_> = paths.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["scan_001.tif", "scan_002.tif", "scan_003.tif"]);
        assert_eq!(tiff_page_sizes(&paths[1]), vec![(600, 600)]);
    }

    #[test]
    fn jpeg_and_png_naming() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = ExportOptions { format: OutputFormat::Jpeg, ..Default::default() };
        let paths = export_at(&mixed(), dir.path(), "r", when(), 1, &jpeg).unwrap();
        let names: Vec<_> = paths.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["r_001.jpg", "r_002.jpg", "r_003.jpg"]);
        let decoded = image::open(&paths[0]).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (850, 1100));

        let png = ExportOptions { format: OutputFormat::Png, ..Default::default() };
        let one = export_at(&mixed()[2..], dir.path(), "r", when(), 1, &png).unwrap();
        assert_eq!(one, vec![dir.path().join("r.png")]);
        // Exporting again doesn't overwrite.
        let again = export_at(&mixed()[2..], dir.path(), "r", when(), 1, &png).unwrap();
        assert_eq!(again, vec![dir.path().join("r (2).png")]);
        let back = image::open(&one[0]).unwrap().to_luma8();
        assert_eq!(back, gray8(&mixed()[2].image));
        no_temp_files(dir.path());
    }

    #[test]
    fn auto_counter_picks_next_free_number() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ExportOptions::default();
        let a = export(&mixed()[..1], dir.path(), "scan_{n}", &opts).unwrap();
        let b = export(&mixed()[..1], dir.path(), "scan_{n}", &opts).unwrap();
        assert_eq!(a, vec![dir.path().join("scan_001.pdf")]);
        assert_eq!(b, vec![dir.path().join("scan_002.pdf")]);
    }

    #[test]
    fn empty_export_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            export(&[], dir.path(), "x", &ExportOptions::default()),
            Err(ExportError::NoPages)
        ));
    }

    #[test]
    fn render_name_expands_and_sanitizes() {
        assert_eq!(render_name("{date}_{time}_{n}", when(), 7), "2026-10-17_093005_007");
        assert_eq!(render_name("scan {n}", when(), 1234), "scan 1234");
        let s = render_name("../../etc/pass:wd*?{n}", when(), 1);
        assert!(!s.contains('/') && !s.contains(':') && !s.contains('*'), "{s}");
        assert!(!s.starts_with('.'), "{s}");
        assert_eq!(render_name("  ..  ", when(), 1), "scan");
        assert_eq!(render_name("a\u{0}b\nc", when(), 1), "a_b_c");
        assert!(render_name(&"é".repeat(300), when(), 1).len() <= 200);
    }

    #[test]
    fn unique_path_adds_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = unique_path(dir.path(), "scan", "pdf");
        assert_eq!(p1, dir.path().join("scan.pdf"));
        fs::write(&p1, b"x").unwrap();
        let p2 = unique_path(dir.path(), "scan", "pdf");
        assert_eq!(p2, dir.path().join("scan (2).pdf"));
        fs::write(&p2, b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "scan", "pdf"), dir.path().join("scan (3).pdf"));
        assert_eq!(unique_path(dir.path(), "scan", "jpg"), dir.path().join("scan.jpg"));
    }

    #[test]
    fn estimate_is_in_the_right_ballpark() {
        // Noisy pages so the encoded size isn't trivially tiny.
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        let big = GrayImage::from_fn(2000, 2600, |x, y| Luma([((x / 7 + y / 9) % 200) as u8 / 2 + noise() / 4]));
        let pages = vec![
            ExportPage { image: DynamicImage::ImageLuma8(big.clone()), dpi: 300 },
            ExportPage { image: DynamicImage::ImageLuma8(big), dpi: 300 },
        ];
        let dir = tempfile::tempdir().unwrap();
        for format in [OutputFormat::Pdf, OutputFormat::Jpeg, OutputFormat::Tiff] {
            let opts = ExportOptions { format, ..Default::default() };
            let est = estimate_size(&pages, &opts) as f64;
            let actual: u64 = export_at(&pages, dir.path(), "e", when(), 1, &opts)
                .unwrap()
                .iter()
                .map(|p| fs::metadata(p).unwrap().len())
                .sum();
            let ratio = est / actual as f64;
            assert!((0.25..=4.0).contains(&ratio), "{format:?}: estimate {est} vs actual {actual}");
        }
        assert_eq!(estimate_size(&[], &ExportOptions::default()), 0);
    }
}
