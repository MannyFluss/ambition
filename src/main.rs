//! `ambition`: write `^` at the start of a line in any file, press a key on
//! it, and the answer lands in the file.

use ambition::app::{log, App};
use ambition::{config, inbox, t3::T3, util};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "\
Ambition: a document you talk to.

  ambition                  run it. It sleeps until a key press reaches it.
  ambition fire FILE:LINE   a press: send the ^ line at LINE of FILE. Your
                            editor's key runs this (README: In your editor).
  ambition allow FOLDER     let presses in FOLDER run without asking first.
  ambition allow            the folders allowed so far.
  ambition lsp              the language server, for editors

For the agent, while its turn runs:

  ambition more up|down [N]          N more lines of the document (40)
  ambition ask coordinator MESSAGE   ask the thread that knows the others

In a file:

  ^ make this shorter       a prompt: a press sends it to the agent
  ^/help                    a command: answered at once, with no model
  ^> ...                    a reply: your next press in that file clears it

Everything Ambition keeps is in ~/.ambition (or $AMBITION_HOME).
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = || args[1..].join(" ");
    match args.first().map(String::as_str) {
        None => run(),
        Some("lsp") => ambition::lsp::main(),
        Some("-h" | "--help" | "help") => print!("{USAGE}"),
        Some("-V" | "--version" | "version") => println!("ambition {}", env!("CARGO_PKG_VERSION")),
        Some("fire") => fire(&rest()),
        Some("allow") => allow(&rest()),
        Some("more") => more(&args[1..]),
        Some("ask") => ask(&args[1..]),
        Some(a) if a.starts_with('-') => fail(2, &format!("ambition: no option {a}.\n\n{USAGE}")),
        Some(a) => fail(2, &format!(
            "ambition: Ambition 0.2 watches no folder, so `ambition {a}` does nothing. Run `ambition` alone, then press \
             your editor's Ambition key on a ^ line in any file. To let a folder run without asking: ambition allow {a}")),
    }
}

/// The daemon: one per user, asleep on its inbox.
fn run() {
    let home = util::state_dir();
    if let Err(e) = std::fs::create_dir_all(&home) {
        fail(1, &format!("ambition: cannot make {}: {e}", home.display()));
    }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(home.join("lock"));
    let lock = lock.unwrap_or_else(|e| fail(1, &format!("ambition: cannot write in {}: {e}", home.display())));
    if lock.try_lock().is_err() {
        fail(1, &format!("ambition: Ambition is already running for {}.", util::tilde(&home)));
    }
    let label = format!("Ambition · {}", &util::sha(&util::state_real().to_string_lossy())[..6]);
    let app = Arc::new(App::new(Arc::new(T3::new(&label))));
    if let Err(e) = app.start() {
        fail(1, &format!("ambition: {e}"));
    }
    let inbox = inbox::Inbox::open().unwrap_or_else(|e| fail(1, &format!("ambition: {e}")));
    log(&format!("Ambition {} is running, for {}. Press your editor's Ambition key on a ^ line in any file.",
        env!("CARGO_PKG_VERSION"), util::tilde(&home)));
    let a = app.clone();
    inbox.serve(Arc::new(move |req| a.handle(req)));
    drop(lock);
}

/// `FILE:LINE`, or `FILE:LINE:COLUMN` as some editors give it.
fn place(arg: &str) -> Option<(PathBuf, u64)> {
    let mut parts: Vec<&str> = arg.rsplitn(3, ':').collect();
    parts.reverse();
    let (file, line) = match parts.as_slice() {
        [f, l, c] if l.parse::<u64>().is_ok() && c.parse::<u64>().is_ok() => (f.to_string(), l.parse().ok()?),
        [f, l, c] if c.parse::<u64>().is_ok() => (format!("{f}:{l}"), c.parse().ok()?),
        [f, l] => (f.to_string(), l.parse().ok()?),
        _ => return None,
    };
    let p = util::expand(&file);
    let p = if p.is_absolute() { p } else { std::env::current_dir().ok()?.join(p) };
    Some((p, line))
}

fn fire(arg: &str) {
    let Some((file, line)) = place(arg.trim()) else { fail(2, "ambition fire: name the file and the line, as FILE:LINE.") };
    let req = json!({"op": "press", "file": file.to_string_lossy(), "line": line});
    say(inbox::send(req, Duration::from_secs(600)));
}

fn allow(arg: &str) {
    let arg = arg.trim();
    if arg.is_empty() {
        let g = config::grants();
        if g.is_empty() {
            println!("No folder is allowed yet. The first press in a folder asks, or: ambition allow FOLDER");
        }
        for f in g {
            println!("{}", util::tilde(&f));
        }
        return;
    }
    match config::allow(&util::expand(arg)) {
        Ok(t) => println!("{t}"),
        Err(e) => fail(1, &format!("ambition allow: {e}")),
    }
}

fn cwd() -> String {
    std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

fn more(args: &[String]) {
    let dir = args.first().map(String::as_str).unwrap_or("down");
    if dir != "up" && dir != "down" {
        fail(2, "ambition more: up or down, then how many lines if you like: ambition more up 60");
    }
    let n = match args.get(1) {
        Some(n) => n.parse::<u64>().unwrap_or_else(|_| fail(2, "ambition more: the number of lines is a number.")),
        None => ambition::hook::MORE as u64,
    };
    say(inbox::send(json!({"op": "more", "dir": dir, "n": n, "cwd": cwd()}), Duration::from_secs(30)));
}

fn ask(args: &[String]) {
    let (Some(to), true) = (args.first(), args.len() > 1) else {
        fail(2, "ambition ask: whom, then the message: ambition ask coordinator \"what does the thread for todo.md know?\"")
    };
    let message = args[1..].join(" ");
    say(inbox::send(json!({"op": "ask", "to": to, "message": message, "cwd": cwd()}), Duration::from_secs(4 * 3600)));
}

fn say(r: Result<(bool, String), String>) {
    match r {
        Ok((true, t)) => println!("{t}"),
        Ok((false, t)) => fail(1, &format!("ambition: {t}")),
        Err(e) => fail(1, &format!("ambition: {e}")),
    }
}

fn fail(code: i32, msg: &str) -> ! {
    eprintln!("{}", msg.trim_end());
    std::process::exit(code)
}
