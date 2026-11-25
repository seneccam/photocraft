//! The Assistant panel's link to the person's own `claude` command-line tool (Claude Code): one
//! long-running `claude -p` process per conversation, fed user messages as stream-json on stdin and
//! read as stream-json events on stdout. The CLI runs the model's tool loop and signs in with the
//! person's own account, so PhotoCraft holds no key. Its only tools are PhotoCraft's own MCP server
//! (`photocraft-cli mcp --bridge`), pointed at this window's control server and token, so every
//! tool call edits this window: never another one, and never a session without a window.
//!
//! A reader thread turns each output line into an [`Event`] and wakes the UI; stderr is kept for
//! the error shown when the process ends early. Dropping the [`Process`] ends it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// The MCP server name the CLI lists PhotoCraft's tools under (`mcp__photocraft__<tool>`).
pub(crate) const SERVER: &str = "photocraft";

/// What the CLI reported, one per output line that matters.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    /// The conversation's id, for resuming it after a stop.
    Started { session_id: String },
    /// Text the model wrote.
    Text(String),
    /// The model called a tool.
    ToolUse { id: String, name: String, input: Value },
    /// A tool answered.
    ToolResult { id: String, failed: bool, text: String },
    /// The turn ended: `error` is set when it ended in failure.
    TurnDone { error: Option<String> },
    /// The process ended; the text is what it last said on stderr.
    Exited(String),
}

/// How to start the CLI.
#[derive(Clone, Debug, Default)]
pub(crate) struct Launch {
    pub claude: PathBuf,
    pub photocraft_cli: PathBuf,
    /// This window's control server: its loopback port and the file holding its token.
    pub bridge: (u16, PathBuf),
    /// A model alias or id (`sonnet`, `opus`, ...); `None` is the CLI's default.
    pub model: Option<String>,
    /// The conversation to continue, after a stop or a model change.
    pub resume: Option<String>,
    pub system_prompt: String,
}

/// A running `claude` process.
pub(crate) struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
}

impl Process {
    /// Starts the CLI; `wake` is called after every event so the UI polls it.
    pub(crate) fn start(launch: &Launch, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        let mcp = json!({"mcpServers": {SERVER: mcp_server(launch)}});
        let work = std::env::temp_dir().join("photocraft-assistant");
        std::fs::create_dir_all(&work).map_err(|e| format!("couldn't create {}: {e}", work.display()))?;
        let mut cmd = Command::new(&launch.claude);
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            // Only PhotoCraft's tools: no shell, no file edits, no web, and none of the person's
            // other MCP servers, hooks or plugins.
            .args(["--tools", "", "--strict-mcp-config", "--setting-sources", ""])
            .arg("--mcp-config")
            .arg(mcp.to_string())
            .args(["--allowedTools", &format!("mcp__{SERVER}")])
            .args(["--permission-mode", "dontAsk"])
            .arg("--append-system-prompt")
            .arg(&launch.system_prompt)
            .current_dir(&work)
            .env("PATH", search_path(&launch.claude))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(model) = &launch.model {
            cmd.args(["--model", model]);
        }
        if let Some(id) = &launch.resume {
            cmd.args(["--resume", id]);
        }
        let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", launch.claude.display()))?;
        let stdout = child.stdout.take().ok_or("the claude process has no output")?;
        let stderr = child.stderr.take().ok_or("the claude process has no error output")?;
        let (tx, events) = channel();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let last_err = Arc::new(Mutex::new(String::new()));
        {
            let last_err = Arc::clone(&last_err);
            std::thread::Builder::new()
                .name("assistant-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if !line.trim().is_empty()
                            && let Ok(mut l) = last_err.lock()
                        {
                            *l = line;
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        std::thread::Builder::new().name("assistant-stdout".into()).spawn(move || read_events(stdout, &tx, &*wake, &last_err)).map_err(|e| e.to_string())?;
        Ok(Self { stdin: child.stdin.take(), child, events })
    }

    /// Sends one user message: `content` is the Messages API content array (text and image blocks).
    pub(crate) fn send(&mut self, content: Value) -> Result<(), String> {
        let line = json!({"type": "user", "message": {"role": "user", "content": content}});
        let stdin = self.stdin.as_mut().ok_or("the claude process has stopped")?;
        writeln!(stdin, "{line}").and_then(|()| stdin.flush()).map_err(|e| format!("couldn't reach the claude process: {e}"))
    }

    /// The events that arrived since the last call.
    pub(crate) fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn mcp_server(launch: &Launch) -> Value {
    let (port, token_file) = &launch.bridge;
    json!({
        "type": "stdio",
        "command": launch.photocraft_cli.to_string_lossy(),
        // Straight to this window: a bridge never falls back to a session without one.
        "args": ["mcp", "--bridge", format!("127.0.0.1:{port}"), "--control-token-file", token_file.to_string_lossy()],
    })
}

fn read_events(stdout: std::process::ChildStdout, tx: &Sender<Event>, wake: &(dyn Fn() + Send + Sync), last_err: &Mutex<String>) {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        for event in parse(&value) {
            if tx.send(event).is_err() {
                return;
            }
        }
        wake();
    }
    let err = last_err.lock().map(|e| e.clone()).unwrap_or_default();
    let _ = tx.send(Event::Exited(err));
    wake();
}

/// The events in one stream-json line.
pub(crate) fn parse(v: &Value) -> Vec<Event> {
    let blocks = || v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten();
    match v.get("type").and_then(Value::as_str) {
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            v.get("session_id").and_then(Value::as_str).map(|id| Event::Started { session_id: id.to_owned() }).into_iter().collect()
        }
        Some("assistant") => blocks()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()).map(|t| Event::Text(t.to_owned())),
                Some("tool_use") => Some(Event::ToolUse {
                    id: str_of(b, "id"),
                    name: str_of(b, "name"),
                    input: b.get("input").cloned().unwrap_or(Value::Null),
                }),
                _ => None,
            })
            .collect(),
        Some("user") => blocks()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| Event::ToolResult {
                id: str_of(b, "tool_use_id"),
                failed: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                text: result_text(b.get("content")),
            })
            .collect(),
        Some("result") => {
            let failed = v.get("is_error").and_then(Value::as_bool).unwrap_or(false) || v.get("subtype").and_then(Value::as_str).is_some_and(|s| s != "success");
            let error = failed.then(|| {
                v.get("result").and_then(Value::as_str).filter(|s| !s.is_empty()).map_or_else(|| str_of(v, "subtype"), str::to_owned)
            });
            vec![Event::TurnDone { error }]
        }
        _ => Vec::new(),
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// A tool result's text: a string, or the text blocks of a content array.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Where to look for programs: an app opened from the Dock or a file manager gets a short PATH
/// that leaves out the usual install folders of `claude` and of the `node` it may need.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from) {
        dirs.extend([home.join(".claude/local"), home.join(".local/bin"), home.join(".npm-global/bin"), home.join("bin")]);
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs
}

