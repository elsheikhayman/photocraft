//! File › Automate › ID Photo: turns a photo with a face rectangle selected into an exact
//! passport/ID photo (size in mm at a dpi, face height, eye line, shift and turn). The picture is
//! sampled straight from the flattened document, so the area the frame reaches beyond the photo is
//! filled by repeating the photo's edge pixels.

use photocraft_algo::resample::Resample;
use photocraft_doc::{Document, LayerContent};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::idphoto_layout::{self, FaceRect, Layout, Settings};
use crate::{EngineError, Result, Session};

const CMD: &str = "image.idPhoto.make";
/// The largest finished photo (px): about the 36 MP the other heavy commands are measured on.
const MAX_OUT_PIXELS: u64 = 36_000_000;
/// The most source samples read for one photo (floats), so an absurd frame can't exhaust memory.
const MAX_SOURCE_SAMPLES: u64 = 500_000_000;
/// The most channels sampled per pixel (CMYK with alpha is 5).
const MAX_CHANNELS: usize = 8;
/// The most sub-samples per output pixel along one axis when the picture is reduced.
const MAX_SUPERSAMPLE: usize = 8;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// A numeric parameter, or `default` when absent or null.
fn num(p: &Value, key: &str, default: f64) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_f64().filter(|n| n.is_finite()).ok_or_else(|| bad(format!("`{key}` = {v} must be a number"))),
    }
}

/// The settings in `p`, with the defaults of [`Settings`] (a 35 x 45 mm photo at 300 dpi).
pub fn settings(p: &Value) -> Result<Settings> {
    let d = Settings::default();
    let st = Settings {
        width_mm: num(p, "widthMm", d.width_mm)?,
        height_mm: num(p, "heightMm", d.height_mm)?,
        dpi: num(p, "dpi", d.dpi)?,
        face_height_mm: num(p, "faceHeightMm", d.face_height_mm)?,
        eye_from_bottom_pct: num(p, "eyeFromBottomPct", d.eye_from_bottom_pct)?,
        eye_in_face: num(p, "eyeInFace", d.eye_in_face)?,
        h_offset_pct: num(p, "hOffsetPct", d.h_offset_pct)?,
    };
    idphoto_layout::validate(&st)?;
    Ok(st)
}

/// The face rectangle: `face` {x, y, width, height} when given, else the selection's bounds, both
/// clipped to the canvas.
pub fn face_rect(s: &Session, p: &Value) -> Result<FaceRect> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    // An explicit `face` may reach past the photo (the dialog's fit-to-frame start does); only a
    // selection is clipped to it.
    let explicit = p.get("face").is_some_and(|v| !v.is_null());
    let r = match p.get("face") {
        None | Some(Value::Null) => d.doc.selection.as_ref().map(|sel| sel.content_bounds()).unwrap_or(Rect::EMPTY),
        Some(f) => {
            let get = |k: &str| -> Result<i32> {
                let v = f.get(k).and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok());
                v.ok_or_else(|| bad(format!("`face.{k}` must be a whole number of pixels")))
            };
            let (x, y, w, h) = (get("x")?, get("y")?, get("width")?, get("height")?);
            match (x.checked_add(w), y.checked_add(h)) {
                (Some(x1), Some(y1)) if w > 0 && h > 0 => Rect::new(x, y, x1, y1),
                _ => return Err(bad("`face` needs a positive `width` and `height`")),
            }
        }
    };
    let r = if explicit { r } else { r.intersect(&canvas) };
    if r.is_empty() {
        return Err(EngineError::Other("ID Photo needs a rectangular selection around the face (or a `face` rectangle)".into()));
    }
    Ok(FaceRect { x0: f64::from(r.x0), y0: f64::from(r.y0), x1: f64::from(r.x1), y1: f64::from(r.y1) })
}

