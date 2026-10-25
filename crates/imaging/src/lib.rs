//! Page image processing: thumbnails, color conversion, blank-page detection,
//! auto-crop, deskew and rotation.
//!
//! Everything here is a pure function on [`DynamicImage`] / [`GrayImage`].
//! Detection (blank, crop, skew) runs on a box-downscaled grayscale copy so a
//! 2500x3300 page at 300 dpi stays cheap on old hardware; only the final
//! transform touches the full-size image.

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Luma, Rgb, RgbImage};
use scan_core::ColorMode;
use serde::{Deserialize, Serialize};

/// Largest skew [`apply`] will try to correct, in degrees.
pub const DESKEW_MAX_DEGREES: f32 = 10.0;

/// Skew angles smaller than this (degrees) are left alone.
pub const DESKEW_MIN_DEGREES: f32 = 0.2;

/// How a grayscale image is turned into pure black and white.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Threshold {
    /// Pick the level automatically (Otsu's method).
    #[default]
    Otsu,
    /// Pixels at or above this level become white, the rest black.
    Manual(u8),
}

/// Quarter-turn rotation, clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rotation {
    R90,
    R180,
    R270,
}

/// Post-scan processing settings, run by [`apply`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Processing {
    pub color: ColorMode,
    pub auto_crop: bool,
    pub deskew: bool,
    /// Added to every channel value, roughly -255..=255; 0 = unchanged.
    pub brightness: i32,
    /// Multiplier around mid-gray; 1.0 = unchanged.
    pub contrast: f32,
    /// Used only when `color` is [`ColorMode::Bw`].
    pub bw_threshold: Threshold,
}

impl Default for Processing {
    fn default() -> Self {
        Self {
            color: ColorMode::Color,
            auto_crop: false,
            deskew: false,
            brightness: 0,
            contrast: 1.0,
            bw_threshold: Threshold::Otsu,
        }
    }
}

// ---------------------------------------------------------------------------
// Resizing
// ---------------------------------------------------------------------------

fn fit_within(img: &DynamicImage, max_side: u32, filter: FilterType) -> DynamicImage {
    let max_side = max_side.max(1);
    let (w, h) = img.dimensions();
    if w <= max_side && h <= max_side {
        return img.clone();
    }
    img.resize(max_side, max_side, filter)
}

/// Small image for the page strip. Keeps the aspect ratio, never upscales,
/// uses the fast Triangle filter.
pub fn thumbnail(img: &DynamicImage, max_side: u32) -> DynamicImage {
    fit_within(img, max_side, FilterType::Triangle)
}

/// Larger view of the selected page, so the UI never loads the full scan.
/// Same rules as [`thumbnail`]; Triangle is effectively area averaging at
/// these reduction ratios, which is sharp enough for a screen-sized view.
pub fn preview(img: &DynamicImage, max_side: u32) -> DynamicImage {
    fit_within(img, max_side, FilterType::Triangle)
}

// ---------------------------------------------------------------------------
// Color conversion
// ---------------------------------------------------------------------------

/// 8-bit luminance copy of the image.
pub fn to_gray(img: &DynamicImage) -> GrayImage {
    match img {
        DynamicImage::ImageLuma8(g) => g.clone(),
        other => other.to_luma8(),
    }
}

fn histogram(gray: &GrayImage) -> [u64; 256] {
    let mut hist = [0u64; 256];
    for &p in gray.as_raw() {
        hist[p as usize] += 1;
    }
    hist
}

/// Otsu's threshold: pixels `>= level` are background (white).
pub fn otsu_level(gray: &GrayImage) -> u8 {
    otsu_from_hist(&histogram(gray))
}

fn otsu_from_hist(hist: &[u64; 256]) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let (mut w_b, mut sum_b) = (0f64, 0f64);
    // Track the whole plateau of maximal variance: with two well separated
    // peaks the variance is flat across the empty gap, and its middle is the
    // most robust split.
    let (mut best_lo, mut best_hi, mut best_var) = (0usize, 0usize, -1f64);
    for (t, &c) in hist.iter().enumerate() {
        w_b += c as f64;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total as f64 - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += t as f64 * c as f64;
        let m_b = sum_b / w_b;
        let m_f = (sum_all - sum_b) / w_f;
        let var = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if var > best_var {
            best_var = var;
            best_lo = t;
            best_hi = t;
        } else if var == best_var {
            best_hi = t;
        }
    }
    // Class "background" starts just above the best split.
    ((best_lo + best_hi) / 2 + 1).min(255) as u8
}

