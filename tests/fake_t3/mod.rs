//! A fake T3 Code server for end-to-end tests: the sign-in endpoints, the MCP
//! tool calls Ambition makes, and a scripted agent. Tool answers come back as
//! a chunked event stream, as the real server's can.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// What the scripted agent does on one turn. It gets the turn and returns
/// what it said and how the turn ended.
pub type Behavior = Box<dyn FnOnce(&Turn) -> Outcome + Send>;

pub struct Turn {
    pub thread: String,
    pub message: String,
    pub folder: PathBuf,
}

pub struct Outcome {
    pub text: String,
    pub status: &'static str,
}

pub fn said(text: &str) -> Outcome {
    Outcome { text: text.to_string(), status: "completed" }
}

#[derive(Default)]
pub struct Run {
    pub id: String,
    pub message: String,
    pub done: bool,
    pub status: String,
    pub text: String,
}

#[derive(Default)]
pub struct Th {
    pub id: String,
    pub project: String,
    pub link: String,
    pub model: Option<Value>,
    pub runs: Vec<Run>,
    pub archived: bool,
}

#[derive(Default)]
pub struct S {
    pub projects: Vec<Value>,
    pub threads: BTreeMap<String, Th>,
    pub launched: Vec<(String, String, Option<Value>)>,
    pub sent: Vec<(String, String)>,
    pub configured: Vec<(String, Value)>,
    pub defaults: Vec<(String, Value)>,
    pub settled: Vec<String>,
    pub interrupted: Vec<String>,
    pub script: VecDeque<Behavior>,
    pub down: bool,
    pub signins: usize,
    pub calls: Vec<String>,
    n: u64,
}

/// The fake's state, shared by its connections, with a signal for "a run ended".
pub type Shared = Arc<(Mutex<S>, Condvar)>;

pub struct Fake {
    pub port: u16,
    pub st: Shared,
    pub home: PathBuf,
}

impl Fake {
    /// Start a fake T3 whose home (runtime file and `t3` command) is `home`.
    pub fn start(home: &Path) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let st: Shared = Arc::new((Mutex::new(S::default()), Condvar::new()));
        let s2 = st.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let s3 = s2.clone();
                std::thread::spawn(move || serve(c, s3));
            }
        });
        std::fs::create_dir_all(home.join("userdata")).unwrap();
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("userdata/server-runtime.json"), json!({"origin": format!("http://127.0.0.1:{port}")}).to_string()).unwrap();
        let cli = home.join("bin/t3");
        std::fs::write(&cli, "#!/bin/sh\ncase \"$1 $2 $3\" in\n  \"auth pairing create\") echo '{\"credential\":\"PAIR\"}' ;;\n  \"auth session list\") echo '[]' ;;\n  \"auth session revoke\") echo '{}' ;;\n  *) echo \"unknown $*\" >&2; exit 1 ;;\nesac\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Fake { port, st, home: home.to_path_buf() }
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, S> {
        self.st.0.lock().unwrap()
    }

    /// Queue what the agent does on its next turn.
    pub fn next(&self, b: impl FnOnce(&Turn) -> Outcome + Send + 'static) {
        self.lock().script.push_back(Box::new(b));
    }

    pub fn messages(&self) -> Vec<String> {
        let s = self.lock();
        let mut all: Vec<(String, String)> = Vec::new();
        for t in s.threads.values() {
            for r in &t.runs {
                all.push((r.id.clone(), r.message.clone()));
            }
        }
        all.sort_by_key(|(id, _)| id.trim_start_matches("run-").parse::<u64>().unwrap_or(0));
        all.into_iter().map(|(_, m)| m).collect()
    }

    pub fn running(&self) -> bool {
        self.lock().threads.values().any(|t| t.runs.iter().any(|r| !r.done))
    }
}

/// A request: its path, its headers (names in lower case), and its body.
type Request = (String, Vec<(String, String)>, String);

