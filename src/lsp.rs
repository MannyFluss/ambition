//! `ambition lsp`: the language server. Any editor that speaks LSP can start
//! it. It knows what the buffer says as you type, and reads what Ambition
//! writes in `~/.ambition/`: `state.json` (the presses on their way),
//! `models.json` and `threads`. It never writes a file, never calls a model,
//! and never sends anything: only a press does.
//!
//! It is also the running Ambition's way into your open buffers
//! (`editor.rs`): it keeps a line open to it, says which files are open,
//! gives a buffer's unsaved text when a press asks, and puts Ambition's
//! changes in with `workspace/applyEdit`, only the ranges that change, so the
//! buffer stays unsaved and the editor's undo takes them back.
//!
//! - Colors (semantic tokens): `^` prompts, `^/` commands, `^>` replies, and
//!   status lines.
//! - Completion: command names after `^/`, models after `^/model `, efforts
//!   after a model.
//! - Hover: what a command does; on a prompt, what a press on it sends.
//! - Highlight: with the cursor on a `^` line, the lines a press sends light
//!   up: 8 above it and 8 below.
//! - Diagnostics: an unknown command, `^ /x` with a space, a folder busy with
//!   another turn, a running prompt's state, a status line with no run.
//! - Inlay hints: the lines each prompt sends, what each command does, how
//!   long the agent has worked.
//! - Code actions: keep a reply (drop its `^>`), make `^ /x` a command, fix a
//!   misspelt command.

use crate::carets as C;
use crate::commands as Cm;
use crate::diff;
use crate::hook;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TYPES: [&str; 7] = ["keyword", "function", "type", "parameter", "string", "comment", "operator"];
const KW: u32 = 0;
const FN: u32 = 1;
const TY: u32 = 2;
const PA: u32 = 3;
const ST: u32 = 4;
const CM: u32 = 5;

struct Doc {
    text: String,
    /// The file's name, which says how its carets are written.
    rel: String,
    path: PathBuf,
}

struct Server {
    out: Mutex<std::io::Stdout>,
    docs: Mutex<HashMap<String, Doc>>,
    inlay_refresh: Mutex<bool>,
    n: Mutex<u64>,
    /// The editor's name, from `initialize`.
    client: Mutex<String>,
    /// Whether the editor shows a range it is asked to (window/showDocument).
    shows: Mutex<bool>,
    /// The line to the running Ambition, when there is one.
    bridge: Mutex<Option<std::net::TcpStream>>,
    /// Our requests to the editor, waiting for its answer.
    waiting: Mutex<HashMap<String, std::sync::mpsc::Sender<Value>>>,
}

pub fn main() {
    let s = Arc::new(Server {
        out: Mutex::new(std::io::stdout()), docs: Mutex::new(HashMap::new()), inlay_refresh: Mutex::new(false), n: Mutex::new(0),
        client: Mutex::new(String::new()), shows: Mutex::new(false), bridge: Mutex::new(None), waiting: Mutex::new(HashMap::new()),
    });
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut last_try = std::time::Instant::now() - Duration::from_secs(60);
    while let Some(msg) = read_message(&mut input) {
        let method = msg["method"].as_str().unwrap_or("").to_string();
        if method == "exit" {
            break;
        }
        if method.is_empty() {
            // The editor's answer to one of our requests.
            let id = match &msg["id"] {
                Value::String(x) => x.clone(),
                v => v.to_string(),
            };
            if let Some(tx) = s.waiting.lock().unwrap().remove(&id) {
                let _ = tx.send(msg);
            }
            continue;
        }
        if method == "initialized" {
            let s2 = s.clone();
            std::thread::spawn(move || s2.watch_state());
            let s3 = s.clone();
            std::thread::spawn(move || s3.connect());
        }
        let touched = method == "textDocument/didOpen" || method == "textDocument/didChange";
        s.handle(&msg);
        // Find the running Ambition again when it was not there, or restarted:
        // on opening or typing, at most every two seconds.
        if touched && s.bridge.lock().unwrap().is_none() && last_try.elapsed() > Duration::from_secs(2) {
            last_try = std::time::Instant::now();
            let s4 = s.clone();
            std::thread::spawn(move || s4.connect());
        }
    }
}

impl Server {
    /// Open the line to the running Ambition, if it runs, and say which
    /// files are open. Then answer its requests until the line closes.
    fn connect(self: Arc<Self>) {
        use std::io::{BufReader, Write as _};
        let mut b = self.bridge.lock().unwrap();
        if b.is_some() {
            return;
        }
        let Some((addr, token)) = crate::inbox::location() else { return };
        let Ok(mut c) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else { return };
        let hello = json!({"op": "editor", "token": token, "client": self.client.lock().unwrap().clone(),
            "shows": *self.shows.lock().unwrap()});
        if writeln!(c, "{hello}").is_err() {
            return;
        }
        let Ok(read) = c.try_clone() else { return };
        let mut r = BufReader::new(read);
        let mut line = String::new();
        let _ = c.set_read_timeout(Some(Duration::from_secs(3)));
        if r.read_line(&mut line).is_err() || !line.contains("\"ok\":true") {
            return;
        }
        let _ = c.set_read_timeout(None);
        for d in self.docs.lock().unwrap().values() {
            let _ = writeln!(c, "{}", json!({"open": d.path}));
        }
        *b = Some(c);
        drop(b);
        loop {
            line.clear();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let me = self.clone();
            std::thread::spawn(move || {
                let mut out = me.serve(&req);
                out["id"] = req["id"].clone();
                me.up(out);
            });
        }
        *self.bridge.lock().unwrap() = None;
    }

