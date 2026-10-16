//! Filter › Print Sheet…: lays copies of the active photo on a sheet of printer paper (Canon Selphy
//! 100 x 148 mm, borderless), with a live preview of the sheet and of what the printer trims. The
//! engine command `image.selphySheet.make` does the work and stays directly callable with
//! parameters by CLI, MCP and Actions; the preview is never an edit.

use egui::{Align2, Color32, ColorImage, Rect, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use photocraft_engine::selphy_cmds as engine;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::idphoto_ui;
use crate::theme::Tokens;
use crate::widgets;

pub const COMMAND: &str = "image.selphySheet.make";
const MARK: &str = "__selphy";
const TEMPLATES: &str = "image.selphySheet.make.templates";
/// Layout settings a built-in paper preset supplies.
const LAYOUT_KEYS: [&str; 11] =
    ["paperWidthMm", "paperHeightMm", "dpi", "copies", "gapMm", "scalePct", "leftMm", "topMm", "trimLeftMm", "trimRightMm", "trimTopMm"];
/// Every setting a user template saves, including print-job options and printer trims.
const TEMPLATE_KEYS: [&str; 14] = [
    "paperWidthMm",
    "paperHeightMm",
    "dpi",
    "copies",
    "gapMm",
    "scalePct",
    "leftMm",
    "topMm",
    "trimLeftMm",
    "trimRightMm",
    "trimTopMm",
    "cutMarks",
    "rotate",
    "copiesAuto",
];
const PREVIEW_PX: f64 = 900.0;
/// Paper presets: id, card title, and the [`LAYOUT_KEYS`] values.
const PRESETS: [(&str, &str, [f64; 11]); 1] = [("selphy", "Canon Selphy postcard", [148.0, 100.0, 300.0, 8.0, 1.0, 93.0, 6.0, 3.0, 6.0, 4.0, 3.0])];
const VISIBLE_CARDS: usize = 6;

#[derive(Clone)]
struct Cached {
    key: String,
    texture: TextureHandle,
}

pub fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key(MARK)
}

fn num(f: &Map<String, Value>, key: &str, default: f64) -> f64 {
    f.get(key).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(default)
}

fn valid(key: &str, v: &Value) -> bool {
    let range = match key {
        "paperWidthMm" | "paperHeightMm" => 20.0..=1000.0,
        "dpi" => 72.0..=1200.0,
        "copies" => 1.0..=100.0,
        "gapMm" => 0.0..=50.0,
        "scalePct" => 10.0..=300.0,
        "leftMm" | "topMm" => -100.0..=500.0,
        "trimLeftMm" | "trimRightMm" | "trimTopMm" => 0.0..=50.0,
        "cutMarks" | "rotate" | "copiesAuto" => return v.is_boolean(),
        _ => return false,
    };
    v.as_f64().is_some_and(|n| range.contains(&n))
}

fn templates(app: &PhotocraftApp) -> Vec<Map<String, Value>> {
    let saved = app.session.prefs().dialogs.get(TEMPLATES).and_then(Value::as_array).cloned().unwrap_or_default();
    saved
        .into_iter()
        .filter_map(|t| t.as_object().cloned())
        .filter(|t| {
            t.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim().is_empty()) && TEMPLATE_KEYS.iter().all(|k| t.get(*k).is_none_or(|v| valid(k, v)))
        })
        .collect()
}

fn store_templates(app: &mut PhotocraftApp, list: Vec<Map<String, Value>>) {
    let value = Value::Array(list.into_iter().map(Value::Object).collect());
    app.session.prefs.edit(|prefs| prefs.dialogs.insert(TEMPLATES.into(), value));
}

fn mark_custom(f: &mut Map<String, Value>) {
    f.insert("__preset".into(), json!("custom"));
}

fn template_choice(f: &Map<String, Value>) -> String {
    f.get("__preset").and_then(Value::as_str).and_then(|p| p.strip_prefix("t:")).unwrap_or_default().to_string()
}

fn dimensions(width_mm: f64, height_mm: f64) -> String {
    format!("{} × {} mm", widgets::fmt_num_prec(width_mm, 1), widgets::fmt_num_prec(height_mm, 1))
}

