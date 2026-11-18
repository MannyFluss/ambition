//! T3 Code as Ambition's engine: one thread per document, with T3's whole kit.
//!
//! Ambition signs in to the T3 server running on this machine the way an
//! outside agent does, then calls the same tools a T3 thread calls, over
//! local HTTP:
//!
//! - Sign-in: `t3 auth pairing create` mints a one-time code, and T3's MCP
//!   OAuth endpoints trade it for a full-access token. The token lives in
//!   this process's memory only. Each sign-in revokes this Ambition's older
//!   sessions, so they do not pile up. (A token from `t3 auth session issue`
//!   is capped at approval-required, so it cannot drive a full-access thread.)
//! - Then tools: thread launch, send, wait, read, configure, interrupt and
//!   organize; projects; and `orchestrator_capabilities`, T3's own list of the
//!   models it offers right now.

use crate::engine::{Engine, Model, Res, Selection, Status, Thread};
use crate::http;
use crate::pkce;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PROTOCOL: &str = "2025-06-18";
/// Never opened: the code comes back in the decision's answer.
const REDIRECT: &str = "http://127.0.0.1:9/ambition-callback";

pub struct T3 {
    home: PathBuf,
    cli: PathBuf,
    label: String,
    conn: Mutex<Conn>,
}

#[derive(Default, Clone)]
struct Conn {
    origin: String,
    token: String,
    session: String,
    n: u64,
}

impl T3 {
    /// `label` names this Ambition's sign-ins in T3's session list.
    pub fn new(label: &str) -> T3 {
        let home = std::env::var_os("T3CODE_HOME").map(PathBuf::from).unwrap_or_else(|| crate::util::home().join(".t3"));
        let cli = std::env::var_os("AMBITION_T3_CLI").map(PathBuf::from).unwrap_or_else(|| home.join("bin").join("t3"));
        T3 { home, cli, label: label.to_string(), conn: Mutex::new(Conn::default()) }
    }

    fn origin(&self) -> Res<String> {
        let p = self.home.join("userdata").join("server-runtime.json");
        let text = std::fs::read_to_string(&p).map_err(|_| "T3 Code is not running. Open T3 Code.".to_string())?;
        let v: Value = serde_json::from_str(&text).map_err(|_| "T3 Code's runtime file is unreadable".to_string())?;
        Ok(v["origin"].as_str().ok_or("T3 Code's runtime file names no address")?.trim_end_matches('/').to_string())
    }