/// The part of the source the frame can touch, clipped to the canvas.
fn source_region(lay: &Layout, canvas: Rect) -> Rect {
    let (hx, hy) = (lay.frame_width / 2.0 + 2.0, lay.frame_height / 2.0 + 2.0);
    let clip = |v: f64, hi: i32| v.clamp(0.0, f64::from(hi));
    Rect::new(
        clip((lay.centre_x - hx).floor(), canvas.x1) as i32,
        clip((lay.centre_y - hy).floor(), canvas.y1) as i32,
        clip((lay.centre_x + hx).ceil(), canvas.x1) as i32,
        clip((lay.centre_y + hy).ceil(), canvas.y1) as i32,
    )
}

/// Renders the finished photo (interleaved normalised floats, `width * height * n`) from `src`,
/// the `n`-channel pixels of `region`. Each output pixel averages a grid of bilinear samples (more
/// when the picture is reduced) and reads outside `region` repeat its edge pixels.
pub(crate) fn render(src: &[f32], region: Rect, n: usize, lay: &Layout) -> Vec<f32> {
    let (rw, rh) = (region.width() as usize, region.height() as usize);
    let (w, h) = (lay.out_width_px as usize, lay.out_height_px as usize);
    let mut out = vec![0.0f32; w * h * n];
    if rw == 0 || rh == 0 || n == 0 || n > MAX_CHANNELS || src.len() < rw * rh * n {
        return out;
    }
    let ss = (1.0 / lay.scale).ceil().clamp(1.0, MAX_SUPERSAMPLE as f64) as usize;
    let (x_lo, x_hi) = (0.0, (rw - 1) as f64);
    let (y_lo, y_hi) = (0.0, (rh - 1) as f64);
    let at = |x: usize, y: usize, c: usize| f64::from(src[(y * rw + x) * n + c]);
    out.par_chunks_mut(w * n).enumerate().for_each(|(oy, row)| {
        for ox in 0..w {
            let mut acc = [0.0f64; MAX_CHANNELS];
            for j in 0..ss {
                for i in 0..ss {
                    let u = ox as f64 + (i as f64 + 0.5) / ss as f64;
                    let v = oy as f64 + (j as f64 + 0.5) / ss as f64;
                    let (du, dv) = ((u - w as f64 / 2.0) / lay.scale, (v - h as f64 / 2.0) / lay.scale);
                    let sx = lay.centre_x + du - f64::from(region.x0) - 0.5;
                    let sy = lay.centre_y + dv - f64::from(region.y0) - 0.5;
                    let (sx, sy) = (sx.clamp(x_lo, x_hi), sy.clamp(y_lo, y_hi));
                    let (x0, y0) = (sx.floor(), sy.floor());
                    let (fx, fy) = (sx - x0, sy - y0);
                    let (x0, y0) = (x0 as usize, y0 as usize);
                    let (x1, y1) = ((x0 + 1).min(rw - 1), (y0 + 1).min(rh - 1));
                    for (c, a) in acc.iter_mut().enumerate().take(n) {
                        let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
                        let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
                        *a += top * (1.0 - fy) + bottom * fy;
                    }
                }
            }
            let k = (ss * ss) as f64;
            for c in 0..n {
                row[ox * n + c] = (acc[c] / k) as f32;
            }
        }
    });
    out
}

/// Builds the finished photo's pixels for the active document without changing it (the dialog preview).
pub fn build(s: &Session, st: &Settings, face: FaceRect) -> Result<(Layout, Surface)> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let lay = idphoto_layout::layout(face, st)?;
    let (w, h) = (lay.out_width_px, lay.out_height_px);
    if u64::from(w) * u64::from(h) > MAX_OUT_PIXELS {
        return Err(bad(format!("{w} x {h} px is over the {} megapixel limit; lower the size or dpi", MAX_OUT_PIXELS / 1_000_000)));
    }
    crate::image_cmds::check_resample_budget(CMD, &d.doc, w, h, "")?;
    let fmt = d.doc.pixel_format();
    let n = fmt.channels();
    if n > MAX_CHANNELS {
        return Err(EngineError::Other(format!("ID Photo supports up to {MAX_CHANNELS} channels, this document has {n}")));
    }
    let region = source_region(&lay, d.doc.bounds());
    if region.is_empty() {
        return Err(EngineError::Other("the photo frame is outside the image".into()));
    }
    if u64::from(region.width()) * u64::from(region.height()) * n as u64 > MAX_SOURCE_SAMPLES {
        return Err(bad("the face is too small for this photo size: the frame covers too much of the image; select the face more closely"));
    }
    let flat = photocraft_compose::flatten_to_surface(&d.doc, fmt, Some([1.0, 1.0, 1.0]));
    let src = flat.read_region(region);
    let out = render(&src, region, n, &lay);
    let mut surf = Surface::new(fmt);
    surf.write_region(Rect::new(0, 0, w as i32, h as i32), &out);
    Ok((lay, surf))
}

