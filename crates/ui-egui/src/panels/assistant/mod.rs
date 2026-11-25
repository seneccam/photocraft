//! Window › Assistant: a chat with Claude about the open document, through the person's own
//! `claude` command-line tool ([`claude_cli`]). Claude edits the document with PhotoCraft's
//! registered commands over the agent connection, as any agent does, so everything it can do the
//! automation interface can do too; the panel adds what an agent outside the window can't know:
//! the active document, the person's pins on the canvas and a picture of what they see.
//!
//! While a reply is running the canvas takes no tool input and the shortcuts are off (Esc stops
//! the reply), and every edit of the reply is coalesced into one history step per document, so
//! one Undo takes the whole reply back.

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod claude_cli;
pub(crate) mod canvas;
pub(crate) mod context;
pub(crate) mod references;
mod panel;

pub(crate) use panel::body;

use photocraft_doc::DocId;
use serde_json::Value;

use crate::PhotocraftApp;

/// A point the person placed on a document for Claude, in document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pin {
    pub doc: DocId,
    pub x: f64,
    pub y: f64,
}

/// How a tool call stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StepState {
    Running,
    Done,
    Failed(String),
}

/// One row of the conversation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    User { text: String, pins: usize, canvas: bool, references: Vec<references::Reference> },
    Reply(String),
    Step { id: String, label: String, state: StepState },
    /// The end of a reply: how to take its edits back, while nothing else changed the document.
    End { undo: Option<UndoPoint>, error: Option<String> },
    Notice(String),
}

/// The history state a reply started from, and the state it left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UndoPoint {
    pub doc: DocId,
    pub before: usize,
    pub after: usize,
}

/// The model the CLI uses: its default, or one of the aliases it understands.
pub(crate) const MODELS: [(&str, Option<&str>); 4] = [("Default", None), ("Sonnet", Some("sonnet")), ("Opus", Some("opus")), ("Haiku", Some("haiku"))];

/// The reply in progress.
#[derive(Clone, Debug, Default)]
struct Turn {
    coalesce: String,
    /// Each open document's history position when the reply began.
    starts: Vec<(DocId, usize)>,
}

/// The panel's state; it lives on the app, so closing or collapsing the panel keeps the
/// conversation and a running reply goes on.
pub(crate) struct Assistant {
    pub input: String,
    /// Images attached to the next message as inspiration.
    pub references: Vec<references::Reference>,
    pub items: Vec<Item>,
    pub pins: Vec<Pin>,
    /// The next canvas click places a pin.
    pub placing_pins: bool,
    pub send_canvas: bool,
    /// An index into [`MODELS`].
    pub model: usize,
    #[cfg(not(target_arch = "wasm32"))]
    process: Option<claude_cli::Process>,
    session_id: Option<String>,
    turn: Option<Turn>,
    turns: u64,
    /// The model of the running process, to restart it on a change.
    running_model: usize,
    /// The window connection the running process uses, to restart it when the connection moves.
    #[cfg(not(target_arch = "wasm32"))]
    running_bridge: Option<(u16, std::path::PathBuf)>,
    /// A message waits for the agent connection, which sending turned on.
    pub(crate) connecting: bool,
    pub(crate) scroll_to_end: bool,
}

impl Default for Assistant {
    fn default() -> Self {
        Self {
            input: String::new(),
            references: Vec::new(),
            items: Vec::new(),
            pins: Vec::new(),
            placing_pins: false,
            send_canvas: true,
            model: 0,
            #[cfg(not(target_arch = "wasm32"))]
            process: None,
            session_id: None,
            turn: None,
            turns: 0,
            running_model: 0,
            #[cfg(not(target_arch = "wasm32"))]
            running_bridge: None,
            connecting: false,
            scroll_to_end: false,
        }
    }
}

impl Assistant {
    /// Whether a reply is running.
    pub(crate) fn busy(&self) -> bool {
        self.turn.is_some()
    }
}

/// Whether the canvas tools must not take input: a reply is running, or the next click places a pin.
pub(crate) fn blocks_canvas(app: &PhotocraftApp) -> bool {
    app.assistant.busy() || app.assistant.placing_pins
}

/// Whether the window's shortcuts are off: a reply is editing the document.
pub(crate) fn blocks_shortcuts(app: &PhotocraftApp) -> bool {
    app.assistant.busy()
}

/// An engine command an agent runs over the control channel during a reply joins the reply's
/// history step (`coalesce`), unless it names its own.
pub(crate) fn tag_edit(app: &PhotocraftApp, mut params: Value) -> Value {
    if let Some(turn) = &app.assistant.turn
        && let Some(fields) = params.as_object_mut()
        && !fields.contains_key("coalesce")
    {
        fields.insert("coalesce".into(), Value::String(turn.coalesce.clone()));
    }
    params
}