fn preset_cards(templates: &[Map<String, Value>]) -> Vec<(String, String, String)> {
    let mut cards: Vec<_> = templates
        .iter()
        .filter_map(|template| {
            let name = template.get("name")?.as_str()?;
            let width = template.get("paperWidthMm")?.as_f64()?;
            let height = template.get("paperHeightMm")?.as_f64()?;
            Some((format!("t:{name}"), name.to_string(), dimensions(width, height)))
        })
        .collect();
    cards.extend(PRESETS.iter().map(|(id, name, values)| ((*id).to_string(), (*name).to_string(), dimensions(values[0], values[1]))));
    cards
}

fn selected_card(f: &Map<String, Value>) -> String {
    let template = template_choice(f);
    if template.is_empty() { f.get("__preset").and_then(Value::as_str).unwrap_or("selphy").to_string() } else { format!("t:{template}") }
}

/// Applies a built-in paper preset or saved complete print template to the dialog's fields.
fn apply_choice(f: &mut Map<String, Value>, choice: &str, templates: &[Map<String, Value>]) {
    if let Some((_, _, values)) = PRESETS.iter().find(|(id, ..)| *id == choice) {
        for (k, v) in LAYOUT_KEYS.iter().zip(values) {
            f.insert((*k).into(), json!(v));
        }
        for (key, value) in [("cutMarks", true), ("rotate", false), ("copiesAuto", true)] {
            f.insert(key.into(), json!(value));
        }
    } else if let Some(t) = choice.strip_prefix("t:").and_then(|name| templates.iter().find(|t| t.get("name").and_then(Value::as_str) == Some(name))) {
        for key in TEMPLATE_KEYS {
            if let Some(v) = t.get(key) {
                f.insert(key.into(), v.clone());
            }
        }
    }
    f.insert("__preset".into(), json!(choice));
}

/// Dialog fields: the Selphy preset, then what was used last time.
fn fields(app: &PhotocraftApp) -> Map<String, Value> {
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(COMMAND));
    f.insert("__label".into(), json!("Print Sheet…"));
    apply_choice(&mut f, "selphy", &[]);
    f.insert("cutMarks".into(), json!(true));
    f.insert("rotate".into(), json!(false));
    f.insert("copiesAuto".into(), json!(true));
    if let Some(Value::Object(saved)) = app.session.prefs().dialogs.get(COMMAND) {
        let preset = saved.get("__preset").and_then(Value::as_str).unwrap_or("selphy");
        let list = templates(app);
        if preset == "custom" {
            for key in TEMPLATE_KEYS {
                if let Some(value) = saved.get(key).filter(|v| valid(key, v)) {
                    f.insert(key.into(), value.clone());
                }
            }
            f.insert("__preset".into(), json!("custom"));
        } else {
            let known = PRESETS.iter().any(|(id, ..)| *id == preset)
                || preset.strip_prefix("t:").is_some_and(|n| list.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(n)));
            apply_choice(&mut f, if known { preset } else { "selphy" }, &list);
        }
        for key in ["cutMarks", "rotate", "copiesAuto"] {
            if let Some(b) = saved.get(key).filter(|v| v.is_boolean()) {
                f.insert(key.into(), b.clone());
            }
        }
        if let Some(c) = saved.get("copies").filter(|v| valid("copies", v)) {
            f.insert("copies".into(), c.clone());
        }
    }
    f
}

pub fn open(app: &mut PhotocraftApp) -> u64 {
    if app.session.active().is_none() {
        let mut f = Map::new();
        f.insert("message".into(), json!("Open the photo to print first."));
        return app.ui.open_dialog(crate::state::DialogKind::Error, f);
    }
    let f = fields(app);
    app.ui.open_dialog(crate::state::DialogKind::Command, f)
}

/// The engine parameters (the dialog-only `trim*` and `__` fields are left out).
pub fn params(f: &Map<String, Value>) -> Value {
    let mut p: Map<String, Value> = ["paperWidthMm", "paperHeightMm", "dpi", "copies", "gapMm", "scalePct", "leftMm", "topMm", "cutMarks", "rotate"]
        .iter()
        .filter_map(|k| f.get(*k).map(|v| ((*k).to_string(), v.clone())))
        .collect();
    // Automatic copies: as many as fit on the paper.
    if f.get("copiesAuto").and_then(Value::as_bool).unwrap_or(true) {
        p.insert("copies".into(), json!(0));
    }
    Value::Object(p)
}

