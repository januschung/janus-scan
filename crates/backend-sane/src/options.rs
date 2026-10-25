//! SANE option model and the pure logic that maps it to and from scan-core
//! types. Nothing here touches libsane, so it is unit-tested on any platform.

use scan_core::{Capabilities, ColorMode, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Bool,
    Int,
    Fixed,
    String,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Pixel,
    Mm,
    Other,
}

/// Constraint values are already converted from `SANE_Fixed` where relevant.
#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    None,
    Range { min: f64, max: f64, quant: f64 },
    WordList(Vec<f64>),
    StringList(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptDesc {
    pub index: i32,
    pub name: String,
    pub ty: ValueType,
    pub unit: Unit,
    /// Value size in bytes (for strings, the buffer size including the NUL).
    pub size: i32,
    pub active: bool,
    pub constraint: Constraint,
}

impl OptDesc {
    pub fn strings(&self) -> &[String] {
        match &self.constraint {
            Constraint::StringList(v) => v,
            _ => &[],
        }
    }

    /// Bring a number inside the option's constraint: clamp (and quantize) to
    /// a range, or pick the nearest word-list entry.
    pub fn constrain(&self, v: f64) -> f64 {
        match &self.constraint {
            Constraint::Range { min, max, quant } => {
                let mut x = v.clamp(*min, *max);
                if *quant > 0.0 {
                    x = min + ((x - min) / quant).round() * quant;
                    if x > *max {
                        x -= quant;
                    }
                }
                x
            }
            Constraint::WordList(list) => nearest(list, v).unwrap_or(v),
            _ => v,
        }
    }

    pub fn range_max(&self) -> Option<f64> {
        match &self.constraint {
            Constraint::Range { max, .. } => Some(*max),
            Constraint::WordList(l) => l.iter().copied().reduce(f64::max),
            _ => None,
        }
    }

    pub fn range_min(&self) -> Option<f64> {
        match &self.constraint {
            Constraint::Range { min, .. } => Some(*min),
            Constraint::WordList(l) => l.iter().copied().reduce(f64::min),
            _ => None,
        }
    }
}

fn nearest(list: &[f64], v: f64) -> Option<f64> {
    list.iter()
        .copied()
        .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()).then(a.total_cmp(b)))
}

/// The options we care about, looked up by their standard SANE names.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub all: Vec<OptDesc>,
}

impl Options {
    /// Find an option by name, preferring an active one if names repeat.
    pub fn get(&self, name: &str) -> Option<&OptDesc> {
        let mut found = self.all.iter().filter(|o| o.name == name);
        let first = found.next()?;
        if first.active {
            return Some(first);
        }
        found.find(|o| o.active).or(Some(first))
    }
}

fn lc(s: &str) -> String {
    s.to_ascii_lowercase()
}

pub fn is_duplex_source(s: &str) -> bool {
    lc(s).contains("duplex")
}

pub fn is_feeder_source(s: &str) -> bool {
    let l = lc(s);
    l.contains("adf") || l.contains("feeder") || l.contains("document") || l.contains("duplex")
}

/// Choose the SANE `source` string for a request. `None` means the device has
/// no source option (or no suitable value) and should be left alone.
pub fn pick_source(sources: &[String], source: Source, duplex: bool) -> Option<String> {
    let pick = match (source, duplex) {
        (Source::Feeder, true) => sources.iter().find(|s| is_duplex_source(s)),
        (Source::Feeder, false) => sources
            .iter()
            .find(|s| is_feeder_source(s) && !is_duplex_source(s))
            .or_else(|| sources.iter().find(|s| is_feeder_source(s))),
        (Source::Flatbed, _) => sources
            .iter()
            .find(|s| lc(s).contains("flatbed"))
            .or_else(|| sources.iter().find(|s| !is_feeder_source(s))),
    };
    pick.cloned()
}

/// Rank mode strings for a color mode; lower is better, `None` = not a match.
fn mode_score(mode: &str, want: ColorMode) -> Option<u32> {
    let l = lc(mode);
    let dithered = l.contains("diffusion") || l.contains("halftone") || l.contains("dither");
    let is_bw = l.contains("lineart")
        || l.contains("line art")
        || l.contains("binary")
        || l.contains("black & white")
        || l.contains("black and white")
        || l == "bw"
        || l == "b&w";
    let is_gray = !is_bw && (l.contains("gray") || l.contains("grey"));
    let is_color = !is_bw && !is_gray && (l.contains("color") || l.contains("colour"));
    let matches = match want {
        ColorMode::Color => is_color,
        ColorMode::Gray => is_gray,
        ColorMode::Bw => is_bw,
    };
    if !matches {
        return None;
    }
    let exact = matches!(l.as_str(), "color" | "colour" | "gray" | "grey" | "lineart");
    Some(if exact { 0 } else { 1 } + if dithered { 10 } else { 0 } + l.len() as u32 / 64)
}

