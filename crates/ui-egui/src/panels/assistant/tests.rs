use serde_json::json;

use super::*;

fn app_with_doc() -> Box<PhotocraftApp> {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    app
}

fn begin_reply(app: &mut PhotocraftApp) {
    let starts = app.session.documents().iter().map(|d| (d.doc.id, d.history.past_len())).collect();
    app.assistant.turn = Some(Turn { coalesce: "assistant-reply-test".into(), starts });
}

// Edits an agent makes during a reply join one history step, so one Undo takes the reply back;
// outside a reply, and when the agent names its own key, nothing changes.
#[test]
fn a_reply_edits_in_one_history_step() {
    let mut app = app_with_doc();
    assert_eq!(tag_edit(&app, json!({"amount": 1})), json!({"amount": 1}));
    begin_reply(&mut app);
    assert_eq!(tag_edit(&app, json!({"coalesce": "mine"}))["coalesce"], "mine");
    let before = app.session.active().unwrap().history.past_len();
    for id in ["image.adjustments.invert", "image.adjustments.desaturate", "image.adjustments.invert"] {
        let params = tag_edit(&app, json!({}));
        app.run(id, params).unwrap();
    }
    let after = app.session.active().unwrap().history.past_len();
    assert_eq!(after, before + 1, "three edits of one reply are one step");
    finish(&mut app, None);
    let Some(Item::End { undo: Some(point), .. }) = app.assistant.items.last().cloned() else { panic!("the reply offers its undo") };
    assert!(can_undo(&app, &point));
    let revision = app.session.active().unwrap().revision;
    undo(&mut app, point);
    assert_eq!(app.session.active().unwrap().history.past_len(), before, "the document is back where the reply began");
    assert_ne!(app.session.active().unwrap().revision, revision);
}

// A reply's undo is withdrawn once the person edits the document themselves.
#[test]
fn a_later_edit_withdraws_the_replys_undo() {
    let mut app = app_with_doc();
    begin_reply(&mut app);
    let params = tag_edit(&app, json!({}));
    app.run("image.adjustments.invert", params).unwrap();
    finish(&mut app, None);
    let Some(Item::End { undo: Some(point), .. }) = app.assistant.items.last().cloned() else { panic!("the reply offers its undo") };
    app.run("image.adjustments.invert", json!({})).unwrap();
    assert!(!can_undo(&app, &point));
}

// While a reply runs or a pin is being placed, the canvas tools and shortcuts stand aside;
// stopping hands them back.
#[test]
fn the_canvas_is_locked_only_while_needed() {
    let mut app = app_with_doc();
    assert!(!blocks_canvas(&app) && !blocks_shortcuts(&app));
    app.assistant.placing_pins = true;
    assert!(blocks_canvas(&app) && !blocks_shortcuts(&app));
    app.assistant.placing_pins = false;
    begin_reply(&mut app);
    assert!(blocks_canvas(&app) && blocks_shortcuts(&app));
    stop(&mut app);
    assert!(!blocks_canvas(&app) && !blocks_shortcuts(&app));
    assert!(matches!(app.assistant.items.last(), Some(Item::End { error: Some(_), .. })), "a stopped reply says so");
}

// The context names the document, its layers and the pins, and carries the canvas when asked.
#[test]
fn the_message_describes_what_the_person_sees() {
    let app = app_with_doc();
    let doc = app.session.active().unwrap().doc.id;
    let pins = [Pin { doc, x: 10.0, y: 20.0 }];
    let with = context::message(&app, "warm it up", &pins, true);
    let blocks = with.as_array().unwrap();
    assert_eq!(blocks.iter().filter(|b| b["type"] == "image").count(), 2, "the canvas and the pin's crop");
    let text = blocks.last().unwrap()["text"].as_str().unwrap();
    assert!(text.contains("64×48") && text.contains("Pin 1 at document pixel (10, 20)") && text.ends_with("warm it up"));
    let without = context::message(&app, "hi", &pins, false);
    assert!(without.as_array().unwrap().iter().all(|b| b["type"] == "text"));
}