    /// One line to the running Ambition.
    fn up(&self, v: Value) {
        use std::io::Write as _;
        let mut b = self.bridge.lock().unwrap();
        if let Some(c) = b.as_mut() {
            if writeln!(c, "{v}").is_err() {
                *b = None;
            }
        }
    }

    fn uri_of(&self, path: &str) -> Option<(String, String)> {
        let p = PathBuf::from(path);
        self.docs.lock().unwrap().iter().find(|(_, d)| d.path == p).map(|(u, d)| (u.clone(), d.text.clone()))
    }

    /// Ask the editor something and wait for its answer.
    fn request(&self, method: &str, params: Value) -> Option<Value> {
        let id = {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            format!("ambition-{n}")
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.waiting.lock().unwrap().insert(id.clone(), tx);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let r = rx.recv_timeout(Duration::from_secs(5)).ok();
        self.waiting.lock().unwrap().remove(&id);
        r
    }

    /// Show a range in the editor for a moment, then put the cursor at its
    /// end, where the editor can (window/showDocument).
    fn show(&self, uri: &str, span: &Value) {
        if !*self.shows.lock().unwrap() {
            return;
        }
        self.request("window/showDocument", json!({"uri": uri, "takeFocus": true, "selection": span}));
        std::thread::sleep(Duration::from_millis(1000));
        let end = json!({"start": span["end"], "end": span["end"]});
        self.request("window/showDocument", json!({"uri": uri, "takeFocus": true, "selection": end}));
    }

    /// Answer the running Ambition.
    fn serve(&self, req: &Value) -> Value {
        let no = |t: &str| json!({"ok": false, "text": t});
        if let Some(p) = req["read"].as_str() {
            return match self.uri_of(p) {
                Some((_, text)) => json!({"ok": true, "text": text}),
                None => no("that file is not open here any more."),
            };
        }
        if let Some(p) = req["edit"].as_str() {
            let Some((uri, text)) = self.uri_of(p) else { return no("that file is not open here any more.") };
            if Some(text.as_str()) != req["old"].as_str() {
                return no("the buffer changed since Ambition last read it.");
            }
            let edit = json!({"label": "Ambition", "edit": {"changes": {uri.clone(): req["edits"]}}});
            let r = self.request("workspace/applyEdit", edit);
            if r.as_ref().map(|r| r["result"]["applied"].as_bool() == Some(true)) != Some(true) {
                return no("the editor did not take the edit.");
            }
            let new = apply_edits(&text, req["edits"].as_array().map(Vec::as_slice).unwrap_or(&[]));
            if req["mark"].as_bool() == Some(true) {
                self.show(&uri, &req["span"]);
            }
            return json!({"ok": true, "text": new});
        }
        if let Some(p) = req["show"].as_str() {
            let Some((uri, text)) = self.uri_of(p) else { return no("that file is not open here any more.") };
            if Some(text.as_str()) != req["text"].as_str() {
                return no("the buffer changed.");
            }
            self.show(&uri, &req["span"]);
            return json!({"ok": true});
        }
        no("the language server does not know that request.")
    }
}

/// LSP TextEdits on `text`, all relative to it, applied.
fn apply_edits(text: &str, edits: &[Value]) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let offset = |pos: &Value| -> usize {
        let (l, c) = (pos["line"].as_u64().unwrap_or(0) as usize, pos["character"].as_u64().unwrap_or(0) as u32);
        let before: usize = lines.iter().take(l).map(|x| x.len() + 1).sum();
        before + lines.get(l).map(|x| byte_at(x, c)).unwrap_or(0)
    };
    let mut spans: Vec<(usize, usize, &str)> = edits.iter()
        .map(|e| (offset(&e["range"]["start"]), offset(&e["range"]["end"]), e["newText"].as_str().unwrap_or(""))).collect();
    spans.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut out = text.to_string();
    for (a, b, t) in spans {
        out.replace_range(a.min(out.len())..b.min(out.len()), t);
    }
    out
}

fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse::<usize>().ok();
            }
        }
    }
    let mut buf = vec![0u8; len?];
    r.read_exact(&mut buf).ok()?;
    Some(serde_json::from_slice(&buf).unwrap_or(json!({})))
}