/// Pure black/white image (only 0 and 255).
pub fn threshold_bw(img: &DynamicImage, threshold: Threshold) -> GrayImage {
    let mut gray = to_gray(img);
    let level = match threshold {
        Threshold::Otsu => otsu_level(&gray),
        Threshold::Manual(l) => l,
    };
    for p in gray.iter_mut() {
        *p = if *p >= level { 255 } else { 0 };
    }
    gray
}

// ---------------------------------------------------------------------------
// Downscaled detection copy
// ---------------------------------------------------------------------------

/// Grayscale copy box-averaged by an integer factor so the longer side is at
/// most about `target`. Returns the copy and the factor.
fn detect_gray(img: &DynamicImage, target: u32) -> (GrayImage, u32) {
    let gray = to_gray(img);
    let (w, h) = gray.dimensions();
    let k = w.max(h).div_ceil(target.max(1)).max(1);
    (box_downscale(&gray, k), k)
}

fn box_downscale(gray: &GrayImage, k: u32) -> GrayImage {
    if k <= 1 {
        return gray.clone();
    }
    let (w, h) = gray.dimensions();
    let (sw, sh) = (w.div_ceil(k), h.div_ceil(k));
    let src = gray.as_raw();
    let mut sums = vec![0u32; sw as usize];
    let mut counts = vec![0u32; sw as usize];
    let mut out = Vec::with_capacity((sw * sh) as usize);
    for sy in 0..sh {
        sums.iter_mut().for_each(|s| *s = 0);
        counts.iter_mut().for_each(|c| *c = 0);
        let y0 = sy * k;
        let y1 = (y0 + k).min(h);
        for y in y0..y1 {
            let row = &src[(y * w) as usize..((y + 1) * w) as usize];
            for (x, &p) in row.iter().enumerate() {
                let sx = x / k as usize;
                sums[sx] += p as u32;
                counts[sx] += 1;
            }
        }
        out.extend(sums.iter().zip(&counts).map(|(&s, &c)| (s / c.max(1)) as u8));
    }
    GrayImage::from_raw(sw, sh, out).expect("buffer size matches")
}

fn percentile(hist: &[u64; 256], q: f64) -> u8 {
    let total: u64 = hist.iter().sum();
    let target = (total as f64 * q).ceil() as u64;
    let mut acc = 0;
    for (i, &c) in hist.iter().enumerate() {
        acc += c;
        if acc >= target.max(1) {
            return i as u8;
        }
    }
    255
}

// ---------------------------------------------------------------------------
// Blank-page detection
// ---------------------------------------------------------------------------

/// Margin ignored on every side when looking for ink (fraction of the side).
const BLANK_MARGIN: f32 = 0.05;
/// A pixel counts as ink when it is this much darker than the background.
const INK_DELTA: u8 = 40;

/// Fraction of "ink" pixels (inside a 5% margin) at or below which a page
/// counts as blank, for a sensitivity in 0..=1. Log scale: 0 -> 0.01%,
/// 0.5 -> 0.1%, 1 -> 1%. Higher sensitivity skips more pages.
pub fn blank_ink_cutoff(sensitivity: f32) -> f32 {
    let s = sensitivity.clamp(0.0, 1.0);
    10f32.powf(-4.0 + 2.0 * s)
}

