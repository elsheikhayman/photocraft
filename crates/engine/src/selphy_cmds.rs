//! Filter › Print Sheet: lays copies of the active photo on a new sheet of printer paper (a grid
//! of copies with a gap, shrunk and shifted to survive a borderless print), with a cut
//! guide on each photo edge. Meant for Canon Selphy 100 x 148 mm paper, where borderless printing
//! enlarges the picture (about 107%) and trims the edges.

use photocraft_algo::resample::{Resample, resize_surface_in_canvas};
use photocraft_color::{Color, ColorMode};
use photocraft_doc::{Document, Layer, Size};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const CMD: &str = "image.selphySheet.make";
/// The largest sheet (px).
const MAX_SHEET_PIXELS: u64 = 60_000_000;
const MAX_COPIES: u32 = 100;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// What the sheet looks like; all lengths in mm.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub paper_width_mm: f64,
    pub paper_height_mm: f64,
    pub dpi: f64,
    pub copies: u32,
    pub gap_mm: f64,
    /// Scale applied to the whole grid (percent).
    pub scale_pct: f64,
    /// Where the scaled grid's top-left corner goes.
    pub left_mm: f64,
    pub top_mm: f64,
    /// Make room for the printer's borderless enlargement: scale and shift the grid by `scale_pct`,
    /// `left_mm` and `top_mm`. Off: 100%, centred on the paper.
    pub margin: bool,
    /// Lay the photos on their side (turned 90 degrees).
    pub rotate: bool,
    pub cut_guides: bool,
    /// Print the cut marks on the sheet.
    pub cut_marks: bool,
    /// Size of one photo; `None` takes it from the source's pixels and dpi.
    pub photo_mm: Option<(f64, f64)>,
}

impl Default for Sheet {
    fn default() -> Self {
        Self { paper_width_mm: 148.0, paper_height_mm: 100.0, dpi: 300.0, copies: 0, gap_mm: 1.0, scale_pct: 93.0, left_mm: 6.0, top_mm: 3.0, margin: true, rotate: false, cut_guides: true, cut_marks: true, photo_mm: None }
    }
}

fn num(p: &Value, key: &str, default: f64) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|n| n.is_finite()).ok_or_else(|| bad(format!("`{key}` = {v} must be a number"))),
    }
}

pub fn sheet(p: &Value) -> Result<Sheet> {
    let d = Sheet::default();
    // `copies` absent, 0 or "max": as many as fit on the paper.
    let copies = match p.get("copies") {
        None | Some(Value::Null) => 0.0,
        Some(Value::String(t)) if t.eq_ignore_ascii_case("max") => 0.0,
        Some(_) => num(p, "copies", 0.0)?,
    };
    let photo_mm = match (p.get("photoWidthMm"), p.get("photoHeightMm")) {
        (Some(w), Some(h)) if !w.is_null() && !h.is_null() => Some((num(p, "photoWidthMm", 0.0)?, num(p, "photoHeightMm", 0.0)?)),
        _ => None,
    };
    let st = Sheet {
        paper_width_mm: num(p, "paperWidthMm", d.paper_width_mm)?,
        paper_height_mm: num(p, "paperHeightMm", d.paper_height_mm)?,
        dpi: num(p, "dpi", d.dpi)?,
        copies: if copies == 0.0 || (1.0..=f64::from(MAX_COPIES)).contains(&copies) { copies as u32 } else { return Err(bad(format!("`copies` must be 1..={MAX_COPIES}, 0 or \"max\""))) },
        gap_mm: num(p, "gapMm", d.gap_mm)?,
        scale_pct: num(p, "scalePct", d.scale_pct)?,
        left_mm: num(p, "leftMm", d.left_mm)?,
        top_mm: num(p, "topMm", d.top_mm)?,
        cut_guides: p.get("cutGuides").and_then(Value::as_bool).unwrap_or(d.cut_guides),
        margin: p.get("margin").and_then(Value::as_bool).unwrap_or(d.margin),
        rotate: p.get("rotate").and_then(Value::as_bool).unwrap_or(d.rotate),
        cut_marks: p.get("cutMarks").and_then(Value::as_bool).unwrap_or(d.cut_marks),
        photo_mm,
    };
    if !(10.0..=2000.0).contains(&st.paper_width_mm) || !(10.0..=2000.0).contains(&st.paper_height_mm) {
        return Err(bad("the paper size must be 10..2000 mm"));
    }
    if !(10.0..=1200.0).contains(&st.dpi) {
        return Err(bad("`dpi` must be 10..1200"));
    }
    if !(0.0..=50.0).contains(&st.gap_mm) || !(10.0..=300.0).contains(&st.scale_pct) {
        return Err(bad("`gapMm` must be 0..50 and `scalePct` 10..300"));
    }
    if !(-500.0..=2000.0).contains(&st.left_mm) || !(-500.0..=2000.0).contains(&st.top_mm) {
        return Err(bad("`leftMm` and `topMm` are out of range"));
    }
    if let Some((w, h)) = st.photo_mm {
        if !(1.0..=2000.0).contains(&w) || !(1.0..=2000.0).contains(&h) {
            return Err(bad("the photo size must be 1..2000 mm"));
        }
    }
    let px = |mm: f64| mm / 25.4 * st.dpi;
    if (px(st.paper_width_mm) * px(st.paper_height_mm)) as u64 > MAX_SHEET_PIXELS {
        return Err(bad("the sheet is over the pixel limit; lower the dpi"));
    }
    Ok(st)
}

