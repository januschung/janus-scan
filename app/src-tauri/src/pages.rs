//! The desktop view of the shared page store: pages go to the front end with
//! their thumbnails inlined as data URLs.

use std::io::Cursor;

use base64::Engine;
use document::{PageInfo, PageStore};
use image::{DynamicImage, ImageFormat};
use serde::Serialize;

const TEMP_PREFIX: &str = "janus-scan-app-";

#[derive(Serialize, Clone)]
pub struct PageView {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub thumb: String,
}

pub fn new_store() -> std::io::Result<PageStore> {
    PageStore::new(TEMP_PREFIX)
}

pub fn view(store: &PageStore, info: PageInfo) -> PageView {
    let thumb = store.thumb(info.id).map(|t| data_url(&t, 80)).unwrap_or_default();
    PageView { id: info.id, width: info.width, height: info.height, dpi: info.dpi, thumb }
}

pub fn views(store: &PageStore) -> Vec<PageView> {
    store.infos().into_iter().map(|i| view(store, i)).collect()
}

pub fn data_url(img: &DynamicImage, quality: u8) -> String {
    let mut buf = Vec::new();
    let rgb = img.to_rgb8();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut buf), quality);
    if enc.encode_image(&rgb).is_err() {
        // Fall back to PNG; never fails for RGB8.
        buf.clear();
        let _ = rgb.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png);
        return format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf));
    }
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf))
}