    fn cli(&self, args: &[&str]) -> Res<String> {
        let out = Command::new(&self.cli).args(args).output().map_err(|e| format!("cannot run {}: {e}", self.cli.display()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
            return Err(format!("`t3 {}` failed: {}", args.iter().take(3).cloned().collect::<Vec<_>>().join(" "), tail(&err, 300)));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn post_json(url: &str, body: &Value, headers: &[(&str, &str)], timeout: Duration) -> Result<http::Response, http::Error> {
        let mut h = vec![("Content-Type", "application/json"), ("Accept", "application/json")];
        h.extend_from_slice(headers);
        http::post(url, body.to_string().as_bytes(), &h, timeout)
    }

    fn signin(&self) -> Res<Conn> {
        let o = self.origin()?;
        let t = Duration::from_secs(30);
        let err = |e: http::Error| format!("cannot sign in to T3 Code ({e})");
        let reg = Self::post_json(&format!("{o}/oauth/mcp/register"), &json!({"client_name": self.label, "redirect_uris": [REDIRECT]}), &[], t)
            .map_err(err)?;
        let client_id = parse(&reg.body)?["client_id"].as_str().ok_or("T3 gave no client id")?.to_string();
        let verifier = pkce::b64url(&crate::util::random(48));
        let challenge = pkce::b64url(&pkce::sha256(verifier.as_bytes()));
        let state = pkce::b64url(&crate::util::random(12));
        let code = parse(&self.cli(&["auth", "pairing", "create", "--ttl", "2m", "--label", &format!("{} sign-in", self.label), "--json"])?)?
            ["credential"]
            .as_str()
            .ok_or("t3 gave no pairing code")?
            .to_string();
        let auth = json!({"response_type": "code", "client_id": client_id, "redirect_uri": REDIRECT,
            "code_challenge": challenge, "code_challenge_method": "S256", "state": state, "resource": format!("{o}/mcp")});
        let dec = Self::post_json(&format!("{o}/oauth/mcp/decision"),
            &json!({"authorization": auth, "decision": {"_tag": "pairing-code", "access": "full-access", "code": code}}), &[], t)
            .map_err(err)?;
        let to = parse(&dec.body)?["redirectTo"].as_str().unwrap_or("").to_string();
        let q = pkce::query(&to);
        let get = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        if get("state").as_deref() != Some(state.as_str()) || get("code").is_none() {
            return Err(format!("T3 refused the sign-in: {}", get("error").unwrap_or_else(|| "no code".into())));
        }
        let form = [("grant_type", "authorization_code"), ("code", &get("code").unwrap()), ("redirect_uri", REDIRECT),
            ("client_id", &client_id), ("code_verifier", &verifier)]
            .iter()
            .map(|(k, v)| format!("{}={}", k, pkce::urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let tok = http::post(&format!("{o}/oauth/mcp/token"), form.as_bytes(),
            &[("Content-Type", "application/x-www-form-urlencoded"), ("Accept", "application/json")], t)
            .map_err(err)?;
        let token = parse(&tok.body)?["access_token"].as_str().ok_or("T3 gave no token")?.to_string();
        self.revoke_old();
        Ok(Conn { origin: o, token, session: String::new(), n: 0 })
    }

    /// Revoke this Ambition's earlier sign-ins, keeping the newest (this one).
    fn revoke_old(&self) {
        let Ok(text) = self.cli(&["auth", "session", "list", "--json"]) else { return };
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) else { return };
        let mut mine: Vec<&Value> = rows
            .iter()
            .filter(|s| s["client"]["label"].as_str() == Some(self.label.as_str()) && s["subject"].as_str() == Some("mcp-client"))
            .collect();
        mine.sort_by_key(|s| s["issuedAt"].as_str().unwrap_or("").to_string());
        for s in mine.iter().take(mine.len().saturating_sub(1)) {
            if let Some(id) = s["sessionId"].as_str() {
                let _ = self.cli(&["auth", "session", "revoke", id]);
            }
        }
    }

    fn rpc(c: &Conn, body: &Value, timeout: Duration) -> Result<(Option<Value>, Option<String>), http::Error> {
        let auth = format!("Bearer {}", c.token);
        let mut h = vec![("Authorization", auth.as_str()), ("Accept", "application/json, text/event-stream"),
            ("mcp-protocol-version", PROTOCOL), ("Content-Type", "application/json")];
        if !c.session.is_empty() {
            h.push(("mcp-session-id", c.session.as_str()));
        }
        let r = http::post(&format!("{}/mcp", c.origin), body.to_string().as_bytes(), &h, timeout)?;
        let session = r.header("mcp-session-id").map(str::to_string);
        let mut raw = r.body.as_str();
        for line in r.body.lines() {
            if let Some(d) = line.strip_prefix("data:") {
                raw = d; // an event stream carries the answer on a data: line
            }
        }
        let v = if raw.trim().is_empty() { None } else { serde_json::from_str(raw.trim()).ok() };
        Ok((v, session))
    }

    fn open(c: &mut Conn) -> Result<(), http::Error> {
        c.session.clear();
        let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": {"name": "ambition", "version": env!("CARGO_PKG_VERSION")}}});
        let (_, s) = Self::rpc(c, &init, Duration::from_secs(30))?;
        c.session = s.unwrap_or_default();
        Self::rpc(c, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), Duration::from_secs(30))?;
        Ok(())
    }

    /// Call one T3 tool and return its structured result. Calls run side by
    /// side: the lock covers only signing in and opening the session, so a
    /// long wait for a turn never holds up a command.
    pub fn call(&self, name: &str, args: Value, timeout: Duration) -> Res<Value> {
        let mut out = None;
        let mut last_err = String::new();
        for attempt in 0..3 {
            let c = {
                let mut g = self.conn.lock().unwrap();
                if g.token.is_empty() {
                    match self.signin() {
                        Ok(c) => *g = c,
                        Err(e) => {
                            last_err = e;
                            if attempt == 2 {
                                return Err(format!("{name}: {last_err}"));
                            }
                            drop(g);
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    }
                }
                if g.session.is_empty() {
                    if let Err(e) = Self::open(&mut g) {
                        last_err = e.to_string();
                        if matches!(e, http::Error::Status(401, _)) {
                            g.token.clear();
                        }
                        drop(g);
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                }
                g.n += 1;
                g.clone()
            };
            let body = json!({"jsonrpc": "2.0", "id": c.n, "method": "tools/call", "params": {"name": name, "arguments": args}});
            match Self::rpc(&c, &body, timeout) {
                Ok((v, _)) => {
                    out = v;
                    break;
                }
                Err(http::Error::Status(401, _)) => {
                    let mut g = self.conn.lock().unwrap();
                    if g.token == c.token {
                        g.token.clear(); // expired or revoked: sign in again
                    }
                    last_err = "T3 would not take the sign-in".into();
                }
                Err(http::Error::Status(code @ (400 | 404), _)) => {
                    let mut g = self.conn.lock().unwrap();
                    if g.session == c.session {
                        g.session.clear(); // the server forgot the session
                    }
                    last_err = format!("HTTP {code}");
                }
                Err(http::Error::Status(code, body)) => return Err(format!("{name}: HTTP {code} {}", tail(&body, 200))),
                Err(http::Error::Net(e)) => {
                    let mut g = self.conn.lock().unwrap();
                    if g.session == c.session {
                        g.session.clear();
                    }
                    last_err = format!("cannot reach T3 Code ({e})");
                    if attempt < 2 {
                        drop(g);
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }
        let Some(out) = out else { return Err(format!("{name}: {last_err}")) };
        if let Some(e) = out.get("error") {
            return Err(format!("{name}: {}", e["message"].as_str().unwrap_or("no answer")));
        }
        let r = &out["result"];
        let mut body = r.get("structuredContent").cloned().unwrap_or(Value::Null);
        if body.is_null() {
            let text: String = r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect();
            body = serde_json::from_str(&text).unwrap_or(json!({"text": text}));
        }
        if r["isError"].as_bool() == Some(true) || body["_tag"].as_str() == Some("OrchestratorMcpFailure") {
            let msg = body["message"].as_str().map(str::to_string).unwrap_or_else(|| body.to_string());
            return Err(format!("{name}: {msg}"));
        }
        Ok(body)
    }

    fn c(&self, name: &str, args: Value) -> Res<Value> {
        self.call(name, args, Duration::from_secs(60))
    }

    fn read(&self, thread: &str, limit: u64, after: Option<&Value>, chars: u64) -> Res<Value> {
        let mut a = json!({"threadId": thread, "limit": limit, "maxCharsPerItem": chars, "runLimit": 1, "view": "messages"});
        if let Some(p) = after {
            a["afterPosition"] = p.clone();
        }
        self.c("t3_thread_read", a)
    }

    fn selection(v: &Value) -> Option<Selection> {
        let model = v["model"].as_str()?.to_string();
        let effort = v["options"].as_array().into_iter().flatten().find_map(|o| {
            let id = o["id"].as_str()?;
            matches!(id, "effort" | "reasoningEffort").then(|| (id.to_string(), o["value"].as_str().unwrap_or("").to_string()))
        });
        Some(Selection { instance: v["instanceId"].as_str().unwrap_or("").to_string(), model, effort })
    }

    fn selection_json(s: &Selection) -> Value {
        let mut v = json!({"instanceId": s.instance, "model": s.model});
        if let Some((id, value)) = &s.effort {
            v["options"] = json!([{"id": id, "value": value}]);
        }
        v
    }
}

impl Engine for T3 {
    fn name(&self) -> &str {
        "T3 Code"
    }

    fn project(&self, folder: &Path) -> Res<String> {
        let want = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let mut cursor = json!(0);
        loop {
            let page = self.c("t3_project_list", json!({"cursor": cursor, "limit": 100}))?;
            for p in page["projects"].as_array().into_iter().flatten() {
                let root = PathBuf::from(p["workspaceRoot"].as_str().unwrap_or(""));
                let root = std::fs::canonicalize(&root).unwrap_or(root);
                if root == want && p["archivedAt"].is_null() && p["deletedAt"].is_null() {
                    return Ok(p["id"].as_str().unwrap_or("").to_string());
                }
            }
            match page.get("nextCursor") {
                Some(c) if !c.is_null() => cursor = c.clone(),
                _ => break,
            }
        }
        let title = want.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Ambition".into());
        let p = self.c("t3_project_create", json!({"title": title, "workspaceRoot": want.to_string_lossy()}))?;
        Ok(p["id"].as_str().or(p["project"]["id"].as_str()).unwrap_or("").to_string())
    }

    fn launch(&self, project: &str, title: &str, message: &str, model: Option<&Selection>) -> Res<(String, String)> {
        let mut a = json!({"projectId": project, "title": title, "message": message, "runtimeMode": "full-access",
            "interactionMode": "default", "workspaceStrategy": {"type": "root"}});
        if let Some(m) = model {
            a["modelSelection"] = Self::selection_json(m);
        }
        let r = self.call("t3_thread_launch", a, Duration::from_secs(120))?;
        Ok((r["threadId"].as_str().unwrap_or("").to_string(), r["runId"].as_str().unwrap_or("").to_string()))
    }

    fn send(&self, thread: &str, message: &str) -> Res<String> {
        let r = self.call("t3_thread_send", json!({"threadId": thread, "message": message, "mode": "queue"}), Duration::from_secs(120))?;
        Ok(r["runId"].as_str().unwrap_or("").to_string())
    }

    fn wait(&self, thread: &str, run: &str, limit: Duration) -> Res<Status> {
        let end = Instant::now() + limit;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let slice = left.min(Duration::from_secs(60)).max(Duration::from_secs(1));
            let w = self.call("t3_thread_wait", json!({"threadId": thread, "runId": run, "timeoutMs": slice.as_millis() as u64}),
                slice + Duration::from_secs(30))?;
            if w["timedOut"].as_bool() != Some(true) {
                return Ok(match w["status"].as_str().unwrap_or("completed") {
                    "completed" => Status::Completed,
                    "failed" => Status::Failed,
                    "interrupted" => Status::Interrupted,
                    "needs-input" => Status::NeedsInput,
                    other => Status::Other(other.to_string()),
                });
            }
            let t = self.read(thread, 1, None, 4000)?;
            if t["thread"]["pendingRequestCount"].as_u64().unwrap_or(0) > 0 {
                return Ok(Status::NeedsInput);
            }
            if Instant::now() >= end {
                return Ok(Status::Running);
            }
        }
    }

    fn reply(&self, thread: &str, run: &str) -> Res<String> {
        let mut items = Vec::new();
        let mut after: Option<Value> = None;
        for _ in 0..50 {
            let page = self.read(thread, 100, after.as_ref(), 50_000)?;
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            if page["hasMore"].as_bool() != Some(true) {
                break;
            }
            after = page.get("nextPosition").cloned();
        }
        Ok(items
            .iter()
            .rev()
            .find(|i| i["runId"].as_str() == Some(run) && i["type"].as_str() == Some("assistant_message"))
            .and_then(|i| i["text"].as_str())
            .unwrap_or("")
            .to_string())
    }

    fn thread(&self, thread: &str) -> Res<Option<Thread>> {
        match self.read(thread, 1, None, 4000) {
            Ok(v) => {
                let t = &v["thread"];
                if !t["archivedAt"].is_null() || t["archived"].as_bool() == Some(true) {
                    return Ok(None);
                }
                Ok(Some(Thread {
                    link: t["link"].as_str().unwrap_or("").to_string(),
                    status: t["status"].as_str().unwrap_or("").to_string(),
                    turns: t["runCount"].as_u64().unwrap_or(0),
                }))
            }
            Err(e) => {
                let l = e.to_lowercase();
                if ["no longer available", "not found", "unknown thread"].iter().any(|w| l.contains(w)) {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    fn interrupt(&self, thread: &str) -> Res<()> {
        self.c("t3_thread_interrupt", json!({"threadId": thread})).map(|_| ())
    }

    fn settle(&self, thread: &str) -> Res<()> {
        self.c("t3_thread_organize", json!({"threadId": thread, "action": "settle"})).map(|_| ())
    }

    fn models(&self, search: Option<&str>) -> Res<Vec<Model>> {
        let caps = self.c("orchestrator_capabilities", match search {
            Some(q) => json!({"search": q}),
            None => json!({}),
        })?;
        let mut out = Vec::new();
        for p in caps["providers"].as_array().into_iter().flatten() {
            let off = p["constraints"].as_array().into_iter().flatten().any(|c| c.as_str().unwrap_or("").to_lowercase().contains("disabled"));
            if off {
                continue;
            }
            let instance = p["providerInstanceId"].as_str().unwrap_or("").to_string();
            let provider = p["displayName"].as_str().unwrap_or(&instance).to_string();
            for m in p["models"].as_array().into_iter().flatten() {
                let id = m["id"].as_str().unwrap_or("").to_string();
                let (mut effort_id, mut efforts) = (String::new(), Vec::new());
                for o in m["options"].as_array().into_iter().flatten() {
                    if matches!(o["id"].as_str(), Some("effort" | "reasoningEffort")) {
                        effort_id = o["id"].as_str().unwrap_or("").to_string();
                        efforts = o["values"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                    }
                }
                out.push(Model { instance: instance.clone(), provider: provider.clone(), label: m["label"].as_str().unwrap_or(&id).to_string(),
                    id, effort_id, efforts });
            }
        }
        Ok(out)
    }

    fn model_of(&self, thread: &str) -> Res<Option<Selection>> {
        Ok(Self::selection(&self.c("t3_thread_configure", json!({"threadId": thread}))?["modelSelection"]))
    }

    fn project_model(&self, project: &str) -> Res<Option<Selection>> {
        let mut cursor = json!(0);
        loop {
            let page = self.c("t3_project_list", json!({"cursor": cursor, "limit": 100}))?;
            if let Some(p) = page["projects"].as_array().into_iter().flatten().find(|p| p["id"].as_str() == Some(project)) {
                return Ok(Self::selection(&p["defaultModelSelection"]));
            }
            match page.get("nextCursor") {
                Some(c) if !c.is_null() => cursor = c.clone(),
                _ => return Ok(None),
            }
        }
    }

    fn use_model(&self, thread: Option<&str>, project: &str, model: &Selection) -> Res<()> {
        let sel = Self::selection_json(model);
        if let Some(t) = thread {
            self.c("t3_thread_configure", json!({"threadId": t, "modelSelection": sel}))?;
        }
        self.c("t3_project_update", json!({"projectId": project, "defaultModelSelection": sel}))?;
        Ok(())
    }
}

fn parse(text: &str) -> Res<Value> {
    serde_json::from_str(text).map_err(|_| format!("T3 sent something unreadable: {}", tail(text, 120)))
}

fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}