/// Fraction of ink pixels inside the margin, the measure [`is_blank`] uses.
pub fn ink_fraction(img: &DynamicImage) -> f32 {
    let (small, _) = detect_gray(img, 1600);
    let (w, h) = small.dimensions();
    let mx = (w as f32 * BLANK_MARGIN).round() as u32;
    let my = (h as f32 * BLANK_MARGIN).round() as u32;
    if w <= 2 * mx || h <= 2 * my {
        return 0.0;
    }
    let mut hist = [0u64; 256];
    for y in my..h - my {
        for x in mx..w - mx {
            hist[small.get_pixel(x, y)[0] as usize] += 1;
        }
    }
    // Most of a page is paper, so a high percentile is the paper shade even
    // when there is a lot of text.
    let background = percentile(&hist, 0.75);
    let cut = background.saturating_sub(INK_DELTA) as usize;
    let total: u64 = hist.iter().sum();
    let ink: u64 = hist[..cut].iter().sum();
    ink as f32 / total.max(1) as f32
}

/// True when the page has (almost) no ink. `sensitivity` is 0..=1; see
/// [`blank_ink_cutoff`] for how it maps to the ink fraction.
pub fn is_blank(img: &DynamicImage, sensitivity: f32) -> bool {
    ink_fraction(img) <= blank_ink_cutoff(sensitivity)
}

// ---------------------------------------------------------------------------
// Auto-crop
// ---------------------------------------------------------------------------

/// Bounding box `(x, y, w, h)` of the content in full-image pixels, or `None`
/// when nothing differs from the border color.
pub fn content_bounds(img: &DynamicImage) -> Option<(u32, u32, u32, u32)> {
    let (small, k) = detect_gray(img, 1000);
    let (w, h) = small.dimensions();
    if w < 8 || h < 8 {
        return None;
    }
    // Background: median of a 2-pixel ring around the border.
    let mut hist = [0u64; 256];
    for y in 0..h {
        for x in 0..w {
            if x < 2 || y < 2 || x >= w - 2 || y >= h - 2 {
                hist[small.get_pixel(x, y)[0] as usize] += 1;
            }
        }
    }
    let bg = percentile(&hist, 0.5) as i32;
    const TOL: i32 = 48;

    let mut rows = vec![0u32; h as usize];
    let mut cols = vec![0u32; w as usize];
    for (x, y, p) in small.enumerate_pixels() {
        if (p[0] as i32 - bg).abs() > TOL {
            rows[y as usize] += 1;
            cols[x as usize] += 1;
        }
    }
    // Ignore rows/columns with only a stray speck or two.
    let row_min = (w / 200).max(2);
    let col_min = (h / 200).max(2);
    let y0 = rows.iter().position(|&c| c >= row_min)? as u32;
    let y1 = rows.iter().rposition(|&c| c >= row_min)? as u32;
    let x0 = cols.iter().position(|&c| c >= col_min)? as u32;
    let x1 = cols.iter().rposition(|&c| c >= col_min)? as u32;

    let pad = ((w.max(h) as f32) * 0.01).round().max(2.0) as u32;
    let (fw, fh) = img.dimensions();
    let fx0 = x0.saturating_sub(pad) * k;
    let fy0 = y0.saturating_sub(pad) * k;
    let fx1 = ((x1 + 1 + pad) * k).min(fw);
    let fy1 = ((y1 + 1 + pad) * k).min(fh);
    if fx1 <= fx0 || fy1 <= fy0 {
        return None;
    }
    Some((fx0, fy0, fx1 - fx0, fy1 - fy0))
}

/// Trim the background around the content, keeping a small padding.
/// Returns an unchanged copy when no content is found.
pub fn auto_crop(img: &DynamicImage) -> DynamicImage {
    match content_bounds(img) {
        Some((x, y, w, h)) if (w, h) != img.dimensions() => img.crop_imm(x, y, w, h),
        _ => img.clone(),
    }
}

// ---------------------------------------------------------------------------
// Rotation
// ---------------------------------------------------------------------------

/// Quarter-turn rotation (clockwise).
pub fn rotate(img: &DynamicImage, rotation: Rotation) -> DynamicImage {
    match rotation {
        Rotation::R90 => img.rotate90(),
        Rotation::R180 => img.rotate180(),
        Rotation::R270 => img.rotate270(),
    }
}

