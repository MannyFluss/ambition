//! `ambition lsp`: the language server. Any editor that speaks LSP can start
//! it. It acts only on files inside a folder Ambition watches (one with a
//! `.ambition/` folder above the file), knows what the buffer says as you
//! type, and reads what the app writes in `.ambition/`: `state.json` (the run
//! going now), `models.json` and `thread.json`. It never writes a file and
//! never calls a model.
//!
//! - Colors (semantic tokens): `^` prompts, `^/` commands, `^>` replies, and
//!   status lines.
//! - Completion: command names after `^/`, models after `^/model `, efforts
//!   after a model.
//! - Hover: what a command does; on a prompt, what is around its caret and
//!   what the agent will be sent.
//! - Highlight: with the cursor on a `^` line, the paragraphs just above and
//!   below it light up, before you save: what the agent is told it is near.
//! - Diagnostics: an unknown command, `^ /x` with a space, a prompt waiting
//!   for a run, a running prompt's state, a status line with no run.
//! - Inlay hints: what is around each prompt, what each command does, how
//!   long the agent has worked.
//! - Code actions: keep a reply (drop its `^>`), make `^ /x` a command, fix a
//!   misspelt command.

use crate::carets as C;
use crate::commands as Cm;
use crate::diff;
use crate::folder::DOT;
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
    rel: String,
    root: PathBuf,
}

struct Server {
    out: Mutex<std::io::Stdout>,
    docs: Mutex<HashMap<String, Doc>>,
    inlay_refresh: Mutex<bool>,
    n: Mutex<u64>,
}

pub fn main() {
    let s = Arc::new(Server { out: Mutex::new(std::io::stdout()), docs: Mutex::new(HashMap::new()), inlay_refresh: Mutex::new(false), n: Mutex::new(0) });
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    while let Some(msg) = read_message(&mut input) {
        let method = msg["method"].as_str().unwrap_or("").to_string();
        if method == "exit" {
            break;
        }
        if method == "initialized" {
            let s2 = s.clone();
            std::thread::spawn(move || s2.watch_state());
        }
        s.handle(&msg);
    }
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

/// The watched folder a file is in: the nearest folder above it with a
/// `.ambition/` folder. (root, path relative to it)
fn locate(path: &Path) -> Option<(PathBuf, String)> {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut dir = p.parent();
    while let Some(d) = dir {
        if d.join(DOT).is_dir() {
            let rel = p.strip_prefix(d).ok()?.to_string_lossy().replace('\\', "/");
            if rel.split('/').any(|x| x.starts_with('.')) {
                return None;
            }
            return Some((d.to_path_buf(), rel));
        }
        dir = d.parent();
    }
    None
}

fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut p = crate::pkce::urldecode(&rest.replace('+', "%2B"));
    if cfg!(windows) && p.starts_with('/') && p.chars().nth(2) == Some(':') {
        p.remove(0);
    }
    Some(PathBuf::from(p))
}

fn load(root: &Path, name: &str) -> Value {
    crate::app::load(&root.join(DOT).join(name))
}

