//! Turn raw SANE frames into images.

use image::{DynamicImage, GrayImage, RgbImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gray,
    Rgb,
    Red,
    Green,
    Blue,
}

/// One frame as read from `sane_read`, plus the parameters describing it.
#[derive(Debug, Clone)]
pub struct Frame {
    pub format: Format,
    pub pixels_per_line: usize,
    pub bytes_per_line: usize,
    /// `None` when the backend didn't know the line count up front; the line
    /// count is then derived from the data length.
    pub lines: Option<usize>,
    pub depth: u32,
    pub data: Vec<u8>,
}

impl Frame {
    fn height(&self) -> usize {
        let have = self.data.len().checked_div(self.bytes_per_line).unwrap_or(0);
        match self.lines {
            Some(l) => l.min(have),
            None => have,
        }
    }

    fn row(&self, y: usize) -> &[u8] {
        &self.data[y * self.bytes_per_line..(y + 1) * self.bytes_per_line]
    }

    /// Single-channel 8-bit samples for a gray or single-color frame.
    fn channel8(&self) -> Result<(usize, usize, Vec<u8>), String> {
        let (w, h) = (self.pixels_per_line, self.height());
        let need = match self.depth {
            1 => w.div_ceil(8),
            8 => w,
            16 => w * 2,
            d => return Err(format!("unsupported gray depth {d}")),
        };
        if self.bytes_per_line < need {
            return Err(format!("bytes_per_line {} too small for {w} pixels at depth {}", self.bytes_per_line, self.depth));
        }
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = self.row(y);
            match self.depth {
                // SANE line-art: 1 = black, most significant bit first.
                1 => out.extend((0..w).map(|x| if row[x / 8] & (0x80 >> (x % 8)) != 0 { 0 } else { 255 })),
                8 => out.extend_from_slice(&row[..w]),
                _ => out.extend(row[..w * 2].as_chunks::<2>().0.iter().map(sample16_to_8)),
            }
        }
        Ok((w, h, out))
    }
}

/// 16-bit SANE samples are in host byte order.
fn sample16_to_8(b: &[u8; 2]) -> u8 {
    (u16::from_ne_bytes(*b) >> 8) as u8
}

/// Assemble one page from its frames: a single gray or RGB frame, or three
/// single-color frames from a three-pass scanner.
pub fn assemble(frames: &[Frame]) -> Result<DynamicImage, String> {
    match frames {
        [] => Err("no image data".into()),
        [f] if f.format == Format::Gray => {
            let (w, h, px) = f.channel8()?;
            Ok(DynamicImage::ImageLuma8(gray(w, h, px)?))
        }
        [f] if f.format == Format::Rgb => rgb_frame(f),
        _ => three_pass(frames),
    }
}

fn gray(w: usize, h: usize, px: Vec<u8>) -> Result<GrayImage, String> {
    GrayImage::from_raw(w as u32, h as u32, px).ok_or_else(|| "gray buffer size mismatch".into())
}

fn rgb_frame(f: &Frame) -> Result<DynamicImage, String> {
    let (w, h) = (f.pixels_per_line, f.height());
    let bpp = match f.depth {
        8 => 3,
        16 => 6,
        d => return Err(format!("unsupported RGB depth {d}")),
    };
    if f.bytes_per_line < w * bpp {
        return Err(format!("bytes_per_line {} too small for {w} RGB pixels", f.bytes_per_line));
    }
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let row = &f.row(y)[..w * bpp];
        if f.depth == 8 {
            out.extend_from_slice(row);
        } else {
            out.extend(row.as_chunks::<2>().0.iter().map(sample16_to_8));
        }
    }
    RgbImage::from_raw(w as u32, h as u32, out)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "RGB buffer size mismatch".into())
}