/// UTF-16 length, the unit LSP columns count in.
fn u16len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The byte index in `line` of a UTF-16 column.
fn byte_at(line: &str, col: u32) -> usize {
    let mut n = 0u32;
    for (i, c) in line.char_indices() {
        if n >= col {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    line.len()
}

fn rng(l1: usize, c1: u32, l2: usize, c2: u32) -> Value {
    json!({"start": {"line": l1, "character": c1}, "end": {"line": l2, "character": c2}})
}

/// A range over whole lines s..e-1.
fn line_range(lines: &[&str], s: usize, e: usize) -> Value {
    let e = e.max(s + 1);
    rng(s, 0, e - 1, lines.get(e - 1).map(|l| u16len(l)).unwrap_or(0))
}

fn md(text: String) -> Value {
    json!({"contents": {"kind": "markdown", "value": text}})
}

fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut p = crate::pkce::urldecode(&rest.replace('+', "%2B"));
    if cfg!(windows) && p.starts_with('/') && p.chars().nth(2) == Some(':') {
        p.remove(0);
    }
    Some(PathBuf::from(p))
}

fn load(name: &str) -> Value {
    crate::app::load(&crate::util::state_dir().join(name))
}

/// What Ambition is doing: (is it running, the presses on their way).
fn run_state() -> (bool, Vec<Value>) {
    let d = load("state.json");
    let alive = d["pid"].as_u64().is_some_and(|pid| alive(pid as u32));
    let turns = if alive { d["turns"].as_array().cloned().unwrap_or_default() } else { Vec::new() };
    (alive, turns)
}

/// The press on its way for this status line, if any.
fn turn_of<'a>(turns: &'a [Value], path: &Path, id: &str) -> Option<&'a Value> {
    turns.iter().find(|t| t["id"] == json!(id) && t["doc"].as_str() == Some(&path.to_string_lossy()))
}

/// A turn with an agent now in the folder that holds this file.
fn busy_with<'a>(turns: &'a [Value], path: &Path) -> Option<&'a Value> {
    turns.iter().find(|t| t["state"] == "working" && t["folder"].as_str().is_some_and(|f| path.starts_with(f)))
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}

struct Scan<'a> {
    lines: Vec<&'a str>,
    status: BTreeMap<usize, String>,
    replies: Vec<usize>,
    commands: Vec<C::Command>,
    prompts: Vec<C::Prompt>,
    caret: BTreeSet<usize>,
}

fn scan<'a>(rel: &str, text: &'a str) -> Scan<'a> {
    let lines: Vec<&str> = text.split('\n').collect();
    let status: BTreeMap<usize, String> = C::statuses(rel, text).into_iter().map(|(id, i)| (i, id)).collect();
    let replies: Vec<usize> = (0..lines.len()).filter(|&i| C::is_reply(rel, lines[i])).collect();
    let commands = C::commands(rel, text);
    let prompts = C::prompts(rel, text);
    let mut caret: BTreeSet<usize> = status.keys().cloned().collect();
    caret.extend(replies.iter().cloned());
    caret.extend(commands.iter().map(|c| c.line));
    for p in &prompts {
        caret.extend(p.start..p.end);
    }
    Scan { lines, status, replies, commands, prompts, caret }
}

/// What a caret at line `start` acts on, for a command's stdin: the
/// paragraph (or last list item) just above.
fn target(rel: &str, sc: &Scan, start: usize) -> Option<(usize, usize)> {
    C::above(rel, &sc.lines, start, &sc.caret)
}

/// 0-based (start, end) lines, end exclusive.
type Span = (usize, usize);

/// The lines a press on this prompt sends, 0-based: 8 above it and 8 below,
/// in the file as it will be once the prompt is a status line.
fn window(sc: &Scan, p: &C::Prompt) -> (Option<Span>, Option<Span>) {
    let n = hook::lines(&sc.lines.join("\n")).len();
    let up = (p.start > 0).then(|| (p.start.saturating_sub(hook::EACH_WAY), p.start));
    let down = (p.end < n).then(|| (p.end, (p.end + hook::EACH_WAY).min(n)));
    (up, down)
}

/// "sends lines 3-20", in the file's numbers as they are now.
fn window_label(sc: &Scan, p: &C::Prompt) -> String {
    let (up, down) = window(sc, p);
    let lo = up.map(|u| u.0).unwrap_or(p.start);
    let hi = down.map(|d| d.1).unwrap_or(p.end);
    format!("a press sends lines {}-{}", lo + 1, hi)
}

fn where_(rel: &str, sc: &Scan, t: Option<(usize, usize)>) -> String {
    match t {
        None => "the whole file".into(),
        Some((s, e)) => {
            let item = C::is_note(rel) && C::is_item(sc.lines[s]);
            format!("{}{}", C::span(s, e), if item { " (the last list item)" } else { "" })
        }
    }
}

