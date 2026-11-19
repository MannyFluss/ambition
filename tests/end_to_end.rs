//! End to end: the real `ambition` binary, a folder in a scratch home, and a
//! fake T3 (`fake_t3`). Each step writes a file the way an editor saves it,
//! then presses a line the way an editor's key does (`ambition fire
//! FILE:LINE`), and waits for what should happen in the file.
//!
//! The core steps were first written against the app Ambition replaced, and
//! then against Ambition 0.1, which sent on save; they are the behavior
//! Ambition is held to, with a press where the save was.

mod fake_t3;

use fake_t3::{said, Fake, Outcome};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ambition");

struct H {
    root: PathBuf,
    dir: PathBuf,
    state: PathBuf,
    t3: Fake,
    child: Child,
    fails: Vec<String>,
}

impl H {
    fn new() -> H {
        let root = std::env::temp_dir().join(format!("ambition-e2e-{}-{}", std::process::id(), nonce()));
        let home = root.join("home");
        let dir = home.join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(root.join("state")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let t3 = Fake::start(&root.join("t3"));
        // The folder is its own repo, to show Ambition never touches it.
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("readme.md"), "Mine.\n").unwrap();
        git(&dir, &["add", "readme.md"]);
        git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "the user's own commit"]);
        // The dialog answers what the file `dialog` says (AMBITION_TEST_DIALOG),
        // and the global key stays off, so a test never takes the real one.
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let state = root.join("state");
        std::fs::write(state.join("config"), "hotkey = none\n").unwrap();
        let child = spawn(&root);
        let h = H { root, dir, state, t3, child, fails: Vec::new() };
        assert!(wait(10.0, || h.state.join("inbox").exists()), "Ambition never started: {}", h.output());
        h
    }

    fn env(&self, c: &mut Command) {
        env(&self.root, c);
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.state.join("inbox"));
        self.child = spawn(&self.root);
        assert!(wait(10.0, || self.state.join("inbox").exists()), "Ambition never started again: {}", self.output());
    }

    fn history(&self) -> String {
        logs(&self.state.join("history"), "md")
    }

    /// The log of Ambition's own history of the folder, kept outside it.
    fn log(&self) -> String {
        let store = self.state.join("history");
        let git = std::fs::read_dir(&store).ok().into_iter().flatten().flatten().map(|e| e.path())
            .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("notes-") && n.to_string_lossy().ends_with(".git")))
            .unwrap_or_default();
        let args: Vec<String> = vec![format!("--git-dir={}", git.display()), "log".into(), "--format=%s".into()];
        String::from_utf8_lossy(&Command::new("git").args(&args).output().unwrap().stdout).to_string()
    }

    fn output(&self) -> String {
        format!("{}{}", std::fs::read_to_string(self.root.join("out.txt")).unwrap_or_default(),
            std::fs::read_to_string(self.root.join("err.txt")).unwrap_or_default())
    }

    fn save(&self, rel: &str, text: &str) {
        let p = self.dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn note(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap_or_default()
    }

    /// `ambition fire FILE:LINE`, as an editor's key runs it: (ok, what it printed).
    fn fire_at(&self, rel: &str, line: usize) -> (bool, String) {
        let mut c = Command::new(BIN);
        c.arg("fire").arg(format!("{}:{line}", self.dir.join(rel).display()));
        self.env(&mut c);
        let o = c.output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    /// Press the first line of a file that starts with `needle`.
    fn fire(&self, rel: &str, needle: &str) -> (bool, String) {
        let text = self.note(rel);
        let Some(k) = text.split('\n').position(|l| l.starts_with(needle)) else { return (false, format!("no line {needle:?} in {text:?}")) };
        self.fire_at(rel, k + 1)
    }

    /// Save a file, then press its line that starts with `needle`.
    fn ask(&self, rel: &str, text: &str, needle: &str) -> (bool, String) {
        self.save(rel, text);
        self.fire(rel, needle)
    }

    fn dialog(&self, answer: &str) {
        std::fs::write(self.root.join("dialog"), answer).unwrap();
    }

    fn dialogs(&self) -> usize {
        std::fs::read_to_string(self.root.join("dialog.log")).unwrap_or_default().lines().count()
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        if ok {
            println!("ok   {name}");
        } else {
            println!("FAIL {name}: {detail:?}");
            self.fails.push(name.to_string());
        }
    }

    /// Wait for a condition on a file.
    fn until(&self, rel: &str, secs: f64, f: impl Fn(&str) -> bool) -> bool {
        wait(secs, || f(&self.note(rel)))
    }

    fn state_json(&self) -> Value {
        std::fs::read_to_string(self.state.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Wait until no turn is running and Ambition says so.
    fn idle(&self) {
        wait(10.0, || !self.t3.running() && self.state_json()["turns"].as_array().is_some_and(|t| t.is_empty()));
        std::thread::sleep(Duration::from_millis(300));
    }

    fn command(&self, name: &str, body: &str) {
        let d = self.state.join("commands");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn finish(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.fails.is_empty() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
        assert!(self.fails.is_empty(), "failed: {:?}\napp output:\n{}", self.fails, self.output());
    }
}

fn env(root: &Path, c: &mut Command) {
    let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    c.env("HOME", root.join("home"))
        .env("AMBITION_HOME", root.join("state"))
        .env("T3CODE_HOME", root.join("t3"))
        .env("AMBITION_COMMAND_TIMEOUT", "2")
        .env("AMBITION_TEST_DIALOG", if cfg!(target_os = "macos") { root.join("dialog") } else { PathBuf::new() })
        .env("PATH", path);
}

fn spawn(root: &Path) -> Child {
    let out = std::fs::OpenOptions::new().create(true).append(true).open(root.join("out.txt")).unwrap();
    let err = std::fs::OpenOptions::new().create(true).append(true).open(root.join("err.txt")).unwrap();
    let mut c = Command::new(BIN);
    env(root, &mut c);
    c.stdout(out).stderr(err).spawn().unwrap()
}

/// All the files with this extension in a folder, joined.
fn logs(dir: &Path, ext: &str) -> String {
    let mut out = String::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == ext) {
            out.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
        }
    }
    out
}

fn wait(secs: f64, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    f()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).stderr(Stdio::null()).output().unwrap();
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn nonce() -> String {
    format!("{:x}", Instant::now().elapsed().as_nanos() ^ (std::process::id() as u128) << 20 ^ rand_u64() as u128)
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(1);
    h.finish()
}

/// An agent turn that replaces `from` with `to` in a file.
fn edit(rel: &'static str, from: &'static str, to: &'static str, reply: &'static str) -> impl FnOnce(&fake_t3::Turn) -> Outcome {
    move |t| {
        let p = t.folder.join(rel);
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        std::fs::write(&p, text.replacen(from, to, 1)).unwrap();
        said(reply)
    }
}

/// Saving sends nothing; the first press in a folder asks.
fn grants(h: &mut H) {
    let first = "# Notes\n\nA long rambling paragraph that goes on and on.\n^ make this shorter\n\nThe end.\n";
    h.save("notes.md", first);
    std::thread::sleep(Duration::from_millis(1200));
    h.check("saving a ^ line sends nothing and changes nothing", h.t3.messages().is_empty() && h.note("notes.md") == first, h.t3.messages().len());
    let (ok, out) = h.fire_at("notes.md", 5);
    h.check("a press on a reply-free blank line still asks first, and Deny or no grant sends nothing",
        !ok && h.t3.messages().is_empty() && h.note("notes.md") == first, &out);

    if cfg!(target_os = "macos") {
        h.dialog("Deny");
        let (ok, out) = h.fire("notes.md", "^ make this shorter");
        h.check("the first press in a new folder asks, and Deny sends nothing",
            !ok && out.contains("denied") && h.dialogs() == if cfg!(target_os = "macos") { 2 } else { 0 } && h.t3.messages().is_empty() && h.note("notes.md") == first, (&out, h.dialogs()));
        h.dialog("Allow once");
    } else {
        let (ok, out) = h.fire("notes.md", "^ make this shorter");
        h.check("off macOS, a press in a folder not allowed says how to allow it", !ok && out.contains("ambition allow"), &out);
        let mut c = Command::new(BIN);
        c.arg("allow").arg(&h.dir);
        h.env(&mut c);
        let o = c.output().unwrap();
        h.check("ambition allow grants the folder", o.status.success(), &o);
    }

    // The first prompt: a thread, a status line, an edit, one commit.
    let (tx, rx) = mpsc::channel();
    h.t3.next(move |t| {
        let p = t.folder.join("notes.md");
        let text = std::fs::read_to_string(&p).unwrap();
        tx.send(text.clone()).unwrap();
        std::fs::write(&p, text.replace("A long rambling paragraph that goes on and on.", "Short.")).unwrap();
        said("Shortened the paragraph on line 3.")
    });
    let (ok, out) = h.fire("notes.md", "^ make this shorter");
    h.check("the press is taken", ok && out.contains("sent to the thread for notes.md"), &out);
    let landed = h.until("notes.md", 10.0, |t| t.contains("Short.") && !t.contains('◌'));
    h.check("a first prompt: the agent's edit lands and its status line goes", landed, h.note("notes.md"));
    let seen = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    h.check("the agent saw the status line where the prompt stood",
        seen.contains("^ ◌ p1 · make this shorter · working") && !seen.contains("^ make this"), &seen);
    let msgs = h.t3.messages();
    let launched = h.t3.lock().launched.clone();
    h.check("the first prompt starts one thread, named for the file", launched.len() == 1 && msgs.len() == 1
        && launched[0].0 == "Ambition · notes.md", launched.iter().map(|l| l.0.clone()).collect::<Vec<_>>());
    let m0 = msgs.first().cloned().unwrap_or_default();
    let opening = m0.starts_with("[Ambition] A new thread starts here, for ~/notes/notes.md, in the folder ~/notes.")
        && m0.contains("# Ambition") && m0.contains("## Your answer goes in the document") && m0.contains("[Ambition] A press in ~/notes/notes.md, line 4");
    h.check("its first message is the handoff with the rules, then the press", opening, &m0[..m0.len().min(500)]);
    h.check("the handoff says how to run this Ambition, which is not the one on the PATH",
        m0.contains("Here, run Ambition as `AMBITION_HOME="), &m0[m0.len().saturating_sub(1500)..]);
    h.check("the press sends the lines around the caret, numbered, as on disk",
        m0.contains("\n3| A long rambling paragraph") && m0.contains("\n4| ^ ◌ p1 · make this shorter · working") && m0.contains("all 6 lines"),
        &m0[m0.len().saturating_sub(700)..]);
    h.idle();
    let log = h.log();
    let mut subjects = log.lines();
    h.check("the turn is one commit, and the whole folder is kept only after it",
        subjects.next().unwrap_or("").contains("kept the whole folder") && subjects.next().unwrap_or("").contains("agent answered p1 in notes.md")
            && subjects.next().unwrap_or("").contains("pressed p1 in notes.md") && subjects.next().is_none(), &log);

    if cfg!(target_os = "macos") {
        h.dialog("Always");
        h.t3.next(|_| said("^> ok"));
        let (ok, out) = h.ask("notes.md", "Short.\n^ say ok\n", "^ say ok");
        h.check("Allow once asked again on the next press, and Always lets it through", ok && h.dialogs() == 4, (&out, h.dialogs()));
        h.until("notes.md", 10.0, |t| t.contains("^> ok"));
        h.idle();
        let cfg = std::fs::read_to_string(h.state.join("config")).unwrap_or_default();
        h.check("Always writes a standing grant for the folder", cfg.contains("allow = ~/notes"), &cfg);
    }
    h.t3.next(|_| said("^> fine"));
    let n = h.dialogs();
    let (ok, _) = h.ask("notes.md", "Short.\n^ and again\n", "^ and again");
    h.until("notes.md", 10.0, |t| t.contains("^> fine"));
    h.idle();
    h.check("a granted folder never asks again", ok && h.dialogs() == n, h.dialogs());
}

/// The core: what a press does.
fn core(h: &mut H) {
    // A press clears the replies in its own file only.
    h.save("other.md", "Other.\n^> a reply to keep for now\n");
    let n = h.t3.messages().len();
    h.ask("notes.md", "# Notes\n\nShort.\n^> an old reply\n^> its second line\n\n^/help\n", "^/help");
    let helped = h.until("notes.md", 10.0, |t| t.contains("^> /help:") && t.contains("^> /undo:"));
    h.check("^/help lists the built-ins", helped, h.note("notes.md"));
    h.check("a press clears the old replies in its file", !h.note("notes.md").contains("an old reply"), h.note("notes.md"));
    h.check("and leaves other files' replies alone", h.note("other.md").contains("^> a reply to keep"), h.note("other.md"));
    h.check("a command never reaches the agent", h.t3.messages().len() == n, h.t3.messages().len());
    let (ok, out) = h.fire("notes.md", "^> /help:");
    h.check("a press on a reply says it is one", !ok && out.contains("is a reply"), &out);

    // Your own commands, from ~/.ambition/commands, in every folder.
    h.idle();
    h.command("count.sh", "#!/bin/sh\n# Count the words of the paragraph above.\nwc -w | tr -d ' '\n");
    let (fenv, lenv, denv) = ("AMBITION_FILE", "AMBITION_LINE", "AMBITION_DIR");
    h.command("echo.sh", &format!("#!/bin/sh\n# Echo its words.\necho \"args: $* | file: ${fenv} | line: ${lenv} | dir: ${denv}\"\n"));
    h.command("slow.sh", "#!/bin/sh\n# Take too long.\nsleep 6\necho late\n");
    h.ask("notes.md", "One two three four five.\n^/count\n\n^/echo a b c\n\n# ^/echo nope\n", "^/count");
    let counted = h.until("notes.md", 10.0, |t| t.contains("One two three four five.\n^> 5\n"));
    h.check("^/count runs the script on the paragraph above", counted, h.note("notes.md"));
    h.check("a press runs only the command it names", h.note("notes.md").contains("^/echo a b c"), h.note("notes.md"));
    h.fire("notes.md", "^/echo");
    let dir = h.dir.display().to_string();
    let echoed = h.until("notes.md", 10.0, |t| t.contains(&format!("^> args: a b c | file: notes.md | line: 4 | dir: {dir}")));
    h.check("^/echo gets its words, the file, the line and the folder", echoed, h.note("notes.md"));
    h.t3.next(|_| said("^> that line is a heading"));
    let (ok, out) = h.fire("notes.md", "# ^/echo nope");
    let asked = h.until("notes.md", 10.0, |t| t.contains("# ^/echo nope\n^> that line is a heading"));
    h.check("a ^/ line after # in a note is text: a press on it is a request, not a command",
        ok && asked && !h.note("notes.md").contains("args: nope"), (&out, h.note("notes.md")));
    h.idle();
    h.ask("notes.md", "Text.\n\n^/slwo\n", "^/slwo");
    let near = h.until("notes.md", 10.0, |t| t.contains("Did you mean ^/slow?"));
    h.check("an unknown command says so and suggests one", near, h.note("notes.md"));
    let t0 = Instant::now();
    h.ask("notes.md", "Text.\n\n^/slow\n", "^/slow");
    let stopped = h.until("notes.md", 10.0, |t| t.contains("(stopped after 2 seconds)"));
    h.check("a slow command is stopped at its limit", stopped && t0.elapsed() < Duration::from_secs(6), (h.note("notes.md"), t0.elapsed()));

    // The same thread again, then undo.
    h.idle();
    h.t3.next(edit("notes.md", "A paragraph to keep.", "A paragraph to keep.\n\nhello", "Added hello."));
    h.ask("notes.md", "A paragraph to keep.\n^ add a line that says hello\n", "^ add a line");
    let hello = h.until("notes.md", 10.0, |t| t.lines().any(|l| l == "hello") && !t.contains('◌'));
    let launched = h.t3.lock().launched.len();
    h.check("another prompt in the file goes to its same thread", hello && launched == 1, (h.note("notes.md"), launched));
    h.idle();
    h.ask("notes.md", &format!("{}\n^/undo\n", h.note("notes.md").trim_end()), "^/undo");
    let undone = h.until("notes.md", 10.0, |t| !t.lines().any(|l| l == "hello") && t.contains("^> undid"));
    h.check("^/undo takes the agent's edit back", undone, h.note("notes.md"));

    // Undo puts back a longer paragraph above the command: the reply still
    // lands where ^/undo was written. (Found by using it in Zed.)
    h.idle();
    h.t3.next(edit("notes.md", "Line one of three,\nline two of three,\nline three of three.", "One line.", "Joined them."));
    h.ask("notes.md", "Line one of three,\nline two of three,\nline three of three.\n^ join these into one line\n\nThe end.\n", "^ join");
    h.until("notes.md", 10.0, |t| t.starts_with("One line.") && !t.contains('◌'));
    h.idle();
    h.ask("notes.md", "One line.\n\nThe end.\n\n^/undo\n", "^/undo");
    let placed = h.until("notes.md", 10.0, |t| t.contains("line three of three.\n\nThe end.\n\n^> undid"));
    h.check("after ^/undo grows the text above, its reply stays where it was written", placed, h.note("notes.md"));

    // A command pressed while the agent works is answered at once; ^/undo waits.
    h.idle();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    h.t3.next(move |_| {
        let _ = go_rx.recv_timeout(Duration::from_secs(8));
        said("Done waiting.")
    });
    h.ask("notes.md", "Wait for me.\n^ take your time\n", "^ take your time");
    h.until("notes.md", 10.0, |t| t.contains('◌'));
    // The status line is written before the message reaches T3; on a busy
    // machine that send can lag, so wait until the turn is running there.
    let t0 = Instant::now();
    while !h.t3.running() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(40));
    }
    h.ask("notes.md", &format!("{}\n\nMeanwhile.\n^/help\n\n^/undo\n", h.note("notes.md").trim_end()), "^/help");
    let mid = h.until("notes.md", 10.0, |t| t.contains("^> /help:"));
    let still = h.t3.running();
    h.check("a command during a turn is answered before the turn ends", mid && still, (h.note("notes.md"), still));
    h.fire("notes.md", "^/undo");
    let refused = h.until("notes.md", 10.0, |t| t.contains("^> undo: an agent is working in ~/notes"));
    h.check("^/undo during a turn in its folder waits for the turn to end", refused, h.note("notes.md"));
    let _ = go_tx.send(());
    h.idle();

    // The answer reaches the file when the agent leaves it only in the thread.
    h.t3.next(|_| said("The answer is 4."));
    h.ask("notes.md", "Two plus two.\n^ what is it?\n", "^ what is it?");
    let ans = h.until("notes.md", 10.0, |t| t.contains("Two plus two.\n^> The answer is 4.\n"));
    h.check("the thread's answer lands in the file when the file is untouched", ans, h.note("notes.md"));
    h.idle();
    h.t3.next(|t| {
        std::fs::write(t.folder.join("hi.sh"), "echo hi\n").unwrap();
        said("I wrote hi.sh.\n^> wrote hi.sh\n^> it prints hi")
    });
    h.ask("notes.md", "Make a script.\n^ write hi.sh\n", "^ write hi.sh");
    let both = h.until("notes.md", 10.0, |t| t.contains("^> wrote hi.sh\n^> it prints hi"));
    h.check("^> lines left only in the thread reach the file, even when the turn made a file", both, h.note("notes.md"));
    h.idle();
    h.t3.next(edit("notes.md", "Old words.", "New words.", "Changed the paragraph."));
    h.ask("notes.md", "Old words.\n^ change them\n", "^ change them");
    let quiet = h.until("notes.md", 10.0, |t| t.contains("New words.") && !t.contains('◌'));
    h.idle();
    h.check("a turn that edited and named its change adds no reply", quiet && !h.note("notes.md").contains("^>"), h.note("notes.md"));

    // Caret lines leave no growing gap.
    h.t3.next(|_| said(""));
    h.ask("notes.md", "a\n\n^ do nothing\n\nb\n", "^ do nothing");
    let gap = h.until("notes.md", 10.0, |t| !t.contains('^'));
    h.idle();
    h.check("a prompt between blank lines leaves one blank line, not two", gap && h.note("notes.md") == "a\n\nb\n", h.note("notes.md"));

    // When T3 is away, the prompt says so where it stood.
    h.t3.lock().down = true;
    h.ask("notes.md", "Anyone?\n^ hello\n", "^ hello");
    let away = h.until("notes.md", 20.0, |t| t.contains("did not reach the agent"));
    h.t3.lock().down = false;
    h.check("a T3 failure is written where the prompt stood", away, h.note("notes.md"));
    h.idle();
}

