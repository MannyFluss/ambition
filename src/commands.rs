//! `^/` commands: answered at once, in code, never by a model.
//!
//! The built-ins live in `app.rs`. Your own are files in
//! `.ambition/commands/`: `count`, `count.sh` and `count.py` all answer
//! `^/count`.
//!
//! - It runs in the folder, with the words after its name as arguments, for
//!   at most ten seconds, with no sandbox.
//! - Its stdin is the paragraph just above the command line.
//! - `AMBITION_FILE` (the file, relative to the folder), `AMBITION_LINE`
//!   (1-based) and `AMBITION_DIR` say where it was written.
//! - What it prints (stdout, then stderr) becomes the `^>` reply.
//! - Its first comment line (after a `#!` line) is its description, shown by
//!   `^/help` and in the editor.
//! - An executable file runs as it is. Otherwise `.py` runs with python3,
//!   `.sh` with sh, `.js` with node, `.rb` with ruby, `.ps1` with PowerShell.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const BUILTINS: [(&str, &str); 6] = [
    ("help", "list the commands"),
    ("model", "show the models the engine offers, or switch: ^/model opus, ^/model sonnet high"),
    ("new", "start a new thread on the next ^ prompt, with a handoff from this one"),
    ("thread", "the link to this folder's thread, and what it is doing"),
    ("stop", "stop the agent's turn that is running now"),
    ("undo", "take back Ambition's last change (an agent's turn or a command)"),
];

pub fn timeout() -> Duration {
    let s: f64 = std::env::var("AMBITION_COMMAND_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    Duration::from_secs_f64(s)
}

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(n, _)| *n == name)
}

/// {name: path} of the command files in `<folder>/.ambition/commands/`.
pub fn find(folder: &Path) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(folder.join(crate::folder::COMMANDS)) else { return out };
    let mut paths: Vec<PathBuf> = dir.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        let file = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !p.is_file() || file.starts_with('.') || file.eq_ignore_ascii_case("readme.md") {
            continue;
        }
        let name = p.file_stem().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let ok = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-');
        if ok {
            out.entry(name).or_insert(p);
        }
    }
    out
}

/// The first comment line of a command file, or "".
pub fn describe(path: &Path) -> String {
    let Ok(text) = std::fs::read(path) else { return String::new() };
    let text = String::from_utf8_lossy(&text);
    for l in text.lines().take(8) {
        let s = l.trim();
        if s.starts_with("#!") || s.is_empty() {
            continue;
        }
        for m in ["#", "//", "--", ";", "rem ", "REM "] {
            if let Some(rest) = s.strip_prefix(m) {
                let rest = rest.trim();
                if !rest.is_empty() {
                    return rest.to_string();
                }
            }
        }
        if let Some(rest) = s.strip_prefix("\"\"\"").or_else(|| s.strip_prefix("'''")) {
            return rest.trim_matches(|c| c == '"' || c == '\'' || c == ' ').to_string();
        }
        return String::new();
    }
    String::new()
}

/// Split arguments the way a shell would, for quotes and spaces only.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Run a command file. Returns (output, exit code; -1 when it never finished).
pub fn run(folder: &Path, path: &Path, args: &str, rel: &str, line: usize, stdin: &str) -> (String, i32) {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut argv: Vec<String> = if executable(path) {
        vec![path.to_string_lossy().to_string()]
    } else {
        let runner: &[&str] = match ext.as_str() {
            "py" => &["python3"],
            "js" => &["node"],
            "rb" => &["ruby"],
            "ps1" => &["powershell", "-NoProfile", "-File"],
            _ => &["sh"],
        };
        let mut v: Vec<String> = runner.iter().map(|s| s.to_string()).collect();
        v.push(path.to_string_lossy().to_string());
        v
    };
    argv.extend(split_args(args));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(folder)
        .env("AMBITION_FILE", rel)
        .env("AMBITION_LINE", line.to_string())
        .env("AMBITION_DIR", folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // its own group, so a timeout stops everything it started
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("could not start it: {e}"), -1),
    };
    if let Some(mut si) = child.stdin.take() {
        let data = stdin.to_string();
        std::thread::spawn(move || {
            let _ = si.write_all(data.as_bytes());
        });
    }
    // Readers fill shared buffers as output comes, so a timeout can hand back
    // what was printed without waiting for pipes a stray child still holds.
    let out = Arc::new(Mutex::new(Vec::new()));
    let err = Arc::new(Mutex::new(Vec::new()));
    let pump = |src: Option<Box<dyn Read + Send>>, buf: Arc<Mutex<Vec<u8>>>| {
        std::thread::spawn(move || {
            let Some(mut src) = src else { return };
            let mut chunk = [0u8; 4096];
            while let Ok(n) = src.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        })
    };
    let t_out = pump(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), out.clone());
    let t_err = pump(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>), err.clone());
    let text = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&b.lock().unwrap()).trim_end().to_string();
    let limit = timeout();
    let began = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) if began.elapsed() >= limit => {
                kill_group(&mut child);
                std::thread::sleep(Duration::from_millis(50));
                let said = format!("{}\n(stopped after {} seconds)", text(&out), fmt_secs(limit));
                return (said.trim().to_string(), -1);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return (format!("it failed: {e}"), -1),
        }
    };
    // It ended: give the readers a moment to drain, but never wait on a pipe
    // that something it left behind still holds open.
    let drained = Instant::now();
    while !(t_out.is_finished() && t_err.is_finished()) && drained.elapsed() < Duration::from_millis(500) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let all = [text(&out), text(&err)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    (all, code)
}

#[cfg(unix)]
fn kill_group(child: &mut std::process::Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_group(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn fmt_secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s.fract() == 0.0 {
        format!("{}", s as u64)
    } else {
        format!("{s}")
    }
}

#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn executable(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()), Some("exe" | "bat" | "cmd" | "com"))
}

pub const README: &str = "\
# Your commands

A command is a line that starts with `^/`: `^/count`, `^/stamp`. Ambition
runs it the moment you save, with no model, and what it prints takes the
line's place as `^>` reply lines.

Each file here is one command, named for it: `count.sh`, `count.py` or
`count` all answer `^/count`. A built-in (`^/help` lists them) wins over a
file of the same name.

- It runs in the folder, with the words after its name as arguments, for at
  most ten seconds.
- Its stdin is the paragraph just above the command line, so `^/count`
  under a paragraph counts that paragraph.
- `AMBITION_FILE`, `AMBITION_LINE` and `AMBITION_DIR` say where it was
  written.
- What it prints becomes the reply. It may also change files: `^/undo`
  takes that back.
- Its first comment line is its description: `^/help` shows it.
- An executable file runs as it is; otherwise `.py` runs with python3, `.sh`
  with sh, `.js` with node, `.rb` with ruby.

You do not have to write one yourself. Under any paragraph, write:

    ^ write me a command called shout that prints the paragraph above in capitals
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_split_like_a_shell() {
        assert_eq!(split_args(r#"a "b c" 'd e' f\ g"#), vec!["a", "b c", "d e", "f g"]);
        assert_eq!(split_args(r#""" x"#), vec!["", "x"]);
        assert!(split_args("   ").is_empty());
    }
}