/// {name: (description, where it comes from)} of every command.
fn all_commands() -> BTreeMap<String, (String, String)> {
    let mut out: BTreeMap<String, (String, String)> = Cm::BUILTINS.iter().map(|(n, d)| (n.to_string(), (d.to_string(), "built in".to_string()))).collect();
    for (n, p) in Cm::find() {
        out.entry(n).or_insert_with(|| {
            let d = Cm::describe(&p);
            let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            (if d.is_empty() { file.clone() } else { d }, format!("runs ~/.ambition/commands/{file}"))
        });
    }
    out
}

fn models() -> Vec<Value> {
    load("models.json")["models"].as_array().cloned().unwrap_or_default()
}

fn thread_line(path: &Path) -> String {
    let t = load("threads")[path.to_string_lossy().as_ref()].clone();
    let mut bits = Vec::new();
    if let Some(m) = t["model"].as_str() {
        bits.push(format!("model: {m}"));
    }
    match t["link"].as_str().filter(|l| !l.is_empty()) {
        Some(l) => bits.push(format!("[this file's thread]({l})")),
        None => bits.push("no thread yet: this file's first prompt starts one".into()),
    }
    bits.join(" · ")
}

fn since(run: &Value) -> u64 {
    (crate::util::epoch() - run["since"].as_f64().unwrap_or_else(crate::util::epoch)).max(0.0) as u64
}

impl Server {
    fn send(&self, v: Value) {
        let body = v.to_string();
        let mut o = self.out.lock().unwrap();
        let _ = write!(o, "Content-Length: {}\r\n\r\n{body}", body.len());
        let _ = o.flush();
    }

    fn respond(&self, id: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn handle(&self, msg: &Value) {
        let method = msg["method"].as_str().unwrap_or("");
        let id = msg.get("id").cloned();
        let p = &msg["params"];
        if method.is_empty() {
            return; // an answer to one of our requests
        }
        match method {
            "initialize" => {
                *self.inlay_refresh.lock().unwrap() = p["capabilities"]["workspace"]["inlayHint"]["refreshSupport"].as_bool() == Some(true);
                *self.client.lock().unwrap() = p["clientInfo"]["name"].as_str().unwrap_or("").to_string();
                *self.shows.lock().unwrap() = p["capabilities"]["window"]["showDocument"]["support"].as_bool() == Some(true);
                self.respond(&id.unwrap_or(json!(0)), json!({"capabilities": {
                    "positionEncoding": "utf-16",
                    "textDocumentSync": {"openClose": true, "change": 1, "save": false},
                    "semanticTokensProvider": {"legend": {"tokenTypes": TYPES, "tokenModifiers": []}, "full": true},
                    "completionProvider": {"triggerCharacters": ["/", " "], "resolveProvider": false},
                    "hoverProvider": true,
                    "documentHighlightProvider": true,
                    "inlayHintProvider": true,
                    "codeActionProvider": {"codeActionKinds": ["quickfix"]},
                }, "serverInfo": {"name": "ambition", "version": env!("CARGO_PKG_VERSION")}}));
            }
            "shutdown" => self.respond(&id.unwrap_or(json!(0)), Value::Null),
            "textDocument/didOpen" | "textDocument/didChange" => {
                let uri = p["textDocument"]["uri"].as_str().unwrap_or("").to_string();
                let text = if method == "textDocument/didOpen" {
                    p["textDocument"]["text"].as_str().unwrap_or("").to_string()
                } else {
                    p["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str()).unwrap_or("").to_string()
                };
                let place = if method == "textDocument/didOpen" {
                    path_of(&uri).map(|p| {
                        let p = std::fs::canonicalize(&p).unwrap_or(p);
                        (p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), p)
                    })
                } else {
                    self.docs.lock().unwrap().get(&uri).map(|d| (d.rel.clone(), d.path.clone()))
                };
                let Some((rel, path)) = place else { return };
                if method == "textDocument/didOpen" {
                    self.up(json!({"open": path}));
                }
                self.docs.lock().unwrap().insert(uri.clone(), Doc { text, rel, path });
                self.publish(&uri);
            }
            "textDocument/didClose" => {
                if let Some(d) = self.docs.lock().unwrap().remove(p["textDocument"]["uri"].as_str().unwrap_or("")) {
                    self.up(json!({"close": d.path}));
                }
            }
            _ => {
                let Some(id) = id else { return };
                let uri = p["textDocument"]["uri"].as_str().unwrap_or("");
                let docs = self.docs.lock().unwrap();
                let Some(d) = docs.get(uri) else {
                    let empty = if method == "textDocument/semanticTokens/full" { json!({"data": []}) } else { Value::Null };
                    drop(docs);
                    return self.respond(&id, empty);
                };
                let pos = (p["position"]["line"].as_u64().unwrap_or(0) as usize, p["position"]["character"].as_u64().unwrap_or(0) as u32);
                let range = (p["range"]["start"]["line"].as_u64().unwrap_or(0) as usize, p["range"]["end"]["line"].as_u64().unwrap_or(0) as usize);
                let result = match method {
                    "textDocument/semanticTokens/full" => tokens(&d.rel, &d.text),
                    "textDocument/completion" => json!({"isIncomplete": false, "items": completion(&d.path, &d.rel, &d.text, pos)}),
                    "textDocument/hover" => hover(&d.path, &d.rel, &d.text, pos.0),
                    "textDocument/documentHighlight" => highlights(&d.rel, &d.text, pos.0),
                    "textDocument/inlayHint" => inlay_hints(&d.path, &d.rel, &d.text, range),
                    "textDocument/codeAction" => code_actions(uri, &d.rel, &d.text, range),
                    _ => {
                        drop(docs);
                        return self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("{method} is not handled")}}));
                    }
                };
                drop(docs);
                self.respond(&id, result);
            }
        }
    }

    fn publish(&self, uri: &str) {
        let diags = {
            let docs = self.docs.lock().unwrap();
            let Some(d) = docs.get(uri) else { return };
            diagnostics(&d.path, &d.rel, &d.text)
        };
        self.send(json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": diags}}));
    }

    /// Re-publish diagnostics when Ambition's state changes, and every few
    /// seconds while a turn runs (for its clock). Otherwise it sleeps.
    fn watch_state(self: Arc<Self>) {
        use notify::Watcher;
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tx2 = tx.clone();
        let mut watcher = notify::recommended_watcher(move |_e: notify::Result<notify::Event>| {
            let _ = tx2.send(());
        })
        .ok();
        let dir = crate::util::state_dir();
        let mut watching = false;
        loop {
            if !watching {
                if let Some(w) = watcher.as_mut() {
                    watching = w.watch(&dir, notify::RecursiveMode::NonRecursive).is_ok();
                }
            }
            let running = !run_state().1.is_empty();
            let wait = if running { Duration::from_secs(3) } else if watching { Duration::from_secs(30) } else { Duration::from_secs(5) };
            let _ = rx.recv_timeout(wait);
            while rx.try_recv().is_ok() {}
            let uris: Vec<String> = self.docs.lock().unwrap().keys().cloned().collect();
            for u in &uris {
                self.publish(u);
            }
            if *self.inlay_refresh.lock().unwrap() && !uris.is_empty() {
                let mut n = self.n.lock().unwrap();
                *n += 1;
                self.send(json!({"jsonrpc": "2.0", "id": format!("ambition-{n}"), "method": "workspace/inlayHint/refresh", "params": null}));
            }
        }
    }
}