/// The number of copies the sheet will hold (the automatic one resolved).
fn copies_of(app: &PhotocraftApp, f: &Map<String, Value>) -> Option<u32> {
    let sheet = engine::sheet(&params(f)).ok()?;
    if sheet.copies == 0 { engine::max_copies(&app.session, &sheet).ok() } else { Some(sheet.copies) }
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let result = app.run(COMMAND, params(f));
    if result.is_ok() {
        let mut remembered: Map<String, Value> = TEMPLATE_KEYS.iter().filter_map(|k| f.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect();
        remembered.insert("__preset".into(), f.get("__preset").cloned().unwrap_or(Value::Null));
        app.session.prefs.edit(|prefs| prefs.dialogs.insert(COMMAND.into(), Value::Object(remembered)));
    }
    result
}

fn preview(app: &PhotocraftApp, ctx: &egui::Context, f: &Map<String, Value>) -> Result<TextureHandle, String> {
    let st = app.session.active().ok_or("no document")?;
    let p = params(f);
    let key = format!("{p}|{:?}|{}", st.doc.id, st.revision);
    let id = egui::Id::new("selphy-preview");
    if let Some(c) = ctx.data(|d| d.get_temp::<Cached>(id)).filter(|c| c.key == key) {
        return Ok(c.texture);
    }
    let mut sheet = engine::sheet(&p).map_err(|e| e.to_string())?;
    sheet.dpi = (PREVIEW_PX / sheet.paper_width_mm * 25.4).min(sheet.dpi).max(10.0);
    let (w, h, rgba) = engine::preview(&app.session, &sheet).map_err(|e| e.to_string())?;
    let texture = ctx.load_texture("selphy-preview", ColorImage::from_rgba_unmultiplied([w, h], &rgba), TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, Cached { key, texture: texture.clone() }));
    Ok(texture)
}

/// The grid summary follows the same layout calculation as the generated sheet.
fn grid_summary(app: &PhotocraftApp, f: &Map<String, Value>) -> Option<(u32, u32)> {
    let active = app.session.active()?;
    let sheet = engine::sheet(&params(f)).ok()?;
    let dpi_src = active.doc.resolution_dpi;
    let dpi_src = if dpi_src.is_finite() && dpi_src > 0.0 { f64::from(dpi_src) } else { 72.0 };
    let (mut photo_w, mut photo_h) = (f64::from(active.doc.size.width) / dpi_src * 25.4, f64::from(active.doc.size.height) / dpi_src * 25.4);
    if sheet.rotate {
        std::mem::swap(&mut photo_w, &mut photo_h);
    }
    let scale = if sheet.margin { sheet.scale_pct / 100.0 } else { 1.0 };
    let origin = if sheet.margin { (sheet.left_mm, sheet.top_mm) } else { (0.0, 0.0) };
    Some(engine::grid(copies_of(app, f)?, (photo_w, photo_h), sheet.gap_mm, scale, (sheet.paper_width_mm, sheet.paper_height_mm), origin))
}

