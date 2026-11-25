//! The Assistant panel's body: what Claude will see, the conversation, and the message box.

use egui::{RichText, ScrollArea, Stroke, TextEdit};

use super::{Item, MODELS, StepState, Unavailable};
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The message box's id, for focus.
const INPUT: &str = "assistant-input";

pub(crate) fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.spacing_mut().item_spacing.y = 6.0;
    egui::Frame::new().inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        if let Some(why) = super::unavailable(app) {
            unavailable(app, ui, &t, why);
            ui.separator();
        }
        context_card(app, ui, &t);
        let composer_height = 128.0;
        let list_height = (ui.available_height() - composer_height).max(80.0);
        ScrollArea::vertical().id_salt("assistant-conversation").max_height(list_height).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            conversation(app, ui, &t);
            if std::mem::take(&mut app.assistant.scroll_to_end) {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
        });
        ui.separator();
        composer(app, ui, &t);
    });
}

fn unavailable(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens, why: Unavailable) {
    let text = match why {
        #[cfg(target_arch = "wasm32")]
        Unavailable::Web => tl!("The Assistant runs Claude Code on your computer, so it needs the desktop app."),
        Unavailable::NoLink => tl!("This launch of PhotoCraft has no agent connection, so Claude can't edit documents here."),
        Unavailable::LauncherOff => tl!("Claude edits through PhotoCraft's agent connection. Turn on Let AI agents use Craft apps in the ArtCraft Launcher's settings."),
        Unavailable::AgentsOff => tl!("Claude edits through PhotoCraft's agent connection, which is off."),
    };
    ui.label(RichText::new(text).color(t.warning));
    if why == Unavailable::AgentsOff && ui.button(tl!("Let Agents Use PhotoCraft")).clicked() {
        super::allow_agents(app);
    }
}

fn context_card(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens) {
    egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0, t.card_border)).corner_radius(t.radius).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match app.session.active() {
            Some(st) => {
                let doc = st.doc.id;
                let pins = app.assistant.pins.iter().filter(|p| p.doc == doc).count();
                ui.label(RichText::new(st.doc.name.clone()).strong().color(t.text));
                let layers = crate::i18n::fmt(tl!("{n} layers"), &[("n", st.doc.layers.len().to_string().as_str())]);
                let pins = crate::i18n::fmt(tl!("{n} pins"), &[("n", pins.to_string().as_str())]);
                ui.label(RichText::new(format!("{}×{} px · {layers} · {pins}", st.doc.size.width, st.doc.size.height)).small().color(t.text_dim));
            }
            None => {
                ui.label(RichText::new(tl!("No document open")).color(t.text_dim));
            }
        }
        ui.checkbox(&mut app.assistant.send_canvas, tl!("Send canvas image"));
        let busy = app.assistant.busy();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tl!("Model:")).small().color(t.text_dim));
                for (i, (name, _)) in MODELS.iter().enumerate() {
                    ui.selectable_value(&mut app.assistant.model, i, crate::i18n::t(name));
                }
            });
        });
    });
}

