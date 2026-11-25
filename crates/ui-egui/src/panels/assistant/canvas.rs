//! The Assistant on the canvas: the person's numbered pins, the click that places one, and the
//! notice while a reply edits the document. The canvas tools take no input meanwhile
//! ([`super::blocks_canvas`]), so the click lands here and not on the active tool.

use craft::behavior::kurbo::Point;
use craft::ui::canvas::CanvasSpace;
use egui::{Align2, FontId, Rect, Sense, Stroke, vec2};
use photocraft_doc::DocId;

use super::Pin;
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The radius of a pin's disc, in points.
const PIN_RADIUS: f32 = 11.0;

/// Shows the pins on document `doc`'s canvas at `rect`, places one on a click while placing, and
/// shows the editing notice while a reply runs.
pub(crate) fn overlay(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, space: &CanvasSpace, doc: DocId) {
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter_at(rect);
    let pins: Vec<(usize, Pin)> = app.assistant.pins.iter().copied().filter(|p| p.doc == doc).enumerate().collect();
    for (i, pin) in &pins {
        let at = space.view.to_screen(Point::new(pin.x, pin.y));
        painter.circle(at, PIN_RADIUS, t.accent, Stroke::new(2.0, t.primary_text));
        painter.text(at, Align2::CENTER_CENTER, (i + 1).to_string(), FontId::proportional(12.0), t.primary_text);
    }
    if app.assistant.placing_pins && !app.assistant.busy() {
        let response = ui.interact(rect, egui::Id::unique(("assistant-pins", doc)), Sense::click()).on_hover_cursor(egui::CursorIcon::Crosshair);
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let p = space.view.to_doc(pos);
            let size = app.session.documents().iter().find(|d| d.doc.id == doc).map(|d| d.doc.size);
            if let Some(size) = size
                && p.x.is_finite()
                && p.y.is_finite()
                && (0.0..f64::from(size.width)).contains(&p.x)
                && (0.0..f64::from(size.height)).contains(&p.y)
            {
                app.assistant.pins.push(Pin { doc, x: p.x, y: p.y });
            }
        }
        banner(ui, rect, &t, tl!("Click the image to place a pin for Claude"), None);
    }
    if app.assistant.busy() {
        // Presses on the canvas go nowhere while the reply edits it.
        let _ = ui.interact(rect, egui::Id::unique(("assistant-lock", doc)), Sense::click_and_drag());
        if banner(ui, rect, &t, tl!("Claude is editing this document"), Some(tl!("Stop (Esc)"))) {
            super::stop(app);
        }
    }
}

/// A notice at the top of the canvas, with an optional button; returns whether it was clicked.
fn banner(ui: &mut egui::Ui, rect: Rect, t: &Tokens, text: &str, button: Option<&str>) -> bool {
    let mut clicked = false;
    let width = (rect.width() - 24.0).clamp(120.0, 520.0);
    let at = Rect::from_center_size(rect.center_top() + vec2(0.0, 26.0), vec2(width, 36.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(at), |ui| {
        egui::Frame::new()
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_border))
            .corner_radius(t.radius_lg)
            .inner_margin(egui::Margin::symmetric(12, 6))
            .shadow(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: t.shadow })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if button.is_some() {
                        ui.add(egui::Spinner::new().size(14.0).color(t.accent));
                    }
                    ui.label(egui::RichText::new(text).color(t.text));
                    if let Some(label) = button {
                        clicked = ui.button(label).clicked();
                    }
                });
            });
    });
    clicked
}