/// Rotate by an arbitrary angle about the center, keeping the image size and
/// filling uncovered areas with white. Positive angles turn the content
/// clockwise on screen (y axis points down). Bilinear sampling. Gray images
/// stay gray; everything else comes back as RGB8.
pub fn rotate_degrees(img: &DynamicImage, degrees: f32) -> DynamicImage {
    match img {
        DynamicImage::ImageLuma8(g) => {
            let (w, h) = g.dimensions();
            let out = rotate_raw::<1>(g.as_raw(), w, h, degrees, [255]);
            DynamicImage::ImageLuma8(ImageBuffer::<Luma<u8>, _>::from_raw(w, h, out).unwrap())
        }
        other => {
            let rgb: RgbImage = match other {
                DynamicImage::ImageRgb8(r) => r.clone(),
                o => o.to_rgb8(),
            };
            let (w, h) = rgb.dimensions();
            let out = rotate_raw::<3>(rgb.as_raw(), w, h, degrees, [255; 3]);
            DynamicImage::ImageRgb8(ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, out).unwrap())
        }
    }
}

fn rotate_raw<const N: usize>(src: &[u8], w: u32, h: u32, degrees: f32, fill: [u8; N]) -> Vec<u8> {
    let (w_us, h_us) = (w as usize, h as usize);
    let mut out = vec![0u8; w_us * h_us * N];
    let theta = (degrees as f64).to_radians();
    let (sin, cos) = theta.sin_cos();
    let cx = (w as f64 - 1.0) / 2.0;
    let cy = (h as f64 - 1.0) / 2.0;
    let sample = |x: i64, y: i64, c: usize| -> f64 {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            fill[c] as f64
        } else {
            src[(y as usize * w_us + x as usize) * N + c] as f64
        }
    };
    for (y, row) in out.chunks_exact_mut(w_us * N).enumerate() {
        let dy = y as f64 - cy;
        // Inverse map: source = R(-theta) * (p - c) + c.
        let mut sx = cos * -cx + sin * dy + cx;
        let mut sy = -sin * -cx + cos * dy + cy;
        for px in row.as_chunks_mut::<N>().0 {
            let fx = sx.floor();
            let fy = sy.floor();
            let (ax, ay) = (sx - fx, sy - fy);
            let (x0, y0) = (fx as i64, fy as i64);
            let inside = x0 >= 0 && y0 >= 0 && x0 + 1 < w as i64 && y0 + 1 < h as i64;
            for (c, v) in px.iter_mut().enumerate() {
                let (p00, p10, p01, p11) = if inside {
                    let i = (y0 as usize * w_us + x0 as usize) * N + c;
                    let j = i + w_us * N;
                    (src[i] as f64, src[i + N] as f64, src[j] as f64, src[j + N] as f64)
                } else {
                    (
                        sample(x0, y0, c),
                        sample(x0 + 1, y0, c),
                        sample(x0, y0 + 1, c),
                        sample(x0 + 1, y0 + 1, c),
                    )
                };
                let top = p00 + (p10 - p00) * ax;
                let bot = p01 + (p11 - p01) * ax;
                *v = (top + (bot - top) * ay).round().clamp(0.0, 255.0) as u8;
            }
            sx += cos;
            sy -= sin;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Deskew
// ---------------------------------------------------------------------------

/// Angle in degrees that [`rotate_degrees`] needs to straighten the page
/// (the negation of the detected skew), searched within `±max_degrees`.
/// Returns 0.0 when there is too little content to tell.
pub fn detect_skew(img: &DynamicImage, max_degrees: f32) -> f32 {
    let (small, _) = detect_gray(img, 800);
    let level = otsu_level(&small);
    let (w, h) = small.dimensions();
    let cx = (w as f32 - 1.0) / 2.0;
    let cy = (h as f32 - 1.0) / 2.0;
    let mut pts: Vec<(f32, f32)> = small
        .enumerate_pixels()
        .filter(|(_, _, p)| p[0] < level)
        .map(|(x, y, _)| (x as f32 - cx, y as f32 - cy))
        .collect();
    if pts.len() < 100 || pts.len() as u64 * 2 > w as u64 * h as u64 {
        // Nothing to align, or Otsu split something that isn't ink on paper.
        return 0.0;
    }
    const MAX_POINTS: usize = 200_000;
    if pts.len() > MAX_POINTS {
        let step = pts.len().div_ceil(MAX_POINTS);
        pts = pts.into_iter().step_by(step).collect();
    }
    let half_diag = ((w * w + h * h) as f32).sqrt() / 2.0 + 2.0;
    let bins = (half_diag * 2.0).ceil() as usize + 1;
    let mut hist = vec![0u32; bins];
    let mut score = |deg: f32| -> f64 {
        let (s, c) = deg.to_radians().sin_cos();
        hist.iter_mut().for_each(|b| *b = 0);
        for &(x, y) in &pts {
            let yr = s * x + c * y + half_diag;
            hist[yr as usize] += 1;
        }
        hist.iter().map(|&n| (n as f64) * (n as f64)).sum()
    };
    let max = max_degrees.abs().min(45.0);
    let search = |score: &mut dyn FnMut(f32) -> f64, lo: f32, hi: f32, step: f32| {
        let n = ((hi - lo) / step).round() as i32;
        let mut best = (0.0f32, f64::MIN);
        for i in 0..=n {
            let a = lo + i as f32 * step;
            let v = score(a);
            if v > best.1 {
                best = (a, v);
            }
        }
        best.0
    };
    let coarse = search(&mut score, -max, max, 0.25);
    let lo = (coarse - 0.3).max(-max);
    let hi = (coarse + 0.3).min(max);
    search(&mut score, lo, hi, 0.05)
}

/// Straighten a crooked page. Returns the result and the rotation applied
/// (degrees, see [`rotate_degrees`]); 0.0 and an unchanged copy when the skew
/// is below [`DESKEW_MIN_DEGREES`].
pub fn deskew(img: &DynamicImage, max_degrees: f32) -> (DynamicImage, f32) {
    let angle = detect_skew(img, max_degrees);
    if angle.abs() < DESKEW_MIN_DEGREES {
        return (img.clone(), 0.0);
    }
    (rotate_degrees(img, angle), angle)
}

// ---------------------------------------------------------------------------
// Brightness / contrast and the full pipeline
// ---------------------------------------------------------------------------

/// Brightness offset and contrast multiplier around mid-gray, per channel.
/// Gray stays gray; other formats come back as RGB8.
pub fn adjust(img: &DynamicImage, brightness: i32, contrast: f32) -> DynamicImage {
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = (i as f32 - 128.0) * contrast + 128.0 + brightness as f32;
        *v = x.round().clamp(0.0, 255.0) as u8;
    }
    match img {
        DynamicImage::ImageLuma8(g) => {
            let mut g = g.clone();
            g.iter_mut().for_each(|p| *p = lut[*p as usize]);
            DynamicImage::ImageLuma8(g)
        }
        other => {
            let mut rgb = match other {
                DynamicImage::ImageRgb8(r) => r.clone(),
                o => o.to_rgb8(),
            };
            rgb.iter_mut().for_each(|p| *p = lut[*p as usize]);
            DynamicImage::ImageRgb8(rgb)
        }
    }
}

/// Run the processing pipeline: convert to the working color space, crop,
/// deskew, adjust brightness/contrast, then produce the requested mode.
///
/// Output: Color -> RGB8, Gray -> Luma8, Bw -> Luma8 containing only 0/255.
pub fn apply(img: &DynamicImage, p: &Processing) -> DynamicImage {
    // Going gray first makes every later step cheaper for Gray/Bw output.
    let mut cur = match (p.color, img) {
        (ColorMode::Color, DynamicImage::ImageRgb8(_)) => img.clone(),
        (ColorMode::Color, _) => DynamicImage::ImageRgb8(img.to_rgb8()),
        (_, _) => DynamicImage::ImageLuma8(to_gray(img)),
    };
    // Crop before deskew so a dark scanner background is gone before the
    // rotation fills corners with white.
    if p.auto_crop {
        cur = auto_crop(&cur);
    }
    if p.deskew {
        cur = deskew(&cur, DESKEW_MAX_DEGREES).0;
    }
    if p.brightness != 0 || (p.contrast - 1.0).abs() > f32::EPSILON {
        cur = adjust(&cur, p.brightness, p.contrast);
    }
    match p.color {
        ColorMode::Color => DynamicImage::ImageRgb8(cur.to_rgb8()),
        ColorMode::Gray => DynamicImage::ImageLuma8(to_gray(&cur)),
        ColorMode::Bw => DynamicImage::ImageLuma8(threshold_bw(&cur, p.bw_threshold)),
    }
}

/// True when the image is 8-bit gray containing only 0 and 255, i.e. what
/// `apply` produces for [`ColorMode::Bw`].
pub fn is_pure_bw(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageLuma8(g) => g.as_raw().iter().all(|&p| p == 0 || p == 255),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift noise.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % (hi - lo + 1) as u64) as i32
        }
    }

    fn page(w: u32, h: u32, shade: u8) -> GrayImage {
        GrayImage::from_pixel(w, h, Luma([shade]))
    }

    fn fill_rect(img: &mut GrayImage, x: u32, y: u32, w: u32, h: u32, v: u8) {
        for yy in y..(y + h).min(img.height()) {
            for xx in x..(x + w).min(img.width()) {
                img.put_pixel(xx, yy, Luma([v]));
            }
        }
    }

    /// Lines of "words" (dark bars) like a page of text.
    fn text_page(w: u32, h: u32, ink: u8, seed: u64) -> GrayImage {
        let mut img = page(w, h, 245);
        let mut rng = Rng(seed);
        let margin = w / 8;
        let mut y = h / 10;
        while y + 14 < h - h / 10 {
            let mut x = margin;
            while x < w - margin {
                let word = rng.range(30, 120) as u32;
                fill_rect(&mut img, x, y, word.min(w - margin - x), 12, ink);
                x += word + rng.range(12, 25) as u32;
            }
            y += 40;
        }
        img
    }

    fn add_noise(img: &mut GrayImage, amp: i32, seed: u64) {
        let mut rng = Rng(seed);
        for p in img.iter_mut() {
            *p = (*p as i32 + rng.range(-amp, amp)).clamp(0, 255) as u8;
        }
    }

    #[test]
    fn thumbnail_fits_and_never_upscales() {
        let img = DynamicImage::ImageLuma8(page(2500, 3300, 255));
        let t = thumbnail(&img, 200);
        assert_eq!(t.height(), 200);
        assert!((t.width() as i32 - 152).abs() <= 1, "{}", t.width());
        let p = preview(&img, 1200);
        assert_eq!(p.height(), 1200);
        let small = DynamicImage::ImageLuma8(page(100, 50, 255));
        assert_eq!(thumbnail(&small, 200).dimensions(), (100, 50));
    }

    #[test]
    fn threshold_is_pure_bw_and_splits_bimodal() {
        let mut g = page(200, 100, 230);
        fill_rect(&mut g, 0, 0, 100, 100, 40);
        add_noise(&mut g, 15, 7);
        let img = DynamicImage::ImageLuma8(g);
        let level = otsu_level(&img.to_luma8());
        assert!((60..=210).contains(&level), "otsu {level}");
        let bw = threshold_bw(&img, Threshold::Otsu);
        assert!(bw.iter().all(|&p| p == 0 || p == 255));
        assert_eq!(bw.get_pixel(50, 50)[0], 0);
        assert_eq!(bw.get_pixel(150, 50)[0], 255);
        let manual = threshold_bw(&img, Threshold::Manual(20));
        assert!(manual.iter().all(|&p| p == 255));
    }

    #[test]
    fn light_text_is_not_blank() {
        let img = DynamicImage::ImageLuma8(text_page(1700, 2200, 175, 3));
        for s in [0.0, 0.5, 1.0] {
            assert!(!is_blank(&img, s), "sensitivity {s}, ink {}", ink_fraction(&img));
        }
    }

    #[test]
    fn sparse_text_is_not_blank_at_default_sensitivity() {
        let mut g = page(2500, 3300, 240);
        // One short line, like a page with just a signature line.
        fill_rect(&mut g, 400, 1500, 900, 10, 120);
        add_noise(&mut g, 8, 11);
        let img = DynamicImage::ImageLuma8(g);
        assert!(!is_blank(&img, 0.5), "ink {}", ink_fraction(&img));
    }

    #[test]
    fn noisy_blank_page_with_edge_shadow_is_blank() {
        let (w, h) = (2500u32, 3300u32);
        let mut g = page(w, h, 232);
        // Shadow along the left and top edges, inside the ignored margin.
        for x in 0..(w * 4 / 100) {
            let v = 80 + (150 * x / (w * 4 / 100)) as u8;
            fill_rect(&mut g, x, 0, 1, h, v);
        }
        fill_rect(&mut g, 0, 0, w, h * 3 / 100, 90);
        add_noise(&mut g, 12, 5);
        // A few dust specks.
        let mut rng = Rng(9);
        for _ in 0..40 {
            let (x, y) = (rng.range(200, 2300) as u32, rng.range(200, 3100) as u32);
            fill_rect(&mut g, x, y, 2, 2, 30);
        }
        let img = DynamicImage::ImageLuma8(g);
        assert!(is_blank(&img, 0.5), "ink {}", ink_fraction(&img));
        assert!(is_blank(&img, 0.2), "ink {}", ink_fraction(&img));
        // Color input works the same way.
        let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
        assert!(is_blank(&rgb, 0.5));
    }

    #[test]
    fn auto_crop_finds_content_on_white() {
        let mut g = page(1000, 1400, 250);
        fill_rect(&mut g, 200, 300, 400, 600, 20);
        add_noise(&mut g, 10, 1);
        let img = DynamicImage::ImageLuma8(g);
        let (x, y, w, h) = content_bounds(&img).unwrap();
        assert!((180..=200).contains(&x) && (280..=300).contains(&y), "{x},{y}");
        assert!((400..=440).contains(&w) && (600..=640).contains(&h), "{w}x{h}");
        let c = auto_crop(&img);
        assert_eq!(c.dimensions(), (w, h));
    }

    #[test]
    fn auto_crop_trims_dark_scanner_background() {
        let mut g = page(1200, 1600, 30);
        fill_rect(&mut g, 100, 150, 1000, 1300, 240);
        fill_rect(&mut g, 300, 400, 500, 20, 40); // some text on the paper
        let img = DynamicImage::ImageRgb8(DynamicImage::ImageLuma8(g).to_rgb8());
        let c = auto_crop(&img);
        let (w, h) = c.dimensions();
        assert!((1000..=1040).contains(&w) && (1300..=1340).contains(&h), "{w}x{h}");
        assert!(matches!(c, DynamicImage::ImageRgb8(_)));
    }

    #[test]
    fn auto_crop_is_noop_without_content() {
        let mut g = page(800, 1000, 245);
        add_noise(&mut g, 10, 2);
        let img = DynamicImage::ImageLuma8(g);
        assert_eq!(auto_crop(&img).dimensions(), (800, 1000));
    }

    #[test]
    fn rotate_quarter_turns() {
        let mut g = page(40, 20, 255);
        g.put_pixel(0, 0, Luma([0]));
        let img = DynamicImage::ImageLuma8(g);
        let r = rotate(&img, Rotation::R90);
        assert_eq!(r.dimensions(), (20, 40));
        assert_eq!(r.to_luma8().get_pixel(19, 0)[0], 0);
        let r = rotate(&img, Rotation::R180);
        assert_eq!(r.to_luma8().get_pixel(39, 19)[0], 0);
        let r = rotate(&img, Rotation::R270);
        assert_eq!(r.to_luma8().get_pixel(0, 39)[0], 0);
    }

    #[test]
    fn rotate_degrees_zero_is_identity_and_fills_white() {
        let img = DynamicImage::ImageLuma8(text_page(300, 400, 0, 1));
        assert_eq!(rotate_degrees(&img, 0.0).to_luma8(), img.to_luma8());
        let dark = DynamicImage::ImageLuma8(page(300, 400, 0));
        let r = rotate_degrees(&dark, 20.0).to_luma8();
        assert_eq!(r.get_pixel(0, 0)[0], 255);
        assert_eq!(r.get_pixel(150, 200)[0], 0);
    }

    #[test]
    fn deskew_recovers_known_angle() {
        let base = DynamicImage::ImageLuma8(text_page(1700, 2200, 30, 42));
        for skew in [3.0f32, -1.5, 7.0, -0.8] {
            let crooked = rotate_degrees(&base, skew);
            let (out, angle) = deskew(&crooked, 10.0);
            assert!(
                (angle + skew).abs() <= 0.5,
                "skew {skew}: correction {angle}"
            );
            assert_eq!(out.dimensions(), crooked.dimensions());
        }
        // Color input.
        let crooked = DynamicImage::ImageRgb8(rotate_degrees(&base, 2.0).to_rgb8());
        let (_, angle) = deskew(&crooked, 10.0);
        assert!((angle + 2.0).abs() <= 0.5, "{angle}");
    }

    #[test]
    fn deskew_skips_straight_and_empty_pages() {
        let straight = DynamicImage::ImageLuma8(text_page(1700, 2200, 30, 4));
        let (_, angle) = deskew(&straight, 10.0);
        assert_eq!(angle, 0.0);
        let empty = DynamicImage::ImageLuma8(page(500, 700, 250));
        assert_eq!(deskew(&empty, 10.0).1, 0.0);
    }

    #[test]
    fn adjust_brightness_contrast() {
        let img = DynamicImage::ImageLuma8(page(4, 4, 100));
        assert_eq!(adjust(&img, 20, 1.0).to_luma8().get_pixel(0, 0)[0], 120);
        assert_eq!(adjust(&img, 0, 2.0).to_luma8().get_pixel(0, 0)[0], 72);
    }

    #[test]
    fn apply_produces_requested_modes() {
        let mut g = text_page(800, 1000, 60, 8);
        add_noise(&mut g, 6, 3);
        let rgb = DynamicImage::ImageRgb8(DynamicImage::ImageLuma8(g).to_rgb8());

        let bw = apply(
            &rgb,
            &Processing { color: ColorMode::Bw, auto_crop: true, deskew: true, ..Default::default() },
        );
        assert!(is_pure_bw(&bw));
        assert!(bw.width() < 800, "auto-crop trimmed the margins");

        let gray = apply(&rgb, &Processing { color: ColorMode::Gray, ..Default::default() });
        assert!(matches!(gray, DynamicImage::ImageLuma8(_)));
        assert_eq!(gray.dimensions(), (800, 1000));

        let gray_in = DynamicImage::ImageLuma8(rgb.to_luma8());
        let color = apply(&gray_in, &Processing { brightness: 10, ..Default::default() });
        assert!(matches!(color, DynamicImage::ImageRgb8(_)));
    }

    /// Timing on a full-size 300 dpi letter page. Run with
    /// `cargo test -p imaging --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn timing_full_page() {
        use std::time::Instant;
        let base = DynamicImage::ImageLuma8(text_page(2550, 3300, 30, 5));
        let color = DynamicImage::ImageRgb8(rotate_degrees(&base, 2.5).to_rgb8());
        let t = Instant::now();
        let _ = thumbnail(&color, 256);
        println!("thumbnail: {:?}", t.elapsed());
        let t = Instant::now();
        let _ = preview(&color, 1400);
        println!("preview: {:?}", t.elapsed());
        let t = Instant::now();
        let _ = is_blank(&color, 0.5);
        println!("is_blank: {:?}", t.elapsed());
        let t = Instant::now();
        let (_, a) = deskew(&color, 10.0);
        println!("deskew (color, {a:.2} deg): {:?}", t.elapsed());
        for mode in [ColorMode::Color, ColorMode::Gray, ColorMode::Bw] {
            let p = Processing { color: mode, auto_crop: true, deskew: true, ..Default::default() };
            let t = Instant::now();
            let _ = apply(&color, &p);
            println!("apply {mode:?} crop+deskew: {:?}", t.elapsed());
        }
    }

    #[test]
    fn processing_serde_round_trip() {
        let p = Processing {
            color: ColorMode::Bw,
            bw_threshold: Threshold::Manual(140),
            contrast: 1.2,
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Processing = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        let partial: Processing = serde_json::from_str(r#"{"deskew":true}"#).unwrap();
        assert!(partial.deskew);
        assert_eq!(partial.contrast, 1.0);
    }
}