fn tokens(rel: &str, text: &str) -> Value {
    let sc = scan(rel, text);
    let mut toks: Vec<(usize, u32, u32, u32)> = Vec::new();
    for (i, l) in sc.lines.iter().enumerate() {
        let Some(c) = C::caret_at(rel, l) else { continue };
        if sc.status.contains_key(&i) {
            toks.push((i, u16len(&l[..c]), u16len(&l[c..]), CM));
        } else if sc.replies.contains(&i) {
            toks.push((i, u16len(&l[..c]), 2, CM));
            if l.len() > c + 2 {
                toks.push((i, u16len(&l[..c + 2]), u16len(&l[c + 2..]), ST));
            }
        }
    }
    for c in &sc.commands {
        let l = sc.lines[c.line];
        let a = c.lead.len();
        toks.push((c.line, u16len(&l[..a]), 2, KW));
        let name_end = a + 2 + c.name.len();
        toks.push((c.line, u16len(&l[..a + 2]), u16len(&l[a + 2..name_end.min(l.len())]), TY));
        let rest = &l[name_end.min(l.len())..];
        if !rest.trim().is_empty() {
            let k = name_end + (rest.len() - rest.trim_start().len());
            toks.push((c.line, u16len(&l[..k]), u16len(l[k..].trim_end()), PA));
        }
    }
    for p in &sc.prompts {
        for i in p.start..p.end {
            let l = sc.lines[i];
            let Some(a) = C::caret_at(rel, l) else { continue };
            toks.push((i, u16len(&l[..a]), 1, KW));
            if !l[a + 1..].trim().is_empty() {
                toks.push((i, u16len(&l[..a + 1]), u16len(l[a + 1..].trim_end()), FN));
            }
        }
    }
    toks.sort();
    let (mut data, mut pl, mut pc) = (Vec::new(), 0usize, 0u32);
    for (line, start, len, typ) in toks {
        if len == 0 {
            continue;
        }
        let dl = (line - pl) as u32;
        let dc = if dl == 0 { start - pc } else { start };
        data.extend([dl, dc, len, typ, 0]);
        pl = line;
        pc = start;
    }
    json!({"data": data})
}