fn three_pass(frames: &[Frame]) -> Result<DynamicImage, String> {
    let plane = |fmt: Format| -> Result<(usize, usize, Vec<u8>), String> {
        frames
            .iter()
            .find(|f| f.format == fmt)
            .ok_or_else(|| format!("three-pass scan is missing the {fmt:?} frame"))?
            .channel8()
    };
    let (rw, rh, r) = plane(Format::Red)?;
    let (gw, gh, g) = plane(Format::Green)?;
    let (bw, bh, b) = plane(Format::Blue)?;
    if rw != gw || rw != bw {
        return Err("three-pass frames differ in width".into());
    }
    let (w, h) = (rw, rh.min(gh).min(bh));
    let mut out = Vec::with_capacity(w * h * 3);
    for i in 0..w * h {
        out.extend_from_slice(&[r[i], g[i], b[i]]);
    }
    RgbImage::from_raw(w as u32, h as u32, out)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "RGB buffer size mismatch".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(format: Format, w: usize, bpl: usize, lines: Option<usize>, depth: u32, data: Vec<u8>) -> Frame {
        Frame { format, pixels_per_line: w, bytes_per_line: bpl, lines, depth, data }
    }

    #[test]
    fn gray8() {
        let img = assemble(&[frame(Format::Gray, 2, 2, Some(2), 8, vec![1, 2, 3, 4])]).unwrap();
        let g = img.as_luma8().unwrap();
        assert_eq!(g.dimensions(), (2, 2));
        assert_eq!(g.as_raw(), &[1, 2, 3, 4]);
    }

    #[test]
    fn gray1_is_black_on_one() {
        // 10 pixels in 2 bytes per line; padding bits ignored.
        let img = assemble(&[frame(Format::Gray, 10, 2, Some(1), 1, vec![0b1010_0000, 0b0100_0000])]).unwrap();
        assert_eq!(img.as_luma8().unwrap().as_raw(), &[0, 255, 0, 255, 255, 255, 255, 255, 255, 0]);
    }

    #[test]
    fn gray16_to_8() {
        let mut data = Vec::new();
        for v in [0x1234u16, 0xff00] {
            data.extend_from_slice(&v.to_ne_bytes());
        }
        let img = assemble(&[frame(Format::Gray, 2, 4, Some(1), 16, data)]).unwrap();
        assert_eq!(img.as_luma8().unwrap().as_raw(), &[0x12, 0xff]);
    }

    #[test]
    fn rgb8_with_padding_and_unknown_lines() {
        // 1 pixel per line, 4 bytes per line (one byte padding), 3 lines, lines unknown.
        let data = vec![1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        let img = assemble(&[frame(Format::Rgb, 1, 4, None, 8, data)]).unwrap();
        let rgb = img.as_rgb8().unwrap();
        assert_eq!(rgb.dimensions(), (1, 3));
        assert_eq!(rgb.as_raw(), &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn rgb16_to_8() {
        let mut data = Vec::new();
        for v in [0x0100u16, 0x8000, 0xffff] {
            data.extend_from_slice(&v.to_ne_bytes());
        }
        let img = assemble(&[frame(Format::Rgb, 1, 6, Some(1), 16, data)]).unwrap();
        assert_eq!(img.as_rgb8().unwrap().as_raw(), &[1, 0x80, 0xff]);
    }

    #[test]
    fn partial_last_line_is_dropped() {
        let img = assemble(&[frame(Format::Gray, 2, 2, None, 8, vec![1, 2, 3])]).unwrap();
        assert_eq!(img.as_luma8().unwrap().dimensions(), (2, 1));
    }

    #[test]
    fn three_pass_merges() {
        let frames = [
            frame(Format::Red, 2, 2, Some(1), 8, vec![10, 11]),
            frame(Format::Green, 2, 2, Some(1), 8, vec![20, 21]),
            frame(Format::Blue, 2, 2, Some(1), 8, vec![30, 31]),
        ];
        let img = assemble(&frames).unwrap();
        assert_eq!(img.as_rgb8().unwrap().as_raw(), &[10, 20, 30, 11, 21, 31]);
        assert!(assemble(&frames[..2]).is_err());
    }

    #[test]
    fn rejects_bad_depth() {
        assert!(assemble(&[frame(Format::Rgb, 1, 3, Some(1), 1, vec![0; 3])]).is_err());
        assert!(assemble(&[]).is_err());
    }
}
