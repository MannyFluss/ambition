//! The inbox: the one door into the running Ambition. Every trigger posts
//! here. Today that is `ambition fire`, which an editor's key runs; a global
//! hotkey can post the same way later. The agent's own `ambition more` and
//! `ambition ask` come in here too.
//!
//! It is a socket on 127.0.0.1. Its port and a random token are in
//! `~/.ambition/inbox`, which only you can read, and a request without the
//! token is turned away. A request is one line of JSON; the answer is one
//! line of JSON, `{"ok": true, "text": "…"}`. Between requests the daemon
//! sleeps in `accept`: nothing polls, and nothing watches files.
//!
//! ```text
//! {"op": "press", "file": "/abs/notes.md", "line": 12}
//! {"op": "more", "dir": "up", "n": 40, "cwd": "/abs"}
//! {"op": "ask", "to": "coordinator", "message": "…", "cwd": "/abs"}
//! ```

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub fn path() -> PathBuf {
    crate::util::state_dir().join("inbox")
}

/// What answers a request.
pub type Handler = Arc<dyn Fn(&Value) -> Value + Send + Sync>;

/// What takes an editor's connection (`editor.rs`): `{"op": "editor"}`
/// keeps it open, one line each way, instead of one answer.
pub type EditorHandler = Box<dyn Fn(TcpStream, &Value) + Send + Sync>;

pub static EDITORS: std::sync::OnceLock<EditorHandler> = std::sync::OnceLock::new();

pub struct Inbox {
    listener: TcpListener,
    token: String,
}

pub fn ok(text: impl Into<String>) -> Value {
    json!({"ok": true, "text": text.into()})
}

pub fn no(text: impl Into<String>) -> Value {
    json!({"ok": false, "text": text.into()})
}

impl Inbox {
    /// Open the socket and write where it is.
    pub fn open() -> Result<Inbox, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot open the inbox: {e}"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let token: String = crate::util::random(24).iter().map(|b| format!("{b:02x}")).collect();
        let p = path();
        let tmp = p.with_extension(format!("tmp{}", crate::util::nonce()));
        std::fs::write(&tmp, format!("{addr} {token}\n")).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
        Ok(Inbox { listener, token })
    }

    /// Answer requests until the process ends, each on its own thread, so a
    /// long one (a dialog, a question for the coordinator) holds up no other.
    pub fn serve(self, handler: Handler) {
        let token = Arc::new(self.token);
        for conn in self.listener.incoming().flatten() {
            let (h, token) = (handler.clone(), token.clone());
            std::thread::spawn(move || answer(conn, &token, &h));
        }
    }
}

fn answer(mut conn: TcpStream, token: &str, h: &Handler) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = String::new();
    if BufReader::new((&conn).take(4 << 20)).read_line(&mut line).is_err() {
        return;
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    if req["op"].as_str() == Some("editor") && req["token"].as_str() == Some(token) {
        if let Some(h) = EDITORS.get() {
            let _ = conn.set_read_timeout(None);
            return h(conn, &req);
        }
    }
    let out = if req["token"].as_str() != Some(token) {
        no("the inbox turned this away: wrong token")
    } else {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(&req))) {
            Ok(v) => v,
            Err(_) => no("Ambition failed on this request; its log says more"),
        }
    };
    let _ = conn.write_all(format!("{out}\n").as_bytes());
}

/// Where the running Ambition listens, and its token.
pub fn location() -> Option<(std::net::SocketAddr, String)> {
    let text = std::fs::read_to_string(path()).ok()?;
    let (addr, token) = text.trim().split_once(' ')?;
    Some((addr.parse().ok()?, token.to_string()))
}

/// Post a request to the running Ambition and wait up to `wait` for its
/// answer: (ok, text).
pub fn send(mut req: Value, wait: Duration) -> Result<(bool, String), String> {
    let down = "Ambition is not running. Start it with `ambition`.";
    let (addr, token) = location().ok_or(down)?;
    req["token"] = json!(token);
    let mut c = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| down.to_string())?;
    let _ = c.set_read_timeout(Some(wait));
    c.write_all(format!("{req}\n").as_bytes()).map_err(|e| format!("cannot reach Ambition: {e}"))?;
    let mut line = String::new();
    BufReader::new(c).read_line(&mut line).map_err(|e| format!("Ambition did not answer in time: {e}"))?;
    let v: Value = serde_json::from_str(&line).map_err(|_| "Ambition gave no answer".to_string())?;
    Ok((v["ok"].as_bool() == Some(true), v["text"].as_str().unwrap_or("").to_string()))
}