/// What the app is doing for this folder: (is it running, the run going now).
fn run_state(root: &Path) -> (bool, Option<Value>) {
    let d = load(root, "state.json");
    let alive = d["pid"].as_u64().is_some_and(|pid| alive(pid as u32))
        && root.join(DOT).join("lock").exists();
    let run = if alive { d.get("run").filter(|r| !r.is_null()).cloned() } else { None };
    (alive, run)
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

/// What a caret at line `start` acts on: the paragraph (or last list item)
/// just above, as the app reads it once the caret lines are gone; None for
/// the whole file.
fn target(rel: &str, sc: &Scan, start: usize) -> Option<(usize, usize)> {
    C::above(rel, &sc.lines, start, &sc.caret)
}

/// 0-based (start, end) lines, end exclusive.
type Span = (usize, usize);

/// The text around a prompt's caret, as the agent is told of it: the
/// paragraph (or last list item) just above, and the paragraph just below.
fn around(rel: &str, sc: &Scan, p: &C::Prompt) -> (Option<Span>, Option<Span>) {
    (target(rel, sc, p.start), C::below(&sc.lines, p.end - 1, &sc.caret))
}

/// "↑ lines 3-4 · ↓ line 6", or "the whole file" at the top.
fn around_label(rel: &str, sc: &Scan, p: &C::Prompt) -> String {
    match around(rel, sc, p) {
        (None, _) => "→ the whole file".into(),
        (Some(up), None) => format!("↑ {}", where_(rel, sc, Some(up))),
        (Some(up), Some(down)) => format!("↑ {} · ↓ {}", where_(rel, sc, Some(up)), C::span(down.0, down.1)),
    }
}

fn whole(sc: &Scan) -> Option<(usize, usize)> {
    let rows: Vec<usize> = (0..sc.lines.len()).filter(|&i| !sc.lines[i].trim().is_empty() && !sc.caret.contains(&i)).collect();
    Some((*rows.first()?, rows.last()? + 1))
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

/// {name: (description, where it comes from)} of every command for a folder.
fn all_commands(root: &Path) -> BTreeMap<String, (String, String)> {
    let mut out: BTreeMap<String, (String, String)> = Cm::BUILTINS.iter().map(|(n, d)| (n.to_string(), (d.to_string(), "built in".to_string()))).collect();
    for (n, p) in Cm::find(root) {
        out.entry(n).or_insert_with(|| {
            let d = Cm::describe(&p);
            let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            (if d.is_empty() { file.clone() } else { d }, format!("runs .ambition/commands/{file}"))
        });
    }
    out
}

fn models(root: &Path) -> Vec<Value> {
    load(root, "models.json")["models"].as_array().cloned().unwrap_or_default()
}

fn thread_line(root: &Path) -> String {
    let t = load(root, "thread.json");
    let mut bits = Vec::new();
    if let Some(m) = t["model"].as_str() {
        bits.push(format!("model: {m}"));
    }
    match t["link"].as_str().filter(|l| !l.is_empty()) {
        Some(l) => bits.push(format!("[this folder's thread]({l})")),
        None => bits.push("no thread yet: the next prompt starts one".into()),
    }
    bits.join(" · ")
}

fn since(run: &Value) -> u64 {
    (crate::util::epoch() - run["since"].as_f64().unwrap_or_else(crate::util::epoch)).max(0.0) as u64
}

fn ids(run: &Value) -> Vec<String> {
    run["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
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
                    path_of(&uri).and_then(|p| locate(&p))
                } else {
                    self.docs.lock().unwrap().get(&uri).map(|d| (d.root.clone(), d.rel.clone()))
                };
                let Some((root, rel)) = place else { return };
                self.docs.lock().unwrap().insert(uri.clone(), Doc { text, rel, root });
                self.publish(&uri);
            }
            "textDocument/didClose" => {
                self.docs.lock().unwrap().remove(p["textDocument"]["uri"].as_str().unwrap_or(""));
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
                    "textDocument/completion" => json!({"isIncomplete": false, "items": completion(&d.root, &d.rel, &d.text, pos)}),
                    "textDocument/hover" => hover(&d.root, &d.rel, &d.text, pos.0),
                    "textDocument/documentHighlight" => highlights(&d.root, &d.rel, &d.text, pos.0),
                    "textDocument/inlayHint" => inlay_hints(&d.root, &d.rel, &d.text, range),
                    "textDocument/codeAction" => code_actions(&d.root, uri, &d.rel, &d.text, range),
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
            diagnostics(&d.root, &d.rel, &d.text)
        };
        self.send(json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": diags}}));
    }

    /// Re-publish diagnostics when an app's state changes, and every few
    /// seconds while a run goes (for its clock). Otherwise it sleeps.
    fn watch_state(self: Arc<Self>) {
        use notify::Watcher;
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watching: BTreeSet<PathBuf> = BTreeSet::new();
        let tx2 = tx.clone();
        let mut watcher = notify::recommended_watcher(move |_e: notify::Result<notify::Event>| {
            let _ = tx2.send(());
        })
        .ok();
        loop {
            let roots: BTreeSet<PathBuf> = self.docs.lock().unwrap().values().map(|d| d.root.clone()).collect();
            if let Some(w) = watcher.as_mut() {
                for r in roots.difference(&watching.clone()) {
                    if w.watch(&r.join(DOT).join("state.json"), notify::RecursiveMode::NonRecursive).is_ok()
                        || w.watch(&r.join(DOT), notify::RecursiveMode::NonRecursive).is_ok()
                    {
                        watching.insert(r.clone());
                    }
                }
            }
            let running = roots.iter().any(|r| run_state(r).1.is_some());
            let wait = if running { Duration::from_secs(3) } else if watcher.is_some() { Duration::from_secs(30) } else { Duration::from_secs(2) };
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

fn completion(root: &Path, rel: &str, text: &str, pos: (usize, u32)) -> Vec<Value> {
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
        return all_commands(root).into_iter().map(|(n, (d, how))| {
            let builtin = Cm::is_builtin(&n);
            json!({"label": n, "kind": if builtin { 14 } else { 3 }, "detail": crate::util::cut(&d, 80),
                "documentation": {"kind": "markdown", "value": format!("`^/{n}` — {d}\n\n{how}")},
                "sortText": format!("{}{n}", if builtin { "0" } else { "1" }), "textEdit": edit(col, &n)})
        }).collect();
    }
    if parts.first() != Some(&"model") {
        return vec![];
    }
    let cur = load(root, "thread.json")["model"].as_str().unwrap_or("").to_string();
    let ms = models(root);
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

fn prompt_card(root: &Path, rel: &str, sc: &Scan, p: &C::Prompt, run: &Option<Value>) -> String {
    let (up, down) = around(rel, sc, p);
    let real: Vec<usize> = (0..sc.lines.len()).filter(|k| !sc.caret.contains(k)).collect();
    let chars: usize = real.iter().map(|&k| sc.lines[k].len() + 1).sum();
    let ctx = if real.len() <= 78 && chars <= 5800 { "this whole file" } else { "the lines around the caret (the file is long)" };
    let mut body = vec!["**`^` prompt** — on save it goes to this folder's thread.".to_string(), String::new()];
    if let Some(name) = p.text.trim_start().strip_prefix('/') {
        let name = name.split(' ').next().unwrap_or("");
        body.push(format!("This starts with `^ /`: the space makes it a prompt. For the command, write `^/{name}`."));
        body.push(String::new());
    }
    let quote = |(s, e): (usize, usize)| {
        let mut q: Vec<String> = sc.lines[s..e].iter().take(3).map(|q| format!("> {}", q.replace('*', "\\*").replace('_', "\\_"))).collect();
        if e - s > 3 {
            q.push("> …".into());
        }
        q.join("  \n")
    };
    match (up, down) {
        (None, _) => body.push("**Around the caret:** the whole file (the prompt is on its first line).".into()),
        (Some(u), d) => {
            body.push(format!("**Around the caret:** above, {}{}. Your words decide which it acts on.", where_(rel, sc, Some(u)),
                d.map(|d| format!("; below, {}", C::span(d.0, d.1))).unwrap_or_default()));
            body.push(String::new());
            body.push(quote(u));
            if let Some(d) = d {
                body.push(String::new());
                body.push("below:".into());
                body.push(quote(d));
            }
            body.push(String::new());
        }
    }
    body.push(format!("**The agent is sent:** your words; where the caret is and the paragraphs above and below it, by line numbers; {ctx}, as it is on disk; and what you typed in this folder since its last turn. It remembers the thread, and can read anything else itself."));
    body.push(String::new());
    body.push(thread_line(root));
    if let Some(r) = run {
        body.push(String::new());
        body.push(format!("The agent is working on {} now: this prompt runs when that turn ends.", ids(r).join(", ")));
    }
    body.join("\n")
}

fn hover(root: &Path, rel: &str, text: &str, i: usize) -> Value {
    let sc = scan(rel, text);
    if i >= sc.lines.len() {
        return Value::Null;
    }
    let (alive, run) = run_state(root);
    if let Some(c) = sc.commands.iter().find(|c| c.line == i) {
        let cmds = all_commands(root);
        let Some((d, how)) = cmds.get(&c.name) else {
            let near = diff::closest(&c.name, cmds.keys().map(String::as_str));
            return md(format!("**`^/{}`** is no command.{}\n\n`^/help` lists them.", c.name,
                near.map(|n| format!(" Did you mean `^/{n}`?")).unwrap_or_default()));
        };
        let mut body = format!("**`^/{}`** — {d}\n\n", c.name);
        if Cm::is_builtin(&c.name) {
            body.push_str("Built in. Runs the moment you save, with no model.");
        } else {
            let t = target(rel, &sc, i);
            body.push_str(&format!("{} the moment you save, with no model. It reads {} on stdin, and what it prints becomes the reply.",
                capital(how), t.map(|_| where_(rel, &sc, t)).unwrap_or_else(|| "nothing".into())));
        }
        if c.name == "model" || c.name == "thread" {
            body.push_str(&format!("\n\n{}", thread_line(root)));
        }
        if c.name == "model" {
            let ms = models(root);
            if !ms.is_empty() {
                let rows: Vec<String> = ms.iter().map(|m| format!("- {} (`{}`)", m["label"].as_str().unwrap_or(""), m["model"].as_str().unwrap_or(""))).collect();
                body.push_str(&format!("\n\n{}", rows.join("\n")));
            }
        }
        return md(body);
    }
    if let Some(pid) = sc.status.get(&i) {
        if let Some(r) = run.as_ref().filter(|r| ids(r).contains(pid)) {
            let thread = r["thread"].as_str().filter(|t| !t.is_empty()).map(|t| format!(" [Watch it in the thread]({t}).")).unwrap_or_default();
            return md(format!("**{pid}** is with the agent ({}), {} s so far.{thread}\n\n`^/stop` stops it. Ambition removes this line when the turn ends.",
                r["model"].as_str().unwrap_or("the engine"), since(r)));
        }
        return md(format!("**{pid}**: no run is behind this status line.{}",
            if alive { "" } else { " Ambition is not running for this folder: start it with `ambition <folder>`." }));
    }
    if sc.replies.contains(&i) {
        return md("A reply. Your next save clears it. To keep the text, delete its `^>` (the code action **Keep this reply** does that).".into());
    }
    if let Some(p) = sc.prompts.iter().find(|p| p.start <= i && i < p.end) {
        return md(prompt_card(root, rel, &sc, p, &run));
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

fn highlights(root: &Path, rel: &str, text: &str, i: usize) -> Value {
    let _ = root;
    let sc = scan(rel, text);
    let mut t = None;
    if let Some(p) = sc.prompts.iter().find(|p| p.start <= i && i < p.end) {
        return match around(rel, &sc, p) {
            (None, _) => whole(&sc).map(|(s, e)| json!([{"range": line_range(&sc.lines, s, e), "kind": 1}])).unwrap_or(json!([])),
            (Some(u), d) => json!([u].into_iter().chain(d).map(|(s, e)| json!({"range": line_range(&sc.lines, s, e), "kind": 1})).collect::<Vec<_>>()),
        };
    }
    if let Some(c) = sc.commands.iter().find(|c| c.line == i) {
        if !Cm::is_builtin(&c.name) {
            t = target(rel, &sc, i);
        }
    }
    if sc.status.contains_key(&i) {
        t = target(rel, &sc, i);
    }
    match t {
        Some((s, e)) => json!([{"range": line_range(&sc.lines, s, e), "kind": 1}]),
        None => json!([]),
    }
}

fn diagnostics(root: &Path, rel: &str, text: &str) -> Vec<Value> {
    let sc = scan(rel, text);
    let (alive, run) = run_state(root);
    let cmds = all_commands(root);
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
    for p in &sc.prompts {
        if let Some(name) = p.text.trim_start().strip_prefix('/') {
            let name = name.split(' ').next().unwrap_or("").to_lowercase();
            if cmds.contains_key(&name) && sc.lines[p.start].trim_start().starts_with("^ /") {
                out.push(json!({"range": line_range(&sc.lines, p.start, p.start + 1), "severity": 3, "source": "Ambition", "code": "spaced-command",
                    "message": format!("the space after ^ makes this a prompt for the agent. For the command, write ^/{name}.")}));
            }
        }
        if let Some(r) = &run {
            out.push(json!({"range": line_range(&sc.lines, p.start, p.end), "severity": 4, "source": "Ambition",
                "message": format!("the agent is working on {}: this prompt runs when that turn ends.", ids(r).join(", "))}));
        }
    }
    for (&i, pid) in &sc.status {
        let l = sc.lines[i];
        match &run {
            Some(r) if ids(r).contains(pid) => {
                let what = l.split_once('·').map(|x| x.1.trim()).unwrap_or("working");
                let t = target(rel, &sc, i);
                let thread = r["thread"].as_str().filter(|t| !t.is_empty()).map(|t| format!(" · {t}")).unwrap_or_default();
                out.push(json!({"range": t.map(|(s, e)| line_range(&sc.lines, s, e)).unwrap_or_else(|| line_range(&sc.lines, i, i + 1)),
                    "severity": 3, "source": "Ambition", "code": "running", "message": format!("{pid} · {what} · {} s{thread}", since(r))}));
            }
            Some(_) => {}
            None => out.push(json!({"range": line_range(&sc.lines, i, i + 1), "severity": 2, "source": "Ambition", "code": "stale",
                "message": format!("{pid}: no run is behind this status line{}", if alive { "." } else { ": Ambition is not running for this folder." })})),
        }
    }
    out
}

fn inlay_hints(root: &Path, rel: &str, text: &str, (lo, hi): (usize, usize)) -> Value {
    let sc = scan(rel, text);
    let cmds = all_commands(root);
    let mut out = Vec::new();
    for p in &sc.prompts {
        let last = p.end - 1;
        if lo <= last && last <= hi {
            out.push(json!({"position": {"line": last, "character": u16len(sc.lines[last])}, "label": around_label(rel, &sc, p), "paddingLeft": true}));
        }
    }
    for c in &sc.commands {
        if lo <= c.line && c.line <= hi {
            if let Some((d, _)) = cmds.get(&c.name) {
                out.push(json!({"position": {"line": c.line, "character": u16len(sc.lines[c.line])}, "label": format!("→ {}", crate::util::cut(d, 60)), "paddingLeft": true}));
            }
        }
    }
    let (alive, run) = run_state(root);
    for (&i, pid) in &sc.status {
        if i < lo || i > hi {
            continue;
        }
        let label = match &run {
            Some(r) if ids(r).contains(pid) => format!("⏱ {} s · ^/stop stops it", since(r)),
            Some(_) => continue,
            None => format!("no run behind this line{}", if alive { "" } else { ": Ambition is not running" }),
        };
        out.push(json!({"position": {"line": i, "character": u16len(sc.lines[i])}, "label": label, "paddingLeft": true}));
    }
    json!(out)
}

fn code_actions(root: &Path, uri: &str, rel: &str, text: &str, (lo, hi): (usize, usize)) -> Value {
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
    let cmds = all_commands(root);
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