fn best_mode(modes: &[String], want: ColorMode) -> Option<String> {
    modes
        .iter()
        .filter_map(|m| mode_score(m, want).map(|s| (s, m.len(), m)))
        .min_by_key(|(s, len, _)| (*s, *len))
        .map(|(_, _, m)| m.clone())
}

/// Choose the SANE `mode` string. Black & white falls back to gray when the
/// device has no line-art mode; the imaging crate thresholds it later.
pub fn pick_mode(modes: &[String], want: ColorMode) -> Option<String> {
    best_mode(modes, want).or_else(|| match want {
        ColorMode::Bw => best_mode(modes, ColorMode::Gray),
        _ => None,
    })
}

const STANDARD_DPI: [u32; 7] = [75, 100, 150, 200, 300, 400, 600];

/// Highest resolution offered. The ADS-1000W's optical resolution is 600 dpi;
/// Brother's driver advertises up to 9600 (interpolated), which only makes
/// huge, slow files.
pub const MAX_DPI: u32 = 600;

pub fn resolutions(opt: Option<&OptDesc>) -> Vec<u32> {
    let Some(opt) = opt else { return vec![] };
    let mut out: Vec<u32> = match &opt.constraint {
        Constraint::WordList(l) => l.iter().map(|v| v.round() as u32).collect(),
        Constraint::Range { min, max, .. } => {
            let v: Vec<u32> = STANDARD_DPI
                .into_iter()
                .filter(|d| (*d as f64) >= *min && (*d as f64) <= *max)
                .collect();
            if v.is_empty() { vec![min.round() as u32] } else { v }
        }
        _ => vec![],
    };
    out.sort_unstable();
    out.dedup();
    out.retain(|d| *d > 0);
    if out.iter().any(|d| *d <= MAX_DPI) {
        out.retain(|d| *d <= MAX_DPI);
    } else {
        out.truncate(1);
    }
    out
}

/// Nearest supported resolution to `dpi`, in the option's own value space.
/// Requests above [`MAX_DPI`] are capped to it.
pub fn nearest_resolution(opt: &OptDesc, dpi: u32) -> f64 {
    opt.constrain(dpi.min(MAX_DPI) as f64).round()
}

/// Convert a scan-area length in mm to the option's unit.
pub fn mm_to_option(opt: &OptDesc, mm: f64, dpi: f64) -> f64 {
    match opt.unit {
        Unit::Pixel => mm / 25.4 * dpi,
        _ => mm,
    }
}

fn option_to_mm(opt: &OptDesc, v: f64, dpi: f64) -> f64 {
    match opt.unit {
        Unit::Pixel if dpi > 0.0 => v / dpi * 25.4,
        _ => v,
    }
}

