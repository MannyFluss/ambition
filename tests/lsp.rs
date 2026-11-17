//! The language server over stdio, as an editor talks to it.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

struct Lsp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    n: u64,
    notes: Vec<Value>,
}

impl Lsp {
    fn start() -> Lsp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ambition")).arg("lsp")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Lsp { child, out, n: 0, notes: Vec::new() }
    }

    fn send(&mut self, v: Value) {
        let body = v.to_string();
        let stdin = self.child.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            self.out.read_line(&mut line).unwrap();
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        self.out.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.n += 1;
        let id = self.n;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m["id"] == json!(id) {
                return m["result"].clone();
            }
            self.notes.push(m);
        }
    }

    fn tell(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

fn uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

#[test]
fn the_language_server() {
    let root = std::env::temp_dir().join(format!("ambition-lsp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("notes");
    std::fs::create_dir_all(dir.join(".ambition/commands")).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    std::fs::write(dir.join(".ambition/commands/count.sh"), "#!/bin/sh\n# Count the words above.\nwc -w\n").unwrap();
    std::fs::write(dir.join(".ambition/models.json"), json!({"models": [
        {"instance": "claudeAgent", "provider": "Claude", "model": "claude-sonnet-5-5", "label": "Claude Sonnet 5.5", "effortId": "effort", "efforts": ["low", "high"]},
        {"instance": "codex", "provider": "Codex", "model": "gpt-5.5", "label": "GPT-5.5", "effortId": "reasoningEffort", "efforts": []}]}).to_string()).unwrap();
    std::fs::write(dir.join(".ambition/thread.json"), json!({"model": "Claude Sonnet 5.5", "link": "t3://thread/x"}).to_string()).unwrap();
    let text = "Intro paragraph.\n\nSome text here\nthat spans two lines.\n^ make this shorter\n\n- one\n- two\n^ expand this\n\n^/help\n^/modle\n^ /help\n^> a reply\n^> second line\n^ ◌ p9 · stale · working\n^/count\n";
    let file = dir.join("notes.md");
    std::fs::write(&file, text).unwrap();
    let outside = root.join("elsewhere.md");
    std::fs::write(&outside, "^ not watched\n").unwrap();

    let mut l = Lsp::start();
    let init = l.ask("initialize", json!({"capabilities": {}}));
    assert_eq!(init["capabilities"]["positionEncoding"], "utf-16");
    l.tell("initialized", json!({}));
    l.tell("textDocument/didOpen", json!({"textDocument": {"uri": uri(&file), "languageId": "markdown", "version": 1, "text": text}}));
    l.tell("textDocument/didOpen", json!({"textDocument": {"uri": uri(&outside), "languageId": "markdown", "version": 1, "text": "^ not watched\n"}}));
    let doc = json!({"uri": uri(&file)});

    let tokens = l.ask("textDocument/semanticTokens/full", json!({"textDocument": doc}));
    let data: Vec<u64> = tokens["data"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert!(data.len() >= 5 * 10, "colors for every caret line: {data:?}");
    assert_eq!(&data[..5], &[4, 0, 1, 0, 0], "the first token is the ^ of the first prompt, as a keyword");

    let names: Vec<String> = l.ask("textDocument/completion", json!({"textDocument": doc, "position": {"line": 10, "character": 2}}))["items"]
        .as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"help".to_string()) && names.contains(&"model".to_string()) && names.contains(&"count".to_string()), "{names:?}");

    let hov = l.ask("textDocument/hover", json!({"textDocument": doc, "position": {"line": 4, "character": 3}}));
    let card = hov["contents"]["value"].as_str().unwrap();
    assert!(card.contains("**Around the caret:** above, lines 3-4; below, lines 7-8.") && card.contains("> Some text here"), "{card}");
    let hov = l.ask("textDocument/hover", json!({"textDocument": doc, "position": {"line": 16, "character": 2}}));
    assert!(hov["contents"]["value"].as_str().unwrap().contains("Count the words above."), "{hov}");

    let hl = l.ask("textDocument/documentHighlight", json!({"textDocument": doc, "position": {"line": 4, "character": 1}}));
    assert_eq!((hl[0]["range"]["start"]["line"].clone(), hl[0]["range"]["end"]["line"].clone()), (json!(2), json!(3)), "above: {hl}");
    assert_eq!((hl[1]["range"]["start"]["line"].clone(), hl[1]["range"]["end"]["line"].clone()), (json!(6), json!(7)), "below: {hl}");
    let hl = l.ask("textDocument/documentHighlight", json!({"textDocument": doc, "position": {"line": 8, "character": 1}}));
    assert_eq!((hl[0]["range"]["start"]["line"].clone(), hl[0]["range"]["end"]["line"].clone()), (json!(7), json!(7)), "under a list, the last item");

    let hints = l.ask("textDocument/inlayHint", json!({"textDocument": doc, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 0}}}));
    let labels: Vec<String> = hints.as_array().unwrap().iter().map(|h| h["label"].as_str().unwrap().to_string()).collect();
    assert!(labels.contains(&"↑ lines 3-4 · ↓ lines 7-8".to_string()) && labels.contains(&"↑ line 8 (the last list item)".to_string())
        && labels.contains(&"→ list the commands".to_string()), "{labels:?}");

    let acts = |l: &mut Lsp, line: usize| -> Vec<String> {
        l.ask("textDocument/codeAction", json!({"textDocument": {"uri": uri(&file)}, "range": {"start": {"line": line, "character": 0}, "end": {"line": line, "character": 0}}, "context": {"diagnostics": []}}))
            .as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap().to_string()).collect()
    };
    let a = acts(&mut l, 13);
    assert!(a.contains(&"Keep this reply (drop its ^>)".to_string()) && a.contains(&"Keep all 2 lines of this reply".to_string()), "{a:?}");
    assert!(acts(&mut l, 11).contains(&"Change to ^/model".to_string()));
    assert!(acts(&mut l, 12).contains(&"Make it the command ^/help".to_string()));

    let models: Vec<String> = {
        let t2 = format!("{text}^/model \n");
        l.tell("textDocument/didChange", json!({"textDocument": {"uri": uri(&file), "version": 2}, "contentChanges": [{"text": t2}]}));
        l.ask("textDocument/completion", json!({"textDocument": doc, "position": {"line": 17, "character": 8}}))["items"]
            .as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(models, vec!["Claude Sonnet 5.5", "GPT-5.5"]);

    // Diagnostics came as notifications along the way.
    let diags: Vec<Value> = l.notes.iter().filter(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == json!(uri(&file)))
        .flat_map(|n| n["params"]["diagnostics"].as_array().cloned().unwrap_or_default()).collect();
    let codes: Vec<&str> = diags.iter().filter_map(|d| d["code"].as_str()).collect();
    assert!(codes.contains(&"no-command") && codes.contains(&"spaced-command") && codes.contains(&"stale"), "{codes:?}");
    assert!(!l.notes.iter().any(|n| n["params"]["uri"] == json!(uri(&outside))), "a file outside a watched folder is left alone");
    let empty = l.ask("textDocument/semanticTokens/full", json!({"textDocument": {"uri": uri(&outside)}}));
    assert_eq!(empty["data"], json!([]));

    let _ = l.ask("shutdown", Value::Null);
    l.tell("exit", Value::Null);
    let _ = l.child.wait();
    let _ = std::fs::remove_dir_all(&root);
}