/// One thread per document, one turn at a time per folder, the hook, and
/// the coordinator.
fn documents(h: &mut H) {
    // A press while the folder is busy waits in line, and says so.
    let (go_tx, go_rx) = mpsc::channel::<()>();
    h.t3.next(move |_| {
        let _ = go_rx.recv_timeout(Duration::from_secs(8));
        said("^> first done")
    });
    let before = h.t3.lock().launched.len();
    h.ask("notes.md", "Busy.\n^ hold the folder\n", "^ hold the folder");
    h.until("notes.md", 10.0, |t| t.contains("· working"));
    h.t3.next(|_| said("^> second done"));
    let (ok, out) = h.ask("sub/b.md", "Another document.\n^ wait your turn\n", "^ wait your turn");
    let waiting = h.until("sub/b.md", 5.0, |t| t.contains("· wait your turn · waiting for p"));
    h.check("a press while its folder is busy waits in line, and its line says so", ok && out.contains("waits for p") && waiting,
        (&out, h.note("sub/b.md")));
    let now_launched = h.t3.lock().launched.len();
    h.check("nothing reaches the agent for it while it waits", now_launched == before, now_launched);
    let _ = go_tx.send(());
    let ran = h.until("sub/b.md", 10.0, |t| t.contains("^> second done") && !t.contains('◌'));
    h.check("it runs when the turn before it ends", ran && h.note("notes.md").contains("^> first done"), h.note("sub/b.md"));
    h.idle();
    let launched = h.t3.lock().launched.clone();
    h.check("a second document gets its own thread", launched.len() == before + 1 && launched.last().unwrap().0 == "Ambition · sub/b.md",
        launched.iter().map(|l| l.0.clone()).collect::<Vec<_>>());

    // The hook: 8 lines each way, what changed since, and `ambition more`.
    let mut long = String::new();
    for k in 1..=40 {
        long.push_str(&format!("Line {k} of the long note.\n"));
    }
    long.push_str("^ what is on line 2?\n");
    for k in 41..=60 {
        long.push_str(&format!("Line {k} of the long note.\n"));
    }
    h.t3.next(|_| said("^> seen"));
    h.ask("long.md", &long, "^ what is on line 2");
    h.until("long.md", 10.0, |t| t.contains("^> seen"));
    h.idle();
    let root = h.root.clone();
    let (tx, rx) = mpsc::channel();
    h.t3.next(move |t| {
        let mut c = Command::new(BIN);
        c.args(["more", "up", "30"]).current_dir(&t.folder);
        env(&root, &mut c);
        let up = c.output().unwrap();
        let mut c = Command::new(BIN);
        c.args(["more", "down"]).current_dir(&t.folder);
        env(&root, &mut c);
        let down = c.output().unwrap();
        tx.send((String::from_utf8_lossy(&up.stdout).to_string(), String::from_utf8_lossy(&down.stdout).to_string())).unwrap();
        said("^> looked")
    });
    let typed = long.replace("Line 5 of the long note.", "Line 5, typed before asking.").replace("^ what is on line 2?\n", "").replace(
        "Line 30 of the long note.\n", "Line 30 of the long note.\n^ and line 3?\n");
    h.ask("long.md", &typed, "^ and line 3");
    h.until("long.md", 10.0, |t| t.contains("^> looked"));
    h.idle();
    let m = h.t3.messages().last().cloned().unwrap_or_default();
    h.check("a press sends 8 lines each way and no more",
        m.contains("lines 23-39 of 61") && m.contains("23| Line 23 of") && m.contains("39| Line 38 of") && !m.contains("| Line 22 of") && !m.contains("| Line 39 of"),
        &m);
    h.check("and what changed in the document since its last turn", m.contains("+Line 5, typed before asking.") && !m.contains("-^> seen"), &m);
    h.check("and how to see more", m.contains("more up [n]` or `") && m.contains("more down [n]`"), &m);
    let (up, down) = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    h.check("ambition more up shows the lines above what was sent",
        up.contains("lines 1-22 of 61") && up.contains(" 1| Line 1 of") && up.contains("22| Line 22 of"), &up);
    h.check("ambition more down picks up below it", down.contains("lines 40-61 of 61") && down.contains("40| Line 39 of"), &down);

    // The coordinator: a thread asks it, and the answer comes back.
    let root = h.root.clone();
    let (tx, rx) = mpsc::channel();
    h.t3.next(move |t| {
        let mut c = Command::new(BIN);
        c.args(["ask", "coordinator", "who else is working here?"]).current_dir(&t.folder);
        env(&root, &mut c);
        let o = c.output().unwrap();
        tx.send(String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)).unwrap();
        said("^> asked")
    });
    let (ctx, crx) = mpsc::channel();
    h.t3.next(move |t| {
        ctx.send(t.message.clone()).unwrap();
        said("The thread for sub/b.md is.")
    });
    h.ask("notes.md", "Coordinate.\n^ ask the coordinator\n", "^ ask the coordinator");
    h.until("notes.md", 10.0, |t| t.contains("^> asked"));
    h.idle();
    let answer = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    h.check("ambition ask coordinator prints the coordinator's answer", answer.trim() == "The thread for sub/b.md is.", &answer);
    let first = crx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    h.check("the coordinator's thread is told what it is, who asked, and the question",
        first.contains("You are the coordinator") && first.contains("from the thread for ~/notes/notes.md") && first.contains("who else is working here?")
            && first.contains("~/.ambition/coordinator.md"), &first[..first.len().min(800)]);
    let coord = h.t3.lock().launched.iter().any(|l| l.0 == "Ambition · coordinator");
    h.check("the coordinator is a thread of its own", coord, "");
    let threads = std::fs::read_to_string(h.state.join("threads")).unwrap_or_default();
    h.check("~/.ambition/threads lists every document's thread",
        threads.contains("notes/notes.md") && threads.contains("notes/sub/b.md") && threads.contains("coordinator.md"), &threads);

    // The coordinator passes a message on.
    let root = h.root.clone();
    let (tx, rx) = mpsc::channel();
    let coordinator = h.state.join("coordinator.md");
    let b = h.dir.join("sub/b.md");
    h.t3.next(move |_| {
        let mut c = Command::new(BIN);
        c.args(["ask", &b.to_string_lossy(), "notes.md says hello"]).current_dir(root.join("state"));
        env(&root, &mut c);
        let o = c.output().unwrap();
        tx.send(String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)).unwrap();
        said("^> passed it on")
    });
    let (btx, brx) = mpsc::channel();
    h.t3.next(move |t| {
        btx.send((t.thread.clone(), t.message.clone())).unwrap();
        said("Hello back.")
    });
    std::fs::write(&coordinator, "# The coordinator\n\n^ tell b.md hello\n").unwrap();
    let mut c = Command::new(BIN);
    c.arg("fire").arg(format!("{}:3", coordinator.display()));
    h.env(&mut c);
    let o = c.output().unwrap();
    h.check("a press in ~/.ambition/coordinator.md needs no grant", o.status.success(), &o);
    let passed = rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    h.check("the coordinator passes a message on with ambition ask <document>", passed.contains("passed on to the thread for ~/notes/sub/b.md"), &passed);
    let (bthread, bmsg) = brx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let bt = serde_json::from_str::<Value>(&threads).unwrap_or_default()[b_key(h)]["thread"].as_str().unwrap_or("").to_string();
    h.check("and it reaches that document's own thread", bthread == bt && bmsg.contains("from the coordinator") && bmsg.contains("notes.md says hello"),
        (&bthread, &bt, &bmsg));
    h.idle();
    let mut c = Command::new(BIN);
    c.args(["ask", &h.dir.join("readme.md").to_string_lossy(), "hi"]);
    h.env(&mut c);
    let o = c.output().unwrap();
    h.check("a message cannot start a thread", !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("has no thread yet"), &o);
}

