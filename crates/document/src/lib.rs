//! Scanned pages of the document being assembled, shared by the desktop app
//! and the server. Full-size pixels live in a private temporary folder
//! (uncompressed, so writing is cheap); only small thumbnails stay in memory.
//! Rotation is applied lazily on load.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use image::{DynamicImage, GrayImage, RgbImage};
use imaging::Rotation;
use serde::Serialize;

const THUMB_SIDE: u32 = 360;

#[derive(Clone, Copy)]
enum Pixels {
    Gray,
    Rgb,
}

struct Stored {
    id: u64,
    dpi: u32,
    width: u32,
    height: u32,
    pixels: Pixels,
    /// Quarter turns clockwise.
    turns: u8,
    thumb: DynamicImage,
    path: PathBuf,
}

/// What callers see of a page. `width` and `height` already account for
/// rotation; `turns` changes whenever the page is rotated, so it doubles as a
/// cache key for thumbnails.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct PageInfo {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub turns: u8,
}

pub struct PageStore {
    dir: PathBuf,
    pages: Vec<Stored>,
    next_id: u64,
}

fn rotation(turns: u8) -> Option<Rotation> {
    match turns % 4 {
        1 => Some(Rotation::R90),
        2 => Some(Rotation::R180),
        3 => Some(Rotation::R270),
        _ => None,
    }
}

fn rotated(img: &DynamicImage, turns: u8) -> DynamicImage {
    match rotation(turns) {
        Some(r) => imaging::rotate(img, r),
        None => img.clone(),
    }
}

impl PageStore {
    /// Creates a fresh private folder named after `prefix` and removes ones
    /// with the same prefix left by earlier runs that crashed or were killed.
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let tmp = std::env::temp_dir();
        let ours = format!("{prefix}{}-", std::process::id());
        if let Ok(entries) = fs::read_dir(&tmp) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(prefix) && !name.starts_with(&ours) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        let dir = tmp.join(format!("{ours}{}", COUNT.fetch_add(1, Ordering::Relaxed)));
        // Names are unique within a process, so an existing folder is from an
        // earlier process with the same id: a restarted container, where the
        // server is always PID 1.
        let _ = fs::remove_dir_all(&dir);
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(Self { dir, pages: Vec::new(), next_id: 1 })
    }

    pub fn add(&mut self, img: &DynamicImage, dpi: u32) -> std::io::Result<PageInfo> {
        let id = self.next_id;
        self.next_id += 1;
        let path = self.dir.join(format!("{id}.raw"));
        let pixels = if img.color().has_color() {
            fs::write(&path, img.to_rgb8().as_raw())?;
            Pixels::Rgb
        } else {
            fs::write(&path, img.to_luma8().as_raw())?;
            Pixels::Gray
        };
        self.pages.push(Stored {
            id,
            dpi,
            width: img.width(),
            height: img.height(),
            pixels,
            turns: 0,
            thumb: imaging::thumbnail(img, THUMB_SIDE),
            path,
        });
        Ok(self.info_at(self.pages.len() - 1))
    }

    fn info_at(&self, i: usize) -> PageInfo {
        let p = &self.pages[i];
        let (width, height) = if p.turns % 2 == 1 { (p.height, p.width) } else { (p.width, p.height) };
        PageInfo { id: p.id, width, height, dpi: p.dpi, turns: p.turns }
    }

    pub fn infos(&self) -> Vec<PageInfo> {
        (0..self.pages.len()).map(|i| self.info_at(i)).collect()
    }

    fn index(&self, id: u64) -> Result<usize, String> {
        self.pages.iter().position(|p| p.id == id).ok_or_else(|| "That page no longer exists.".into())
    }

    pub fn info(&self, id: u64) -> Result<PageInfo, String> {
        Ok(self.info_at(self.index(id)?))
    }

    /// The page's thumbnail (longest side 360 px), rotated.
    pub fn thumb(&self, id: u64) -> Result<DynamicImage, String> {
        let p = &self.pages[self.index(id)?];
        Ok(rotated(&p.thumb, p.turns))
    }

    pub fn load(&self, id: u64) -> Result<(DynamicImage, u32), String> {
        let p = &self.pages[self.index(id)?];
        let bytes = fs::read(&p.path).map_err(|e| format!("Couldn't read page: {e}"))?;
        let img = match p.pixels {
            Pixels::Rgb => RgbImage::from_raw(p.width, p.height, bytes).map(DynamicImage::ImageRgb8),
            Pixels::Gray => GrayImage::from_raw(p.width, p.height, bytes).map(DynamicImage::ImageLuma8),
        }
        .ok_or("Page data is corrupted.")?;
        Ok((rotated(&img, p.turns), p.dpi))
    }

    pub fn ids(&self) -> Vec<u64> {
        self.pages.iter().map(|p| p.id).collect()
    }

    pub fn rotate(&mut self, id: u64, clockwise: bool) -> Result<PageInfo, String> {
        let i = self.index(id)?;
        let p = &mut self.pages[i];
        p.turns = (p.turns + if clockwise { 1 } else { 3 }) % 4;
        Ok(self.info_at(i))
    }

    pub fn delete(&mut self, id: u64) -> Result<(), String> {
        let p = self.pages.remove(self.index(id)?);
        let _ = fs::remove_file(p.path);
        Ok(())
    }

    /// Reorder to match `order`, which must hold exactly the current ids.
    pub fn reorder(&mut self, order: &[u64]) -> Result<(), String> {
        let mut ids = self.ids();
        let mut want = order.to_vec();
        ids.sort_unstable();
        want.sort_unstable();
        if ids != want {
            return Err("Page list is out of date; try again.".into());
        }
        self.pages.sort_by_key(|p| order.iter().position(|&o| o == p.id));
        Ok(())
    }

    pub fn clear(&mut self) {
        for p in self.pages.drain(..) {
            let _ = fs::remove_file(p.path);
        }
    }

    /// Delete every page and the private folder now, for shutting down
    /// without waiting for the store to be dropped.
    pub fn discard(&mut self) {
        self.clear();
        let _ = fs::remove_dir_all(&self.dir);
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

impl Drop for PageStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn page(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([200, 10, 10])))
    }

    #[test]
    fn add_rotate_load_round_trip() {
        let mut s = PageStore::new("janus-scan-document-test-").unwrap();
        let a = s.add(&page(40, 60), 300).unwrap();
        let r = s.rotate(a.id, true).unwrap();
        assert_eq!((r.width, r.height, r.turns), (60, 40, 1));
        let (img, dpi) = s.load(a.id).unwrap();
        assert_eq!((img.width(), img.height(), dpi), (60, 40, 300));
        assert_eq!(s.thumb(a.id).unwrap().width(), 60);
    }

    #[test]
    fn two_stores_in_one_process_keep_their_folders() {
        let mut a = PageStore::new("janus-scan-document-test2-").unwrap();
        let b = PageStore::new("janus-scan-document-test2-").unwrap();
        let p = a.add(&page(4, 4), 300).unwrap();
        assert!(a.load(p.id).is_ok());
        drop(b);
        assert!(a.load(p.id).is_ok());
    }

    #[test]
    fn reorder_rejects_stale_list() {
        let mut s = PageStore::new("janus-scan-document-test3-").unwrap();
        let a = s.add(&page(4, 4), 300).unwrap().id;
        let b = s.add(&page(4, 4), 300).unwrap().id;
        assert!(s.reorder(&[a]).is_err());
        s.reorder(&[b, a]).unwrap();
        assert_eq!(s.ids(), vec![b, a]);
    }
}