/// Why messages can't be sent now, in words for the panel; `None` when they can.
#[cfg(target_arch = "wasm32")]
pub(crate) fn unavailable(_: &PhotocraftApp) -> Option<Unavailable> {
    Some(Unavailable::Web)
}

/// Why messages can't be sent now, in words for the panel; `None` when they can.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn unavailable(app: &PhotocraftApp) -> Option<Unavailable> {
    let Some(link) = app.services.agent_link.as_ref() else { return Some(Unavailable::NoLink) };
    if link.status().port.is_some() {
        return None;
    }
    let launcher = app.services.agent_switch.as_ref().is_some_and(|switch| switch.mcp_flag());
    Some(if launcher { Unavailable::LauncherOff } else { Unavailable::AgentsOff })
}

/// What keeps the Assistant from working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// The browser build runs no programs.
    #[cfg(target_arch = "wasm32")]
    Web,
    /// This build or launch has no agent connection.
    NoLink,
    /// Agents are off in the ArtCraft Launcher.
    LauncherOff,
    /// Agents are off in Preferences.
    AgentsOff,
}

/// Lets agents use the open window (Preferences › AI Integrations), for [`Unavailable::AgentsOff`].
pub(crate) fn allow_agents(app: &mut PhotocraftApp) {
    let _ = app.run("prefs.set", serde_json::json!({"path": "integrations.connectAgents", "value": true}));
}

/// The step label shown for a tool call.
pub(crate) fn step_label(name: &str, input: &Value) -> String {
    let tool = name.rsplit("__").next().unwrap_or(name);
    let command = |id: &str| photocraft_engine::commands::find(id).map_or_else(|| id.to_owned(), |c| c.label.trim_end_matches('…').to_owned());
    match tool {
        "command_run" => input.get("id").and_then(Value::as_str).map_or_else(|| tl!("Ran a command").to_owned(), command),
        "command_batch" => {
            let steps: Vec<String> =
                input.get("steps").and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.get("id").and_then(Value::as_str)).map(command).collect();
            crate::i18n::fmt(tl!("Ran {steps}"), &[("steps", steps.join(", ").as_str())])
        }
        "command_list" => tl!("Looked up commands").to_owned(),
        "doc_inspect" => tl!("Read the document").to_owned(),
        "doc_render_preview" | "render_preview" | "ui_screenshot" => tl!("Looked at the canvas").to_owned(),
        "session_list" => tl!("Listed open documents").to_owned(),
        "doc_select" => tl!("Switched documents").to_owned(),
        "ui_inspect" => tl!("Read the window").to_owned(),
        _ => tool.replace('_', " "),
    }
}

#[cfg(not(target_arch = "wasm32"))]
const SYSTEM_PROMPT: &str = "You are the Assistant panel inside PhotoCraft, a layer-based image editor, talking with the person \
editing the open document. Each of their messages starts with a [PhotoCraft context] block that describes the active document \
(size, layers, selection, numbered pins they placed in document pixels) and may attach a scaled preview of the canvas and 100% \
crops around the pins. Edit with the photocraft MCP tools: find command ids with command_list (filter by words, enabled_only), \
then run them with command_run or command_batch; read the document again with doc_inspect when you need ids. Work on the active \
document unless asked otherwise. Prefer non-destructive edits (adjustment layers, masks, new layers named for what they do) and \
change only what was asked. Images the person attached as references are inspiration (style, colour, mood, layout), not part of \
the document: recreate their look with PhotoCraft's own tools, and say plainly what you can't reproduce. Never save, close, export or overwrite files unless asked. When you are done, answer in two to four \
plain sentences: what you changed and where, naming pins as \"pin N\". If something can't be done, say so plainly.";

/// Sends the composed message with the document's context. With the agent connection off in
/// Preferences, sending is the consent: it turns the connection on and the message follows as soon
/// as the window serves it ([`tick`]).
pub(crate) fn send(app: &mut PhotocraftApp) {
    let text = app.assistant.input.trim().to_owned();
    if text.is_empty() || app.assistant.busy() {
        return;
    }
    match unavailable(app) {
        None => {}
        Some(Unavailable::AgentsOff) => {
            allow_agents(app);
            app.assistant.connecting = true;
            return;
        }
        Some(_) => return,
    }
    app.assistant.connecting = false;
    let doc = app.session.active().map(|st| st.doc.id);
    let pins = app.assistant.pins.iter().filter(|p| Some(p.doc) == doc).count();
    let canvas = app.assistant.send_canvas && doc.is_some();
    let content = context::message(app, &text, &app.assistant.pins, canvas, &app.assistant.references);
    if let Err(error) = deliver(app, content) {
        app.assistant.items.push(Item::Notice(error));
        app.assistant.scroll_to_end = true;
        return;
    }
    app.assistant.turns += 1;
    let starts = app.session.documents().iter().map(|d| (d.doc.id, d.history.past_len())).collect();
    app.assistant.turn = Some(Turn { coalesce: format!("assistant-reply-{}", app.assistant.turns), starts });
    let references = std::mem::take(&mut app.assistant.references);
    app.assistant.items.push(Item::User { text, pins, canvas, references });
    app.assistant.input.clear();
    app.assistant.placing_pins = false;
    app.assistant.scroll_to_end = true;
}