fn read_request(c: &mut TcpStream) -> Option<Request> {
    let mut got = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = c.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        got.extend_from_slice(&buf[..n]);
        if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..i]).to_string();
            let mut lines = head.split("\r\n");
            let path = lines.next()?.split_whitespace().nth(1)?.to_string();
            let headers: Vec<(String, String)> =
                lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))).collect();
            let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
            while got.len() < i + 4 + len {
                let n = c.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            let body = String::from_utf8_lossy(&got[i + 4..(i + 4 + len).min(got.len())]).to_string();
            return Some((path, headers, body));
        }
    }
}

fn answer(c: &mut TcpStream, status: &str, ctype: &str, extra: &[(&str, &str)], body: &str, chunked: bool) {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nConnection: close\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    let payload = if chunked {
        head.push_str("Transfer-Encoding: chunked\r\n\r\n");
        let b = body.as_bytes();
        let mid = b.len() / 2;
        let mut out = Vec::new();
        for part in [&b[..mid], &b[mid..]] {
            if !part.is_empty() {
                out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                out.extend_from_slice(part);
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    } else {
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        body.as_bytes().to_vec()
    };
    let mut all = head.into_bytes();
    all.extend_from_slice(&payload);
    let _ = c.write_all(&all);
}

fn serve(mut c: TcpStream, st: Shared) {
    let Some((path, headers, body)) = read_request(&mut c) else { return };
    if st.0.lock().unwrap().down {
        return; // T3 away: the connection just closes
    }
    let header = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone()).unwrap_or_default();
    match path.as_str() {
        "/oauth/mcp/register" => answer(&mut c, "201 Created", "application/json", &[], &json!({"client_id": "client-1"}).to_string(), false),
        "/oauth/mcp/decision" => {
            let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
            let state = v["authorization"]["state"].as_str().unwrap_or("");
            assert_eq!(v["decision"]["access"], "full-access");
            assert_eq!(v["authorization"]["code_challenge_method"], "S256");
            let to = format!("http://127.0.0.1:9/ambition-callback?code=CODE&state={state}");
            answer(&mut c, "200 OK", "application/json", &[], &json!({"redirectTo": to}).to_string(), false);
        }
        "/oauth/mcp/token" => {
            assert!(body.contains("grant_type=authorization_code") && body.contains("code_verifier="));
            st.0.lock().unwrap().signins += 1;
            answer(&mut c, "200 OK", "application/json", &[], &json!({"access_token": "tok", "token_type": "Bearer"}).to_string(), false);
        }
        "/mcp" => {
            if header("authorization") != "Bearer tok" {
                return answer(&mut c, "401 Unauthorized", "application/json", &[], "{}", false);
            }
            let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
            match v["method"].as_str().unwrap_or("") {
                "initialize" => answer(&mut c, "200 OK", "application/json", &[("mcp-session-id", "session-1")],
                    &json!({"jsonrpc": "2.0", "id": v["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}}}).to_string(), false),
                "notifications/initialized" => answer(&mut c, "202 Accepted", "application/json", &[], "", false),
                "tools/call" => {
                    if header("mcp-session-id") != "session-1" {
                        return answer(&mut c, "400 Bad Request", "application/json", &[], "{}", false);
                    }
                    let name = v["params"]["name"].as_str().unwrap_or("").to_string();
                    let args = v["params"]["arguments"].clone();
                    let result = tool(&st, &name, &args);
                    let reply = match result {
                        Ok(r) => json!({"jsonrpc": "2.0", "id": v["id"], "result": {"structuredContent": r, "content": []}}),
                        Err(e) => json!({"jsonrpc": "2.0", "id": v["id"], "result": {"isError": true,
                            "structuredContent": {"_tag": "OrchestratorMcpFailure", "message": e}, "content": []}}),
                    };
                    answer(&mut c, "200 OK", "text/event-stream", &[], &format!("event: message\ndata: {reply}\n\n"), true);
                }
                _ => answer(&mut c, "400 Bad Request", "application/json", &[], "{}", false),
            }
        }
        _ => answer(&mut c, "404 Not Found", "text/plain", &[], "no", false),
    }
}

fn providers(search: Option<&str>) -> Value {
    let effort = |vals: &[&str]| json!([{"id": "effort", "values": vals}]);
    let mut claude = vec![
        json!({"id": "claude-sonnet-5-5", "label": "Claude Sonnet 5.5", "options": effort(&["low", "medium", "high"])}),
        json!({"id": "claude-opus-5-5", "label": "Claude Opus 5.5", "options": effort(&["low", "medium", "high", "max"])}),
        json!({"id": "claude-opus-5", "label": "Claude Opus 5", "options": effort(&["low", "high"])}),
    ];
    let mut codex = vec![json!({"id": "gpt-5.5", "label": "GPT-5.5", "options": [{"id": "reasoningEffort", "values": ["low", "medium", "high", "xhigh"]}]})];
    let hidden = vec![json!({"id": "claude-haiku-4-5", "label": "Claude Haiku 4.5", "options": []})];
    if let Some(q) = search {
        let q = q.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        let fits = |m: &Value| {
            let hay = format!("{} {}", m["id"].as_str().unwrap_or(""), m["label"].as_str().unwrap_or("")).to_lowercase();
            !words.is_empty() && words.iter().all(|w| hay.contains(w))
        };
        claude.extend(hidden);
        claude.retain(fits);
        codex.retain(fits);
    }
    json!({"providers": [
        {"providerInstanceId": "claudeAgent", "displayName": "Claude", "constraints": [], "models": claude},
        {"providerInstanceId": "codex", "displayName": "Codex", "constraints": [], "models": codex},
        {"providerInstanceId": "off", "displayName": "Off", "constraints": ["Disabled in settings"], "models": [{"id": "off-1", "label": "Off 1"}]}
    ]})
}

fn start_run(st: &Shared, tid: &str, message: &str) -> String {
    let (lock, cv) = &**st;
    let mut s = lock.lock().unwrap();
    s.n += 1;
    let run = format!("run-{}", s.n);
    let folder = s.threads.get(tid).and_then(|t| s.projects.iter().find(|p| p["id"] == json!(t.project)))
        .map(|p| PathBuf::from(p["workspaceRoot"].as_str().unwrap_or(""))).unwrap_or_default();
    let th = s.threads.get_mut(tid).unwrap();
    th.runs.push(Run { id: run.clone(), message: message.to_string(), ..Default::default() });
    let behavior: Behavior = s.script.pop_front().unwrap_or_else(|| Box::new(|_| said("")));
    drop(s);
    let turn = Turn { thread: tid.to_string(), message: message.to_string(), folder };
    let st2 = st.clone();
    let run2 = run.clone();
    let tid2 = tid.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        let out = behavior(&turn);
        let (lock, cv) = &*st2;
        let mut s = lock.lock().unwrap();
        if let Some(r) = s.threads.get_mut(&tid2).and_then(|t| t.runs.iter_mut().find(|r| r.id == run2)) {
            if !r.done {
                r.done = true;
                r.status = out.status.to_string();
                r.text = out.text;
            }
        }
        cv.notify_all();
    });
    cv.notify_all();
    run
}

fn tool(st: &Shared, name: &str, a: &Value) -> Result<Value, String> {
    st.0.lock().unwrap().calls.push(name.to_string());
    match name {
        "t3_project_list" => {
            let s = st.0.lock().unwrap();
            Ok(json!({"projects": s.projects, "nextCursor": null}))
        }
        "t3_project_create" => {
            let mut s = st.0.lock().unwrap();
            let p = json!({"id": format!("project-{}", s.projects.len() + 1), "title": a["title"], "workspaceRoot": a["workspaceRoot"],
                "archivedAt": null, "deletedAt": null, "defaultModelSelection": null});
            s.projects.push(p.clone());
            Ok(p)
        }
        "t3_project_update" => {
            let mut s = st.0.lock().unwrap();
            s.defaults.push((a["projectId"].as_str().unwrap_or("").to_string(), a["defaultModelSelection"].clone()));
            for p in s.projects.iter_mut() {
                if p["id"] == a["projectId"] {
                    p["defaultModelSelection"] = a["defaultModelSelection"].clone();
                }
            }
            Ok(json!({"ok": true}))
        }
        "orchestrator_capabilities" => Ok(providers(a["search"].as_str())),
        "t3_thread_launch" => {
            let tid = {
                let mut s = st.0.lock().unwrap();
                let tid = format!("thread-{}", s.threads.len() + 1);
                let model = a.get("modelSelection").cloned().or_else(|| {
                    s.projects.iter().find(|p| p["id"] == a["projectId"]).map(|p| p["defaultModelSelection"].clone()).filter(|v| !v.is_null())
                });
                s.launched.push((a["title"].as_str().unwrap_or("").to_string(), a["message"].as_str().unwrap_or("").to_string(),
                    a.get("modelSelection").cloned()));
                s.threads.insert(tid.clone(), Th { id: tid.clone(), project: a["projectId"].as_str().unwrap_or("").to_string(),
                    link: format!("t3://thread/{tid}"), model, ..Default::default() });
                tid
            };
            let run = start_run(st, &tid, a["message"].as_str().unwrap_or(""));
            Ok(json!({"threadId": tid, "runId": run}))
        }
        "t3_thread_send" => {
            let tid = a["threadId"].as_str().unwrap_or("").to_string();
            {
                let mut s = st.0.lock().unwrap();
                if !s.threads.contains_key(&tid) {
                    return Err("Thread not found".into());
                }
                s.sent.push((tid.clone(), a["message"].as_str().unwrap_or("").to_string()));
            }
            let run = start_run(st, &tid, a["message"].as_str().unwrap_or(""));
            Ok(json!({"runId": run}))
        }
        "t3_thread_wait" => {
            let (lock, cv) = &**st;
            let tid = a["threadId"].as_str().unwrap_or("");
            let rid = a["runId"].as_str().unwrap_or("");
            let end = Instant::now() + Duration::from_millis(a["timeoutMs"].as_u64().unwrap_or(1000));
            let mut s = lock.lock().unwrap();
            loop {
                if let Some(r) = s.threads.get(tid).and_then(|t| t.runs.iter().find(|r| r.id == rid)) {
                    if r.done {
                        return Ok(json!({"status": r.status, "timedOut": false}));
                    }
                } else {
                    return Err("Thread not found".into());
                }
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(json!({"timedOut": true}));
                }
                s = cv.wait_timeout(s, left).unwrap().0;
            }
        }
        "t3_thread_read" => {
            let s = st.0.lock().unwrap();
            let tid = a["threadId"].as_str().unwrap_or("");
            let Some(t) = s.threads.get(tid) else { return Err("Thread is no longer available".into()) };
            let running = t.runs.iter().any(|r| !r.done);
            let items: Vec<Value> = t.runs.iter().filter(|r| r.done && !r.text.is_empty())
                .map(|r| json!({"type": "assistant_message", "runId": r.id, "text": r.text})).collect();
            Ok(json!({"thread": {"id": tid, "link": t.link, "status": if running { "running" } else { "idle" },
                "runCount": t.runs.len(), "pendingRequestCount": 0, "archivedAt": if t.archived { json!("now") } else { json!(null) }},
                "items": items, "hasMore": false}))
        }
        "t3_thread_configure" => {
            let mut s = st.0.lock().unwrap();
            let tid = a["threadId"].as_str().unwrap_or("").to_string();
            if let Some(sel) = a.get("modelSelection") {
                s.configured.push((tid.clone(), sel.clone()));
                if let Some(t) = s.threads.get_mut(&tid) {
                    t.model = Some(sel.clone());
                }
                return Ok(json!({"ok": true}));
            }
            let m = s.threads.get(&tid).ok_or("Thread not found")?.model.clone().unwrap_or(json!(null));
            Ok(json!({"modelSelection": m}))
        }
        "t3_thread_interrupt" => {
            let (lock, cv) = &**st;
            let mut s = lock.lock().unwrap();
            let tid = a["threadId"].as_str().unwrap_or("").to_string();
            s.interrupted.push(tid.clone());
            if let Some(t) = s.threads.get_mut(&tid) {
                for r in t.runs.iter_mut().filter(|r| !r.done) {
                    r.done = true;
                    r.status = "interrupted".into();
                }
            }
            cv.notify_all();
            Ok(json!({"ok": true}))
        }
        "t3_thread_organize" => {
            st.0.lock().unwrap().settled.push(a["threadId"].as_str().unwrap_or("").to_string());
            Ok(json!({"ok": true}))
        }
        other => Err(format!("unknown tool {other}")),
    }
}
