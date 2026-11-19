//! Open editor buffers. Ambition's language server (`lsp.rs`), started by
//! your editor, keeps a line open to the running Ambition and says which
//! files it has open. For a press in such a file, Ambition reads the buffer
//! as it is, unsaved, from the language server, and writes back only the
//! changed ranges with `workspace/applyEdit`: the buffer changes the way
//! typing would, unsaved, its cursor and the rest left alone, and the
//! editor's own undo takes it back. A file no editor has open is read and
//! written on disk.
//!
//! The line is the inbox's: `{"op": "editor", "client": "Zed"}` turns a
//! connection into it. Then, one line of JSON each way:
//!
//! ```text
//! server → Ambition   {"open": "/abs/a.md"}   {"close": "/abs/a.md"}
//! Ambition → server   {"id": 3, "read": "/abs/a.md"}
//!                     {"id": 4, "edit": "/abs/a.md", "old": "…", "edits": [TextEdit], "mark": true}
//!                     {"id": 5, "show": "/abs/a.md", "text": "…", "span": [a, b]}
//! server → Ambition   {"id": 4, "ok": true, "text": "…"}
//! ```

use crate::field::{self, Surface};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Editor {
    pub client: String,
    /// Whether the editor shows a range it is asked to (window/showDocument).
    shows: bool,
    out: Mutex<TcpStream>,
    open: Mutex<BTreeSet<PathBuf>>,
    pending: Mutex<HashMap<u64, Sender<Value>>>,
    n: AtomicU64,
    alive: AtomicBool,
}

impl Editor {
    fn call(&self, mut req: Value, wait: Duration) -> Result<Value, String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(format!("{} closed its line to Ambition.", self.client));
        }
        let id = self.n.fetch_add(1, Ordering::SeqCst) + 1;
        req["id"] = json!(id);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        let sent = writeln!(self.out.lock().unwrap(), "{req}");
        if sent.is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("{} is not answering.", self.client));
        }
        let r = rx.recv_timeout(wait);
        self.pending.lock().unwrap().remove(&id);
        r.map_err(|_| format!("{} did not answer in time.", self.client))
    }
}

/// The editors whose language servers are connected.
#[derive(Default)]
pub struct Editors(Mutex<Vec<Arc<Editor>>>);

fn canon(p: &str) -> PathBuf {
    let p = PathBuf::from(p);
    std::fs::canonicalize(&p).unwrap_or(p)
}

impl Editors {
    /// A language server's connection, handed over by the inbox.
    pub fn connect(&self, conn: TcpStream, hello: &Value) {
        let Ok(read) = conn.try_clone() else { return };
        let mut out = conn;
        if writeln!(out, "{}", json!({"ok": true})).is_err() {
            return;
        }
        let ed = Arc::new(Editor {
            client: hello["client"].as_str().filter(|c| !c.is_empty()).unwrap_or("your editor").to_string(),
            shows: hello["shows"].as_bool() == Some(true),
            out: Mutex::new(out),
            open: Mutex::new(BTreeSet::new()),
            pending: Mutex::new(HashMap::new()),
            n: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        });
        self.0.lock().unwrap().push(ed.clone());
        crate::app::log(&format!("{}'s language server is connected{}", ed.client,
            if hello["versioned"].as_bool() == Some(true) { " (its edits are tied to a version of the text)" } else { "" }));
        for line in BufReader::new(read).lines() {
            let Ok(line) = line else { break };
            let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            if let Some(p) = v["open"].as_str() {
                ed.open.lock().unwrap().insert(canon(p));
            } else if let Some(p) = v["close"].as_str() {
                ed.open.lock().unwrap().remove(&canon(p));
            } else if let Some(id) = v["id"].as_u64() {
                if let Some(tx) = ed.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(v);
                }
            }
        }
        ed.alive.store(false, Ordering::SeqCst);
        ed.pending.lock().unwrap().clear();
        self.0.lock().unwrap().retain(|e| !Arc::ptr_eq(e, &ed));
    }

    /// The open buffer of a file, if an editor has it open.
    pub fn buffer(&self, path: &Path) -> Option<Buffer> {
        let eds = self.0.lock().unwrap();
        let ed = eds.iter().rev().find(|e| e.alive.load(Ordering::SeqCst) && e.open.lock().unwrap().contains(path))?;
        Some(Buffer { ed: ed.clone(), path: path.to_path_buf() })
    }
}

/// A file's open buffer.
pub struct Buffer {
    ed: Arc<Editor>,
    path: PathBuf,
}

impl Buffer {
    pub fn clone_of(b: &Buffer) -> Buffer {
        Buffer { ed: b.ed.clone(), path: b.path.clone() }
    }

    pub fn client(&self) -> &str {
        &self.ed.client
    }

    fn ask(&self, req: Value) -> Result<Value, String> {
        let r = self.ed.call(req, Duration::from_secs(10))?;
        if r["ok"].as_bool() == Some(true) {
            Ok(r)
        } else {
            Err(r["text"].as_str().unwrap_or("the editor said no").to_string())
        }
    }
}