#[cfg(not(target_arch = "wasm32"))]
fn deliver(app: &mut PhotocraftApp, content: Value) -> Result<(), String> {
    let bridge = window_bridge(app).ok_or_else(|| tl!("PhotoCraft's agent connection isn't ready, so Claude can't reach this window yet. Try again in a moment.").to_owned())?;
    let a = &mut app.assistant;
    if a.process.is_some() && (a.running_model != a.model || a.running_bridge.as_ref() != Some(&bridge)) {
        a.process = None;
    }
    if a.process.is_none() {
        let claude = claude_cli::find_claude().ok_or_else(|| {
            tl!("Claude Code isn't installed: PhotoCraft couldn't find the `claude` command. Install it from claude.com/claude-code and sign in once in a terminal.").to_owned()
        })?;
        let photocraft_cli = claude_cli::find_photocraft_cli()
            .ok_or_else(|| tl!("PhotoCraft couldn't find `photocraft-cli` beside the app or on the PATH; Claude needs it to edit the document.").to_owned())?;
        let launch = claude_cli::Launch {
            claude,
            photocraft_cli,
            bridge: bridge.clone(),
            model: MODELS.get(a.model).and_then(|(_, m)| *m).map(str::to_owned),
            resume: a.session_id.clone(),
            system_prompt: SYSTEM_PROMPT.to_owned(),
        };
        let ctx = WAKE.get().cloned();
        a.process = Some(claude_cli::Process::start(&launch, move || {
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        })?);
        a.running_model = a.model;
        a.running_bridge = Some(bridge);
    }
    a.process.as_mut().map_or(Err(tl!("Claude isn't running").to_owned()), |p| p.send(content))
}

/// This window's control server: the port its agent link serves on and the file with its token,
/// as the link published them in the settings folder.
#[cfg(not(target_arch = "wasm32"))]
fn window_bridge(app: &PhotocraftApp) -> Option<(u16, std::path::PathBuf)> {
    use craft::automation::agents::{TOKEN_FILE, read_connection};
    let port = app.services.agent_link.as_ref()?.status().port?;
    let dir = app.services.settings_dir.as_deref()?;
    let published = read_connection(dir).ok().flatten().filter(|c| c.pid == std::process::id() && c.port == port).map(|c| c.token_file);
    let token = published.unwrap_or_else(|| dir.join(TOKEN_FILE));
    token.is_file().then_some((port, token))
}

#[cfg(target_arch = "wasm32")]
fn deliver(_: &mut PhotocraftApp, _: Value) -> Result<(), String> {
    Err(tl!("The Assistant needs the desktop app.").to_owned())
}

/// The UI context the CLI's reader threads wake.
#[cfg(not(target_arch = "wasm32"))]
static WAKE: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// Attaches image files the person picks as references for the next message.
pub(crate) fn add_image_files(app: &mut PhotocraftApp) {
    let picked = app.pick_files_bytes(|app, files| {
        for file in files {
            let attached = file.read().and_then(|(name, bytes)| references::Reference::from_file(&name, &bytes));
            attach(app, attached);
        }
        Ok(Value::Null)
    });
    if let Err(error) = picked {
        app.assistant.items.push(Item::Notice(error));
    }
}

/// Attaches the image on the clipboard (a screenshot, an image copied in another app).
pub(crate) fn paste_image(app: &mut PhotocraftApp) {
    let read = match app.services.clipboard_get_image.as_mut() {
        Some(get) => get(),
        None => Err(tl!("This build can't read images from the clipboard.").to_owned()),
    };
    let attached = match read {
        Ok(Some((w, h, pixels))) => references::Reference::from_rgba(tl!("Pasted image"), w, h, &pixels),
        Ok(None) => Err(tl!("The clipboard holds no image.").to_owned()),
        Err(e) => Err(e),
    };
    attach(app, attached);
}

/// Attaches open document `index` as it looks now.
pub(crate) fn attach_document(app: &mut PhotocraftApp, index: usize) {
    let Some(st) = app.session.documents().get(index) else { return };
    let img = photocraft_compose::thumbnail(&st.doc, references::MAX_SIDE);
    let attached = references::Reference::from_rgba(&st.doc.name, img.width, img.height, &img.pixels);
    attach(app, attached);
}

