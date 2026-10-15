//! File › Automate › ID Photo…: a live preview of the finished photo (head top, eye line and chin
//! guides) beside its settings and saved templates. The engine command `image.idPhoto.make` does
//! the work and stays directly callable with parameters by CLI, MCP and Actions; the preview is
//! never an edit.

use egui::{Align2, Color32, ColorImage, Rect, Sense, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use photocraft_engine::idphoto_cmds as engine;
use photocraft_engine::idphoto_layout::Layout;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;
use crate::widgets;

pub const COMMAND: &str = "image.idPhoto.make";
const MARK: &str = "__idPhoto";
const TEMPLATES: &str = "image.idPhoto.make.templates";
/// The settings a dialog keeps, a template saves and the next dialog starts from.
const KEYS: [&str; 5] = ["widthMm", "heightMm", "dpi", "facePct", "eyeFromBottomPct"];
/// The largest bitmap rendered for the preview, and the settings dock's width, in points.
const PREVIEW_PX: f64 = 800.0;
const PANEL_W: f32 = 340.0;
/// Photo sizes the Photo Size list offers: id, label, width mm, height mm.
const PRESETS: [(&str, &str, f64, f64); 6] = [
    ("35x45", "35 × 45 mm (Schengen / ICAO)", 35.0, 45.0),
    ("50x50", "50 × 50 mm", 50.0, 50.0),
    ("40x40", "40 × 40 mm", 40.0, 40.0),
    ("40x50", "40 × 50 mm", 40.0, 50.0),
    ("30x40", "30 × 40 mm", 30.0, 40.0),
    ("51x51", "51 × 51 mm (US 2 × 2 in)", 50.8, 50.8),
];
/// The default face height as a percentage of the photo height (33.5 mm of 45 mm).
const FACE_PCT: f64 = 74.5;
/// The face height range the 35 x 45 mm standard asks for, mm.
const STANDARD_FACE_MM: (f64, f64) = (31.0, 36.0);
/// The ranges the sliders allow, as (min, max).
const FACE_RANGE: (f32, f32) = (50.0, 90.0);
const EYE_RANGE: (f32, f32) = (40.0, 70.0);
const H_RANGE: (f32, f32) = (-25.0, 25.0);

#[derive(Clone)]
struct Cached {
    key: String,
    texture: TextureHandle,
    layout: Layout,
}

pub fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key(MARK)
}

fn num(f: &Map<String, Value>, key: &str, default: f64) -> f64 {
    f.get(key).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(default)
}

fn valid(key: &str, v: &Value) -> bool {
    let range = match key {
        "widthMm" | "heightMm" => 10.0..=200.0,
        "dpi" => 72.0..=1200.0,
        "facePct" => f64::from(FACE_RANGE.0)..=f64::from(FACE_RANGE.1),
        "eyeFromBottomPct" => f64::from(EYE_RANGE.0)..=f64::from(EYE_RANGE.1),
        "hOffsetPct" => f64::from(H_RANGE.0)..=f64::from(H_RANGE.1),
        _ => return false,
    };
    v.as_f64().is_some_and(|n| range.contains(&n))
}

/// The face rectangle of the active selection, as the command's `face` parameter.
fn face_param(app: &PhotocraftApp) -> Result<Value, String> {
    let r = engine::face_rect(&app.session, &json!({})).map_err(|e| e.to_string())?;
    Ok(json!({"x": r.x0 as i64, "y": r.y0 as i64, "width": (r.x1 - r.x0) as i64, "height": (r.y1 - r.y0) as i64}))
}

/// The user's saved templates: objects with a `name` and the [`KEYS`] settings.
fn templates(app: &PhotocraftApp) -> Vec<Map<String, Value>> {
    let saved = app.session.prefs().dialogs.get(TEMPLATES).and_then(Value::as_array).cloned().unwrap_or_default();
    saved
        .into_iter()
        .filter_map(|t| t.as_object().cloned())
        .filter(|t| t.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim().is_empty()) && KEYS.iter().all(|k| t.get(*k).is_some_and(|v| valid(k, v))))
        .collect()
}