/// The sheet, centred in `area`, with the part the printer trims in borderless mode shaded.
fn preview_pane(app: &PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter().with_clip_rect(area);
    p.rect_filled(area, 0.0, t.canvas);
    let (pw, ph) = (num(f, "paperWidthMm", 148.0), num(f, "paperHeightMm", 100.0));
    match preview(app, ui.ctx(), f) {
        Ok(texture) => {
            let room = area.shrink2(vec2(60.0, 50.0));
            let k = (room.width() / pw as f32).min(room.height() / ph as f32).max(0.01);
            let size = vec2(pw as f32 * k, ph as f32 * k);
            let sheet = Rect::from_center_size(area.center(), size);
            p.image(texture.id(), sheet, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            // Shade what the printer cuts off. Keep the preview geometry valid even when the
            // configured trims add up to more than the sheet size.
            let (l, r, tp, b) = (
                num(f, "trimLeftMm", 0.0).clamp(0.0, pw * 0.49),
                num(f, "trimRightMm", 0.0).clamp(0.0, pw * 0.49),
                num(f, "trimTopMm", 0.0).clamp(0.0, ph * 0.49),
                tp_bottom(f).clamp(0.0, ph * 0.49),
            );
            let shade = Color32::from_rgba_unmultiplied(t.danger.r(), t.danger.g(), t.danger.b(), 64);
            let inner = Rect::from_min_max(
                pos2(sheet.left() + l as f32 * k, sheet.top() + tp as f32 * k),
                pos2(sheet.right() - r as f32 * k, sheet.bottom() - b as f32 * k),
            );
            p.rect_filled(Rect::from_min_max(sheet.min, pos2(sheet.right(), inner.top())), 0.0, shade);
            p.rect_filled(Rect::from_min_max(pos2(sheet.left(), inner.bottom()), sheet.max), 0.0, shade);
            p.rect_filled(Rect::from_min_max(pos2(sheet.left(), inner.top()), pos2(inner.left(), inner.bottom())), 0.0, shade);
            p.rect_filled(Rect::from_min_max(pos2(inner.right(), inner.top()), pos2(sheet.right(), inner.bottom())), 0.0, shade);
            p.rect_stroke(inner, 0.0, Stroke::new(1.0, t.danger), egui::StrokeKind::Inside);
            p.rect_stroke(sheet, 0.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
            p.text(pos2(sheet.center().x, sheet.top() - 12.0), Align2::CENTER_CENTER, format!("{pw:.0} mm"), egui::FontId::proportional(11.0), t.text_dim);
            p.text(pos2(sheet.right() + 8.0, sheet.center().y), Align2::LEFT_CENTER, format!("{ph:.0} mm"), egui::FontId::proportional(11.0), t.text_dim);
            let dpi = num(f, "dpi", 300.0);
            let layout = grid_summary(app, f).map_or_else(
                || format!("{} copies", copies_of(app, f).unwrap_or(0)),
                |(cols, rows)| format!("{} copies · {cols} × {rows} layout", copies_of(app, f).unwrap_or(0)),
            );
            p.text(pos2(area.center().x, area.bottom() - 30.0), Align2::CENTER_CENTER, layout, egui::FontId::proportional(11.0), t.text_dim);
            p.text(
                pos2(area.center().x, area.bottom() - 14.0),
                Align2::CENTER_CENTER,
                format!("{}×{} px · shaded edge is trimmed", (pw / 25.4 * dpi).round(), (ph / 25.4 * dpi).round()),
                egui::FontId::proportional(11.0),
                t.text_dim,
            );
        }
        Err(e) => {
            p.text(area.center(), Align2::CENTER_CENTER, e, egui::FontId::proportional(13.0), t.danger);
        }
    }
}

/// The bottom trim: same as the top one (the printer trims both alike).
fn tp_bottom(f: &Map<String, Value>) -> f64 {
    num(f, "trimTopMm", 0.0)
}

fn mm_row(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, range: std::ops::RangeInclusive<f32>, suffix: &str) {
    ui.horizontal(|ui| {
        let label_width = (ui.available_width() - 82.0).max(24.0);
        widgets::elided_label(ui, label, label_width);
        let mut v = num(f, key, 0.0) as f32;
        if widgets::value_field_prec(ui, &mut v, range, suffix, 70.0, 1).changed() {
            f.insert(key.into(), json!(v));
            mark_custom(f);
        }
    });
}

fn settings_pane(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let mut saved = templates(app);
    ui.vertical(|ui| {
        ui.set_width(ui.available_width());
        widgets::section_label(ui, "Paper & Templates");
        widgets::hairline(ui);
        let mut cards = preset_cards(&saved);
        let mut choice = selected_card(f);
        if let Some(selected) = cards.iter().position(|(id, ..)| id == &choice)
            && selected > 0
        {
            cards.swap(0, selected);
        }
        let expanded = f.get("__showMore").and_then(Value::as_bool).unwrap_or(false);
        let visible_count = if expanded { cards.len() } else { cards.len().min(VISIBLE_CARDS) };
        ui.columns(2, |columns| {
            for (index, (id, name, detail)) in cards.iter().take(visible_count).enumerate() {
                let column = index % 2;
                let response = widgets::preset_card(&mut columns[column], name, detail, choice == *id);
                if response.clicked() {
                    choice.clone_from(id);
                    apply_choice(f, id, &saved);
                }
                columns[column].add_space(5.0);
            }
        });
        if cards.len() > VISIBLE_CARDS {
            let hidden = cards.len().saturating_sub(VISIBLE_CARDS);
            let label = if expanded { "Show less".to_string() } else { format!("Show more ({hidden})") };
            if ui.button(label).clicked() {
                f.insert("__showMore".into(), json!(!expanded));
            }
        }
        if ui.selectable_label(choice == "custom", "Custom dimensions…").clicked() {
            choice = "custom".into();
            apply_choice(f, &choice, &saved);
        }
        if choice == "custom" || choice.starts_with("t:") {
            mm_row(ui, f, "paperWidthMm", tl!("Width"), 20.0..=1000.0, "mm");
            mm_row(ui, f, "paperHeightMm", tl!("Height"), 20.0..=1000.0, "mm");
        }

        ui.add_space(8.0);
        widgets::section_label(ui, "Print Job");
        widgets::hairline(ui);
        ui.horizontal(|ui| {
            let label_width = (ui.available_width() - 170.0).max(24.0);
            widgets::elided_label(ui, "Copies", label_width);
            let mut auto = f.get("copiesAuto").and_then(Value::as_bool).unwrap_or(true);
            let mut copies = num(f, "copies", 8.0) as f32;
            if ui.checkbox(&mut auto, "Maximum").changed() {
                if !auto {
                    // Keep the number that was shown, to edit from there.
                    f.insert("copies".into(), json!(copies_of(app, f).unwrap_or(8)));
                }
                f.insert("copiesAuto".into(), json!(auto));
                mark_custom(f);
            }
            if auto {
                copies = copies_of(app, f).unwrap_or(1) as f32;
                ui.add_enabled_ui(false, |ui| widgets::value_field_prec(ui, &mut copies, 1.0..=100.0, "", 70.0, 0));
            } else if widgets::value_field_prec(ui, &mut copies, 1.0..=100.0, "", 70.0, 0).changed() {
                f.insert("copies".into(), json!(copies.round().clamp(1.0, 100.0)));
                mark_custom(f);
            }
        });
        ui.horizontal(|ui| {
            let label_width = (ui.available_width() - 138.0).max(24.0);
            widgets::elided_label(ui, "Photo orientation", label_width);
            let mut o = if f.get("rotate").and_then(Value::as_bool).unwrap_or(false) { "horizontal" } else { "vertical" }.to_string();
            if widgets::dropdown(ui, "selphy-orient", &mut o, &[("vertical".to_string(), "Vertical"), ("horizontal".to_string(), "Horizontal")], 130.0) {
                f.insert("rotate".into(), json!(o == "horizontal"));
                mark_custom(f);
            }
        });
        let mut margin = f.get("cutMarks").and_then(Value::as_bool).unwrap_or(true);
        if widgets::checkbox(ui, &mut margin, "Print cut guides").changed() {
            f.insert("cutMarks".into(), json!(margin));
            mark_custom(f);
        }

        ui.add_space(8.0);
        widgets::section_label(ui, "Save Template");
        widgets::hairline(ui);
        let mut name = f.get("__tplName").and_then(Value::as_str).unwrap_or_default().to_string();
        ui.horizontal(|ui| {
            let field_width = (ui.available_width() - 140.0).max(70.0);
            if ui.add(egui::TextEdit::singleline(&mut name).hint_text("Template name").desired_width(field_width)).changed() {
                f.insert("__tplName".into(), json!(name));
            }
            let name = name.trim().to_string();
            if ui.add_enabled(!name.is_empty(), egui::Button::new(tl!("Save"))).clicked() {
                let mut t: Map<String, Value> = TEMPLATE_KEYS.iter().filter_map(|k| f.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect();
                t.insert("name".into(), json!(name));
                saved.retain(|s| s.get("name").and_then(Value::as_str) != Some(name.as_str()));
                saved.push(t);
                store_templates(app, saved.clone());
                f.insert("__preset".into(), json!(format!("t:{name}")));
            }
            let current_template = template_choice(f);
            if ui.add_enabled(!current_template.is_empty(), egui::Button::new(tl!("Delete"))).clicked() && !current_template.is_empty() {
                saved.retain(|s| s.get("name").and_then(Value::as_str) != Some(current_template.as_str()));
                store_templates(app, saved.clone());
                mark_custom(f);
            }
        });

        ui.add_space(8.0);
        egui::CollapsingHeader::new("Resolution").id_salt("selphy-resolution-settings").show(ui, |ui| {
            ui.horizontal(|ui| {
                let label_width = (ui.available_width() - 86.0).max(24.0);
                widgets::elided_label(ui, tl!("Resolution"), label_width);
                let mut v = num(f, "dpi", 300.0) as f32;
                if widgets::value_field(ui, &mut v, 72.0..=1200.0, "dpi", 80.0).changed() {
                    f.insert("dpi".into(), json!(v.round()));
                    mark_custom(f);
                }
            });
        });
        egui::CollapsingHeader::new("Photo Placement").id_salt("selphy-placement-settings").show(ui, |ui| {
            mm_row(ui, f, "gapMm", "Gap between photos", 0.0..=50.0, "mm");
            mm_row(ui, f, "scalePct", "Scale", 10.0..=300.0, "%");
            mm_row(ui, f, "leftMm", "Move from left", -100.0..=500.0, "mm");
            mm_row(ui, f, "topMm", "Move from top", -100.0..=500.0, "mm");
        });
        egui::CollapsingHeader::new("Borderless Printer Trim").id_salt("selphy-trim-settings").show(ui, |ui| {
            ui.label("The shaded edge in the preview is trimmed by the printer.");
            mm_row(ui, f, "trimLeftMm", "Left", 0.0..=50.0, "mm");
            mm_row(ui, f, "trimRightMm", "Right", 0.0..=50.0, "mm");
            mm_row(ui, f, "trimTopMm", "Top and bottom", 0.0..=50.0, "mm");
        });
    });
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context, id: u64, title: &str, f: &mut Map<String, Value>) -> Option<bool> {
    idphoto_ui::shell(app, ctx, id, title, f, preview_pane, settings_pane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_layouts_are_choices_in_the_paper_preset_grid() {
        let mut f = Map::new();
        f.insert("__preset".into(), json!("selphy"));
        assert_eq!(selected_card(&f), "selphy");
        assert_eq!(template_choice(&f), "");

        f.insert("__preset".into(), json!("t:Family Print"));
        assert_eq!(selected_card(&f), "t:Family Print");
        assert_eq!(template_choice(&f), "Family Print");

        f.insert("__preset".into(), json!("custom"));
        assert_eq!(selected_card(&f), "custom");
        assert_eq!(template_choice(&f), "");
    }

    #[test]
    fn template_card_shows_paper_dimensions_and_restores_full_print_setup() {
        let template = serde_json::from_value::<Map<String, Value>>(json!({
            "name": "Family",
            "paperWidthMm": 100.0,
            "paperHeightMm": 148.0,
            "dpi": 600.0,
            "copies": 6.0,
            "gapMm": 2.0,
            "scalePct": 95.0,
            "leftMm": 4.0,
            "topMm": 5.0,
            "trimLeftMm": 1.0,
            "trimRightMm": 2.0,
            "trimTopMm": 3.0,
            "cutMarks": false,
            "rotate": true,
            "copiesAuto": false
        }))
        .unwrap_or_default();
        assert!(TEMPLATE_KEYS.iter().all(|key| template.get(*key).is_some_and(|value| valid(key, value))));

        let cards = preset_cards(std::slice::from_ref(&template));
        assert_eq!(cards.first(), Some(&("t:Family".into(), "Family".into(), "100 × 148 mm".into())));

        let mut f = Map::new();
        apply_choice(&mut f, "t:Family", std::slice::from_ref(&template));
        for key in TEMPLATE_KEYS {
            assert_eq!(f.get(key), template.get(key), "template restores `{key}`");
        }
    }
}