/// The child's PATH: the folder `claude` is in, then the usual ones.
fn search_path(claude: &Path) -> std::ffi::OsString {
    let mut dirs = search_dirs();
    if let Some(parent) = claude.parent() {
        dirs.insert(0, parent.to_path_buf());
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

fn executable(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

/// The first `name` found on the search path.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let file = executable(name);
    let mut names = vec![file];
    if cfg!(windows) {
        names.push(format!("{name}.cmd"));
    }
    search_dirs().into_iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// The person's `claude` CLI.
pub(crate) fn find_claude() -> Option<PathBuf> {
    find_on_path("claude")
}

/// `photocraft-cli` beside this app's executable (a build folder, a bundle), else on the path.
pub(crate) fn find_photocraft_cli() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join(executable("photocraft-cli")))).filter(|p| p.is_file());
    beside.or_else(|| find_on_path("photocraft-cli"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_lines_become_events() {
        let init = json!({"type": "system", "subtype": "init", "session_id": "abc"});
        assert_eq!(parse(&init), vec![Event::Started { session_id: "abc".into() }]);
        let assistant = json!({"type": "assistant", "message": {"content": [
            {"type": "thinking", "thinking": "hm"},
            {"type": "text", "text": "Done."},
            {"type": "tool_use", "id": "t1", "name": "mcp__photocraft__command_run", "input": {"id": "image.invert"}},
        ]}});
        let events = parse(&assistant);
        assert_eq!(events.len(), 2, "thinking is not shown");
        assert!(matches!(&events[1], Event::ToolUse { name, .. } if name.ends_with("command_run")));
        let result = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": [{"type": "text", "text": "no document"}]}
        ]}});
        assert_eq!(parse(&result), vec![Event::ToolResult { id: "t1".into(), failed: true, text: "no document".into() }]);
        let ok = json!({"type": "result", "subtype": "success", "is_error": false, "result": "Done."});
        assert_eq!(parse(&ok), vec![Event::TurnDone { error: None }]);
        let failed = json!({"type": "result", "subtype": "error_max_turns", "is_error": true});
        assert!(matches!(&parse(&failed)[..], [Event::TurnDone { error: Some(e) }] if e.contains("max_turns")));
    }

    // The tools always edit this window: the MCP server bridges to its control server and never
    // looks for another window or falls back to a session without one.
    #[test]
    fn the_tools_reach_this_window_only() {
        let launch = Launch { bridge: (51963, PathBuf::from("/settings/agent-control.token")), ..Launch::default() };
        let args: Vec<String> = mcp_server(&launch)["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_owned()).collect();
        assert_eq!(args[..3], ["mcp", "--bridge", "127.0.0.1:51963"]);
        assert!(args.windows(2).any(|w| w == ["--control-token-file", "/settings/agent-control.token"]));
        assert!(!args.iter().any(|a| a == "--headless" || a == "--when-closed"));
    }
}