fn b_key(h: &H) -> String {
    h.dir.join("sub/b.md").to_string_lossy().to_string()
}

/// What only Ambition does.
fn ambition_only(h: &mut H) {
    // Models, through T3's own list and search.
    h.ask("notes.md", "Models.\n\n^/model\n", "^/model");
    let listed = h.until("notes.md", 10.0, |t| t.contains("Claude Sonnet 5.5") && t.contains("GPT-5.5") && !t.contains("Off 1"));
    h.check("^/model lists the enabled models T3 offers", listed, h.note("notes.md"));
    h.ask("notes.md", "Models.\n\n^/model opus\n", "^/model");
    let fits = h.until("notes.md", 10.0, |t| t.contains("fits 2 models") && t.contains("Nothing changed"));
    h.check("a name that fits several models lists them and changes nothing", fits, h.note("notes.md"));
    h.ask("notes.md", "Models.\n\n^/model opus 5.5 high\n", "^/model");
    let switched = h.until("notes.md", 10.0, |t| t.contains("^> model: Claude Opus 5.5 · high."));
    let (conf, dflt) = {
        let s = h.t3.lock();
        (s.configured.last().cloned().unwrap_or_default(), s.defaults.last().map(|c| c.1.clone()).unwrap_or_default())
    };
    let notes_thread = h.t3.lock().launched.iter().position(|l| l.0 == "Ambition · notes.md").map(|k| format!("thread-{}", k + 1)).unwrap_or_default();
    h.check("^/model opus 5.5 high switches this file's thread",
        switched && conf.0 == notes_thread && conf.1["model"] == "claude-opus-5-5" && conf.1["options"][0]["value"] == "high" && dflt == conf.1,
        (h.note("notes.md"), &conf, &dflt));
    h.ask("notes.md", "Models.\n\n^/model gpt ultra\n", "^/model");
    let refused = h.until("notes.md", 10.0, |t| t.contains("has no effort “ultra” (it takes low, medium, high, xhigh)"));
    h.check("an effort the model lacks is refused", refused, h.note("notes.md"));
    h.ask("notes.md", "Models.\n\n^/model haiku\n", "^/model");
    let found = h.until("notes.md", 10.0, |t| t.contains("^> model: Claude Haiku 4.5."));
    h.check("T3's search finds a model outside its short list", found, h.note("notes.md"));
    h.ask("notes.md", "Models.\n\n^/model sonnet\n\n^/model high\n", "^/model sonnet");
    h.until("notes.md", 10.0, |t| t.contains("^> model: Claude Sonnet 5.5."));
    h.fire("notes.md", "^/model high");
    let effort = h.until("notes.md", 10.0, |t| t.contains("^> model: Claude Sonnet 5.5 · high."));
    h.check("^/model high keeps the model and sets its effort", effort, h.note("notes.md"));
    h.t3.next(|_| said("^> new here"));
    h.ask("fresh.md", "A new document.\n^ hello\n", "^ hello");
    h.until("fresh.md", 10.0, |t| t.contains("^> new here"));
    h.idle();
    let model = h.t3.lock().launched.last().and_then(|l| l.2.clone()).unwrap_or_default();
    h.check("a new document's thread starts on the model chosen last", model["model"] == "claude-sonnet-5-5" && model["options"][0]["value"] == "high", model);

    // A changed AGENTS.md starts a new thread on the next prompt, and names it.
    let before = h.t3.lock().launched.len();
    h.save("AGENTS.md", "# Folder rules\n\nBe brief.\n");
    h.t3.next(|_| said("^> ok"));
    h.ask("notes.md", "Anything.\n^ say ok\n", "^ say ok");
    h.until("notes.md", 10.0, |t| t.contains("^> ok"));
    h.idle();
    let (launched, settled, first) = {
        let s = h.t3.lock();
        (s.launched.len(), s.settled.clone(), s.launched.last().map(|l| l.1.clone()).unwrap_or_default())
    };
    h.check("the next prompt starts a new thread and settles the old", launched == before + 1 && settled == [notes_thread.clone()], (launched, &settled));
    h.check("its handoff names the old thread and the AGENTS.md to read",
        first.contains(&format!("t3://thread/{notes_thread}")) && first.contains("- ~/notes/AGENTS.md") && first.contains("An AGENTS.md for this document changed"),
        &first[..first.len().min(1500)]);
    let model = h.t3.lock().launched.last().and_then(|l| l.2.clone()).unwrap_or_default();
    h.check("the new thread keeps the old thread's model", model["model"] == "claude-sonnet-5-5", model);

    // ^/new.
    h.ask("notes.md", "Fresh.\n\n^/new\n", "^/new");
    let said_new = h.until("notes.md", 10.0, |t| t.contains("^> new: this file's next prompt starts a new thread"));
    let now_launched = h.t3.lock().launched.len();
    h.check("^/new answers at once and starts nothing yet", said_new && now_launched == before + 1, h.note("notes.md"));
    h.t3.next(|_| said("^> fresh"));
    h.ask("notes.md", "Fresh.\n^ go\n", "^ go");
    h.until("notes.md", 10.0, |t| t.contains("^> fresh"));
    h.idle();
    let last = h.t3.lock().launched.last().map(|l| l.1.clone()).unwrap_or_default();
    let now_launched = h.t3.lock().launched.len();
    h.check("after ^/new the next prompt starts a thread", now_launched == before + 2 && last.contains("pressed `^/new`"), &last[..last.len().min(300)]);
    h.check("and its handoff quotes the document's history", last.contains("The end of this document's history") && last.contains("^/new"), &last);

    // ^/thread and ^/stop.
    let tid = format!("thread-{now_launched}");
    h.ask("notes.md", "Where?\n\n^/thread\n", "^/thread");
    let th = h.until("notes.md", 10.0, |t| t.contains(&format!("^> thread: t3://thread/{tid}")));
    h.check("^/thread gives the link and the model", th, h.note("notes.md"));
    let (go_tx, go_rx) = mpsc::channel::<()>();
    h.t3.next(move |_| {
        let _ = go_rx.recv_timeout(Duration::from_secs(8));
        said("too late")
    });
    h.ask("notes.md", "Slow.\n^ think forever\n", "^ think forever");
    h.until("notes.md", 10.0, |t| t.contains('◌'));
    std::thread::sleep(Duration::from_millis(300));
    h.ask("notes.md", &format!("{}\n\n^/stop\n", h.note("notes.md").trim_end()), "^/stop");
    let stopped = h.until("notes.md", 10.0, |t| t.contains("was stopped before it finished"));
    let _ = go_tx.send(());
    h.check("^/stop stops the turn, and the prompt says so", stopped, h.note("notes.md"));
    h.idle();

    // A caret in code, after a comment marker.
    h.t3.next(|t| {
        let p = t.folder.join("calc.py");
        let text = std::fs::read_to_string(&p).unwrap();
        let out: Vec<String> = text.lines().flat_map(|l| {
            if l.starts_with("# ^ ◌") { vec![l.to_string(), "# ^> it returns one".to_string()] } else { vec![l.to_string()] }
        }).collect();
        std::fs::write(&p, out.join("\n") + "\n").unwrap();
        said("^> it returns one")
    });
    h.ask("calc.py", "def f():\n    return 1\n# ^ what does f return?\n", "# ^ what");
    let code = h.until("calc.py", 10.0, |t| t.contains("    return 1\n# ^> it returns one\n") && !t.contains('◌'));
    h.check("a caret after # in code is a prompt, and its reply keeps the #", code, h.note("calc.py"));
    h.idle();

    // A stop cuts a turn off: on the next start its prompt is back.
    h.t3.next(|_| {
        std::thread::sleep(Duration::from_secs(8));
        said("never seen")
    });
    h.ask("notes.md", "Cut.\n^ this will be cut off\n", "^ this will be cut off");
    h.until("notes.md", 10.0, |t| t.contains('◌'));
    // Only once the turn has reached T3, or its script waits for the next turn.
    wait(10.0, || h.t3.running());
    h.restart();
    let back = h.until("notes.md", 10.0, |t| t.contains("Cut.\n^ this will be cut off\n^> p") && t.contains("was cut off when Ambition stopped"));
    h.check("a prompt cut off by a stop is put back, ready to press again", back, h.note("notes.md"));

    // Ambition kept to its own folder.
    let own = git(&h.dir, &["log", "--format=%s"]);
    let status = git(&h.dir, &["status", "--porcelain", "--ignored"]);
    h.check("the folder's own repo has only the user's commit", own.trim() == "the user's own commit", &own);
    h.check("Ambition keeps nothing in the folder", !h.dir.join(".ambition").exists() && !status.contains(".ambition"), &status);
    let log = h.log();
    h.check("the private history holds presses, turns, commands and undo",
        log.contains("Ambition: pressed p1 in notes.md") && log.contains("Ambition: agent answered") && log.contains("Ambition: command /help")
            && log.contains("Ambition: undo"), &log);
    let hist = h.history();
    h.check("the history log records prompts, commands and threads",
        hist.contains("^ make this shorter") && hist.contains("^/model opus 5.5 high") && hist.contains("· thread\nA new thread (agents)"), &hist[..hist.len().min(300)]);

    // One Ambition at a time, and no folder to watch.
    let mut c = Command::new(BIN);
    h.env(&mut c);
    let second = c.output().unwrap();
    h.check("a second Ambition stops at once", second.status.code() == Some(1) && String::from_utf8_lossy(&second.stderr).contains("already running"), &second);
    let mut c = Command::new(BIN);
    c.arg(&h.dir);
    h.env(&mut c);
    let old = c.output().unwrap();
    h.check("`ambition <folder>` says 0.2 watches no folder", old.status.code() == Some(2) && String::from_utf8_lossy(&old.stderr).contains("watches no folder"), &old);
}