fn store_templates(app: &mut PhotocraftApp, list: Vec<Map<String, Value>>) {
    let value = Value::Array(list.into_iter().map(Value::Object).collect());
    app.session.prefs.edit(|prefs| prefs.dialogs.insert(TEMPLATES.into(), value));
}

/// Dialog fields: the defaults for a 35 x 45 mm photo, then the valid values used last time.
fn fields(app: &PhotocraftApp, face: Value) -> Map<String, Value> {
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(COMMAND));
    f.insert("__label".into(), json!("ID Photo…"));
    f.insert("__preset".into(), json!("35x45"));
    f.insert("face".into(), face);
    for (k, v) in [("widthMm", 35.0), ("heightMm", 45.0), ("dpi", 300.0), ("facePct", FACE_PCT), ("eyeFromBottomPct", 56.0), ("hOffsetPct", 0.0)] {
        f.insert(k.into(), json!(v));
    }
    // Each time the tool opens, the settings are the last chosen size's own values (a built-in size
    // or a saved template), or the last custom values; H Position always starts at 0.
    if let Some(Value::Object(saved)) = app.session.prefs().dialogs.get(COMMAND) {
        let preset = saved.get("__preset").and_then(Value::as_str).unwrap_or("35x45");
        if preset == "custom" {
            for key in KEYS {
                if let Some(value) = saved.get(key).filter(|v| valid(key, v)) {
                    f.insert(key.into(), value.clone());
                }
            }
            f.insert("__preset".into(), json!("custom"));
        } else {
            let saved_templates = templates(app);
            let known = PRESETS.iter().any(|(id, ..)| *id == preset)
                || preset.strip_prefix("t:").is_some_and(|n| saved_templates.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(n)));
            apply_choice(&mut f, if known { preset } else { "35x45" }, &saved_templates);
        }
    }
    f
}

/// Opens the dialog on the active selection (the face). Without a rectangular selection it opens an
/// error dialog that says what to do instead.
pub fn open(app: &mut PhotocraftApp) -> u64 {
    let mut f = fields(app, Value::Null);
    let face = face_param(app).or_else(|_| default_face(app, &f));
    match face {
        Ok(face) => {
            f.insert("face".into(), face);
            app.ui.open_dialog(crate::state::DialogKind::Command, f)
        }
        Err(message) => {
            let mut f = Map::new();
            f.insert("message".into(), json!(message));
            app.ui.open_dialog(crate::state::DialogKind::Error, f)
        }
    }
}

/// A starting face rectangle for a document with no selection: the one that fits the whole picture
/// in the photo frame (by width or by height, whichever the picture's ratio against the photo's
/// ratio calls for). The head top, chin and middle lines are then dragged onto the real face.
fn default_face(app: &PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let st = app.session.active().ok_or("no document is open")?;
    let (w, h) = (f64::from(st.doc.size.width).max(1.0), f64::from(st.doc.size.height).max(1.0));
    let ratio = num(f, "widthMm", 35.0) / num(f, "heightMm", 45.0);
    // The photo frame, in picture pixels, that holds the whole picture.
    let frame_h = if w / h > ratio { w / ratio } else { h };
    let face_h = (frame_h * num(f, "facePct", FACE_PCT) / 100.0).max(2.0);
    let face_w = face_h * 0.75;
    let eye_y = h / 2.0 - frame_h / 2.0 + (1.0 - num(f, "eyeFromBottomPct", 56.0) / 100.0) * frame_h;
    Ok(json!({"x": (w / 2.0 - face_w / 2.0).round() as i64, "y": (eye_y - face_h / 2.0).round() as i64, "width": face_w.round() as i64, "height": face_h.round() as i64}))
}