/// Fills the part of the Background outside `old` (where the photo was after the crop) by repeating
/// the photo's nearest edge pixels, so a frame that reaches past the photo has no empty border.
fn extend_edges(doc: &mut Document, old: Rect) {
    let canvas = doc.bounds();
    let Some(bg) = doc.background_mut() else { return };
    let LayerContent::Raster(surf) = &mut bg.content else { return };
    let n = surf.channels();
    let (y0, y1) = (old.y0.max(canvas.y0), old.y1.min(canvas.y1));
    if y1 > y0 {
        if old.x0 > canvas.x0 {
            repeat_column(surf, old.x0, Rect::new(canvas.x0, y0, old.x0.min(canvas.x1), y1), n);
        }
        if old.x1 < canvas.x1 {
            repeat_column(surf, old.x1 - 1, Rect::new(old.x1.max(canvas.x0), y0, canvas.x1, y1), n);
        }
    }
    if old.y0 > canvas.y0 {
        repeat_row(surf, old.y0, Rect::new(canvas.x0, canvas.y0, canvas.x1, old.y0.min(canvas.y1)));
    }
    if old.y1 < canvas.y1 {
        repeat_row(surf, old.y1 - 1, Rect::new(canvas.x0, old.y1.max(canvas.y0), canvas.x1, canvas.y1));
    }
}

/// Fills `fill` with the pixels of column `x`, row by row.
fn repeat_column(surf: &mut Surface, x: i32, fill: Rect, n: usize) {
    if fill.is_empty() {
        return;
    }
    let line = surf.read_region(Rect::new(x, fill.y0, x + 1, fill.y1));
    let mut data = Vec::with_capacity(fill.width() as usize * fill.height() as usize * n);
    for px in line.chunks_exact(n) {
        for _ in 0..fill.width() {
            data.extend_from_slice(px);
        }
    }
    surf.write_region(fill, &data);
}

/// Fills `fill` with the pixels of row `y`.
fn repeat_row(surf: &mut Surface, y: i32, fill: Rect) {
    if fill.is_empty() {
        return;
    }
    let line = surf.read_region(Rect::new(fill.x0, y, fill.x1, y + 1));
    let mut data = Vec::with_capacity(line.len() * fill.height() as usize);
    for _ in 0..fill.height() {
        data.extend_from_slice(&line);
    }
    surf.write_region(fill, &data);
}