/// A stand-in for an editor's language server: it holds buffers and takes
/// Ambition's edits the way `ambition lsp` does.
struct Ed {
    out: std::net::TcpStream,
    texts: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    log: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Ed {
    fn connect(state: &Path) -> Ed {
        use std::io::{BufRead, BufReader, Write};
        let text = std::fs::read_to_string(state.join("inbox")).unwrap();
        let (addr, token) = text.trim().split_once(' ').unwrap();
        let mut out = std::net::TcpStream::connect(addr).unwrap();
        writeln!(out, "{}", serde_json::json!({"op": "editor", "token": token, "client": "TestEd", "shows": true})).unwrap();
        let mut r = BufReader::new(out.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("\"ok\":true"), "{line}");
        let texts: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>> = Default::default();
        let log: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let (t2, l2, mut w) = (texts.clone(), log.clone(), out.try_clone().unwrap());
        std::thread::spawn(move || {
            for line in r.lines() {
                let Ok(line) = line else { break };
                let req: Value = serde_json::from_str(&line).unwrap();
                l2.lock().unwrap().push(req.clone());
                let mut t = t2.lock().unwrap();
                let reply = if let Some(p) = req["read"].as_str() {
                    serde_json::json!({"ok": true, "text": t.get(p).cloned().unwrap_or_default()})
                } else if let Some(p) = req["edit"].as_str() {
                    let cur = t.get(p).cloned().unwrap_or_default();
                    if Some(cur.as_str()) != req["old"].as_str() {
                        serde_json::json!({"ok": false, "text": "the buffer changed"})
                    } else {
                        let new = apply(&cur, req["edits"].as_array().unwrap());
                        t.insert(p.to_string(), new.clone());
                        serde_json::json!({"ok": true, "text": new})
                    }
                } else {
                    serde_json::json!({"ok": true})
                };
                let mut reply = reply;
                reply["id"] = req["id"].clone();
                writeln!(w, "{reply}").unwrap();
            }
        });
        Ed { out, texts, log }
    }

    fn open(&mut self, path: &Path, text: &str) {
        use std::io::Write;
        let p = std::fs::canonicalize(path).unwrap().display().to_string();
        self.texts.lock().unwrap().insert(p.clone(), text.to_string());
        writeln!(self.out, "{}", serde_json::json!({"open": p})).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }

    fn text(&self, path: &Path) -> String {
        let p = std::fs::canonicalize(path).unwrap().display().to_string();
        self.texts.lock().unwrap().get(&p).cloned().unwrap_or_default()
    }

    fn type_in(&self, path: &Path, text: &str) {
        let p = std::fs::canonicalize(path).unwrap().display().to_string();
        self.texts.lock().unwrap().insert(p, text.to_string());
    }
}

/// LSP edits, all on the same text, applied.
fn apply(text: &str, edits: &[Value]) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let at = |p: &Value| -> usize {
        let (l, c) = (p["line"].as_u64().unwrap() as usize, p["character"].as_u64().unwrap() as usize);
        let before: usize = lines.iter().take(l).map(|x| x.len() + 1).sum();
        let line = lines.get(l).copied().unwrap_or("");
        let mut n = 0;
        let mut b = line.len();
        for (i, ch) in line.char_indices() {
            if n >= c {
                b = i;
                break;
            }
            n += ch.len_utf16();
        }
        before + b
    };
    let mut spans: Vec<(usize, usize, String)> = edits.iter()
        .map(|e| (at(&e["range"]["start"]), at(&e["range"]["end"]), e["newText"].as_str().unwrap().to_string())).collect();
    spans.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut out = text.to_string();
    for (a, b, t) in spans {
        out.replace_range(a..b, &t);
    }
    out
}