fn efforts(m: &Value) -> Vec<String> {
    m["efforts"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

fn completion(path: &Path, rel: &str, text: &str, pos: (usize, u32)) -> Vec<Value> {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(line) = lines.get(pos.0) else { return vec![] };
    let prefix = &line[..byte_at(line, pos.1)];
    let edit = |col: u32, new: &str| json!({"range": rng(pos.0, col, pos.0, pos.1), "newText": new});
    let Some(a) = C::caret_at(rel, prefix) else { return vec![] };
    let after = &prefix[a + 1..];
    let Some(cmd) = after.strip_prefix('/') else { return vec![] };
    let parts: Vec<&str> = cmd.split(' ').collect();
    if parts.len() == 1 && cmd.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        let col = u16len(&prefix[..a + 2]);
        return all_commands().into_iter().map(|(n, (d, how))| {
            let builtin = Cm::is_builtin(&n);
            json!({"label": n, "kind": if builtin { 14 } else { 3 }, "detail": crate::util::cut(&d, 80),
                "documentation": {"kind": "markdown", "value": format!("`^/{n}` — {d}\n\n{how}")},
                "sortText": format!("{}{n}", if builtin { "0" } else { "1" }), "textEdit": edit(col, &n)})
        }).collect();
    }
    if parts.first() != Some(&"model") {
        return vec![];
    }
    let cur = load("threads")[path.to_string_lossy().as_ref()]["model"].as_str().unwrap_or("").to_string();
    let ms = models();
    let words: Vec<&str> = parts[1..].iter().cloned().filter(|w| !w.is_empty()).collect();
    let typing = !prefix.ends_with(' ');
    if words.is_empty() || (words.len() == 1 && typing) {
        let col = pos.1 - u16len(if typing { words.first().cloned().unwrap_or("") } else { "" });
        return ms.iter().enumerate().map(|(k, m)| {
            let label = m["label"].as_str().unwrap_or("");
            let id = m["model"].as_str().unwrap_or("");
            let using = !label.is_empty() && cur.starts_with(label);
            let e = efforts(m);
            json!({"label": label, "kind": 12, "filterText": format!("{id} {label}"),
                "detail": format!("{}{}", m["provider"].as_str().unwrap_or(""), if using { " · in use" } else { "" }),
                "documentation": {"kind": "markdown", "value": format!("`{id}`{}", if e.is_empty() { String::new() } else { format!("\n\neffort: {}", e.join(", ")) })},
                "sortText": format!("{}{k:03}", if using { "0" } else { "1" }), "textEdit": edit(col, id)})
        }).collect();
    }
    let model_word = words[0].to_lowercase();
    let m = ms.iter().find(|m| m["model"].as_str().unwrap_or("").to_lowercase() == model_word
        || m["label"].as_str().unwrap_or("").to_lowercase() == model_word);
    let vals = m.map(efforts).unwrap_or_else(|| ["low", "medium", "high", "xhigh", "max"].iter().map(|s| s.to_string()).collect());
    let col = pos.1 - u16len(if typing { words.last().cloned().unwrap_or("") } else { "" });
    vals.iter().enumerate().map(|(k, v)| json!({"label": v, "kind": 20, "detail": "effort", "sortText": format!("{k:02}"), "textEdit": edit(col, v)})).collect()
}

fn prompt_card(path: &Path, sc: &Scan, p: &C::Prompt, turns: &[Value]) -> String {
    let mut body = vec!["**`^` prompt** — a press on it (your editor's Ambition key) sends it to this file's thread. Saving sends nothing.".to_string(), String::new()];
    if let Some(name) = p.text.trim_start().strip_prefix('/') {
        let name = name.split(' ').next().unwrap_or("");
        body.push(format!("This starts with `^ /`: the space makes it a prompt. For the command, write `^/{name}`."));
        body.push(String::new());
    }
    let (up, down) = window(sc, p);
    let say = |t: Option<Span>, none: &str| t.map(|(s, e)| C::span(s, e)).unwrap_or_else(|| none.to_string());
    body.push(format!("**The agent is sent:** your words; where this file is; {} above and {} below, as they are on disk; and \
        what changed in this file since its last turn. Nothing else is attached: it asks for more with `ambition more up` or \
        `ambition more down`, and can read anything itself.", say(up, "nothing"), say(down, "nothing")));
    body.push(String::new());
    body.push(thread_line(path));
    if let Some(t) = busy_with(turns, path) {
        body.push(String::new());
        body.push(format!("An agent is working on {} in this folder now: a press here waits for it.", t["id"].as_str().unwrap_or("a prompt")));
    }
    body.join("\n")
}

fn hover(path: &Path, rel: &str, text: &str, i: usize) -> Value {
    let sc = scan(rel, text);
    if i >= sc.lines.len() {
        return Value::Null;
    }
    let (alive, turns) = run_state();
    if let Some(c) = sc.commands.iter().find(|c| c.line == i) {
        let cmds = all_commands();
        let Some((d, how)) = cmds.get(&c.name) else {
            let near = diff::closest(&c.name, cmds.keys().map(String::as_str));
            return md(format!("**`^/{}`** is no command.{}\n\n`^/help` lists them.", c.name,
                near.map(|n| format!(" Did you mean `^/{n}`?")).unwrap_or_default()));
        };
        let mut body = format!("**`^/{}`** — {d}\n\n", c.name);
        if Cm::is_builtin(&c.name) {
            body.push_str("Built in. A press on it runs it at once, with no model.");
        } else {
            let t = target(rel, &sc, i);
            body.push_str(&format!("{}: a press on it runs it at once, with no model. It reads {} on stdin, and what it prints becomes the reply.",
                capital(how), t.map(|_| where_(rel, &sc, t)).unwrap_or_else(|| "nothing".into())));
        }
        if c.name == "model" || c.name == "thread" {
            body.push_str(&format!("\n\n{}", thread_line(path)));
        }
        if c.name == "model" {
            let ms = models();
            if !ms.is_empty() {
                let rows: Vec<String> = ms.iter().map(|m| format!("- {} (`{}`)", m["label"].as_str().unwrap_or(""), m["model"].as_str().unwrap_or(""))).collect();
                body.push_str(&format!("\n\n{}", rows.join("\n")));
            }
        }
        return md(body);
    }
    if let Some(pid) = sc.status.get(&i) {
        if let Some(t) = turn_of(&turns, path, pid) {
            if t["state"] == "waiting" {
                return md(format!("**{pid}** is waiting: another turn is running in this folder, and one runs at a time. It goes next."));
            }
            let thread = t["thread"].as_str().filter(|t| !t.is_empty()).map(|t| format!(" [Watch it in the thread]({t}).")).unwrap_or_default();
            return md(format!("**{pid}** is with the agent, {} s so far.{thread}\n\n`^/stop` stops it. Ambition removes this line when the turn ends.",
                since(t)));
        }
        return md(format!("**{pid}**: no turn is behind this status line.{}",
            if alive { "" } else { " Ambition is not running: start it with `ambition`." }));
    }
    if sc.replies.contains(&i) {
        return md("A reply. Your next press in this file clears it. To keep the text, delete its `^>` (the code action **Keep this reply** does that).".into());
    }
    if let Some(p) = sc.prompts.iter().find(|p| p.start <= i && i < p.end) {
        return md(prompt_card(path, &sc, p, &turns));
    }
    Value::Null
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn highlights(rel: &str, text: &str, i: usize) -> Value {
    let sc = scan(rel, text);
    let range = |(s, e): Span| json!({"range": line_range(&sc.lines, s, e), "kind": 1});
    if let Some(p) = sc.prompts.iter().find(|p| p.start <= i && i < p.end) {
        let (up, down) = window(&sc, p);
        return json!(up.into_iter().chain(down).filter(|(s, e)| e > s).map(range).collect::<Vec<_>>());
    }
    let mut t = None;
    if let Some(c) = sc.commands.iter().find(|c| c.line == i) {
        if !Cm::is_builtin(&c.name) {
            t = target(rel, &sc, i);
        }
    }
    match t {
        Some(t) => json!([range(t)]),
        None => json!([]),
    }
}

fn diagnostics(path: &Path, rel: &str, text: &str) -> Vec<Value> {
    let sc = scan(rel, text);
    let (alive, turns) = run_state();
    let cmds = all_commands();
    let mut out = Vec::new();
    for c in &sc.commands {
        if !cmds.contains_key(&c.name) && !cmds.keys().any(|n| n.starts_with(&c.name)) {
            let l = sc.lines[c.line];
            let a = c.lead.len() + 2;
            let near = diff::closest(&c.name, cmds.keys().map(String::as_str));
            out.push(json!({"range": rng(c.line, u16len(&l[..a]), c.line, u16len(&l[..(a + c.name.len()).min(l.len())])), "severity": 2,
                "source": "Ambition", "code": "no-command",
                "message": format!("no command ^/{}{} ^/help lists them.", c.name, near.map(|n| format!(": did you mean ^/{n}?")).unwrap_or_else(|| ".".into()))}));
        }
    }
    let busy = busy_with(&turns, path);
    for p in &sc.prompts {
        if let Some(name) = p.text.trim_start().strip_prefix('/') {
            let name = name.split(' ').next().unwrap_or("").to_lowercase();
            if cmds.contains_key(&name) && sc.lines[p.start].trim_start().starts_with("^ /") {
                out.push(json!({"range": line_range(&sc.lines, p.start, p.start + 1), "severity": 3, "source": "Ambition", "code": "spaced-command",
                    "message": format!("the space after ^ makes this a prompt for the agent. For the command, write ^/{name}.")}));
            }
        }
        if let Some(t) = busy {
            out.push(json!({"range": line_range(&sc.lines, p.start, p.end), "severity": 4, "source": "Ambition", "code": "busy",
                "message": format!("an agent is working on {} in this folder: a press here waits for it.", t["id"].as_str().unwrap_or("a prompt"))}));
        }
    }
    for (&i, pid) in &sc.status {
        let l = sc.lines[i];
        match turn_of(&turns, path, pid) {
            Some(t) if t["state"] == "waiting" => out.push(json!({"range": line_range(&sc.lines, i, i + 1), "severity": 3, "source": "Ambition",
                "code": "waiting", "message": format!("{pid} · waiting for its folder: one turn at a time runs there")})),
            Some(t) => {
                let what = l.split_once('·').map(|x| x.1.trim()).unwrap_or("working");
                let thread = t["thread"].as_str().filter(|t| !t.is_empty()).map(|t| format!(" · {t}")).unwrap_or_default();
                out.push(json!({"range": line_range(&sc.lines, i, i + 1), "severity": 3, "source": "Ambition", "code": "running",
                    "message": format!("{pid} · {what} · {} s{thread}", since(t))}));
            }
            None => out.push(json!({"range": line_range(&sc.lines, i, i + 1), "severity": 2, "source": "Ambition", "code": "stale",
                "message": format!("{pid}: no turn is behind this status line{}", if alive { "." } else { ": Ambition is not running." })})),
        }
    }
    out
}

fn inlay_hints(path: &Path, rel: &str, text: &str, (lo, hi): (usize, usize)) -> Value {
    let sc = scan(rel, text);
    let cmds = all_commands();
    let mut out = Vec::new();
    for p in &sc.prompts {
        let last = p.end - 1;
        if lo <= last && last <= hi {
            out.push(json!({"position": {"line": last, "character": u16len(sc.lines[last])}, "label": window_label(&sc, p), "paddingLeft": true}));
        }
    }
    for c in &sc.commands {
        if lo <= c.line && c.line <= hi {
            if let Some((d, _)) = cmds.get(&c.name) {
                out.push(json!({"position": {"line": c.line, "character": u16len(sc.lines[c.line])}, "label": format!("→ {}", crate::util::cut(d, 60)), "paddingLeft": true}));
            }
        }
    }
    let (alive, turns) = run_state();
    for (&i, pid) in &sc.status {
        if i < lo || i > hi {
            continue;
        }
        let label = match turn_of(&turns, path, pid) {
            Some(t) if t["state"] == "waiting" => "waiting for its folder".to_string(),
            Some(t) => format!("⏱ {} s · ^/stop stops it", since(t)),
            None => format!("no turn behind this line{}", if alive { "" } else { ": Ambition is not running" }),
        };
        out.push(json!({"position": {"line": i, "character": u16len(sc.lines[i])}, "label": label, "paddingLeft": true}));
    }
    json!(out)
}

fn code_actions(uri: &str, rel: &str, text: &str, (lo, hi): (usize, usize)) -> Value {
    let sc = scan(rel, text);
    let mut acts = Vec::new();
    let act = |acts: &mut Vec<Value>, title: String, edits: Vec<Value>| {
        acts.push(json!({"title": title, "kind": "quickfix", "edit": {"changes": {uri: edits}}}));
    };
    let reps: Vec<usize> = sc.replies.iter().cloned().filter(|&i| lo <= i && i <= hi).collect();
    if let Some(&first) = reps.first() {
        let keep = |i: usize| {
            let l = sc.lines[i];
            let a = l.find("^>").unwrap_or(0);
            let b = a + 2 + usize::from(l[a + 2..].starts_with(' '));
            json!({"range": rng(i, u16len(&l[..a]), i, u16len(&l[..b])), "newText": ""})
        };
        act(&mut acts, "Keep this reply (drop its ^>)".into(), vec![keep(first)]);
        let mut block = vec![first];
        while sc.replies.contains(&(block[block.len() - 1] + 1)) {
            block.push(block[block.len() - 1] + 1);
        }
        while block[0] > 0 && sc.replies.contains(&(block[0] - 1)) {
            block.insert(0, block[0] - 1);
        }
        if block.len() > 1 {
            act(&mut acts, format!("Keep all {} lines of this reply", block.len()), block.into_iter().map(keep).collect());
        }
    }
    let cmds = all_commands();
    for p in &sc.prompts {
        let i = p.start;
        if lo <= i && i <= hi {
            let l = sc.lines[i];
            if let Some(a) = l.find("^ /") {
                let name: String = l[a + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
                if cmds.contains_key(&name.to_lowercase()) {
                    act(&mut acts, format!("Make it the command ^/{name}"), vec![json!({"range": rng(i, u16len(&l[..a]), i, u16len(&l[..a + 3])), "newText": "^/"})]);
                }
            }
        }
    }
    for c in &sc.commands {
        if lo <= c.line && c.line <= hi && !cmds.contains_key(&c.name) {
            if let Some(near) = diff::closest(&c.name, cmds.keys().map(String::as_str)) {
                let l = sc.lines[c.line];
                let a = c.lead.len() + 2;
                act(&mut acts, format!("Change to ^/{near}"), vec![json!({"range": rng(c.line, u16len(&l[..a]), c.line, u16len(&l[..(a + c.name.len()).min(l.len())])), "newText": near})]);
            }
        }
    }
    json!(acts)
}
