//! Pure placement maths for the ID Photo command and its dialog preview: where the source frame
//! sits for a given face rectangle, photo size and face/eye settings, and where the head top, eye
//! line and chin land in the finished photo.

use crate::{EngineError, Result};

const COMMAND: &str = "image.idPhoto.make";
const MM_PER_INCH: f64 = 25.4;

/// The largest photo side (mm), dpi and output side (px) the command accepts.
pub const MAX_MM: f64 = 1000.0;
pub const MAX_DPI: f64 = 10_000.0;
pub const MAX_SIDE_PX: u32 = 20_000;

/// What the user chose, in millimetres and percentages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub width_mm: f64,
    pub height_mm: f64,
    pub dpi: f64,
    /// Head top to chin, in mm of the finished photo.
    pub face_height_mm: f64,
    /// The eye line's distance from the photo's bottom edge, % of the photo height.
    pub eye_from_bottom_pct: f64,
    /// Where the eye line sits inside the face rectangle, as a fraction of its height from the top.
    pub eye_in_face: f64,
    /// Horizontal shift of the picture, % of the photo width (positive moves it right).
    pub h_offset_pct: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            width_mm: 35.0,
            height_mm: 45.0,
            dpi: 300.0,
            face_height_mm: 33.5,
            eye_from_bottom_pct: 56.0,
            eye_in_face: 0.5,
            h_offset_pct: 0.0,
        }
    }
}

/// The face rectangle in source pixels (`x1`/`y1` exclusive).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// Where the source frame is, and what the finished photo looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub out_width_px: u32,
    pub out_height_px: u32,
    /// Output pixels per source pixel.
    pub scale: f64,
    /// The source frame's centre and size (source px).
    pub centre_x: f64,
    pub centre_y: f64,
    pub frame_width: f64,
    pub frame_height: f64,
    /// Guide positions in the finished photo, mm from its top edge.
    pub head_top_mm: f64,
    pub eye_line_mm: f64,
    pub chin_mm: f64,
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: COMMAND.into(), msg: msg.into() }
}

fn mm_to_px(mm: f64, dpi: f64) -> f64 {
    mm * dpi / MM_PER_INCH
}

/// Checks every setting, returning the first problem as a message.
pub fn validate(st: &Settings) -> Result<()> {
    let in_range = |name: &str, v: f64, lo: f64, hi: f64| {
        if v.is_finite() && (lo..=hi).contains(&v) { Ok(()) } else { Err(bad(format!("`{name}` = {v} must be a number from {lo} to {hi}"))) }
    };
    in_range("widthMm", st.width_mm, 1.0, MAX_MM)?;
    in_range("heightMm", st.height_mm, 1.0, MAX_MM)?;
    in_range("dpi", st.dpi, 1.0, MAX_DPI)?;
    in_range("faceHeightMm", st.face_height_mm, 1.0, MAX_MM)?;
    in_range("eyeFromBottomPct", st.eye_from_bottom_pct, 0.0, 100.0)?;
    in_range("eyeInFace", st.eye_in_face, 0.0, 1.0)?;
    in_range("hOffsetPct", st.h_offset_pct, -100.0, 100.0)?;
    Ok(())
}

/// Computes the layout for `face` (source px) and `st`.
pub fn layout(face: FaceRect, st: &Settings) -> Result<Layout> {
    validate(st)?;
    let face_h = face.y1 - face.y0;
    let face_w = face.x1 - face.x0;
    if !(face.x0.is_finite() && face.y0.is_finite() && face.x1.is_finite() && face.y1.is_finite()) || face_h < 1.0 || face_w < 1.0 {
        return Err(bad("the face rectangle must be at least 1 x 1 px"));
    }
    let out_w = mm_to_px(st.width_mm, st.dpi).round();
    let out_h = mm_to_px(st.height_mm, st.dpi).round();
    if out_w < 1.0 || out_h < 1.0 || out_w > f64::from(MAX_SIDE_PX) || out_h > f64::from(MAX_SIDE_PX) {
        return Err(bad(format!("{out_w} x {out_h} px is outside 1 to {MAX_SIDE_PX} px per side; lower the size or dpi")));
    }
    let scale = mm_to_px(st.face_height_mm, st.dpi) / face_h;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(bad("the face size gives no usable scale"));
    }
    let frame_width = out_w / scale;
    let frame_height = out_h / scale;
    // The eye line in the source, and where it must land in the output (px from the top).
    let eye_src_y = face.y0 + st.eye_in_face * face_h;
    let eye_out_y = (1.0 - st.eye_from_bottom_pct / 100.0) * out_h;
    let top = eye_src_y - eye_out_y / scale;
    let face_cx = (face.x0 + face.x1) / 2.0;
    // Moving the picture right moves the frame left over it.
    let left = face_cx - frame_width / 2.0 - st.h_offset_pct / 100.0 * frame_width;
    let to_mm = |src_y: f64| (src_y - top) * scale / st.dpi * MM_PER_INCH;
    Ok(Layout {
        out_width_px: out_w as u32,
        out_height_px: out_h as u32,
        scale,
        centre_x: left + frame_width / 2.0,
        centre_y: top + frame_height / 2.0,
        frame_width,
        frame_height,
        head_top_mm: to_mm(face.y0),
        eye_line_mm: to_mm(eye_src_y),
        chin_mm: to_mm(face.y1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face() -> FaceRect {
        FaceRect { x0: 1000.0, y0: 500.0, x1: 1800.0, y1: 1500.0 }
    }

    #[test]
    fn output_size_is_the_mm_size_at_the_dpi() {
        let l = layout(face(), &Settings::default()).unwrap();
        assert_eq!((l.out_width_px, l.out_height_px), (413, 531));
    }

    #[test]
    fn face_height_and_eye_line_land_where_asked() {
        let st = Settings { face_height_mm: 32.0, eye_from_bottom_pct: 56.0, ..Settings::default() };
        let l = layout(face(), &st).unwrap();
        assert!((l.chin_mm - l.head_top_mm - 32.0).abs() < 1e-9);
        // The height rounds to whole pixels, so the line is within a pixel of the ideal 19.8 mm.
        assert!((l.eye_line_mm - 45.0 * 0.44).abs() < 0.05, "{}", l.eye_line_mm);
    }

    #[test]
    fn frame_is_centred_on_the_face_and_h_offset_moves_it() {
        let l = layout(face(), &Settings::default()).unwrap();
        assert!((l.centre_x - 1400.0).abs() < 1e-9);
        let st = Settings { h_offset_pct: 10.0, ..Settings::default() };
        let r = layout(face(), &st).unwrap();
        assert!(r.centre_x < l.centre_x);
    }

    #[test]
    fn bad_settings_and_faces_are_errors() {
        for st in [
            Settings { width_mm: 0.0, ..Settings::default() },
            Settings { dpi: f64::NAN, ..Settings::default() },
            Settings { face_height_mm: -1.0, ..Settings::default() },
            Settings { eye_from_bottom_pct: 101.0, ..Settings::default() },
            Settings { width_mm: 1000.0, dpi: 10_000.0, ..Settings::default() },
        ] {
            assert!(layout(face(), &st).is_err(), "{st:?}");
        }
        let empty = FaceRect { x0: 5.0, y0: 5.0, x1: 5.0, y1: 5.0 };
        assert!(layout(empty, &Settings::default()).is_err());
    }
}