/// The copy of a document the press message tells the agent to edit.
fn copy_named(message: &str) -> PathBuf {
    let i = message.find("edit its copy, ").expect("the message names a copy") + "edit its copy, ".len();
    let rest = &message[i..];
    let path = &rest[..rest.find(", and Ambition").unwrap()];
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(path.replacen('~', &home, 1))
}

/// A file open in an editor: the press reads the unsaved buffer, the agent
/// edits a copy, and only the changed range goes back into the buffer. The
/// file on disk is never written.
fn buffers(h: &mut H) {
    let mut ed = Ed::connect(&h.state);
    let disk = "# Essay\n\nAn old draft on disk.\n";
    h.save("essay.md", disk);
    let buffer = "# Essay\n\nWhen we make machines that love, the quik question is who they love.\n^ fix the typo\n\nThe end.\n";
    ed.open(&h.dir.join("essay.md"), buffer);
    let (tx, rx) = mpsc::channel();
    let home = h.root.join("home");
    h.t3.next(move |t| {
        let copy = copy_named(&t.message);
        let copy = if copy.starts_with("~") { home.join(copy.strip_prefix("~").unwrap()) } else { copy };
        let text = std::fs::read_to_string(&copy).unwrap_or_default();
        tx.send((t.message.clone(), text.clone())).unwrap();
        std::fs::write(&copy, text.replace("quik", "quick")).unwrap();
        said("Fixed the typo on line 3.")
    });
    let (ok, out) = h.fire_at("essay.md", 4);
    let essay = h.dir.join("essay.md");
    let landed = wait(10.0, || ed.text(&essay).contains("the quick question") && !ed.text(&essay).contains('◌'));
    h.check("a press in an open buffer reads it unsaved, and the answer lands in the buffer", ok && landed, (&out, ed.text(&essay)));
    let (msg, seen) = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    h.check("the agent is told the buffer is open and edits a copy of it, not the file",
        msg.contains("is open in TestEd, with changes not saved to disk") && seen.contains("^ ◌ p") && seen.contains("the quik question"), &msg);
    h.check("the file on disk is never written", h.note("essay.md") == disk, h.note("essay.md"));
    let edits: Vec<Value> = ed.log.lock().unwrap().iter().filter(|r| r["edit"].is_string()).cloned().collect();
    let last = edits.last().cloned().unwrap_or_default();
    let small = edits.iter().all(|e| e["edits"].as_array().is_some_and(|x| x.iter().all(|t| t["range"]["start"]["line"].as_u64() >= Some(2))));
    h.check("only the changed range goes back: the edits never touch the lines above",
        small && last["edits"][0] == serde_json::json!({"range": {"start": {"line": 2, "character": 37}, "end": {"line": 2, "character": 41}}, "newText": "quick"}), &last);
    h.idle();
    let n = h.t3.messages().len();
    let (ok, out) = h.fire_at("essay.md", 3);
    std::thread::sleep(Duration::from_millis(300));
    let shown = ed.log.lock().unwrap().iter().any(|r| r["show"].is_string());
    h.check("a second press on the unchanged buffer shows the change again and sends nothing",
        ok && out.contains("showed what Ambition wrote last") && shown && h.t3.messages().len() == n, &out);

    // A press with no mark: a request, its status just below the line.
    ed.type_in(&essay, "# Essay\n\nWhen we make machines that love, the quick question is who they love\n\nThe end.\n");
    let (tx, rx) = mpsc::channel();
    h.t3.next(move |t| {
        let copy = copy_named(&t.message);
        let text = std::fs::read_to_string(&copy).unwrap_or_default();
        tx.send((t.message.clone(), text.clone())).unwrap();
        std::fs::write(&copy, text.replace("who they love\n", "who they love.\n")).unwrap();
        said("Ended the sentence.")
    });
    let (ok, out) = h.fire_at("essay.md", 3);
    let done = wait(10.0, || ed.text(&essay).contains("who they love.\n") && !ed.text(&essay).contains('◌'));
    let (msg, seen) = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    h.check("a press with no mark is a request: its status goes just below the line, and the agent is told so",
        ok && done && msg.contains("A press with no mark in ~/notes/essay.md, on line 3: a request")
            && seen.contains("who they love\n^ ◌ p") , (&out, &msg[..msg.len().min(300)], &seen));
    h.check("and the text around it is left as it was", ed.text(&essay).ends_with("who they love.\n\nThe end.\n"), ed.text(&essay));
    h.idle();

    // Typing while the press waits behind another turn is kept: the turn
    // takes the buffer as it is when it starts, and the agent edits that.
    let (slow_tx, slow_rx) = mpsc::channel::<()>();
    h.t3.next(move |_| {
        let _ = slow_rx.recv_timeout(Duration::from_secs(10));
        said("^> slow done")
    });
    h.t3.next(move |t| {
        let copy = copy_named(&t.message);
        let text = std::fs::read_to_string(&copy).unwrap_or_default();
        std::fs::write(&copy, text.replace("The end\n", "The very end.\n")).unwrap();
        said("Done.")
    });
    h.save("slow.md", "^ take your time\n");
    h.fire("slow.md", "^ take your time");
    wait(10.0, || h.t3.running());
    let changed = ed.text(&essay).replace("The end.", "The end");
    ed.type_in(&essay, &changed);
    let (ok, out) = h.fire_at("essay.md", 5);
    let waiting = wait(5.0, || ed.text(&essay).contains("· waiting for"));
    let typed = format!("{}More of my own words.\n", ed.text(&essay));
    ed.type_in(&essay, &typed);
    slow_tx.send(()).unwrap();
    let done = wait(10.0, || ed.text(&essay).contains("The very end.") && !ed.text(&essay).contains('◌'));
    h.idle();
    h.check("typing while a press waits is kept, and the agent's edit lands beside it",
        ok && waiting && done && ed.text(&essay).ends_with("The very end.\nMore of my own words.\n"), (&out, ed.text(&essay)));

    // Typing after the agent has the press is never overwritten: nothing is
    // written, and Ambition says why (a notice on a Mac; the log everywhere).
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    h.t3.next(move |t| {
        let copy = copy_named(&t.message);
        let text = std::fs::read_to_string(&copy).unwrap_or_default();
        std::fs::write(&copy, text.replace("More of my own words.", "More of their words.")).unwrap();
        started_tx.send(()).unwrap();
        let _ = go_rx.recv_timeout(Duration::from_secs(10));
        said("Done.")
    });
    std::fs::write(h.root.join("dialog.told"), "").unwrap();
    let before = ed.text(&essay).replace("words.\n", "words\n");
    ed.type_in(&essay, &before);
    let (ok, _) = h.fire_at("essay.md", 6);
    let started = started_rx.recv_timeout(Duration::from_secs(10)).is_ok();
    let typed = format!("{}And then some.\n", ed.text(&essay));
    ed.type_in(&essay, &typed);
    go_tx.send(()).unwrap();
    h.idle();
    let told = h.output().contains("told: p") && h.output().contains("is ready, but not in TestEd");
    let shown = !cfg!(target_os = "macos")
        || std::fs::read_to_string(h.root.join("dialog.told")).unwrap_or_default().contains("is ready, but not in TestEd");
    h.check("typing during the turn is never overwritten: nothing is written, and Ambition says why",
        ok && started && ed.text(&essay) == typed && told && shown, (ed.text(&essay), told, shown));
    h.save("essay.md", disk);
}

