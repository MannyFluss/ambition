//! End to end: the real `ambition` binary, a folder in a scratch home, and a
//! fake T3 (`fake_t3`). Each step writes a file the way an editor saves it,
//! and waits for what should happen in the file.
//!
//! The core steps were first written against the app Ambition replaced, and
//! it passed them too: they are the behavior Ambition was held to.

mod fake_t3;

use fake_t3::{said, Fake, Outcome};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};


struct H {
    root: PathBuf,
    dir: PathBuf,
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
        let dir = std::fs::canonicalize(&dir).unwrap();
        let t3home = root.join("t3");
        let t3 = Fake::start(&t3home);
        // The folder is its own repo, to show Ambition never touches it.
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("readme.md"), "Mine.\n").unwrap();
        git(&dir, &["add", "readme.md"]);
        git(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "the user's own commit"]);
        let out = std::fs::File::create(root.join("out.txt")).unwrap();
        let err = std::fs::File::create(root.join("err.txt")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_ambition"))
            .arg(&dir)
            .env("HOME", &home)
            .env("T3CODE_HOME", &t3home)
            .env("AMBITION_COMMAND_TIMEOUT", "2")
            .stdout(out)
            .stderr(err)
            .spawn()
            .unwrap();
        let h = H { root, dir, t3, child, fails: Vec::new() };
        let state = h.state_path();
        assert!(wait(10.0, || state.exists()), "the app never started: {}", h.output());
        std::thread::sleep(Duration::from_millis(700)); // let the watcher settle in
        h
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(".ambition/state.json")
    }

    fn commands(&self) -> PathBuf {
        self.dir.join(".ambition/commands")
    }

    fn history(&self) -> String {
        std::fs::read_to_string(self.dir.join(".ambition/history.md")).unwrap_or_default()
    }

    /// The log of Ambition's own history.
    fn log(&self) -> String {
        let args: Vec<String> = vec![format!("--git-dir={}", self.dir.join(".ambition/git").display()), "log".into(), "--format=%s".into()];
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

    /// Wait until no turn is running and the app says so.
    fn idle(&self) {
        let p = self.state_path();
        wait(10.0, || {
            !self.t3.running()
                && std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).is_some_and(|v| v["run"].is_null())
        });
        std::thread::sleep(Duration::from_millis(500));
    }

    fn command(&self, name: &str, body: &str) {
        let d = self.commands();
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

/// The core: what a save does.
fn core(h: &mut H) {
    let status = "^ ◌ p1 · make this shorter · working";
    // 1. A first prompt: a thread, a status line, an edit, one commit.
    let (tx, rx) = mpsc::channel();
    h.t3.next(move |t| {
        let p = t.folder.join("notes.md");
        let text = std::fs::read_to_string(&p).unwrap();
        tx.send(text.clone()).unwrap();
        std::fs::write(&p, text.replace("A long rambling paragraph that goes on and on.", "Short.")).unwrap();
        said("Shortened the paragraph on line 3.")
    });
    h.save("notes.md", "# Notes\n\nA long rambling paragraph that goes on and on.\n^ make this shorter\n\nThe end.\n");
    let landed = h.until("notes.md", 10.0, |t| t.contains("Short.") && !t.contains('◌'));
    h.check("a first prompt: the agent's edit lands and its status line goes", landed, h.note("notes.md"));
    let seen = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    h.check("the agent saw the status line where the prompt stood", seen.contains(status) && !seen.contains("^ make this"), &seen);
    let msgs = h.t3.messages();
    let launched = h.t3.lock().launched.len();
    h.check("the first prompt starts one thread", launched == 1 && msgs.len() == 1, (launched, msgs.len()));
    let m0 = msgs.first().cloned().unwrap_or_default();
    let opening = m0.starts_with("[Ambition] A new thread starts here") && m0.contains("# Ambition") && m0.contains("## Your answer goes in the file");
    h.check("its first message is the handoff, then the save", opening, &m0[..m0.len().min(400)]);
    h.check("the message quotes the paragraph above the caret, numbered",
        m0.contains("3| A long rambling paragraph") && m0.contains("line 3"), &m0[m0.len().saturating_sub(900)..]);
    h.idle();
    h.check("the turn is one commit", h.log().lines().next().unwrap_or("").contains("agent answered p1 in notes.md"), h.log());

    // 2. A save with no caret clears replies and starts nothing.
    let n = h.t3.messages().len();
    h.save("notes.md", "# Notes\n\nShort.\n^> an old reply\n^> its second line\n\nThe end.\n");
    let cleared = h.until("notes.md", 5.0, |t| !t.contains("^>"));
    h.check("a plain save clears the replies and starts nothing", cleared && h.t3.messages().len() == n, h.note("notes.md"));

    // 3. Commands, at once, without the agent.
    h.idle();
    h.save("notes.md", "# Notes\n\nShort.\n\n^/help\n");
    let helped = h.until("notes.md", 6.0, |t| t.contains("^> /help:") && t.contains("^> /undo:"));
    h.check("^/help lists the built-ins", helped, h.note("notes.md"));
    h.check("a command never reaches the agent", h.t3.messages().len() == n, h.t3.messages().len());
    h.idle();
    h.command("count.sh", "#!/bin/sh\n# Count the words of the paragraph above.\nwc -w | tr -d ' '\n");
    let (fenv, lenv) = ("AMBITION_FILE", "AMBITION_LINE");
    h.command("echo.sh", &format!("#!/bin/sh\n# Echo its words.\necho \"args: $* | file: ${fenv} | line: ${lenv}\"\n"));
    h.command("slow.sh", "#!/bin/sh\n# Take too long.\nsleep 6\necho late\n");
    h.save("notes.md", "One two three four five.\n^/count\n\n^/echo a b c\n\n# ^/echo nope\n");
    let counted = h.until("notes.md", 6.0, |t| t.contains("One two three four five.\n^> 5\n"));
    h.check("^/count runs the script on the paragraph above", counted, h.note("notes.md"));
    let echoed = h.until("notes.md", 3.0, |t| t.contains("^> args: a b c | file: notes.md | line: 4"));
    h.check("^/echo gets its words, the file and the line", echoed, h.note("notes.md"));
    h.check("a ^/ line after # in a note is text, not a command", h.note("notes.md").contains("# ^/echo nope"), h.note("notes.md"));
    h.idle();
    h.save("notes.md", "Text.\n\n^/slwo\n");
    let near = h.until("notes.md", 6.0, |t| t.contains("Did you mean ^/slow?"));
    h.check("an unknown command says so and suggests one", near, h.note("notes.md"));
    h.idle();
    let t0 = Instant::now();
    h.save("notes.md", "Text.\n\n^/slow\n");
    let stopped = h.until("notes.md", 8.0, |t| t.contains("(stopped after 2 seconds)"));
    h.check("a slow command is stopped at its limit", stopped && t0.elapsed() < Duration::from_secs(6), (h.note("notes.md"), t0.elapsed()));

    // 4. The same thread again, then undo.
    h.idle();
    h.t3.next(edit("notes.md", "A paragraph to keep.", "A paragraph to keep.\n\nhello", "Added hello."));
    h.save("notes.md", "A paragraph to keep.\n^ add a line that says hello\n");
    let hello = h.until("notes.md", 10.0, |t| t.lines().any(|l| l == "hello") && !t.contains('◌'));
    let launched = h.t3.lock().launched.len();
    let sent = h.t3.lock().sent.len();
    h.check("a second prompt goes to the same thread", hello && launched == 1 && sent == 1, (h.note("notes.md"), launched, sent));
    h.idle();
    h.save("notes.md", format!("{}\n^/undo\n", h.note("notes.md").trim_end()).as_str());
    let undone = h.until("notes.md", 6.0, |t| !t.lines().any(|l| l == "hello") && t.contains("^> undid"));
    h.check("^/undo takes the agent's edit back", undone, h.note("notes.md"));

    // Undo puts back a longer paragraph above the command: the reply still
    // lands where ^/undo was written. (Found by using it in Zed.)
    h.idle();
    h.t3.next(edit("notes.md", "Line one of three,\nline two of three,\nline three of three.", "One line.", "Joined them."));
    h.save("notes.md", "Line one of three,\nline two of three,\nline three of three.\n^ join these into one line\n\nThe end.\n");
    h.until("notes.md", 10.0, |t| t.starts_with("One line.") && !t.contains('◌'));
    h.idle();
    h.save("notes.md", "One line.\n\nThe end.\n\n^/undo\n");
    let placed = h.until("notes.md", 6.0, |t| t.contains("line three of three.\n\nThe end.\n\n^> undid"));
    h.check("after ^/undo grows the text above, its reply stays where it was written", placed, h.note("notes.md"));

    // 5. A command written while the agent works is answered at once.
    h.idle();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    h.t3.next(move |_| {
        let _ = go_rx.recv_timeout(Duration::from_secs(8));
        said("Done waiting.")
    });
    h.save("notes.md", "Wait for me.\n^ take your time\n");
    h.until("notes.md", 6.0, |t| t.contains('◌'));
    let during = format!("{}\n\nMeanwhile.\n^/help\n", h.note("notes.md").trim_end());
    h.save("notes.md", &during);
    let mid = h.until("notes.md", 6.0, |t| t.contains("^> /help:"));
    let still = h.t3.running();
    h.check("a command during a run is answered before the run ends", mid && still, (h.note("notes.md"), still));
    let _ = go_tx.send(());
    h.idle();

    // 6. The answer reaches the file when the agent leaves it only in the thread.
    h.t3.next(|_| said("The answer is 4."));
    h.save("notes.md", "Two plus two.\n^ what is it?\n");
    let ans = h.until("notes.md", 8.0, |t| t.contains("Two plus two.\n^> The answer is 4.\n"));
    h.check("the thread's answer lands in the file when the file is untouched", ans, h.note("notes.md"));
    h.idle();
    h.t3.next(|t| {
        std::fs::write(t.folder.join("hi.sh"), "echo hi\n").unwrap();
        said("I wrote hi.sh.\n^> wrote hi.sh\n^> it prints hi")
    });
    h.save("notes.md", "Make a script.\n^ write hi.sh\n");
    let both = h.until("notes.md", 8.0, |t| t.contains("^> wrote hi.sh\n^> it prints hi"));
    h.check("^> lines left only in the thread reach the file, even when the turn made a file", both, h.note("notes.md"));
    h.idle();
    h.t3.next(edit("notes.md", "Old words.", "New words.", "Changed the paragraph."));
    h.save("notes.md", "Old words.\n^ change them\n");
    let quiet = h.until("notes.md", 8.0, |t| t.contains("New words.") && !t.contains('◌'));
    h.idle();
    h.check("a turn that edited and named its change adds no reply", quiet && !h.note("notes.md").contains("^>"), h.note("notes.md"));

    // 7. Caret lines leave no growing gap.
    h.t3.next(|_| said(""));
    h.save("notes.md", "a\n\n^ do nothing\n\nb\n");
    let gap = h.until("notes.md", 8.0, |t| !t.contains('^'));
    h.idle();
    h.check("a prompt between blank lines leaves one blank line, not two", gap && h.note("notes.md") == "a\n\nb\n", h.note("notes.md"));
    h.save("notes.md", "a\n\n^> r\n\nb\n");
    let gap2 = h.until("notes.md", 5.0, |t| !t.contains('^'));
    h.check("a cleared reply between blank lines leaves one blank line, not two", gap2 && h.note("notes.md") == "a\n\nb\n", h.note("notes.md"));

    // 8. When T3 is away, the prompt says so where it stood.
    h.idle();
    h.t3.lock().down = true;
    h.save("notes.md", "Anyone?\n^ hello\n");
    let away = h.until("notes.md", 20.0, |t| t.contains("p") && t.contains("did not reach the agent"));
    h.t3.lock().down = false;
    h.check("a T3 failure is written where the prompt stood", away, h.note("notes.md"));
    h.idle();
}

/// What only Ambition does.
fn ambition_only(h: &mut H) {
    // Models, through T3's own list and search.
    h.save("notes.md", "Models.\n\n^/model\n");
    let listed = h.until("notes.md", 6.0, |t| t.contains("Claude Sonnet 5.5") && t.contains("GPT-5.5") && !t.contains("Off 1"));
    h.check("^/model lists the enabled models T3 offers", listed, h.note("notes.md"));
    h.idle();
    h.save("notes.md", "Models.\n\n^/model opus\n");
    let fits = h.until("notes.md", 6.0, |t| t.contains("fits 2 models") && t.contains("Nothing changed"));
    h.check("a name that fits several models lists them and changes nothing", fits, h.note("notes.md"));
    h.idle();
    h.save("notes.md", "Models.\n\n^/model opus 5.5 high\n");
    let switched = h.until("notes.md", 6.0, |t| t.contains("^> model: Claude Opus 5.5 · high."));
    let conf = h.t3.lock().configured.last().map(|c| c.1.clone()).unwrap_or_default();
    let dflt = h.t3.lock().defaults.last().map(|c| c.1.clone()).unwrap_or_default();
    h.check("^/model opus 5.5 high switches the thread and new threads",
        switched && conf["model"] == "claude-opus-5-5" && conf["options"][0]["value"] == "high" && dflt == conf, (h.note("notes.md"), conf, dflt));
    h.idle();
    h.save("notes.md", "Models.\n\n^/model gpt ultra\n");
    let refused = h.until("notes.md", 6.0, |t| t.contains("has no effort “ultra” (it takes low, medium, high, xhigh)"));
    h.check("an effort the model lacks is refused", refused, h.note("notes.md"));
    h.idle();
    h.save("notes.md", "Models.\n\n^/model haiku\n");
    let found = h.until("notes.md", 6.0, |t| t.contains("^> model: Claude Haiku 4.5."));
    h.check("T3's search finds a model outside its short list", found, h.note("notes.md"));
    h.idle();
    h.save("notes.md", "Models.\n\n^/model sonnet\n\n^/model high\n");
    let effort = h.until("notes.md", 8.0, |t| t.contains("^> model: Claude Sonnet 5.5 · high."));
    h.check("^/model high keeps the model and sets its effort", effort, h.note("notes.md"));
    h.idle();

    // A changed AGENTS.md starts a new thread on the next prompt, with its diff.
    let before = h.t3.lock().launched.len();
    h.save("AGENTS.md", "# Folder rules\n\nBe brief.\n");
    std::thread::sleep(Duration::from_millis(900));
    let now_launched = h.t3.lock().launched.len();
    h.check("saving AGENTS.md alone starts nothing", now_launched == before, now_launched);
    h.t3.next(|_| said("^> ok"));
    h.save("notes.md", "Anything.\n^ say ok\n");
    h.until("notes.md", 8.0, |t| t.contains("^> ok"));
    h.idle();
    let (launched, settled, first) = {
        let s = h.t3.lock();
        (s.launched.len(), s.settled.clone(), s.launched.last().map(|l| l.1.clone()).unwrap_or_default())
    };
    h.check("the next prompt starts a new thread and settles the old", launched == before + 1 && settled == ["thread-1"], (launched, &settled));
    h.check("its handoff names the old thread and the AGENTS.md diff",
        first.contains("t3://thread/thread-1") && first.contains("+Be brief.") && first.contains("## Your answer goes in the file"), &first[..first.len().min(1500)]);
    let model = h.t3.lock().launched.last().and_then(|l| l.2.clone()).unwrap_or_default();
    h.check("the new thread keeps the old thread's model", model["model"] == "claude-sonnet-5-5", model);

    // ^/new.
    h.save("notes.md", "Fresh.\n\n^/new\n");
    let said_new = h.until("notes.md", 6.0, |t| t.contains("^> new: the next ^ prompt starts a new thread"));
    let now_launched = h.t3.lock().launched.len();
    h.check("^/new answers at once and starts nothing yet", said_new && now_launched == before + 1, h.note("notes.md"));
    h.idle();
    h.t3.next(|_| said("^> fresh"));
    h.save("notes.md", "Fresh.\n^ go\n");
    h.until("notes.md", 8.0, |t| t.contains("^> fresh"));
    h.idle();
    let last = h.t3.lock().launched.last().map(|l| l.1.clone()).unwrap_or_default();
    let now_launched = h.t3.lock().launched.len();
    h.check("after ^/new the next prompt starts a thread", now_launched == before + 2 && last.contains("wrote `^/new`"), &last[..last.len().min(300)]);

    // ^/thread and ^/stop.
    h.save("notes.md", "Where?\n\n^/thread\n");
    let th = h.until("notes.md", 6.0, |t| t.contains("^> thread: t3://thread/thread-3"));
    h.check("^/thread gives the link and the model", th, h.note("notes.md"));
    h.idle();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    h.t3.next(move |_| {
        let _ = go_rx.recv_timeout(Duration::from_secs(8));
        said("too late")
    });
    h.save("notes.md", "Slow.\n^ think forever\n");
    h.until("notes.md", 6.0, |t| t.contains('◌'));
    h.save("notes.md", &format!("{}\n\n^/stop\n", h.note("notes.md").trim_end()));
    let stopped = h.until("notes.md", 8.0, |t| t.contains("was stopped before it finished"));
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
    h.save("calc.py", "def f():\n    return 1\n# ^ what does f return?\n");
    let code = h.until("calc.py", 8.0, |t| t.contains("    return 1\n# ^> it returns one\n") && !t.contains('◌'));
    h.check("a caret after # in code is a prompt, and its reply keeps the #", code, h.note("calc.py"));
    h.idle();
    let m = h.t3.messages().last().cloned().unwrap_or_default();
    h.check("the message lists the commands run since the last turn", m.contains("^/stop in notes.md"), &m);

    // The message carries what was typed since, and the commands run.
    h.t3.next(|_| said("^> seen"));
    h.save("notes.md", "Typed before asking.\n\nA line typed before asking.\n^ what did I type?\n");
    h.until("notes.md", 8.0, |t| t.contains("^> seen"));
    h.idle();
    let m = h.t3.messages().last().cloned().unwrap_or_default();
    h.check("the message holds what was typed since the last turn", m.contains("+A line typed before asking."), &m);

    // A long file: only the lines around the caret are quoted.
    let mut long = String::new();
    for k in 1..=120 {
        long.push_str(&format!("Paragraph number {k} of a long note.\n\n"));
    }
    long.push_str("^ shorten the last one\n");
    h.t3.next(|_| said("^> done"));
    h.save("long.md", &long);
    h.until("long.md", 8.0, |t| t.contains("^> done"));
    h.idle();
    let m = h.t3.messages().last().cloned().unwrap_or_default();
    h.check("a long file: only the lines around the caret are quoted",
        m.contains("| Paragraph number 120 of a long note.") && !m.contains("| Paragraph number 50 of a long note."), &m[..m.len().min(600)]);

    // Ambition kept to its own history.
    let own = git(&h.dir, &["log", "--format=%s"]);
    let status = git(&h.dir, &["status", "--porcelain"]);
    h.check("the folder's own repo has only the user's commit", own.trim() == "the user's own commit", &own);
    h.check("the folder's own repo never sees .ambition/", !status.contains(".ambition"), &status);
    let log = h.log();
    h.check("the private history holds saves, turns, commands and undo",
        log.contains("Ambition: saved notes.md") && log.contains("Ambition: agent answered") && log.contains("Ambition: command /help")
            && log.contains("Ambition: undo"), &log);
    let hist = h.history();
    h.check("history.md records prompts, commands and threads",
        hist.contains("^ make this shorter") && hist.contains("^/model opus 5.5 high") && hist.contains("· thread\nA new thread (agents)"), &hist[..hist.len().min(300)]);
    let st: Value = serde_json::from_str(&std::fs::read_to_string(h.state_path()).unwrap_or_default()).unwrap_or_default();
    h.check("state.json says no run is going, and names the model", st["run"].is_null() && st["model"].as_str().is_some(), st);

    // One Ambition per folder.
    let second = Command::new(env!("CARGO_BIN_EXE_ambition")).arg(&h.dir).env("HOME", h.root.join("home"))
        .env("T3CODE_HOME", h.root.join("t3")).output().unwrap();
    h.check("a second Ambition on the same folder stops at once",
        second.status.code() == Some(1) && String::from_utf8_lossy(&second.stderr).contains("already watching"), second);
    let cfg = std::fs::read_to_string(h.root.join("home/.config/ambition")).unwrap_or_default();
    h.check("the first run wrote the one-line config", cfg.trim() == "~/notes", &cfg);
}

#[test]
fn ambition_end_to_end() {
    let mut h = H::new();
    core(&mut h);
    ambition_only(&mut h);
    h.finish();
}