/// Like the Crop tool with Delete Cropped Pixels off: the whole document (layers, masks, effects)
/// is scaled so the face is the chosen size, then the canvas is cropped to the photo frame. The
/// layers keep the pixels outside the frame, and the Background repeats its edges past the photo.
fn make(s: &mut Session, p: &Value) -> Result<Value> {
    let st = settings(p)?;
    let face = face_rect(s, p)?;
    let lay = idphoto_layout::layout(face, &st)?;
    let (w, h) = (lay.out_width_px, lay.out_height_px);
    if u64::from(w) * u64::from(h) > MAX_OUT_PIXELS {
        return Err(bad(format!("{w} x {h} px is over the {} megapixel limit; lower the size or dpi", MAX_OUT_PIXELS / 1_000_000)));
    }
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (ow, oh) = (f64::from(d.doc.size.width), f64::from(d.doc.size.height));
    let (nw, nh) = ((ow * lay.scale).round(), (oh * lay.scale).round());
    if !(1.0..=300_000.0).contains(&nw) || !(1.0..=300_000.0).contains(&nh) {
        return Err(bad("the face is too small or too large for this photo size; select the face more closely"));
    }
    crate::image_cmds::check_resample_budget(CMD, &d.doc, nw as u32, nh as u32, "; select the face more closely")?;
    // The frame's corner in the scaled document, from the scale the pixels actually got.
    let (x0, y0) = (((lay.centre_x - lay.frame_width / 2.0) * nw / ow).round(), ((lay.centre_y - lay.frame_height / 2.0) * nh / oh).round());
    if x0.abs() > 1.0e6 || y0.abs() > 1.0e6 {
        return Err(bad("the photo frame is too far outside the image"));
    }
    let (x0, y0) = (x0 as i32, y0 as i32);
    let frame = Rect::new(x0, y0, x0 + w as i32, y0 + h as i32);
    let photo = Rect::new(-x0, -y0, nw as i32 - x0, nh as i32 - y0);
    let filter = if lay.scale > 1.0 { Resample::PreserveDetails } else { Resample::BicubicSharper };
    let dpi = st.dpi as f32;
    s.edit("ID Photo", |doc, _| {
        crate::image_cmds::resample_doc(doc, nw as u32, nh as u32, filter);
        crate::image_cmds::crop_doc(doc, CMD, frame, false, [1.0, 1.0, 1.0, 1.0])?;
        extend_edges(doc, photo);
        // The result is a normal layer, not a locked Background, so it can be moved and edited.
        if let Some(bg) = doc.background_mut() {
            bg.name = "Layer 0".into();
            bg.locks.transparency = false;
            bg.locks.position = false;
        }
        doc.selection = None;
        doc.resolution_dpi = dpi;
        // Guides at the head top, eye line and chin, and down the middle, to check the result by.
        let px = |mm: f64| mm / 25.4 * st.dpi;
        doc.guides.horizontal.extend([px(lay.head_top_mm), px(lay.eye_line_mm), px(lay.chin_mm)]);
        doc.guides.vertical.push(f64::from(w) / 2.0);
        Ok(())
    })?;
    Ok(json!({
        "width": w,
        "height": h,
        "dpi": st.dpi,
        "scale": lay.scale,
        "headTopMm": lay.head_top_mm,
        "eyeLineMm": lay.eye_line_mm,
        "chinMm": lay.chin_mm,
    }))
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "ID Photo…",
        menu: &[],
        shortcut: None,
        params: r##"{"widthMm":35,"heightMm":45,"dpi":300,"faceHeightMm":33.5 (head top to chin),"eyeFromBottomPct":56,"eyeInFace":0.5 (eye line as a fraction of the face height from its top),"hOffsetPct":0 (-100..100, picture shift as % of the width),"face":{"x","y","width","height"}? (default: the rectangular selection)} → {width, height, dpi, scale, headTopMm, eyeLineMm, chinMm}. Flattens the image and replaces it with the finished photo, repeating the edge pixels where the frame goes past the photo."##,
        enabled: has_doc,
        run: make,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::Size;

    fn session(w: u32, h: u32) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": w, "height": h})).unwrap();
        s
    }

    fn face(s: &mut Session, x: i32, y: i32, w: i32, h: i32) {
        s.execute("select.rect", json!({"x": x, "y": y, "width": w, "height": h})).unwrap();
    }

    #[test]
    fn makes_the_exact_pixel_size_and_dpi_in_one_undo_step() {
        let mut s = session(1200, 1600);
        face(&mut s, 400, 300, 400, 500);
        let r = s.execute(CMD, json!({"widthMm": 35, "heightMm": 45, "dpi": 300})).unwrap();
        assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(413), Some(531)));
        let d = &s.active().unwrap().doc;
        assert_eq!(d.size, Size::new(413, 531));
        assert_eq!(d.resolution_dpi, 300.0);
        assert_eq!(d.layers.len(), 1);
        assert!(d.selection.is_none());
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.size, Size::new(1200, 1600));
        assert!(s.active().unwrap().doc.selection.is_some());
    }

    #[test]
    fn repeats_the_photo_edge_beyond_the_image_and_keeps_the_picture_inside() {
        // A red left half, a blue right half; the face frame reaches past the top and sides.
        let mut s = session(100, 100);
        s.execute("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 50, "height": 100})).unwrap();
        s.execute("edit.fill", json!({"contents": "foreground"})).unwrap();
        s.execute("select.rect", json!({"x": 50, "y": 0, "width": 50, "height": 100})).unwrap();
        s.execute("edit.fill", json!({"contents": "background"})).unwrap();
        face(&mut s, 30, 10, 40, 60);
        s.execute(CMD, json!({"widthMm": 35, "heightMm": 45, "dpi": 100, "faceHeightMm": 12})).unwrap();
        let d = &s.active().unwrap().doc;
        let px = d.layers[0].surface().unwrap();
        let (w, h) = (d.size.width as i32, d.size.height as i32);
        // Left and right edges keep their colours all the way up to the top row (no white fill).
        let left = px.rgba(0, 0);
        let right = px.rgba(w - 1, 0);
        assert!(left[0] > 0.99 && left[2] < 0.01, "left top {left:?}");
        assert!(right[2] > 0.99 && right[0] < 0.01, "right top {right:?}");
        assert!(px.rgba(0, h - 1)[0] > 0.99);
        assert_eq!(px.rgba(w / 2, h / 2)[3], 1.0);
    }

    #[test]
    fn face_height_in_the_result_matches_the_setting() {
        // A white photo with a black 100 x 200 px "face"; at 25.4 dpi 1 mm is 1 px.
        let mut s = session(600, 600);
        s.execute("tools.setColors", json!({"foreground": "#000000"})).unwrap();
        face(&mut s, 250, 200, 100, 200);
        s.execute("edit.fill", json!({"contents": "foreground"})).unwrap();
        s.execute(CMD, json!({"widthMm": 60, "heightMm": 80, "dpi": 25.4, "faceHeightMm": 40})).unwrap();
        let d = &s.active().unwrap().doc;
        let px = d.layers[0].surface().unwrap();
        let x = d.size.width as i32 / 2;
        let dark: Vec<i32> = (0..d.size.height as i32).filter(|y| px.rgba(x, *y)[0] < 0.5).collect();
        let span = dark.last().unwrap() - dark.first().unwrap() + 1;
        assert!((span - 40).abs() <= 1, "face spans {span} px");
    }

    #[test]
    fn needs_a_selection_and_rejects_bad_params_without_changing_the_document() {
        let mut s = session(100, 100);
        assert!(s.execute(CMD, json!({})).is_err());
        face(&mut s, 20, 20, 40, 50);
        for p in [
            json!({"widthMm": 0}),
            json!({"dpi": 0}),
            json!({"faceHeightMm": "big"}),
            json!({"eyeFromBottomPct": 150}),
            json!({"widthMm": 1000, "heightMm": 1000, "dpi": 10000}),
            json!({"face": {"x": 0, "y": 0, "width": 0, "height": 5}}),
        ] {
            assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
            assert_eq!(s.active().unwrap().doc.size, Size::new(100, 100), "{p}");
        }
    }

    #[test]
    fn works_at_every_depth() {
        for depth in [8, 16, 32] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": 200, "height": 300, "depth": depth})).unwrap();
            face(&mut s, 60, 50, 80, 100);
            let r = s.execute(CMD, json!({"dpi": 100})).unwrap();
            assert_eq!(r["width"], 138, "{depth}-bit");
            assert_eq!(s.active().unwrap().doc.size, Size::new(138, 177), "{depth}-bit");
        }
    }
}