/// The engine parameters: the settings (the face size as mm of the photo height) and the face.
pub fn params(f: &Map<String, Value>) -> Value {
    let mut p: Map<String, Value> = KEYS.iter().filter(|k| **k != "facePct").filter_map(|k| f.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
    p.insert("faceHeightMm".into(), json!(num(f, "facePct", FACE_PCT) / 100.0 * num(f, "heightMm", 45.0)));
    p.insert("hOffsetPct".into(), json!(num(f, "hOffsetPct", 0.0)));
    p.insert("eyeInFace".into(), json!(num(f, "eyeInFace", 0.5).clamp(0.0, 1.0)));
    if let Some(face) = f.get("face") {
        p.insert("face".into(), face.clone());
    }
    Value::Object(p)
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let result = app.run(COMMAND, params(f));
    if result.is_ok() {
        let mut remembered: Map<String, Value> = KEYS.iter().filter_map(|k| f.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        remembered.insert("__preset".into(), f.get("__preset").cloned().unwrap_or(Value::Null));
        app.session.prefs.edit(|prefs| prefs.dialogs.insert(COMMAND.into(), Value::Object(remembered)));
    }
    result
}

/// The preview for the dialog's current settings, rebuilt only when they or the document change.
fn preview(app: &PhotocraftApp, ctx: &egui::Context, f: &Map<String, Value>) -> Result<(TextureHandle, Layout), String> {
    let st = app.session.active().ok_or("no document")?;
    let p = params(f);
    let key = format!("{p}|{:?}|{}", st.doc.id, st.revision);
    let id = egui::Id::new("idphoto-preview");
    if let Some(c) = ctx.data(|d| d.get_temp::<Cached>(id)).filter(|c| c.key == key) {
        return Ok((c.texture, c.layout));
    }
    let mut settings = engine::settings(&p).map_err(|e| e.to_string())?;
    let face = engine::face_rect(&app.session, &json!({"face": p.get("face")})).map_err(|e| e.to_string())?;
    // A small bitmap is enough on screen, and keeps every slider drag cheap.
    settings.dpi = (PREVIEW_PX / settings.height_mm * 25.4).min(settings.dpi).max(10.0);
    let (layout, surface) = engine::build(&app.session, &settings, face).map_err(|e| e.to_string())?;
    let (w, h) = (layout.out_width_px as usize, layout.out_height_px as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let px = surface.rgba(x as i32, y as i32);
            rgba.extend(px.iter().map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    let image = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    let texture = ctx.load_texture("idphoto-preview", image, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, Cached { key, texture: texture.clone(), layout }));
    Ok((texture, layout))
}

/// A guide line across the photo, its name overlaid at the left and its value at the right.
fn guide(ui: &egui::Ui, image: Rect, y: f32, name: &str, value: &str, color: Color32) {
    let p = ui.painter();
    p.line_segment([pos2(image.left(), y), pos2(image.right(), y)], Stroke::new(1.0, color));
    let label = |text: &str, right: bool| {
        let galley = p.layout_no_wrap(text.to_string(), egui::FontId::proportional(10.0), Color32::WHITE);
        let x = if right { image.right() - 3.0 - galley.size().x } else { image.left() + 3.0 };
        let at = pos2(x, y - galley.size().y - 1.0);
        p.rect_filled(Rect::from_min_size(at, galley.size()).expand(1.0), 2.0, Color32::from_black_alpha(140));
        p.galley(at, galley, Color32::WHITE);
    };
    label(name, false);
    label(value, true);
}

/// The face rectangle `[x0, y0, x1, y1]` (source px) of the dialog's `face` field.
fn face_of(f: &Map<String, Value>) -> Option<[f64; 4]> {
    let face = f.get("face")?;
    let get = |k: &str| face.get(k).and_then(Value::as_f64);
    let (x, y) = (get("x")?, get("y")?);
    Some([x, y, x + get("width")?, y + get("height")?])
}

/// What dropping line `line` (0 head top, 1 eye line, 2 chin, 3 middle) at `at` (source px: y for
/// the first three, x for the middle line) means: a new face rectangle or eye anchor. The preview
/// then re-fits the whole photo to it.
fn drop_line(f: &mut Map<String, Value>, line: u8, at: f64, canvas: (f64, f64)) {
    let Some([x0, y0, x1, y1]) = face_of(f) else { return };
    let eye_src = y0 + num(f, "eyeInFace", 0.5) * (y1 - y0);
    let min_h = 20.0;
    let (mut nx0, mut ny0, mut nx1, mut ny1) = (x0, y0, x1, y1);
    match line {
        0 => ny0 = at.clamp(0.0, y1 - min_h),
        2 => ny1 = at.clamp(y0 + min_h, canvas.1),
        1 => {
            f.insert("eyeInFace".into(), json!(((at - y0) / (y1 - y0)).clamp(0.0, 1.0)));
            return;
        }
        _ => {
            let half = (x1 - x0) / 2.0;
            let c = at.clamp(half, (canvas.0 - half).max(half));
            (nx0, nx1) = (c - half, c + half);
            // The dropped line is the face's middle now, so the picture is centred on it.
            f.insert("hOffsetPct".into(), json!(0.0));
        }
    }
    if line == 0 || line == 2 {
        // The eyes stay on the same spot of the picture while the face rectangle changes around them.
        f.insert("eyeInFace".into(), json!(((eye_src - ny0) / (ny1 - ny0)).clamp(0.0, 1.0)));
    }
    f.insert("face".into(), json!({"x": nx0.round() as i64, "y": ny0.round() as i64, "width": (nx1 - nx0).round() as i64, "height": (ny1 - ny0).round() as i64}));
}

/// The live preview of the finished photo, with its guides, centred in `area`. The head top, eye,
/// chin and middle lines are dragged onto the face in the picture; on drop the photo is re-fitted.
fn preview_pane(app: &PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>, area: Rect) {
    position_keys(ui.ctx(), f);
    let t = Tokens::get(ui.ctx());
    let (width_mm, height_mm) = (num(f, "widthMm", 35.0), num(f, "heightMm", 45.0));
    let p = ui.painter().with_clip_rect(area);
    p.rect_filled(area, 0.0, t.canvas);
    match preview(app, ui.ctx(), f) {
        Ok((texture, lay)) => {
            let room = area.shrink2(vec2(70.0, 44.0));
            let k = (room.width() / lay.out_width_px as f32).min(room.height() / lay.out_height_px as f32).max(0.01);
            let size = vec2(lay.out_width_px as f32 * k, lay.out_height_px as f32 * k);
            let image = Rect::from_center_size(area.center(), size);
            p.text(pos2(image.center().x, image.top() - 10.0), Align2::CENTER_CENTER, format!("{width_mm:.0} mm"), egui::FontId::proportional(11.0), t.text_dim);
            p.image(texture.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            p.rect_stroke(image, 0.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
            let at = |mm: f64| image.top() + (mm / height_mm) as f32 * size.y;
            let drag_id = egui::Id::new("idphoto-drag");
            let dragging: Option<(u8, f32)> = ui.ctx().data(|d| d.get_temp(drag_id));
            let mut pos = [at(lay.head_top_mm), at(lay.eye_line_mm), at(lay.chin_mm), image.center().x];
            if let Some((i, v)) = dragging
                && let Some(slot) = pos.get_mut(usize::from(i))
            {
                *slot = if i < 3 { v.clamp(image.top(), image.bottom()) } else { v.clamp(image.left(), image.right()) };
            }
            let mm_at = |y: f32| ((y - image.top()) / size.y) as f64 * height_mm;
            p.line_segment([pos2(pos[3], image.top()), pos2(pos[3], image.bottom())], Stroke::new(1.0, Color32::from_rgb(255, 80, 80)));
            guide(ui, image, pos[0], "Head Top", &format!("{:.1} mm", mm_at(pos[0])), t.accent);
            guide(ui, image, pos[1], "Eye Line", &format!("{:.1} mm", height_mm - mm_at(pos[1])), t.accent);
            guide(ui, image, pos[2], "Chin Line", &format!("{:.1} mm", height_mm - mm_at(pos[2])), t.accent);
            p.text(pos2(image.right() + 8.0, image.center().y), Align2::LEFT_CENTER, format!("{height_mm:.0} mm"), egui::FontId::proportional(11.0), t.text_dim);
            let dpi = num(f, "dpi", 300.0);
            p.text(
                pos2(area.center().x, area.bottom() - 14.0),
                Align2::CENTER_CENTER,
                format!("{}×{} px · {} {:.1} mm", (width_mm / 25.4 * dpi).round(), (height_mm / 25.4 * dpi).round(), "Face", lay.chin_mm - lay.head_top_mm),
                egui::FontId::proportional(12.0),
                t.text_dim,
            );
            // Dragging: a line follows the pointer; on drop the settings are re-fitted to it.
            let mut dropped: Option<(u8, f32)> = None;
            for i in 0..4u8 {
                let v = pos[usize::from(i)];
                let hit = if i < 3 {
                    Rect::from_min_max(pos2(image.left(), v - 5.0), pos2(image.right(), v + 5.0))
                } else {
                    Rect::from_min_max(pos2(v - 5.0, image.top()), pos2(v + 5.0, image.bottom()))
                };
                let r = ui.interact(hit, egui::Id::new(("idphoto-line", i)), Sense::drag());
                if r.hovered() || r.dragged() {
                    ui.ctx().set_cursor_icon(if i < 3 { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal });
                }
                if let Some(pt) = r.interact_pointer_pos().filter(|_| r.dragged() || r.drag_stopped()) {
                    let coord = if i < 3 { pt.y } else { pt.x };
                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, (i, coord)));
                    if r.drag_stopped() {
                        dropped = Some((i, coord));
                    }
                    ui.ctx().request_repaint();
                }
            }
            if let Some((i, coord)) = dropped {
                ui.ctx().data_mut(|d| d.remove::<(u8, f32)>(drag_id));
                let canvas = app.session.active().map_or((1.0, 1.0), |st| (f64::from(st.doc.size.width), f64::from(st.doc.size.height)));
                let src = if i < 3 {
                    lay.centre_y - lay.frame_height / 2.0 + f64::from((coord - image.top()) / size.y) * lay.frame_height
                } else {
                    lay.centre_x - lay.frame_width / 2.0 + f64::from((coord - image.left()) / size.x) * lay.frame_width
                };
                drop_line(f, i, src, canvas);
            }
        }
        Err(e) => {
            p.text(area.center(), Align2::CENTER_CENTER, e, egui::FontId::proportional(13.0), t.danger);
        }
    }
}

/// Move the photo inside its output frame by one output pixel (ten with Shift). The horizontal
/// offset already represents the image's horizontal placement; changing the eye's output position
/// shifts the crop vertically while keeping the face rectangle itself unchanged.
fn nudge_position(f: &mut Map<String, Value>, dx: f64, dy: f64, multiplier: f64) {
    let width_px = (num(f, "widthMm", 35.0) * num(f, "dpi", 300.0) / 25.4).round().max(1.0);
    let height_px = (num(f, "heightMm", 45.0) * num(f, "dpi", 300.0) / 25.4).round().max(1.0);
    let h_step = 100.0 * multiplier / width_px;
    let v_step = 100.0 * multiplier / height_px;
    if dx != 0.0 {
        let h_offset = (num(f, "hOffsetPct", 0.0) + dx * h_step).clamp(f64::from(H_RANGE.0), f64::from(H_RANGE.1));
        f.insert("hOffsetPct".into(), json!(h_offset));
    }
    if dy != 0.0 {
        // A larger eye-from-bottom value moves the image up in the output frame.
        let eye_from_bottom = (num(f, "eyeFromBottomPct", 56.0) - dy * v_step).clamp(f64::from(EYE_RANGE.0), f64::from(EYE_RANGE.1));
        f.insert("eyeFromBottomPct".into(), json!(eye_from_bottom));
    }
}

/// Arrow-key positioning belongs to this window, not the main canvas' Move tool. Leave arrows to
/// text fields, focused widgets and popups; Alt/Cmd/Ctrl arrows retain their usual meanings.
fn position_keys(ctx: &egui::Context, f: &mut Map<String, Value>) {
    if egui::Popup::is_any_open(ctx) || crate::shortcut_dispatch::Focus::of(ctx) != crate::shortcut_dispatch::Focus::None {
        return;
    }
    let modifiers = ctx.input(|i| i.modifiers);
    if modifiers.alt || modifiers.command || modifiers.ctrl {
        return;
    }
    let keys = [(egui::Key::ArrowLeft, -1.0, 0.0), (egui::Key::ArrowRight, 1.0, 0.0), (egui::Key::ArrowUp, 0.0, -1.0), (egui::Key::ArrowDown, 0.0, 1.0)];
    for (key, dx, dy) in keys {
        for (shift, step) in [(true, 10.0), (false, 1.0)] {
            let mods = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
            if ctx.input_mut(|i| i.consume_key(mods, key)) {
                nudge_position(f, dx, dy, step);
                ctx.request_repaint();
                return;
            }
        }
    }
}

fn slider(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, range: (f32, f32), suffix: &str, default: f64) {
    let mut v = (num(f, key, default) as f32).clamp(range.0, range.1);
    if widgets::slider_row(ui, label, &mut v, range.0..=range.1, suffix, None).changed() {
        f.insert(key.into(), json!(v));
    }
}

/// Applies a built-in size or a saved template to the dialog's fields.
fn apply_choice(f: &mut Map<String, Value>, choice: &str, templates: &[Map<String, Value>]) {
    if let Some((_, _, w, h)) = PRESETS.iter().find(|(id, ..)| *id == choice) {
        f.insert("widthMm".into(), json!(w));
        f.insert("heightMm".into(), json!(h));
        f.insert("facePct".into(), json!(FACE_PCT));
    } else if let Some(t) = choice.strip_prefix("t:").and_then(|name| templates.iter().find(|t| t.get("name").and_then(Value::as_str) == Some(name))) {
        for key in KEYS {
            if let Some(v) = t.get(key) {
                f.insert(key.into(), v.clone());
            }
        }
    }
    f.insert("__preset".into(), json!(choice));
}

fn settings_pane(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let mut saved = templates(app);
    ui.vertical(|ui| {
        ui.set_width(ui.available_width());
        widgets::section_label(ui, "Photo Settings");
        widgets::hairline(ui);
        ui.label("Photo Size");
        let mut choice = f.get("__preset").and_then(Value::as_str).unwrap_or("custom").to_string();
        let mut options: Vec<(String, String)> = PRESETS.iter().map(|(id, label, ..)| (id.to_string(), label.to_string())).collect();
        options.extend(saved.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).map(|n| (format!("t:{n}"), n.to_string())));
        options.push(("custom".into(), tl!("Custom").to_string()));
        let shown: Vec<(String, &str)> = options.iter().map(|(id, label)| (id.clone(), label.as_str())).collect();
        if widgets::dropdown(ui, "idphoto-preset", &mut choice, &shown, 300.0) {
            apply_choice(f, &choice, &saved);
        }
        ui.horizontal(|ui| {
            for (key, label) in [("widthMm", tl!("Width")), ("heightMm", tl!("Height"))] {
                ui.label(label);
                let mut v = num(f, key, 35.0) as f32;
                if widgets::value_field_prec(ui, &mut v, 10.0..=200.0, "mm", 70.0, 1).changed() {
                    f.insert(key.into(), json!(v));
                    f.insert("__preset".into(), json!("custom"));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(tl!("Resolution"));
            let mut v = num(f, "dpi", 300.0) as f32;
            if widgets::value_field(ui, &mut v, 72.0..=1200.0, "dpi", 80.0).changed() {
                f.insert("dpi".into(), json!(v.round()));
            }
        });
        ui.add_space(6.0);
        let height = num(f, "heightMm", 45.0);
        let face_mm = num(f, "facePct", FACE_PCT) / 100.0 * height;
        slider(ui, f, "facePct", "Face Size", FACE_RANGE, "%", FACE_PCT);
        let standard = (num(f, "widthMm", 35.0) - 35.0).abs() < 0.05 && (height - 45.0).abs() < 0.05;
        let in_range = (STANDARD_FACE_MM.0..=STANDARD_FACE_MM.1).contains(&face_mm);
        let hint = if standard {
            format!("{face_mm:.1} mm · {} {:.0}–{:.0} mm", "standard", STANDARD_FACE_MM.0, STANDARD_FACE_MM.1)
        } else {
            format!("{face_mm:.1} mm")
        };
        ui.colored_label(if standard && !in_range { t.warning } else { t.text_dim }, hint);
        slider(ui, f, "eyeFromBottomPct", "Eye Position From Bottom", EYE_RANGE, "%", 56.0);
        slider(ui, f, "hOffsetPct", "H Position", H_RANGE, "%", 0.0);
        ui.add_space(6.0);
        widgets::section_label(ui, "Templates");
        widgets::hairline(ui);
        let mut name = f.get("__tplName").and_then(Value::as_str).unwrap_or_default().to_string();
        ui.horizontal(|ui| {
            if ui.add(egui::TextEdit::singleline(&mut name).hint_text("Template name").desired_width(150.0)).changed() {
                f.insert("__tplName".into(), json!(name));
            }
            let name = name.trim().to_string();
            if ui.add_enabled(!name.is_empty(), egui::Button::new(tl!("Save"))).clicked() {
                let mut t: Map<String, Value> = KEYS.iter().filter_map(|k| f.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
                t.insert("name".into(), json!(name));
                saved.retain(|s| s.get("name").and_then(Value::as_str) != Some(name.as_str()));
                saved.push(t);
                store_templates(app, saved.clone());
                f.insert("__preset".into(), json!(format!("t:{name}")));
            }
            let current = f.get("__preset").and_then(Value::as_str).and_then(|p| p.strip_prefix("t:")).map(str::to_string);
            if ui.add_enabled(current.is_some(), egui::Button::new(tl!("Delete"))).clicked()
                && let Some(current) = current
            {
                saved.retain(|s| s.get("name").and_then(Value::as_str) != Some(current.as_str()));
                store_templates(app, saved.clone());
                f.insert("__preset".into(), json!("custom"));
            }
        });
    });
}

/// The ID Photo window: its own OS window (movable, resizable, with minimise and maximise) like
/// Camera Raw, the preview filling the left and the settings in a dock on the right. Returns
/// `Some(true)` for OK and `Some(false)` for Cancel.
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context, id: u64, title: &str, f: &mut Map<String, Value>) -> Option<bool> {
    // Modal, as Camera Raw is: the main window is dimmed and takes no input.
    let main = ctx.content_rect();
    egui::Area::new(egui::Id::new("idphoto-modal")).order(egui::Order::Foreground).fixed_pos(main.min).show(ctx, |ui| {
        let (r, _) = ui.allocate_exact_size(main.size(), Sense::click_and_drag());
        ui.painter().rect_filled(r, 0.0, Tokens::get(ui.ctx()).scrim);
    });
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([1100.0, 760.0])
        .with_min_inner_size([760.0, 520.0])
        .with_resizable(true)
        .with_maximize_button(true)
        .with_minimize_button(true);
    let mut outcome = None;
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("idphoto-window", id)), builder, |ui, _class| {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            outcome = Some(false);
        }
        let t = Tokens::get(&ctx);
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new(("idphoto-dialog", id))).order(egui::Order::Foreground).fixed_pos(screen.min).show(&ctx, |ui| {
            let (full, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
            ui.painter().rect_filled(full, 0.0, t.chrome);
            let footer_h = 48.0;
            let body = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - footer_h));
            let dock = Rect::from_min_max(pos2((body.right() - PANEL_W).max(body.left() + 1.0), body.top()), body.max);
            let view = Rect::from_min_max(body.min, pos2(dock.left(), body.bottom()));
            let before = params(f);
            preview_pane(app, ui, f, view);
            ui.painter().rect_filled(dock, 0.0, t.dock);
            ui.painter().line_segment([dock.left_top(), dock.left_bottom()], Stroke::new(1.0, t.separator));
            let mut props = ui.new_child(egui::UiBuilder::new().max_rect(dock.shrink2(vec2(14.0, 10.0))));
            props.spacing_mut().item_spacing.y = 6.0;
            egui::ScrollArea::vertical().id_salt("idphoto-props").show(&mut props, |ui| settings_pane(app, ui, f));
            let bar = Rect::from_min_max(pos2(full.left(), body.bottom()), full.max);
            ui.painter().line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0, t.separator));
            let mut footer = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(16.0, 10.0))).layout(egui::Layout::right_to_left(egui::Align::Min)));
            footer.spacing_mut().item_spacing.x = 10.0;
            let buttons = [widgets::DialogButton::new(widgets::ButtonRole::Default, tl!("OK"), 84.0), widgets::DialogButton::new(widgets::ButtonRole::Cancel, tl!("Cancel"), 84.0)];
            match widgets::dialog_buttons(&mut footer, &buttons) {
                Some(widgets::ButtonRole::Cancel) => outcome = Some(false),
                Some(_) => outcome = Some(true),
                None => {}
            }
            if outcome.is_none() && !egui::Popup::is_any_open(&ctx) {
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    outcome = Some(false);
                } else if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                    outcome = Some(true);
                }
            }
            if before != params(f) {
                ctx.request_repaint();
            }
        });
    });
    outcome
}