fn conversation(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.assistant.items.is_empty() {
        ui.label(RichText::new(tl!("Ask Claude to edit the open document. Place pins on the canvas to point at areas, then mention them as pin 1, pin 2…")).color(t.text_faint));
    }
    let mut undo = None;
    for item in &app.assistant.items {
        match item {
            Item::User { text, pins, canvas } => {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    egui::Frame::new().fill(t.accent_soft).corner_radius(t.radius_lg).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                        ui.set_max_width(ui.available_width() * 0.85);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(text).color(t.text));
                            let mut sent = Vec::new();
                            if *canvas {
                                sent.push(tl!("canvas image").to_owned());
                            }
                            if *pins > 0 {
                                sent.push(crate::i18n::fmt(tl!("{n} pins"), &[("n", pins.to_string().as_str())]));
                            }
                            if !sent.is_empty() {
                                ui.label(RichText::new(format!("+ {}", sent.join(", "))).small().color(t.text_dim));
                            }
                        });
                    });
                });
            }
            Item::Reply(text) => {
                ui.label(RichText::new(text).color(t.text));
            }
            Item::Step { label, state, .. } => {
                ui.horizontal(|ui| {
                    match state {
                        StepState::Running => {
                            ui.add(egui::Spinner::new().size(12.0).color(t.accent));
                        }
                        StepState::Done => {
                            ui.label(RichText::new("✓").color(t.text_dim));
                        }
                        StepState::Failed(_) => {
                            ui.label(RichText::new("✕").color(t.danger));
                        }
                    }
                    let response = ui.label(RichText::new(label).small().color(t.text_dim));
                    if let StepState::Failed(why) = state {
                        response.on_hover_text(why);
                    }
                });
            }
            Item::End { undo: point, error } => {
                ui.horizontal_wrapped(|ui| {
                    if let Some(error) = error {
                        ui.label(RichText::new(error).color(t.danger));
                    }
                    if let Some(point) = point {
                        let possible = super::can_undo(app, point);
                        let button = ui.add_enabled(possible, egui::Button::new(tl!("Undo This Reply")));
                        let button = if possible { button } else { button.on_disabled_hover_text(tl!("The document changed since this reply")) };
                        if button.clicked() {
                            undo = Some(*point);
                        }
                    }
                });
                ui.add_space(6.0);
            }
            Item::Notice(text) => {
                ui.label(RichText::new(text).color(t.warning));
            }
        }
    }
    if app.assistant.busy() {
        let response = ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0).color(t.accent));
            ui.label(RichText::new(tl!("Claude is working…")).color(t.text_dim));
        });
        // Screen readers hear when a reply starts and ends.
        ui.ctx().accesskit_node_builder(response.response.id, |node| node.set_live(egui::accesskit::Live::Polite));
    }
    if let Some(point) = undo {
        super::undo(app, point);
    }
}

fn composer(app: &mut PhotocraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let busy = app.assistant.busy();
    let doc_name = app.session.active().map(|st| st.doc.name.clone());
    let hint = match &doc_name {
        Some(name) => crate::i18n::fmt(tl!("Ask Claude to edit {name}…"), &[("name", name.as_str())]),
        None => tl!("Ask Claude…").to_owned(),
    };
    let id = egui::Id::unique(INPUT);
    let output = TextEdit::multiline(&mut app.assistant.input).id(id).hint_text(hint).desired_rows(3).desired_width(f32::INFINITY).show(ui);
    // Enter sends; Shift+Enter keeps the new line.
    let enter = output.response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
    if enter && app.assistant.input.ends_with('\n') {
        app.assistant.input.pop();
    }
    let mut send = enter && !busy;
    ui.horizontal(|ui| {
        let has_doc = doc_name.is_some();
        let pin = ui.add_enabled(has_doc && !busy, egui::Button::selectable(app.assistant.placing_pins, tl!("Place Pins")));
        if pin.clicked() {
            app.assistant.placing_pins = !app.assistant.placing_pins;
        }
        let doc = app.session.active().map(|st| st.doc.id);
        let any_pins = app.assistant.pins.iter().any(|p| Some(p.doc) == doc);
        if ui.add_enabled(any_pins && !busy, egui::Button::new(tl!("Clear Pins"))).clicked() {
            app.assistant.pins.retain(|p| Some(p.doc) != doc);
        }
        if ui.add_enabled(!app.assistant.items.is_empty(), egui::Button::new(tl!("New Chat"))).clicked() {
            super::new_chat(app);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if busy {
                if ui.button(tl!("Stop")).clicked() {
                    super::stop(app);
                }
            } else {
                let ready = !app.assistant.input.trim().is_empty() && super::unavailable(app).is_none();
                send |= ui.add_enabled(ready, egui::Button::new(RichText::new(tl!("Send")).color(t.primary_text)).fill(t.primary_bg)).clicked();
            }
        });
    });
    if busy {
        ui.label(RichText::new(tl!("The canvas is locked while Claude works. Esc stops.")).small().color(t.text_faint));
    }
    if send {
        super::send(app);
        ui.memory_mut(|m| m.request_focus(id));
    }
}