/// Marks are config lines, and new threads start on the last model.
fn marks_and_models(h: &mut H) {
    let cfg = h.state.join("config");
    let before = std::fs::read_to_string(&cfg).unwrap_or_default();
    std::fs::write(&cfg, format!("{before}ask = >>\nreply = <<\ncommand = !!\n")).unwrap();
    h.ask("marks.md", "Some text.\n\n!!help\n", "!!help");
    let helped = h.until("marks.md", 10.0, |t| t.contains("<< /help:"));
    h.check("marks set in the config are the marks: !!help is a command and its reply is <<", helped, h.note("marks.md"));
    std::fs::write(&cfg, before).unwrap();

    // A prompt on the last line, with no newline after it, under a blank line.
    let (tx, rx) = mpsc::channel();
    h.t3.next(move |t| {
        tx.send(std::fs::read_to_string(t.folder.join("edge.md")).unwrap_or_default()).unwrap();
        said("^> ok")
    });
    h.ask("edge.md", "Title\n\n^ say ok", "^ say ok");
    h.until("edge.md", 10.0, |t| t.contains("^> ok"));
    h.idle();
    let seen = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    h.check("a last-line prompt's status takes its place exactly, and the blank line above stays",
        seen.starts_with("Title\n\n^ ◌ p") && !seen.ends_with('\n') && h.note("edge.md") == "Title\n\n^> ok", (&seen, h.note("edge.md")));

    let models = h.state.join("models.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&models).unwrap()).unwrap();
    let started = m["started"].clone();
    m.as_object_mut().unwrap().remove("last");
    std::fs::write(&models, m.to_string()).unwrap();
    h.t3.next(|_| said("^> fine"));
    h.ask("fresh.md", "^ hello\n", "^ hello");
    h.until("fresh.md", 10.0, |t| t.contains("^> fine"));
    h.idle();
    let launched = h.t3.lock().launched.clone();
    let last = launched.last().cloned().unwrap();
    h.check("with no model chosen, a new thread starts on the last thread's model, never T3's first",
        started.is_object() && last.2.as_ref().map(|v| v["model"].clone()) == Some(started["model"].clone()), (&last, &started));
}

#[test]
fn ambition_end_to_end() {
    let mut h = H::new();
    grants(&mut h);
    core(&mut h);
    documents(&mut h);
    buffers(&mut h);
    ambition_only(&mut h);
    marks_and_models(&mut h);
    h.finish();
}