fn attach(app: &mut PhotocraftApp, attached: Result<references::Reference, String>) {
    match attached {
        Ok(r) => app.assistant.references.push(r),
        Err(e) => {
            app.assistant.items.push(Item::Notice(e));
            app.assistant.scroll_to_end = true;
        }
    }
}

/// Stops the running reply; the conversation continues with the next message.
pub(crate) fn stop(app: &mut PhotocraftApp) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.assistant.process = None;
    }
    if app.assistant.turn.is_some() {
        finish(app, Some(tl!("Stopped").to_owned()));
    }
}

/// Starts a new conversation.
pub(crate) fn new_chat(app: &mut PhotocraftApp) {
    stop(app);
    let a = &mut app.assistant;
    a.items.clear();
    a.pins.clear();
    a.references.clear();
    a.session_id = None;
}

/// Takes back a reply's edits: the document returns to the history state the reply began from.
pub(crate) fn undo(app: &mut PhotocraftApp, point: UndoPoint) {
    let Some(index) = app.session.documents().iter().position(|d| d.doc.id == point.doc) else { return };
    if app.session.active_index() != Some(index) {
        let _ = app.run("document.activate", serde_json::json!({"document": index}));
    }
    if app.run("history.selectState", serde_json::json!({"state": point.before})).is_ok() {
        for item in &mut app.assistant.items {
            if let Item::End { undo, .. } = item
                && *undo == Some(point)
            {
                *undo = None;
            }
        }
    }
}

/// Whether `point` can still be taken back: nothing else edited its document since.
pub(crate) fn can_undo(app: &PhotocraftApp, point: &UndoPoint) -> bool {
    app.session.documents().iter().any(|d| d.doc.id == point.doc && d.history.past_len() == point.after)
}

fn finish(app: &mut PhotocraftApp, error: Option<String>) {
    let Some(turn) = app.assistant.turn.take() else { return };
    for item in &mut app.assistant.items {
        if let Item::Step { state, .. } = item
            && *state == StepState::Running
        {
            *state = if error.is_some() { StepState::Failed(tl!("Stopped").to_owned()) } else { StepState::Done };
        }
    }
    // The document the reply edited: the active one, else the first whose history moved.
    let moved = |id: DocId, before: usize| app.session.documents().iter().find(|d| d.doc.id == id).map(|d| (before, d.history.past_len())).filter(|(b, a)| a != b);
    let active = app.session.active().map(|st| st.doc.id);
    let undo = turn
        .starts
        .iter()
        .filter(|(id, _)| Some(*id) == active)
        .chain(turn.starts.iter())
        .find_map(|&(id, before)| moved(id, before).map(|(before, after)| UndoPoint { doc: id, before, after }));
    app.assistant.items.push(Item::End { undo, error });
    app.assistant.scroll_to_end = true;
}

/// Applies what the CLI reported since the last frame; Esc stops a running reply.
pub(crate) fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = WAKE.get_or_init(|| ctx.clone());
        let events = app.assistant.process.as_ref().map(claude_cli::Process::poll).unwrap_or_default();
        for event in events {
            apply(app, event);
        }
    }
    // The message waiting for the agent connection goes once the window serves it.
    if app.assistant.connecting {
        match unavailable(app) {
            None => send(app),
            Some(Unavailable::AgentsOff) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Some(_) => app.assistant.connecting = false,
        }
    }
    if app.assistant.busy() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        stop(app);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn apply(app: &mut PhotocraftApp, event: claude_cli::Event) {
    use claude_cli::Event;
    app.assistant.scroll_to_end = true;
    match event {
        Event::Started { session_id } => app.assistant.session_id = Some(session_id),
        Event::Text(text) => app.assistant.items.push(Item::Reply(text)),
        Event::ToolUse { id, name, input } => app.assistant.items.push(Item::Step { id, label: step_label(&name, &input), state: StepState::Running }),
        Event::ToolResult { id, failed, text } => {
            let step = app.assistant.items.iter_mut().rev().find(|i| matches!(i, Item::Step { id: s, .. } if *s == id));
            if let Some(Item::Step { state, .. }) = step {
                *state = if failed { StepState::Failed(first_line(&text)) } else { StepState::Done };
            }
        }
        Event::TurnDone { error } => finish(app, error),
        Event::Exited(stderr) => {
            app.assistant.process = None;
            if app.assistant.turn.is_some() {
                let error = if stderr.is_empty() { tl!("Claude stopped unexpectedly").to_owned() } else { stderr };
                finish(app, Some(error));
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
    line.chars().take(160).collect()
}

#[cfg(test)]
mod tests;