/// Columns and rows for `copies` photos of `photo` mm with `gap` mm between them, scaled by `k`:
/// the arrangement that fits the paper from `(left, top)` with the fewest empty places, then the
/// squarest, then the fewest rows. When none fits, one row for 1-2 copies and two from there on.
pub fn grid(copies: u32, photo: (f64, f64), gap: f64, k: f64, paper: (f64, f64), origin: (f64, f64)) -> (u32, u32) {
    let mut best: Option<((u32, u32, u32), (u32, u32))> = None;
    for rows in 1..=copies {
        let cols = copies.div_ceil(rows);
        let (w, h) = (f64::from(cols) * (photo.0 + gap) - gap, f64::from(rows) * (photo.1 + gap) - gap);
        if origin.0 + w * k > paper.0 + 1e-6 || origin.1 + h * k > paper.1 + 1e-6 {
            continue;
        }
        let key = (cols * rows - copies, cols.abs_diff(rows), rows);
        if best.is_none_or(|(b, _)| key < b) {
            best = Some((key, (cols, rows)));
        }
    }
    best.map(|(_, g)| g).unwrap_or_else(|| {
        let rows = if copies <= 2 { 1 } else { 2 };
        (copies.div_ceil(rows), rows)
    })
}

/// The photo's size on the sheet (mm, turned if asked), the grid scale and where the grid starts.
fn basics(src: &Document, st: &Sheet) -> ((f64, f64), f64, (f64, f64)) {
    let dpi_src = if src.resolution_dpi > 0.0 { f64::from(src.resolution_dpi) } else { 72.0 };
    let (pw, ph) = st.photo_mm.unwrap_or((f64::from(src.size.width) / dpi_src * 25.4, f64::from(src.size.height) / dpi_src * 25.4));
    let photo = if st.rotate { (ph, pw) } else { (pw, ph) };
    let k = if st.margin { st.scale_pct / 100.0 } else { 1.0 };
    (photo, k, if st.margin { (st.left_mm, st.top_mm) } else { (0.0, 0.0) })
}

/// The most photos that fit on the paper from the grid's start, as whole columns and rows.
fn fit(photo: (f64, f64), gap: f64, k: f64, paper: (f64, f64), origin: (f64, f64)) -> u32 {
    let count = |avail: f64, size: f64| ((((avail / k) + gap) / (size + gap)) + 1e-9).floor().clamp(0.0, 1000.0) as u32;
    (count(paper.0 - origin.0, photo.0) * count(paper.1 - origin.1, photo.1)).max(1)
}

/// How many copies the sheet holds when `copies` is automatic (the dialog shows it).
pub fn max_copies(s: &Session, st: &Sheet) -> Result<u32> {
    let src = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let (photo, k, origin) = basics(src, st);
    Ok(fit(photo, st.gap_mm, k, (st.paper_width_mm, st.paper_height_mm), origin).min(MAX_COPIES))
}