#[cfg(test)]
mod position_key_tests {
    use super::*;

    fn fields() -> Map<String, Value> {
        let mut f = Map::new();
        f.insert("widthMm".into(), json!(35.0));
        f.insert("heightMm".into(), json!(45.0));
        f.insert("dpi".into(), json!(300.0));
        f.insert("hOffsetPct".into(), json!(0.0));
        f.insert("eyeFromBottomPct".into(), json!(56.0));
        f
    }

    fn press(ctx: &egui::Context, f: &mut Map<String, Value>, key: egui::Key, modifiers: egui::Modifiers) {
        let input = egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| position_keys(ui.ctx(), f)).textures_delta.clear();
    }

    #[test]
    fn arrow_directions_move_the_photo_by_one_output_pixel() {
        let mut f = fields();
        nudge_position(&mut f, 1.0, 0.0, 1.0);
        let horizontal_step = 100.0 / (35.0_f64 * 300.0 / 25.4).round();
        assert!((num(&f, "hOffsetPct", 0.0) - horizontal_step).abs() < 1e-9, "Right moves the photo right");
        nudge_position(&mut f, 0.0, -1.0, 1.0);
        let vertical_step = 100.0 / (45.0_f64 * 300.0 / 25.4).round();
        assert!((num(&f, "eyeFromBottomPct", 0.0) - (56.0 + vertical_step)).abs() < 1e-9, "Up moves the photo up");
        nudge_position(&mut f, -1.0, 0.0, 1.0);
        nudge_position(&mut f, 0.0, 1.0, 1.0);
        assert!(num(&f, "hOffsetPct", 0.0).abs() < 1e-9);
        assert!((num(&f, "eyeFromBottomPct", 0.0) - 56.0).abs() < 1e-9);
    }

    #[test]
    fn shift_nudges_ten_output_pixels_and_position_stays_in_slider_ranges() {
        let mut f = fields();
        nudge_position(&mut f, -1.0, 1.0, 10.0);
        let horizontal_step = 1000.0 / (35.0_f64 * 300.0 / 25.4).round();
        let vertical_step = 1000.0 / (45.0_f64 * 300.0 / 25.4).round();
        assert!((num(&f, "hOffsetPct", 0.0) + horizontal_step).abs() < 1e-9);
        assert!((num(&f, "eyeFromBottomPct", 0.0) - (56.0 - vertical_step)).abs() < 1e-9);

        f.insert("hOffsetPct".into(), json!(24.99));
        f.insert("eyeFromBottomPct".into(), json!(69.99));
        nudge_position(&mut f, 1.0, -1.0, 10.0);
        assert_eq!(num(&f, "hOffsetPct", 0.0), f64::from(H_RANGE.1));
        assert_eq!(num(&f, "eyeFromBottomPct", 0.0), f64::from(EYE_RANGE.1));
    }

    #[test]
    fn arrow_events_update_the_preview_position_fields() {
        let ctx = egui::Context::default();
        let mut f = fields();
        press(&ctx, &mut f, egui::Key::ArrowLeft, egui::Modifiers::NONE);
        press(&ctx, &mut f, egui::Key::ArrowDown, egui::Modifiers::NONE);
        assert!(num(&f, "hOffsetPct", 0.0) < 0.0);
        assert!(num(&f, "eyeFromBottomPct", 0.0) < 56.0);

        let before = num(&f, "hOffsetPct", 0.0);
        press(&ctx, &mut f, egui::Key::ArrowRight, egui::Modifiers::SHIFT);
        assert!(num(&f, "hOffsetPct", 0.0) > before);
    }

    #[test]
    fn modified_arrows_are_not_stolen() {
        let ctx = egui::Context::default();
        let mut f = fields();
        press(&ctx, &mut f, egui::Key::ArrowRight, egui::Modifiers::ALT);
        assert_eq!(num(&f, "hOffsetPct", 0.0), 0.0);
    }
}