/// (line, UTF-16 column) of each UTF-16 offset's position in `text`.
fn position(text16: &[u16], at: usize) -> (usize, usize) {
    let before = &text16[..at.min(text16.len())];
    let line = before.iter().filter(|&&u| u == b'\n' as u16).count();
    let col = before.iter().rev().take_while(|&&u| u != b'\n' as u16).count();
    (line, col)
}

fn lsp_range(text16: &[u16], a: usize, b: usize) -> Value {
    let (l1, c1) = position(text16, a);
    let (l2, c2) = position(text16, b);
    json!({"start": {"line": l1, "character": c1}, "end": {"line": l2, "character": c2}})
}

/// Words, runs of blanks, and single other characters: what an edit is cut
/// to, so a fixed typo is one small edit and not a rewritten line.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let kind = |c: char| if c.is_alphanumeric() || c == '\'' { 0 } else if c.is_whitespace() && c != '\n' { 1 } else { 2 };
    let mut prev: Option<u8> = None;
    for (i, c) in s.char_indices() {
        let k = kind(c);
        if prev.is_some() && (prev != Some(k) || k == 2) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(k);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The edits that turn `old` into `new`, as LSP TextEdits on `old`: the
/// changed lines, cut down to the words that change.
pub fn edits(old: &str, new: &str) -> Vec<Value> {
    let o16: Vec<u16> = old.encode_utf16().collect();
    let o_units: Vec<u16> = o16.clone();
    let mut out = Vec::new();
    for h in field::hunks(old, new) {
        let span = String::from_utf16_lossy(&o_units[h.at..h.at + h.len]);
        let (a, b) = (tokens(&span), tokens(&h.text));
        // UTF-16 starts of each token of the old span.
        let mut starts = Vec::with_capacity(a.len() + 1);
        let mut at = h.at;
        for t in &a {
            starts.push(at);
            at += field::u16len(t);
        }
        starts.push(at);
        for o in crate::diff::ops(&a, &b).into_iter().filter(|o| o.tag != crate::diff::Tag::Equal) {
            out.push(json!({"range": lsp_range(&o16, starts[o.i1], starts[o.i2]), "newText": b[o.j1..o.j2].concat()}));
        }
    }
    out
}

impl Surface for Buffer {
    fn read(&self) -> Result<String, String> {
        Ok(self.ask(json!({"read": self.path}))?["text"].as_str().unwrap_or("").to_string())
    }

    fn write(&self, old: &str, new: &str, mark: bool) -> Result<String, String> {
        let span = field::changed(old, new);
        let n16: Vec<u16> = new.encode_utf16().collect();
        let r = self.ask(json!({"edit": self.path, "old": old, "edits": edits(old, new), "mark": mark,
            "span": lsp_range(&n16, span.0, span.1)}))?;
        Ok(r["text"].as_str().unwrap_or(new).to_string())
    }

    fn highlight(&self, text: &str, span: (usize, usize)) -> Result<(), String> {
        let t16: Vec<u16> = text.encode_utf16().collect();
        self.ask(json!({"show": self.path, "text": text, "span": lsp_range(&t16, span.0, span.1)})).map(|_| ())
    }

    fn can_highlight(&self) -> bool {
        self.ed.shows
    }

    fn window_text(&self) -> Result<String, String> {
        Err(format!("{} is an editor buffer: read the file's copy instead.", self.path.display()))
    }

    fn look(&self, _out: &Path) -> Result<(), String> {
        Err("`ambition look` works in a text field's turn, not an editor's.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_touch_only_what_changes() {
        let old = "A long paragraph that rambles on.\n^ ◌ p1 · working\nThe end.\n";
        let new = "A short paragraph.\nThe end.\n";
        let e = edits(old, new);
        assert_eq!(apply(old, &e), new);
        let e = edits("the quik fox\n^ ◌ p1 · w\n", "the quick fox\n");
        assert_eq!(e[0], json!({"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 8}}, "newText": "quick"}));
        let e = edits("one\ntwo\n", "one\ntwo\nthree\n");
        assert_eq!(e[0]["range"], json!({"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}}));
        assert_eq!(e[0]["newText"], "three\n");
        let e = edits("café ☕ x\n", "café ☕ y\n");
        assert_eq!(e[0]["range"]["start"]["character"], 7);
    }

    /// The edits applied as an editor would, for the test.
    fn apply(text: &str, edits: &[Value]) -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        let at = |p: &Value| -> usize {
            let (l, c) = (p["line"].as_u64().unwrap() as usize, p["character"].as_u64().unwrap() as usize);
            let before: usize = lines.iter().take(l).map(|x| x.len() + 1).sum();
            let line = lines.get(l).copied().unwrap_or("");
            let b = line.char_indices().scan(0, |n, (i, ch)| { let r = (i, *n); *n += ch.len_utf16(); Some(r) })
                .find(|&(_, n)| n >= c).map(|(i, _)| i).unwrap_or(line.len());
            before + b
        };
        let mut spans: Vec<(usize, usize, String)> =
            edits.iter().map(|e| (at(&e["range"]["start"]), at(&e["range"]["end"]), e["newText"].as_str().unwrap().into())).collect();
        spans.sort_by_key(|x| std::cmp::Reverse(x.0));
        let mut out = text.to_string();
        for (a, b, t) in spans {
            out.replace_range(a..b, &t);
        }
        out
    }
}