/// Builds the sheet document from the active document's picture.
pub fn build(s: &Session, st: &Sheet) -> Result<Document> {
    let src = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let px = |mm: f64| mm / 25.4 * st.dpi;
    let ((pw_mm, ph_mm), k, origin) = basics(src, st);
    let paper = (st.paper_width_mm, st.paper_height_mm);
    let copies = if st.copies == 0 { max_copies(s, st)? } else { st.copies };
    let (cols, rows) = grid(copies, (pw_mm, ph_mm), st.gap_mm, k, paper, origin);
    // Without the margin the grid is centred on the paper.
    let (left_mm, top_mm) = if st.margin {
        (st.left_mm, st.top_mm)
    } else {
        ((paper.0 - (f64::from(cols) * (pw_mm + st.gap_mm) - st.gap_mm)) / 2.0, (paper.1 - (f64::from(rows) * (ph_mm + st.gap_mm) - st.gap_mm)) / 2.0)
    };
    let (sheet_w, sheet_h) = (px(st.paper_width_mm).round(), px(st.paper_height_mm).round());
    if !(1.0..=100_000.0).contains(&sheet_w) || !(1.0..=100_000.0).contains(&sheet_h) {
        return Err(bad("the sheet size is out of range"));
    }
    let mode = match src.mode {
        ColorMode::Indexed | ColorMode::Multichannel => ColorMode::Rgb,
        ColorMode::Bitmap | ColorMode::Duotone => ColorMode::Grayscale,
        m => m,
    };
    let mut doc = Document::with_background("Print Sheet", Size::new(sheet_w as u32, sheet_h as u32), mode, src.depth, Color::rgba(1.0, 1.0, 1.0, 1.0));
    doc.resolution_dpi = st.dpi as f32;
    let fmt = doc.pixel_format();
    let mut flat = photocraft_compose::flatten_to_surface(src, fmt, Some([1.0, 1.0, 1.0]));
    let (mut src_w, mut src_h) = (src.size.width, src.size.height);
    if st.rotate {
        // Turn the picture a quarter clockwise.
        let n = fmt.channels();
        let data = flat.read_region(src.bounds());
        let (w, h) = (src_w as usize, src_h as usize);
        let mut turned = vec![0.0f32; data.len()];
        for y in 0..h {
            for x in 0..w {
                let from = (y * w + x) * n;
                let to = (x * h + (h - 1 - y)) * n;
                if let (Some(a), Some(b)) = (data.get(from..from + n), turned.get_mut(to..to + n)) {
                    b.copy_from_slice(a);
                }
            }
        }
        flat = photocraft_raster::Surface::new(fmt);
        flat.write_region(Rect::new(0, 0, h as i32, w as i32), &turned);
        std::mem::swap(&mut src_w, &mut src_h);
    }
    // Cell edges in px, from the mm positions, so neighbouring cells share an exact edge.
    let x_at = |c: u32| (px(left_mm + f64::from(c) * (pw_mm + st.gap_mm) * k)).round() as i32;
    let y_at = |r: u32| (px(top_mm + f64::from(r) * (ph_mm + st.gap_mm) * k)).round() as i32;
    let right = |c: u32| (px(left_mm + (f64::from(c) * (pw_mm + st.gap_mm) + pw_mm) * k)).round() as i32;
    let bottom = |r: u32| (px(top_mm + (f64::from(r) * (ph_mm + st.gap_mm) + ph_mm) * k)).round() as i32;
    let src_bounds = Rect::new(0, 0, src_w as i32, src_h as i32);
    let mut cell_cache: Option<(i32, i32, Vec<f32>)> = None;
    let mut n = 0;
    for r in 0..rows {
        for c in 0..cols {
            if n >= copies {
                break;
            }
            n += 1;
            let (x0, y0, x1, y1) = (x_at(c), y_at(r), right(c), bottom(r));
            let (w, h) = (x1 - x0, y1 - y0);
            if w < 1 || h < 1 || w > 100_000 || h > 100_000 {
                return Err(bad("a photo on the sheet would be empty or too large"));
            }
            let data = match &cell_cache {
                Some((cw, ch, d)) if *cw == w && *ch == h => d.clone(),
                _ => {
                    let scaled = resize_surface_in_canvas(&flat, f64::from(w) / f64::from(src_w.max(1)), f64::from(h) / f64::from(src_h.max(1)), Resample::BicubicSharper, src_bounds);
                    let d = scaled.read_region(Rect::new(0, 0, w, h));
                    cell_cache = Some((w, h, d.clone()));
                    d
                }
            };
            let mut layer = Layer::raster(format!("Photo {n}"), fmt);
            if let Some(surf) = layer.surface_mut() {
                surf.write_region(Rect::new(x0, y0, x1, y1), &data);
            }
            doc.layers.push(layer);
        }
    }
    // The cuts. The right and bottom trims leave a gap's width of white beside the photos; between photos
    // each cut runs exactly along a photo edge (both edges of every gap).
    let trim_x = right(cols - 1) + px(st.gap_mm * k).round() as i32;
    let trim_y = bottom(rows - 1) + px(st.gap_mm * k).round() as i32;
    if st.cut_guides {
        let v = (0..cols.saturating_sub(1)).flat_map(|c| [right(c), x_at(c + 1)]).chain([trim_x]);
        doc.guides.vertical.extend(v.map(f64::from));
        let h = (0..rows.saturating_sub(1)).flat_map(|r| [bottom(r), y_at(r + 1)]).chain([trim_y]);
        doc.guides.horizontal.extend(h.map(f64::from));
    }
    if st.cut_marks {
        // The marks of the Photoshop action: bars 0.15 mm thick, light grey; the horizontal ones start at the
        // paper's edge so the printer's trim leaves them.
        let (w, h) = (sheet_w as i32, sheet_h as i32);
        let (t, len) = ((px(0.15).round() as i32).max(1), (px(10.0).round() as i32).max(2));
        let mut layer = Layer::raster("Cut marks", fmt);
        if let Some(surf) = layer.surface_mut() {
            let black = photocraft_raster::from_rgba(&fmt, [0.65, 0.65, 0.65, 1.0]);
            let mut put = |r: Rect| {
                let r = r.intersect(&Rect::new(0, 0, w, h));
                if r.is_empty() {
                    return;
                }
                let data: Vec<f32> = (0..r.width() * r.height()).flat_map(|_| black.iter().copied()).collect();
                surf.write_region(r, &data);
            };
            // The right trim's bar runs the full height of the sheet.
            put(Rect::new(trim_x, 0, trim_x + t, h));
            // The cut between the rows is drawn in the gap, just above the second row, so no bar
            // touches a photo.
            let mut rows_cut = vec![trim_y];
            if rows > 1 {
                rows_cut.push(y_at(1) - t);
            }
            // The right-hand bars start `len` before the last photo's edge, so most of each is still
            // there after the right trim is cut off.
            let start = right(cols - 1) + (px(0.3).round() as i32).max(1) - len;
            for y in rows_cut {
                put(Rect::new(0, y, len, y + t));
                put(Rect::new(start, y, trim_x + len, y + t));
            }
        }
        doc.layers.push(layer);
    }
    Ok(doc)
}