/// Build [`Capabilities`] from the device's options. `current_dpi` converts
/// pixel-unit geometry options to mm.
pub fn capabilities(opts: &Options, current_dpi: f64) -> Capabilities {
    let source_strings = opts.get("source").map(|o| o.strings().to_vec()).unwrap_or_default();
    let mut sources = Vec::new();
    let has_feeder = source_strings.iter().any(|s| is_feeder_source(s));
    let has_flatbed =
        source_strings.is_empty() || source_strings.iter().any(|s| !is_feeder_source(s));
    if has_feeder {
        sources.push(Source::Feeder);
    }
    if has_flatbed {
        sources.push(Source::Flatbed);
    }
    let duplex = source_strings.iter().any(|s| is_duplex_source(s));

    let modes = opts.get("mode").map(|o| o.strings().to_vec()).unwrap_or_default();
    let mut color_modes = Vec::new();
    for m in [ColorMode::Color, ColorMode::Gray] {
        if best_mode(&modes, m).is_some() {
            color_modes.push(m);
        }
    }
    if pick_mode(&modes, ColorMode::Bw).is_some() {
        color_modes.push(ColorMode::Bw);
    }

    let extent = |tl: &str, br: &str| -> f32 {
        let Some(b) = opts.get(br) else { return 0.0 };
        let max = b.range_max().unwrap_or(0.0);
        let min = opts.get(tl).and_then(|t| t.range_min()).unwrap_or(0.0);
        option_to_mm(b, (max - min).max(0.0), current_dpi) as f32
    };

    Capabilities {
        sources,
        duplex,
        resolutions: resolutions(opts.get("resolution")),
        color_modes,
        max_size_mm: (extent("tl-x", "br-x"), extent("tl-y", "br-y")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn opt(name: &str, ty: ValueType, unit: Unit, constraint: Constraint) -> OptDesc {
        OptDesc { index: 0, name: name.into(), ty, unit, size: 4, active: true, constraint }
    }

    fn range(min: f64, max: f64) -> Constraint {
        Constraint::Range { min, max, quant: 0.0 }
    }

    const BROTHER_SOURCES: &[&str] = &[
        "Automatic Document Feeder(left aligned)",
        "Automatic Document Feeder(left aligned,Duplex)",
        "Automatic Document Feeder(centrally aligned)",
        "Automatic Document Feeder(centrally aligned,Duplex)",
    ];

    #[test]
    fn picks_sources() {
        let b = s(BROTHER_SOURCES);
        assert_eq!(pick_source(&b, Source::Feeder, false).unwrap(), b[0]);
        assert_eq!(pick_source(&b, Source::Feeder, true).unwrap(), b[1]);
        assert_eq!(pick_source(&b, Source::Flatbed, false), None);

        let t = s(&["Flatbed", "Automatic Document Feeder"]);
        assert_eq!(pick_source(&t, Source::Flatbed, false).unwrap(), "Flatbed");
        assert_eq!(pick_source(&t, Source::Feeder, false).unwrap(), "Automatic Document Feeder");
        assert_eq!(pick_source(&t, Source::Feeder, true), None);

        let e = s(&["FlatBed", "ADF Front", "ADF Duplex"]);
        assert_eq!(pick_source(&e, Source::Flatbed, false).unwrap(), "FlatBed");
        assert_eq!(pick_source(&e, Source::Feeder, false).unwrap(), "ADF Front");
        assert_eq!(pick_source(&e, Source::Feeder, true).unwrap(), "ADF Duplex");
    }

    #[test]
    fn picks_modes() {
        let brother = s(&["Black & White", "Gray[Error Diffusion]", "True Gray", "24bit Color", "24bit Color[Fast]"]);
        assert_eq!(pick_mode(&brother, ColorMode::Color).unwrap(), "24bit Color");
        assert_eq!(pick_mode(&brother, ColorMode::Gray).unwrap(), "True Gray");
        assert_eq!(pick_mode(&brother, ColorMode::Bw).unwrap(), "Black & White");

        let std = s(&["Lineart", "Gray", "Color"]);
        assert_eq!(pick_mode(&std, ColorMode::Bw).unwrap(), "Lineart");
        assert_eq!(pick_mode(&std, ColorMode::Gray).unwrap(), "Gray");

        // No line-art: black & white scans gray.
        let test = s(&["Gray", "Color"]);
        assert_eq!(pick_mode(&test, ColorMode::Bw).unwrap(), "Gray");
        assert_eq!(pick_mode(&s(&["Color"]), ColorMode::Gray), None);
    }

    #[test]
    fn resolutions_from_list_and_range() {
        let wl = opt("resolution", ValueType::Int, Unit::Other, Constraint::WordList(vec![300.0, 100.0, 200.0, 600.0]));
        assert_eq!(resolutions(Some(&wl)), vec![100, 200, 300, 600]);
        assert_eq!(nearest_resolution(&wl, 250), 200.0);
        assert_eq!(nearest_resolution(&wl, 1200), 600.0);

        let r = opt("resolution", ValueType::Fixed, Unit::Other, range(1.0, 1200.0));
        assert_eq!(resolutions(Some(&r)), vec![75, 100, 150, 200, 300, 400, 600]);
        assert_eq!(nearest_resolution(&r, 300), 300.0);
        let r2 = opt("resolution", ValueType::Int, Unit::Other, range(150.0, 300.0));
        assert_eq!(resolutions(Some(&r2)), vec![150, 200, 300]);
        assert_eq!(nearest_resolution(&r2, 600), 300.0);
        assert!(resolutions(None).is_empty());
    }

    #[test]
    fn resolutions_capped_at_600() {
        // Brother's driver: 100..9600 dpi, mostly interpolated.
        let brother = opt("resolution", ValueType::Int, Unit::Other, range(100.0, 9600.0));
        assert_eq!(resolutions(Some(&brother)), vec![100, 150, 200, 300, 400, 600]);
        assert_eq!(nearest_resolution(&brother, 1200), 600.0);
        assert_eq!(nearest_resolution(&brother, 9600), 600.0);

        let wl = opt(
            "resolution",
            ValueType::Int,
            Unit::Other,
            Constraint::WordList(vec![100.0, 150.0, 200.0, 300.0, 600.0, 1200.0, 2400.0, 4800.0, 9600.0]),
        );
        assert_eq!(resolutions(Some(&wl)), vec![100, 150, 200, 300, 600]);
        assert_eq!(nearest_resolution(&wl, 2400), 600.0);

        // A device whose lowest resolution is above the cap still offers one.
        let hi = opt("resolution", ValueType::Int, Unit::Other, Constraint::WordList(vec![1200.0, 2400.0]));
        assert_eq!(resolutions(Some(&hi)), vec![1200]);
        assert_eq!(nearest_resolution(&hi, 300), 1200.0);
    }

    #[test]
    fn area_width_is_not_rounded_down() {
        // ADS-1000W br-x: SANE_Fixed mm, max 211.9. Letter (215.9 mm) clamps to
        // 211.9 and must be sent as 211.9 mm (~2502 px at 300 dpi), not less.
        let brx = opt("br-x", ValueType::Fixed, Unit::Mm, range(0.0, 211.9));
        let v = brx.constrain(mm_to_option(&brx, 215.9_f32.min(211.9) as f64, 300.0));
        assert!((v - 211.9).abs() < 1e-4, "{v}");
        let back = crate::ffi::fixed_to_f64(crate::ffi::f64_to_fixed(v));
        assert!((back - 211.9).abs() < 1.0 / 65536.0, "{back}");
        assert_eq!((back / 25.4 * 300.0).round(), 2503.0);
    }

    #[test]
    fn constrain_quantizes() {
        let o = opt("x", ValueType::Int, Unit::Pixel, Constraint::Range { min: 0.0, max: 100.0, quant: 8.0 });
        assert_eq!(o.constrain(13.0), 16.0);
        assert_eq!(o.constrain(99.0), 96.0);
        assert_eq!(o.constrain(-5.0), 0.0);
    }

    #[test]
    fn pixel_units_convert() {
        let px = opt("br-x", ValueType::Int, Unit::Pixel, range(0.0, 2550.0));
        assert!((mm_to_option(&px, 25.4, 300.0) - 300.0).abs() < 1e-9);
        let mm = opt("br-x", ValueType::Fixed, Unit::Mm, range(0.0, 215.9));
        assert_eq!(mm_to_option(&mm, 100.0, 300.0), 100.0);
    }

    #[test]
    fn builds_capabilities() {
        let opts = Options {
            all: vec![
                opt("source", ValueType::String, Unit::Other, Constraint::StringList(s(BROTHER_SOURCES))),
                opt("mode", ValueType::String, Unit::Other, Constraint::StringList(s(&["Black & White", "True Gray", "24bit Color"]))),
                opt("resolution", ValueType::Int, Unit::Other, Constraint::WordList(vec![100.0, 150.0, 200.0, 300.0, 600.0])),
                opt("tl-x", ValueType::Fixed, Unit::Mm, range(0.0, 215.9)),
                opt("tl-y", ValueType::Fixed, Unit::Mm, range(0.0, 863.0)),
                opt("br-x", ValueType::Fixed, Unit::Mm, range(0.0, 215.9)),
                opt("br-y", ValueType::Fixed, Unit::Mm, range(0.0, 863.0)),
            ],
        };
        let c = capabilities(&opts, 300.0);
        assert_eq!(c.sources, vec![Source::Feeder]);
        assert!(c.duplex);
        assert_eq!(c.resolutions, vec![100, 150, 200, 300, 600]);
        assert_eq!(c.color_modes, vec![ColorMode::Color, ColorMode::Gray, ColorMode::Bw]);
        assert!((c.max_size_mm.0 - 215.9).abs() < 1e-3);
        assert!((c.max_size_mm.1 - 863.0).abs() < 1e-3);

        // Pixel geometry, flatbed only, gray only (Bw offered via gray).
        let opts = Options {
            all: vec![
                opt("mode", ValueType::String, Unit::Other, Constraint::StringList(s(&["Gray"]))),
                opt("br-x", ValueType::Int, Unit::Pixel, range(0.0, 3000.0)),
                opt("br-y", ValueType::Int, Unit::Pixel, range(0.0, 6000.0)),
            ],
        };
        let c = capabilities(&opts, 300.0);
        assert_eq!(c.sources, vec![Source::Flatbed]);
        assert!(!c.duplex);
        assert_eq!(c.color_modes, vec![ColorMode::Gray, ColorMode::Bw]);
        assert!((c.max_size_mm.0 - 254.0).abs() < 1e-3);
        assert!((c.max_size_mm.1 - 508.0).abs() < 1e-3);
    }

    #[test]
    fn get_prefers_active() {
        let mut a = opt("mode", ValueType::String, Unit::Other, Constraint::None);
        a.active = false;
        a.index = 1;
        let mut b = a.clone();
        b.active = true;
        b.index = 2;
        let o = Options { all: vec![a, b] };
        assert_eq!(o.get("mode").unwrap().index, 2);
        assert!(o.get("nope").is_none());
    }
}