/// The sheet as 8-bit RGBA (`width`, `height`, bytes) for the dialog preview; builds a document but
/// changes nothing.
pub fn preview(s: &Session, st: &Sheet) -> Result<(usize, usize, Vec<u8>)> {
    let doc = build(s, st)?;
    let buf = photocraft_compose::flatten(&doc).over_background([1.0, 1.0, 1.0]);
    let bytes = buf.px.iter().flat_map(|px| px.iter().map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
    Ok((doc.size.width as usize, doc.size.height as usize, bytes))
}

fn make(s: &mut Session, p: &Value) -> Result<Value> {
    let st = sheet(p)?;
    let copies = if st.copies == 0 { max_copies(s, &st)? } else { st.copies };
    let doc = build(s, &st)?;
    let (w, h) = (doc.size.width, doc.size.height);
    let index = s.add_document(doc, None);
    Ok(json!({"document": index, "width": w, "height": h, "dpi": st.dpi, "copies": copies}))
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "Print Sheet…",
        menu: &[],
        shortcut: None,
        params: r##"{"paperWidthMm":148,"paperHeightMm":100,"dpi":300,"copies":"max" (1..100, or 0/"max"/absent for as many as fit; arranged to fit the paper),"margin":true (scale/shift for a borderless print; false = 100% centred),"rotate":false (photos turned 90 degrees),"gapMm":1,"scalePct":93,"leftMm":6,"topMm":3,"cutGuides":true (Photoshop guides at the cuts),"cutMarks":true (bars 10 mm x 0.15 mm, light grey, printed at the cuts) (the trims leave a gap's width of white),"photoWidthMm"?,"photoHeightMm"? (default: the picture's pixels at its dpi)} → {document, width, height, dpi, copies}. Opens a new document with the flattened active picture repeated in a grid (one layer per copy), scaled and shifted for a borderless print."##,
        enabled: has_doc,
        run: make,
        journal: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_copies_of_35x45_land_on_the_recipe_positions() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 413, "height": 531, "background": "white"})).unwrap();
        s.active_mut().unwrap().doc.resolution_dpi = 300.0;
        let r = s.execute(CMD, json!({})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(1748), Some(1181)));
        let d = &s.active().unwrap().doc;
        assert_eq!(d.layers.len(), 10);
        assert_eq!(d.guides.vertical.len(), 7);
        assert_eq!(d.guides.horizontal.len(), 3);
    }

    #[test]
    fn rejects_bad_params() {
        let mut s = Session::new();
        assert!(s.execute(CMD, json!({})).is_err());
        s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
        for p in [json!({"copies": -1}), json!({"copies": 999}), json!({"dpi": 0}), json!({"scalePct": "x"}), json!({"paperWidthMm": 0})] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
        }
    }
}
