//! One folder, watched. A save does nothing unless the file holds a caret
//! line. Then, on that save:
//!
//! 1. Old `^>` replies in the saved files are cleared.
//! 2. Every `^/command` and `^ prompt` leaves the file, and the save is
//!    committed to the private history without them ("Ambition: saved …").
//! 3. Commands run at once, in code (`commands.rs`), `^/undo` first. Their
//!    output takes the command's place as `^>` lines. A command written while
//!    the agent works runs at once too.
//! 4. A status line goes where each prompt stood: `^ ◌ p3 · <its words> · working`.
//! 5. The prompts go to the folder's thread as one message that carries the
//!    writer's attention: each prompt, where its caret is, the file around
//!    it, and what they changed since the last turn.
//! 6. When the turn ends, the status lines go, and the turn is one commit
//!    ("Ambition: agent answered …"), so `^/undo` takes it back.

use crate::carets as C;
use crate::commands as Cm;
use crate::diff;
use crate::engine::{Engine, Model, Selection, Status};
use crate::folder::{Folder, COMMANDS, HISTORY, NOTES};
use crate::util::{self, now};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const RULES: &str = include_str!("../RULES.md");

/// What the agent is shown of where the writer is looking. Ambition uses
/// `Around`. The other three stay only so the experiment that chose it,
/// `examples/context.rs`, can be run again. On 2026-11-16, 16 prompts twice
/// each: with Claude Sonnet 5.5 all four got all 32 runs right; with Claude
/// Haiku 5.5, Above missed the sentence that goes between two paragraphs
/// (31 of 32), and the rest got all 32, Pointer at the slowest (17.7 s a turn
/// against about 10). Whole sends the whole file, which grows with it;
/// Around stays small.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Context {
    /// The paragraph above the caret is what "this" means; the file around it.
    Above,
    /// The lines above and below the caret, with no default target.
    Around,
    /// Only the file and the line; the agent reads the file itself.
    Pointer,
    /// The whole file, numbered.
    Whole,
}

impl Context {
    /// The rules' section on where the writer is looking.
    pub fn attention(self) -> &'static str {
        match self {
            Context::Above => ATTENTION_ABOVE,
            Context::Around | Context::Whole => ATTENTION_AROUND,
            Context::Pointer => ATTENTION_POINTER,
        }
    }
}

const ATTENTION_ABOVE: &str = "\
The caret is where they are looking. A prompt acts on the paragraph just
above it: \"this\" means that paragraph. Under a list in a note, it means the
last list item, not the whole list, unless their words say otherwise (\"this
list\", \"these\"). A prompt on the first line of a file is about the whole
file. Lines in a row that start with `^` are one prompt. In a code file the
caret may follow a comment marker: `# ^ ...` or `// ^ ...`.

Each message gives you, for every prompt, its file and line, its words, and
the paragraph just above it by line numbers. It also shows the file as it is
on disk, with line numbers (all of a short file, else the lines around the
caret), and what they changed in the folder since your last turn. Edit from
that text: the Edit tool works on these files without a Read first. When a
prompt needs more (another file, the history, the web), go and get it.";

const ATTENTION_AROUND: &str = "\
The caret, the `^` that starts a prompt, is where they are looking, and what
they mean is around it: the text just above the caret and just below it.
Read their words against both. \"This\" is often the paragraph above, but a
prompt can point down (\"fill in the table\", \"the function below\") or at the
caret's own spot (\"a sentence that joins these\", \"write this section\" under a
heading). Where their words point, act there. A prompt on the first line of a
file is about the whole file. Lines in a row that start with `^` are one
prompt. In a code file the caret may follow a comment marker: `# ^ ...` or
`// ^ ...`.

Each message gives you, for every prompt, its file and line, its words, and
the paragraphs just above and below its caret by line numbers. It also shows
the file as it is on disk, with line numbers (all of a short file, else the
lines around the caret), and what they changed in the folder since your last
turn. Edit from that text: the Edit tool works on these files without a Read
first. When a prompt needs more (another file, the history, the web), go and
get it.";

const ATTENTION_POINTER: &str = "\
The caret is where they are looking, and what they mean is around it: the
text just above the caret and just below it. Read their words against both.
A prompt on the first line of a file is about the whole file. Lines in a row
that start with `^` are one prompt. In a code file the caret may follow a
comment marker: `# ^ ...` or `// ^ ...`.

Each message names, for every prompt, its file, the line its caret is on,
and its words. Read the file around that line before you act. Each message
also says what they changed in the folder since your last turn.";

const QUIET: Duration = Duration::from_millis(400);
const WHOLE_LINES: usize = 80;
const WHOLE_CHARS: usize = 6000;
const AROUND: usize = 12;
const LINE_CUT: usize = 600;
const DIFF_CHARS: usize = 3000;
const EFFORT_WORDS: [&str; 9] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra", "ultrathink"];

#[derive(Default)]
struct State {
    working: bool,
    /// Files saved since the last pass: their replies are cleared.
    saved: BTreeSet<String>,
    /// Files that may hold carets.
    carets: BTreeSet<String>,
    /// Files that changed while Ambition was at work.
    dirty: BTreeSet<String>,
    /// Files with a command written during a run.
    mid: BTreeSet<String>,
    /// The prompt ids of the run going now.
    run: Option<Vec<String>>,
}

/// One prompt on its way to the agent.
#[derive(Clone, Debug)]
pub struct Ask {
    pub id: String,
    pub rel: String,
    pub p: C::Prompt,
    /// Where it stood in the file once the caret lines were out.
    pub pos: usize,
}

pub struct App {
    pub f: Folder,
    pub engine: Arc<dyn Engine>,
    pub context: Context,
    pub run_limit: Duration,
    st: Mutex<State>,
    known: Mutex<HashMap<String, String>>,
    told: Mutex<Vec<String>>,
    /// Held for each of the app's own read-modify-writes of a file.
    io: Mutex<()>,
    kick: Mutex<Option<Sender<()>>>,
    mid_kick: Mutex<Option<Sender<()>>>,
    ignored: Mutex<(BTreeSet<String>, BTreeSet<String>)>,
    /// Held while finding or making the engine's project, so it happens once.
    project: Mutex<()>,
}

pub fn log(msg: &str) {
    println!("{} {msg}", now());
}

impl App {
    pub fn new(folder: Folder, engine: Arc<dyn Engine>) -> App {
        let run_limit = std::env::var("AMBITION_RUN_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(1800.0);
        App {
            f: folder,
            engine,
            context: Context::Around,
            run_limit: Duration::from_secs_f64(run_limit),
            st: Mutex::new(State::default()),
            known: Mutex::new(HashMap::new()),
            told: Mutex::new(Vec::new()),
            io: Mutex::new(()),
            kick: Mutex::new(None),
            mid_kick: Mutex::new(None),
            ignored: Mutex::new(Default::default()),
            project: Mutex::new(()),
        }
    }

    /// The rules every new thread gets first, for this folder.
    pub fn rules(&self) -> String {
        RULES.replace("{folder}", &util::tilde(&self.f.root)).replace(ATTENTION_AROUND, self.context.attention())
    }

    // ---------- files ----------

    fn read(&self, rel: &str) -> Option<String> {
        self.f.read(rel)
    }

    /// Write a file and remember the text, so the write is not taken for a save.
    fn write(&self, rel: &str, text: &str) {
        self.known.lock().unwrap().insert(rel.to_string(), text.to_string());
        self.f.write(rel, text);
    }

    // ---------- thread.json ----------

    fn info(&self) -> Value {
        load(&self.f.dot.join("thread.json"))
    }

    fn save_info(&self, v: &Value) {
        save(&self.f.dot.join("thread.json"), v);
    }

    fn update_info(&self, f: impl FnOnce(&mut Value)) -> Value {
        let mut v = self.info();
        if !v.is_object() {
            v = json!({});
        }
        f(&mut v);
        self.save_info(&v);
        v
    }

    pub fn publish(&self, changes: Value) {
        let p = self.f.dot.join("state.json");
        let mut d = load(&p);
        if !d.is_object() {
            d = json!({});
        }
        for (k, v) in changes.as_object().cloned().unwrap_or_default() {
            d[k] = v;
        }
        d["updated"] = json!(util::epoch());
        d["pid"] = json!(std::process::id());
        save(&p, &d);
    }

    fn next_id(&self) -> String {
        let v = self.update_info(|v| {
            let n = v["n"].as_u64().unwrap_or(0) + 1;
            v["n"] = json!(n);
        });
        format!("p{}", v["n"].as_u64().unwrap_or(1))
    }

    // ---------- starting ----------

    /// Everything before the first save: the hidden folder, the history,
    /// carets left from before, and the engine's project and models.
    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        self.f.init()?;
        let readme = self.f.path(COMMANDS).join("README.md");
        if !readme.exists() {
            let _ = std::fs::create_dir_all(self.f.path(COMMANDS));
            let _ = std::fs::write(&readme, Cm::README);
        }
        self.clear_stale();
        let files = self.f.files();
        {
            let mut known = self.known.lock().unwrap();
            let mut st = self.st.lock().unwrap();
            for rel in &files {
                if let Some(t) = self.read(rel) {
                    if t.contains('^') {
                        st.carets.insert(rel.clone());
                    }
                    known.insert(rel.clone(), t);
                }
            }
        }
        self.publish(json!({"run": null}));
        let me = self.clone();
        std::thread::spawn(move || me.start_engine());
        Ok(())
    }

    fn start_engine(&self) {
        match self.project_id() {
            Ok(pid) => {
                let models = self.refresh_models().map(|m| m.len()).unwrap_or(0);
                let label = self.label(self.current().as_ref());
                let v = self.update_info(|v| v["model"] = json!(label));
                self.publish(json!({"thread": v["link"], "model": label}));
                log(&format!("{}: project {pid}, {models} models, thread {}", self.engine.name(),
                    v["threadId"].as_str().unwrap_or("none yet")));
            }
            Err(e) => log(&format!("{} is not ready: {e}", self.engine.name())),
        }
    }

    /// Status lines left by a run that never finished (a crash, a restart).
    fn clear_stale(&self) {
        for rel in self.f.files() {
            let Some(text) = self.read(&rel) else { continue };
            if !text.contains(C::WORKING) {
                continue;
            }
            let st = C::statuses(&rel, &text);
            if st.is_empty() {
                continue;
            }
            let mut lines: Vec<Option<String>> = text.split('\n').map(|l| Some(l.to_string())).collect();
            for (pid, i) in st {
                let lead = C::caret_at(&rel, lines[i].as_deref().unwrap_or("")).map(|a| lines[i].as_ref().unwrap()[..a].to_string()).unwrap_or_default();
                lines[i] = if pid.contains('.') {
                    None
                } else {
                    Some(format!("{lead}^> {pid} was cut off when Ambition stopped. Its prompt is in {HISTORY}; write it again to run it."))
                };
            }
            let out: Vec<String> = lines.into_iter().flatten().collect();
            self.write(&rel, &out.join("\n"));
        }
    }

    // ---------- watching ----------

    /// The watcher calls this with every path the operating system reports.
    pub fn changed(self: &Arc<Self>, paths: Vec<PathBuf>) {
        let mut candidates = BTreeSet::new();
        for p in paths {
            // When the system drops events, it names the folder itself: look at everything.
            if p == self.f.root || std::fs::canonicalize(&p).is_ok_and(|c| c == self.f.root) {
                candidates.extend(self.f.files());
                continue;
            }
            let Some(rel) = self.rel_of(&p) else { continue };
            if rel == ".ambition" || rel.starts_with(".ambition/") || rel.split('/').any(|x| x.starts_with('.')) {
                continue;
            }
            if p.is_dir() {
                for f in walk(&p) {
                    if let Some(r) = self.rel_of(&f) {
                        candidates.insert(r);
                    }
                }
            } else {
                candidates.insert(rel);
            }
        }
        if candidates.is_empty() {
            return;
        }
        let unknown: Vec<String> = {
            let known = self.known.lock().unwrap();
            candidates.iter().filter(|r| !known.contains_key(*r) && self.f.path(r).exists()).cloned().collect()
        };
        let ignored = self.ignored(&unknown);
        let mut save = false;
        let mut mid = false;
        for rel in candidates {
            if ignored.contains(&rel) {
                continue;
            }
            let exists = self.f.path(&rel).exists();
            if !exists {
                let was = self.known.lock().unwrap().remove(&rel).is_some();
                if was {
                    let mut st = self.st.lock().unwrap();
                    if st.working {
                        st.dirty.insert(rel);
                    }
                }
                continue;
            }
            if !self.f.watchable(&rel) {
                continue;
            }
            let Some(text) = self.read(&rel) else { continue };
            let mut known = self.known.lock().unwrap();
            if known.get(&rel) == Some(&text) {
                continue;
            }
            let mut st = self.st.lock().unwrap();
            if st.working {
                // The app at work, the agent's own edit, or a save during a run.
                // Prompts wait for the run to end; a command runs now.
                st.dirty.insert(rel.clone());
                if st.run.is_some() && text.contains("^/") && !C::commands(&rel, &text).is_empty() {
                    st.mid.insert(rel.clone());
                    mid = true;
                }
                continue;
            }
            known.insert(rel.clone(), text.clone());
            st.saved.insert(rel.clone());
            if text.contains('^') {
                st.carets.insert(rel.clone());
            }
            save = true;
        }
        if save {
            if let Some(k) = self.kick.lock().unwrap().as_ref() {
                let _ = k.send(());
            }
        }
        if mid {
            if let Some(k) = self.mid_kick.lock().unwrap().as_ref() {
                let _ = k.send(());
            }
        }
    }

    fn rel_of(&self, p: &std::path::Path) -> Option<String> {
        let rel = p.strip_prefix(&self.f.root).ok().map(|r| r.to_path_buf()).or_else(|| {
            let c = std::fs::canonicalize(p).ok()?;
            c.strip_prefix(&self.f.root).ok().map(|r| r.to_path_buf())
        })?;
        let s = rel.to_string_lossy().replace('\\', "/");
        (!s.is_empty()).then_some(s)
    }

    /// Which of these new paths the folder's .gitignore files hide. Folders
    /// are asked about once and remembered, so a build writing thousands of
    /// ignored files costs one question per folder.
    fn ignored(&self, rels: &[String]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if rels.is_empty() {
            return out;
        }
        let mut ask: BTreeSet<String> = BTreeSet::new();
        {
            let (ign, ok) = &*self.ignored.lock().unwrap();
            for rel in rels {
                let parts: Vec<&str> = rel.split('/').collect();
                let mut hidden = false;
                for k in 1..parts.len() {
                    let dir = format!("{}/", parts[..k].join("/"));
                    if ign.contains(&dir) {
                        hidden = true;
                        break;
                    }
                    if !ok.contains(&dir) {
                        ask.insert(dir);
                    }
                }
                if hidden {
                    out.insert(rel.clone());
                } else {
                    ask.insert(rel.clone());
                }
            }
        }
        if ask.is_empty() {
            return out;
        }
        let list: Vec<String> = ask.iter().cloned().collect();
        let res = self.f.git_in(&["check-ignore", "-z", "--stdin", "--no-index"], Some(list.join("\0").as_bytes()));
        let hit: BTreeSet<String> = res.stdout.split(|&b| b == 0).filter_map(|s| std::str::from_utf8(s).ok()).filter(|s| !s.is_empty())
            .map(str::to_string).collect();
        let (ign, ok) = &mut *self.ignored.lock().unwrap();
        for a in &list {
            if a.ends_with('/') {
                if hit.contains(a) || hit.contains(a.trim_end_matches('/')) {
                    ign.insert(a.clone());
                } else {
                    ok.insert(a.clone());
                }
            }
        }
        for rel in rels {
            let parts: Vec<&str> = rel.split('/').collect();
            let under = (1..parts.len()).any(|k| ign.contains(&format!("{}/", parts[..k].join("/"))));
            if under || hit.contains(rel) {
                out.insert(rel.clone());
            }
        }
        out
    }

    /// The responder: waits for saves, lets them settle, and answers them,
    /// one pass at a time.
    pub fn serve(self: &Arc<Self>) {
        let (tx, rx) = channel();
        *self.kick.lock().unwrap() = Some(tx.clone());
        let (mtx, mrx) = channel();
        *self.mid_kick.lock().unwrap() = Some(mtx);
        let me = self.clone();
        std::thread::spawn(move || me.commander(mrx));
        if !self.st.lock().unwrap().carets.is_empty() {
            let _ = tx.send(());
        }
        loop {
            if rx.recv().is_err() {
                return;
            }
            settle(&rx);
            self.st.lock().unwrap().working = true;
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle())) {
                log(&format!("a save failed: {e:?}"));
            }
            // What changed while it worked: the agent's edits are not saves, but
            // carets written meanwhile run next.
            let dirty: Vec<String> = {
                let mut st = self.st.lock().unwrap();
                st.run = None;
                std::mem::take(&mut st.dirty).into_iter().collect()
            };
            let mut again = false;
            {
                let mut known = self.known.lock().unwrap();
                let mut st = self.st.lock().unwrap();
                for rel in dirty {
                    match self.read(&rel) {
                        Some(t) => {
                            if !C::prompts(&rel, &t).is_empty() || !C::commands(&rel, &t).is_empty() {
                                st.carets.insert(rel.clone());
                                again = true;
                            }
                            known.insert(rel, t);
                        }
                        None => {
                            known.remove(&rel);
                        }
                    }
                }
                st.working = false;
            }
            self.publish(json!({"run": null}));
            if again {
                let _ = tx.send(());
            }
        }
    }

    fn commander(&self, rx: Receiver<()>) {
        while rx.recv().is_ok() {
            if self.st.lock().unwrap().run.is_some() {
                self.mid_run_commands();
            }
        }
    }

    // ---------- one save ----------

    pub fn handle(&self) {
        let (saved, maybe) = {
            let mut st = self.st.lock().unwrap();
            (std::mem::take(&mut st.saved), std::mem::take(&mut st.carets))
        };
        let guard = self.io.lock().unwrap();
        for rel in &saved {
            let Some(text) = self.read(rel) else { continue };
            let (new, gone) = C::clear_replies(rel, &text, true);
            if !gone.is_empty() {
                self.write(rel, &new);
                self.f.history(&format!("## {} · {rel} · cleared\n{}", now(), gone.join("\n")));
            }
        }
        // Every caret waiting in the files that may hold one.
        let mut waiting: Vec<(String, C::Prompt)> = Vec::new();
        let mut cmds: BTreeMap<String, Vec<C::Command>> = BTreeMap::new();
        for rel in saved.union(&maybe) {
            let Some(text) = self.read(rel) else { continue };
            if !text.contains('^') {
                continue;
            }
            waiting.extend(C::prompts(rel, &text).into_iter().map(|p| (rel.clone(), p)));
            let c = C::commands(rel, &text);
            if !c.is_empty() {
                cmds.insert(rel.clone(), c);
            }
        }
        if waiting.is_empty() && cmds.is_empty() {
            return;
        }
        let mut asks: Vec<Ask> = waiting.into_iter().map(|(rel, p)| Ask { id: self.next_id(), rel, p, pos: 0 }).collect();
        for a in &asks {
            let words: Vec<String> = a.p.text.split('\n').map(|l| format!("^ {l}")).collect();
            self.f.history(&format!("## {} · {} · {} · asked\n{}", now(), a.rel, a.id, words.join("\n")));
        }

        // Take every caret line out, and note where each stood.
        let files: BTreeSet<String> = asks.iter().map(|a| a.rel.clone()).chain(cmds.keys().cloned()).collect();
        let mut at: BTreeMap<(String, usize), usize> = BTreeMap::new();
        // Each file as it was once its carets were out: positions count from it.
        let mut clean: BTreeMap<String, String> = BTreeMap::new();
        for rel in &files {
            let text = self.read(rel).unwrap_or_default();
            let mine: Vec<usize> = (0..asks.len()).filter(|&k| asks[k].rel == *rel).collect();
            let mut ranges: Vec<(usize, usize)> = mine.iter().map(|&k| (asks[k].p.start, asks[k].p.end)).collect();
            let cs = cmds.get(rel).cloned().unwrap_or_default();
            ranges.extend(cs.iter().map(|c| (c.line, c.line + 1)));
            let (stripped, pos) = C::remove(&text, &ranges);
            for (n, &k) in mine.iter().enumerate() {
                asks[k].pos = pos[n];
            }
            for (n, c) in cs.iter().enumerate() {
                at.insert((rel.clone(), c.line), pos[mine.len() + n]); // where its reply goes
            }
            self.write(rel, &stripped);
            clean.insert(rel.clone(), stripped);
        }
        let names = files.iter().cloned().collect::<Vec<_>>().join(", ");
        let base = self.f.head();
        let saved_as = self.f.commit(&format!("Ambition: saved {names}"));
        let typed = if saved_as.is_empty() { (String::new(), String::new()) } else { (saved_as.clone(), self.saved_diff(&base)) };

        // Commands, at once. Undo first, while the tree is clean.
        let mut order: Vec<(String, C::Command, usize)> = cmds.iter()
            .flat_map(|(r, cs)| cs.iter().map(|c| (r.clone(), c.clone(), at[&(r.clone(), c.line)])))
            .collect();
        order.sort_by_key(|(r, c, _)| (c.name != "undo", r.clone(), c.line));
        let replies = self.do_commands(&order, &clean, false);

        // A status line where each prompt stood, and the commands' replies. A
        // command such as ^/undo may have changed the file: every position
        // moves with the text it was next to.
        for rel in &files {
            let mut text = self.read(rel).unwrap_or_default();
            let was = clean.get(rel).cloned().unwrap_or_default();
            let mut adds: Vec<(usize, u8, usize, Vec<String>)> = replies.get(rel).cloned().unwrap_or_default()
                .into_iter().map(|(at, line, lines)| (diff::remap(&was, &text, at), 0, line, lines)).collect();
            for a in asks.iter_mut().filter(|a| a.rel == *rel) {
                a.pos = diff::remap(&was, &text, a.pos);
                adds.push((a.pos, 2, a.p.start, vec![C::status_line(&a.p.lead, &a.id, &format!("{} · working", util::cut(&a.p.text, 48)))]));
            }
            adds.sort_by_key(|x| (x.0, x.1, x.2));
            for (at, _, _, lines) in adds.into_iter().rev() {
                text = C::insert(&text, at, &lines);
            }
            self.write(rel, &text);
        }
        if asks.is_empty() {
            let what: Vec<String> = order.iter().map(|(_, c, _)| format!("/{}", c.name)).collect();
            self.f.commit(&format!("Ambition: command {} in {names}", what.join(", ")));
            return;
        }
        // What the files look like as the agent starts, to see what its turn did.
        let before: BTreeMap<String, String> = files.iter().filter_map(|r| self.read(r).map(|t| (r.clone(), t))).collect();
        self.st.lock().unwrap().run = Some(asks.iter().map(|a| a.id.clone()).collect());
        drop(guard);

        // The agent, outside the file lock, so commands written meanwhile still run.
        let msg = self.message(&asks, &typed);
        self.told.lock().unwrap().clear();
        let (mut tid, mut link, mut run, mut status, mut err) = (String::new(), String::new(), String::new(), Status::Failed, String::new());
        match self.ask_agent(&msg, &asks) {
            Ok((t, l, r, s)) => (tid, link, run, status) = (t, l, r, s),
            Err(e) => {
                log(&format!("{}: {e}", self.engine.name()));
                err = e;
            }
        }
        self.mid_run_commands();

        let _guard = self.io.lock().unwrap();
        let said = if !tid.is_empty() && !run.is_empty() && status == Status::Completed {
            self.engine.reply(&tid, &run).unwrap_or_default().trim().to_string()
        } else {
            String::new()
        };
        let dirty: BTreeSet<String> = self.st.lock().unwrap().dirty.clone();
        let touched: BTreeSet<String> = dirty.union(&files).cloned().collect();
        let then = |rel: &str| before.get(rel).cloned().or_else(|| self.f.show("HEAD", rel));
        let changed = touched.iter().any(|r| self.read(r) != then(r));
        let new_replies = touched.iter().any(|r| {
            let was: BTreeSet<String> = C::replies(r, &then(r).unwrap_or_default()).into_iter().collect();
            C::replies(r, &self.read(r).unwrap_or_default()).into_iter().any(|l| !was.contains(&l))
        });
        for (k, a) in asks.iter().enumerate() {
            let mut text = self.read(&a.rel).unwrap_or_default();
            let note = if !err.is_empty() {
                Some(format!("{} did not reach the agent: {err}", a.id))
            } else {
                match &status {
                    Status::Completed => None,
                    Status::NeedsInput => Some(format!("{} is waiting for your answer in {}: {link}", a.id, self.engine.name())),
                    Status::Running => Some(format!("{} is still working after {} minutes: {link}. ^/stop stops it.", a.id,
                        self.run_limit.as_secs() / 60)),
                    Status::Interrupted => Some(format!("{} was stopped before it finished. {link}", a.id)),
                    s => Some(format!("{}: the agent's turn ended as {}. {link}", a.id, s.word())),
                }
            };
            if let Some(n) = note {
                text = C::replace_status(&a.rel, &text, &a.id, &C::reply_lines(&a.p.lead, &n, 12));
            } else if k == 0 && !said.is_empty() && !new_replies {
                // The answer is in the thread but not in the file. The rules have
                // the agent end its turn by repeating its ^> lines; when those
                // never reached the file, they go where the prompt stood. With no
                // ^> lines, the last message counts only when the turn changed
                // no file at all.
                let marked: Vec<String> = said.split('\n').filter(|l| l.trim_start().starts_with("^>"))
                    .map(|l| l.trim_start()[2..].strip_prefix(' ').unwrap_or(&l.trim_start()[2..]).to_string()).collect();
                let body = if !marked.is_empty() {
                    marked.join("\n")
                } else if !changed {
                    said.split('\n').map(|l| {
                        let t = l.trim_start();
                        t.strip_prefix("^>").map(|r| r.strip_prefix(' ').unwrap_or(r).to_string()).unwrap_or_else(|| l.to_string())
                    }).collect::<Vec<_>>().join("\n")
                } else {
                    String::new()
                };
                if !body.trim().is_empty() {
                    text = C::replace_status(&a.rel, &text, &a.id, &C::reply_lines(&a.p.lead, &body, 12));
                }
            }
            self.write(&a.rel, &C::remove_marks(&a.rel, &text, &a.id));
            let reps = C::replies(&a.rel, &self.read(&a.rel).unwrap_or_default());
            self.f.history(&format!("## {} · {} · {} · agent ({}){}{}", now(), a.rel, a.id, status.word(),
                if link.is_empty() { String::new() } else { format!("\nThread: {link}") },
                if reps.is_empty() { String::new() } else { format!("\nReplies in the file now:\n{}", reps.join("\n")) }));
        }
        let rels: BTreeSet<&str> = asks.iter().map(|a| a.rel.as_str()).collect();
        let ids: Vec<&str> = asks.iter().map(|a| a.id.as_str()).collect();
        self.f.commit(&format!("Ambition: agent answered {} in {}: {}", ids.join(", "), rels.into_iter().collect::<Vec<_>>().join(", "),
            util::cut(&asks[0].p.text, 70)));
        log(&format!("answered {} ({})", ids.join(", "), status.word()));
    }

    /// What the save just committed changed since `base`: the writer's own
    /// typing, as text for the agent. The carets are already out of it. Reply
    /// lines, the history, and changes that are only blank lines are left out.
    fn saved_diff(&self, base: &str) -> String {
        if base.is_empty() {
            return String::new();
        }
        let mut out: Vec<String> = Vec::new();
        for (st, rel) in self.f.changed_between(base, "HEAD") {
            if rel == HISTORY {
                continue;
            }
            let (old, new) = (self.f.show(base, &rel), self.f.show("HEAD", &rel));
            match (old, new) {
                (_, None) if st.starts_with('D') => {
                    out.push(format!("{rel}: deleted"));
                }
                (None, Some(new)) => out.push(format!("{rel}: a new file, {} lines", new.lines().count())),
                (Some(old), Some(new)) => {
                    let was = C::clear_replies(&rel, &old, false).0;
                    let now_ = C::clear_replies(&rel, &new, false).0;
                    let a: Vec<&str> = was.split('\n').collect();
                    let b: Vec<&str> = now_.split('\n').collect();
                    let d = diff::unified(&a, &b, 1);
                    if !d.iter().any(|l| (l.starts_with('+') || l.starts_with('-')) && !l[1..].trim().is_empty()) {
                        continue;
                    }
                    out.push(format!("{rel}:"));
                    for (k, l) in d.iter().enumerate() {
                        if l.starts_with("@@") {
                            if k > 0 {
                                out.push("  …".into());
                            }
                        } else {
                            out.push(format!("  {l}"));
                        }
                    }
                }
                _ => {}
            }
        }
        let text = out.join("\n");
        if text.len() > DIFF_CHARS {
            let mut cut = text[..floor(&text, DIFF_CHARS)].rsplit_once('\n').map(|x| x.0).unwrap_or("").to_string();
            cut.push_str("\n  … cut here: the history has all of it");
            return cut;
        }
        text
    }

    // ---------- what the agent is told ----------

    /// The paragraph (or list item) a caret on status line `i` sits under, in
    /// the file as it is on disk: 0-based (start, end), skipping the app's
    /// own status lines.
    fn target_above(rel: &str, text: &str, i: usize) -> Option<(usize, usize)> {
        let lines: Vec<&str> = text.split('\n').collect();
        let skip: BTreeSet<usize> = C::statuses(rel, text).values().cloned().collect();
        C::above(rel, &lines, i, &skip)
    }

    fn target_below(rel: &str, text: &str, i: usize) -> Option<(usize, usize)> {
        let lines: Vec<&str> = text.split('\n').collect();
        let skip: BTreeSet<usize> = C::statuses(rel, text).values().cloned().collect();
        C::below(&lines, i, &skip)
    }

    /// The part of a file the agent needs, numbered, as it is on disk now.
    fn view(&self, rel: &str, text: &str, mine: &[&Ask]) -> String {
        let mut lines: Vec<&str> = text.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        let n = lines.len();
        let width = n.to_string().len();
        let numbered = |lo: usize, hi: usize| -> Vec<String> {
            (lo..=hi).map(|k| {
                let l = lines[k - 1];
                let l = if l.chars().count() > LINE_CUT {
                    let cut: String = l.chars().take(LINE_CUT).collect();
                    format!("{cut} … (cut here: {} more characters on this line, read the file for them)", l.chars().count() - LINE_CUT)
                } else {
                    l.to_string()
                };
                format!("{k:>width$}| {l}")
            }).collect()
        };
        let path = util::tilde(&self.f.path(rel));
        let whole = match self.context {
            Context::Whole => n <= 2000 && text.len() <= 100_000,
            _ => n <= WHOLE_LINES && text.len() <= WHOLE_CHARS,
        };
        if whole {
            let mut out = vec![format!("{path}, all {n} line{}:", if n == 1 { "" } else { "s" })];
            out.extend(numbered(1, n.max(1).min(n)));
            return out.join("\n");
        }
        let st = C::statuses(rel, text);
        let mut out = Vec::new();
        for around in [AROUND, 5, 1] {
            let mut spans: Vec<(usize, usize)> = Vec::new();
            for a in mine {
                let Some(&i) = st.get(&a.id) else { continue };
                let (lo, hi) = match self.context {
                    Context::Above => {
                        let t = Self::target_above(rel, text, i).map(|t| t.0 + 1).unwrap_or(i + 1);
                        (t.saturating_sub(around).max((i + 1).saturating_sub(60)), i + 1 + around)
                    }
                    _ => {
                        let up = Self::target_above(rel, text, i).map(|t| t.0 + 1).unwrap_or(i + 1);
                        let down = Self::target_below(rel, text, i).map(|t| t.1).unwrap_or(i + 1);
                        (up.min((i + 1).saturating_sub(around)).max((i + 1).saturating_sub(60)),
                         down.max(i + 1 + around).min(i + 1 + 60))
                    }
                };
                spans.push((lo, hi));
            }
            let mut merged: Vec<(usize, usize)> = Vec::new();
            let mut s: Vec<(usize, usize)> = spans.into_iter().map(|(lo, hi)| (lo.max(1), hi.min(n))).filter(|(lo, hi)| lo <= hi).collect();
            s.sort();
            for (lo, hi) in s {
                match merged.last_mut() {
                    Some(m) if lo <= m.1 + 1 => m.1 = m.1.max(hi),
                    _ => merged.push((lo, hi)),
                }
            }
            out = vec![format!("{path}, lines {} of {n}:", merged.iter().map(|(a, b)| format!("{a}-{b}")).collect::<Vec<_>>().join(", "))];
            for (k, (lo, hi)) in merged.iter().enumerate() {
                if k > 0 {
                    out.push("…".into());
                }
                out.extend(numbered(*lo, *hi));
            }
            if out.join("\n").len() <= 2 * WHOLE_CHARS {
                break;
            }
        }
        out.join("\n")
    }

    /// What the agent is told for one save. It carries the writer's
    /// attention: each prompt, where its caret is, the file around it, and
    /// what they changed since the last turn. They can read it in the
    /// engine's thread as it was sent.
    pub fn message(&self, asks: &[Ask], typed: &(String, String)) -> String {
        let files: BTreeSet<&str> = asks.iter().map(|a| a.rel.as_str()).collect();
        let n = asks.len();
        let mut parts = vec![format!(
            "[Ambition] A save in {} with {n} caret prompt{}. Each is now a status line in its file. Do them in order, \
             following the rules in this thread's first message: your answer goes in the file.{}",
            files.iter().cloned().collect::<Vec<_>>().join(", "), if n > 1 { "s" } else { "" },
            if self.context == Context::Pointer { "" } else { " The file text below is what is on disk right now, status lines included." }
        )];
        for a in asks {
            let text = self.read(&a.rel).unwrap_or_default();
            let lines: Vec<&str> = text.split('\n').collect();
            let st = C::statuses(&a.rel, &text);
            let i = st.get(&a.id).cloned();
            let line = i.map(|i| (i + 1).to_string()).unwrap_or_else(|| "?".into());
            let first = |(s, _e): (usize, usize)| util::cut(lines.get(s).copied().unwrap_or("").trim(), 50);
            let on = match (self.context, i) {
                (_, None) => "Its status line is gone from the file.".to_string(),
                (Context::Above, Some(i)) => match Self::target_above(&a.rel, &text, i) {
                    None => "The caret is at the top of the file, with nothing above it, so the prompt is about the whole file.".into(),
                    Some((s, e)) => {
                        let blk = {
                            let skip: BTreeSet<usize> = st.values().cloned().collect();
                            let mut j = s;
                            while j > 0 && !lines[j - 1].trim().is_empty() && !skip.contains(&(j - 1)) {
                                j -= 1;
                            }
                            (j, e)
                        };
                        if blk.0 != s {
                            format!("The caret sits right under the list item on {}, the last item of the block on {}. Unless \
                                their words point somewhere else (\"this list\", \"these\"), that item is what they mean.", C::span(s, e), C::span(blk.0, blk.1))
                        } else {
                            format!("The caret sits right under the paragraph on {}. Unless their words point somewhere else, \
                                that paragraph is what they mean.", C::span(s, e))
                        }
                    }
                },
                (Context::Pointer, Some(i)) => format!("The caret is on line {} of {}.", i + 1, util::tilde(&self.f.path(&a.rel))),
                (_, Some(i)) => {
                    let up = Self::target_above(&a.rel, &text, i);
                    let down = Self::target_below(&a.rel, &text, i);
                    let say = |t: Option<(usize, usize)>, none: &str| match t {
                        Some(t) => format!("{} (“{}”)", C::span(t.0, t.1), first(t)),
                        None => none.to_string(),
                    };
                    if up.is_none() {
                        format!("The caret is on line {}, at the top of the file, so the prompt is about the whole file. Below it: {}.",
                            i + 1, say(down, "nothing"))
                    } else {
                        format!("The caret is on line {}. Just above it: {}. Just below it: {}.", i + 1,
                            say(up, "nothing"), say(down, "nothing, the file ends"))
                    }
                }
            };
            let words: Vec<String> = a.p.text.split('\n').map(|l| format!("  {l}")).collect();
            parts.push(format!("\n{} · {}, line {line} (`^ ◌ {} …`):\n{}\n  {on}", a.id, a.rel, a.id, words.join("\n")));
        }
        if self.context != Context::Pointer {
            for rel in &files {
                let mine: Vec<&Ask> = asks.iter().filter(|a| a.rel == *rel).collect();
                let v = self.view(rel, &self.read(rel).unwrap_or_default(), &mine);
                if !v.is_empty() {
                    parts.push(format!("\n{v}"));
                }
            }
        }
        let plural = if n > 1 { "s" } else { "" };
        if !typed.1.is_empty() {
            parts.push(format!("\nWhat they changed in this folder since your last turn, besides the prompt{plural} (+ is new, - is gone):\n{}", typed.1));
        } else {
            parts.push(format!("\nBesides the prompt{plural}, nothing in this folder changed since your last turn."));
        }
        let told = self.told.lock().unwrap();
        if !told.is_empty() {
            parts.push(format!("\nCommands run since your last turn (for your memory, no action needed):\n{}",
                told.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")));
        }
        parts.join("\n")
    }

    // ---------- the thread ----------

    fn agents_text(&self) -> String {
        ["AGENTS.md", "CLAUDE.md"].iter().filter_map(|n| self.read(n)).collect::<Vec<_>>().join("\n")
    }

    fn project_id(&self) -> Result<String, String> {
        let _one = self.project.lock().unwrap();
        if let Some(p) = self.info()["project"].as_str() {
            return Ok(p.to_string());
        }
        let p = self.engine.project(&self.f.root)?;
        self.update_info(|v| v["project"] = json!(p));
        Ok(p)
    }

    /// Why the next prompt needs a new thread, or None to use the one we have.
    fn why_new(&self) -> Option<&'static str> {
        let d = self.info();
        let Some(tid) = d["threadId"].as_str() else { return Some("first") };
        if d["new"].as_bool() == Some(true) {
            return Some("asked");
        }
        if d["rules"].as_str() != Some(util::sha(&self.rules()).as_str()) {
            return Some("rules");
        }
        if d["agents"].as_str() != Some(util::sha(&self.agents_text()).as_str()) {
            return Some("agents");
        }
        match self.engine.thread(tid) {
            Ok(None) => Some("gone"),
            _ => None, // busy or away: keep the thread we have
        }
    }

    fn tail_history(&self, skip: &[String]) -> String {
        let text = self.read(HISTORY).unwrap_or_default();
        let mut entries: Vec<&str> = Vec::new();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("\n## ") {
            entries.push(&rest[..i]);
            rest = &rest[i + 1..];
        }
        entries.push(rest);
        let keep: Vec<&str> = entries.into_iter().filter(|e| e.starts_with("## "))
            .filter(|e| !skip.iter().any(|id| e.lines().next().unwrap_or("").contains(&format!(" · {id} · asked")))).collect();
        let out = keep[keep.len().saturating_sub(6)..].join("\n").trim().to_string();
        if out.len() <= 2500 {
            out
        } else {
            format!("…{}", &out[ceil(&out, out.len() - 2500)..])
        }
    }

    /// The first words of a new thread: the rules, and what came before.
    pub fn handoff(&self, why: &str, ids: &[String]) -> String {
        let d = self.info();
        let old = d["threadId"].as_str().unwrap_or("");
        let reason = match why {
            "first" => "This is the first thread for this folder.".to_string(),
            "asked" => "Someone wrote `^/new`, which starts a new thread.".into(),
            "rules" => "Ambition's rules changed after the last thread began, and a thread reads them only when it starts.".into(),
            "agents" => "The folder's AGENTS.md changed after the last thread began, and a thread reads it only when it starts.".into(),
            _ => format!("{} no longer has the last thread.", self.engine.name()),
        };
        let mut parts = vec![format!("[Ambition] A new thread starts here, for {}. {reason}", util::tilde(&self.f.root)), self.rules()];
        if !old.is_empty() && why != "gone" {
            parts.push(format!("The thread before this one: {} (id {old}). Read it when you need more than this message gives.",
                d["link"].as_str().filter(|l| !l.is_empty()).unwrap_or(old)));
        }
        if why == "agents" {
            let was = d["agents_text"].as_str().unwrap_or("");
            let a: Vec<&str> = was.split('\n').collect();
            let now_ = self.agents_text();
            let b: Vec<&str> = now_.split('\n').collect();
            let mut dd = diff::unified(&a, &b, 1).join("\n");
            if dd.len() > 3000 {
                dd = format!("{}\n… cut here", &dd[..floor(&dd, 3000)]);
            }
            if !dd.is_empty() {
                parts.push(format!("What changed in AGENTS.md (you have loaded the new one):\n{dd}"));
            }
        }
        if self.f.path(NOTES).exists() {
            parts.push(format!("The notes earlier threads kept for you are in {NOTES}."));
        }
        let h = self.tail_history(ids);
        if !h.is_empty() {
            parts.push(format!("The end of {HISTORY}, the record of every prompt and reply:\n{h}"));
        }
        parts.push("The save for you to answer follows.".into());
        parts.join("\n\n")
    }

    /// Send one save's message to the folder's thread and wait for the turn.
    fn ask_agent(&self, text: &str, asks: &[Ask]) -> Result<(String, String, String, Status), String> {
        let ids: Vec<String> = asks.iter().map(|a| a.id.clone()).collect();
        let why = self.why_new();
        let d = self.info();
        let (tid, link, run) = if let Some(why) = why {
            let old = d["threadId"].as_str().unwrap_or("").to_string();
            let project = self.project_id()?;
            let sel = match (!old.is_empty() && why != "gone").then(|| self.engine.model_of(&old).ok().flatten()).flatten() {
                Some(s) => Some(s),
                None => self.engine.project_model(&project).ok().flatten().or_else(|| self.first_model(&project)),
            };
            let first = format!("{}\n\n{text}", self.handoff(why, &ids));
            let title = format!("Ambition · {} · {}", self.f.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                util::minute());
            let (tid, run) = self.engine.launch(&project, &title, &first, sel.as_ref())?;
            let link = self.engine.thread(&tid).ok().flatten().map(|t| t.link).unwrap_or_default();
            if !old.is_empty() && why != "gone" {
                let _ = self.engine.settle(&old);
            }
            let rules = util::sha(&self.rules());
            let agents_text = self.agents_text();
            let label = match &sel {
                Some(s) => self.label(Some(s)),
                None => self.label(self.current().as_ref()),
            };
            self.update_info(|v| {
                let mut prev: Vec<Value> = if old.is_empty() { vec![] } else { vec![json!(old)] };
                prev.extend(v["previous"].as_array().cloned().unwrap_or_default());
                prev.truncate(20);
                v["threadId"] = json!(tid);
                v["link"] = json!(link);
                v["started"] = json!(now());
                v["rules"] = json!(rules);
                v["agents"] = json!(util::sha(&agents_text));
                v["agents_text"] = json!(agents_text);
                v["previous"] = json!(prev);
                v["model"] = json!(label);
                v["new"] = json!(false);
            });
            self.f.history(&format!("## {} · thread\nA new thread ({why}): {}", now(), if link.is_empty() { &tid } else { &link }));
            log(&format!("new thread {tid} ({why})"));
            (tid, link, run)
        } else {
            let tid = d["threadId"].as_str().unwrap_or("").to_string();
            let run = self.engine.send(&tid, text)?;
            (tid, d["link"].as_str().unwrap_or("").to_string(), run)
        };
        self.publish(json!({"run": {"ids": ids, "thread": link, "since": util::epoch(), "model": self.info()["model"]}, "thread": link}));
        let status = self.engine.wait(&tid, &run, self.run_limit)?;
        Ok((tid, link, run, status))
    }

    // ---------- models ----------

    /// For a project with no model yet: the first one the engine lists, made
    /// the project's default so its later threads get it too.
    fn first_model(&self, project: &str) -> Option<Selection> {
        let m = self.refresh_models().ok()?.into_iter().next()?;
        let sel = Selection { instance: m.instance, model: m.id, effort: None };
        let _ = self.engine.use_model(None, project, &sel);
        log(&format!("{} had no model for this folder: using {}, its first. ^/model changes it.", self.engine.name(), m.label));
        Some(sel)
    }

    fn refresh_models(&self) -> Result<Vec<Model>, String> {
        let ms = self.engine.models(None)?;
        let rows: Vec<Value> = ms.iter().map(model_json).collect();
        save(&self.f.dot.join("models.json"), &json!({"models": rows, "at": util::epoch()}));
        Ok(ms)
    }

    fn cached_models(&self) -> Vec<Model> {
        load(&self.f.dot.join("models.json"))["models"].as_array().into_iter().flatten().filter_map(model_from).collect()
    }

    /// "Claude Sonnet 5.5 · high" for a selection.
    fn label(&self, sel: Option<&Selection>) -> String {
        let Some(s) = sel else { return format!("{}'s default", self.engine.name()) };
        let name = self.cached_models().into_iter().find(|m| m.id == s.model && (s.instance.is_empty() || m.instance == s.instance))
            .map(|m| m.label).unwrap_or_else(|| s.model.clone());
        match &s.effort {
            Some((_, e)) if !e.is_empty() => format!("{name} · {e}"),
            _ => name,
        }
    }

    /// The model the thread uses now, else the one a new thread gets.
    fn current(&self) -> Option<Selection> {
        if let Some(tid) = self.info()["threadId"].as_str() {
            if let Ok(Some(s)) = self.engine.model_of(tid) {
                return Some(s);
            }
        }
        let pid = self.project_id().ok()?;
        self.engine.project_model(&pid).ok().flatten()
    }

    fn cmd_model(&self, args: &str) -> String {
        let engine = self.engine.name();
        let cur = self.current();
        let words: Vec<&str> = args.split_whitespace().collect();
        if words.is_empty() {
            let ms = match self.refresh_models() {
                Ok(m) => m,
                Err(e) => return format!("model: could not reach {engine} ({e}). Nothing changed."),
            };
            let curm = cur.as_ref().map(|c| c.model.clone()).unwrap_or_default();
            let cells: Vec<String> = ms.iter().map(|m| format!("{}{}", m.label, if m.id == curm { " ●" } else { "" })).collect();
            let mut rows = vec![format!("model: {} (in use)", self.label(cur.as_ref()))];
            let mut row = String::new();
            for c in cells {
                if !row.is_empty() && row.len() + c.len() > 70 {
                    rows.push(std::mem::take(&mut row));
                }
                if !row.is_empty() {
                    row.push_str(" · ");
                }
                row.push_str(&c);
            }
            if !row.is_empty() {
                rows.push(row);
            }
            rows.push(format!("Switch with ^/model <name> [effort], e.g. ^/model sonnet high. A name searches all of {engine}'s models."));
            return rows.join("\n");
        }
        // Only an effort: keep the model.
        if words.len() == 1 && EFFORT_WORDS.contains(&words[0].to_lowercase().as_str()) {
            if let Some(c) = &cur {
                let m = self.engine.models(Some(&c.model)).ok().and_then(|ms| ms.into_iter().find(|m| m.id == c.model));
                if let Some(m) = m {
                    return self.switch(&m, Some(words[0]));
                }
            }
        }
        let q = words.join(" ");
        let mut hits = match self.engine.models(Some(&q)) {
            Ok(h) => h,
            Err(e) => return format!("model: could not reach {engine} ({e}). Nothing changed."),
        };
        let mut effort = None;
        if hits.is_empty() && words.len() > 1 {
            let q2 = words[..words.len() - 1].join(" ");
            if let Ok(h) = self.engine.models(Some(&q2)) {
                if !h.is_empty() {
                    hits = h;
                    effort = Some(words[words.len() - 1]);
                }
            }
        }
        if hits.is_empty() {
            return format!("model: {engine} has no model matching “{q}”. Nothing changed. ^/model lists them.");
        }
        let key = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        let qq = key(effort.map(|_| words[..words.len() - 1].join(" ")).as_deref().unwrap_or(&q));
        let pick = if hits.len() == 1 {
            Some(hits[0].clone())
        } else {
            let exact: Vec<&Model> = hits.iter().filter(|m| key(&m.id) == qq || key(&m.label) == qq).collect();
            (exact.len() == 1).then(|| exact[0].clone())
        };
        match pick {
            Some(m) => self.switch(&m, effort),
            None => {
                let names: Vec<String> = hits.iter().take(12).map(|m| m.label.clone()).collect();
                format!("model: “{q}” fits {} models: {}. Name one more exactly. Nothing changed.", hits.len(), names.join(" · "))
            }
        }
    }

    fn switch(&self, m: &Model, effort: Option<&str>) -> String {
        let mut sel = Selection { instance: m.instance.clone(), model: m.id.clone(), effort: None };
        if let Some(e) = effort {
            let e = e.to_lowercase();
            if !m.efforts.contains(&e) {
                return if m.efforts.is_empty() {
                    format!("model: {} has no effort setting. Nothing changed.", m.label)
                } else {
                    format!("model: {} has no effort “{e}” (it takes {}). Nothing changed.", m.label, m.efforts.join(", "))
                };
            }
            sel.effort = Some((m.effort_id.clone(), e));
        }
        let tid = self.info()["threadId"].as_str().map(str::to_string);
        let pid = match self.project_id() {
            Ok(p) => p,
            Err(e) => return format!("model: could not reach {} ({e}). Nothing changed.", self.engine.name()),
        };
        if let Err(e) = self.engine.use_model(tid.as_deref(), &pid, &sel) {
            return format!("model: {} would not switch ({e}). Nothing changed.", self.engine.name());
        }
        let mut ms = self.cached_models();
        if !ms.iter().any(|x| x.id == m.id) {
            ms.push(m.clone());
            let rows: Vec<Value> = ms.iter().map(model_json).collect();
            save(&self.f.dot.join("models.json"), &json!({"models": rows, "at": util::epoch()}));
        }
        let label = self.label(Some(&sel));
        self.update_info(|v| v["model"] = json!(label));
        self.publish(json!({"model": label}));
        if tid.is_some() {
            format!("model: {label}. The thread uses it from its next turn, and new threads do too.")
        } else {
            format!("model: {label}. New threads use it.")
        }
    }

    // ---------- commands ----------

    fn help_text(&self) -> String {
        let mut rows: Vec<String> = Cm::BUILTINS.iter().map(|(n, d)| format!("/{n}: {d}")).collect();
        let mine = Cm::find(&self.f.root);
        let own: Vec<(&String, &PathBuf)> = mine.iter().filter(|(n, _)| !Cm::is_builtin(n)).collect();
        for (n, p) in &own {
            let d = Cm::describe(p);
            rows.push(format!("/{n}: {}", if d.is_empty() { p.file_name().unwrap().to_string_lossy().to_string() } else { d }));
        }
        if own.is_empty() {
            rows.push(format!("Your own: a script in {COMMANDS}/ (its README.md says how)."));
        }
        rows.join("\n")
    }

    fn cmd_thread(&self) -> String {
        let d = self.info();
        let Some(tid) = d["threadId"].as_str() else { return "thread: none yet. The next ^ prompt starts one.".into() };
        let th = match self.engine.thread(tid) {
            Ok(None) => return format!("thread: {} no longer has it. The next ^ prompt starts a new one, with a handoff.", self.engine.name()),
            Ok(t) => t,
            Err(_) => None,
        };
        let link = d["link"].as_str().filter(|l| !l.is_empty()).unwrap_or(tid);
        let model = d["model"].as_str().map(str::to_string).unwrap_or_else(|| self.label(self.current().as_ref()));
        let mut out = format!("thread: {link} · model: {model}");
        let mut more = Vec::new();
        if let Some(t) = th {
            more.push(format!("status: {}, {} turn{}", t.status, t.turns, if t.turns == 1 { "" } else { "s" }));
        }
        match self.why_new() {
            Some("asked") => more.push("^/new was written, so the next ^ prompt starts a new thread".into()),
            Some("rules") => more.push("Ambition's rules changed, so the next ^ prompt starts a new thread".into()),
            Some("agents") => more.push("AGENTS.md changed, so the next ^ prompt starts a new thread".into()),
            _ => {}
        }
        if !more.is_empty() {
            out.push('\n');
            out.push_str(&more.join(" · "));
        }
        out
    }

    fn cmd_new(&self) -> String {
        let d = self.info();
        let Some(tid) = d["threadId"].as_str().map(str::to_string) else {
            return "new: there is no thread yet; the next ^ prompt starts the first one.".into();
        };
        self.update_info(|v| v["new"] = json!(true));
        format!("new: the next ^ prompt starts a new thread, with a handoff from {}.", d["link"].as_str().filter(|l| !l.is_empty()).unwrap_or(&tid))
    }

    fn cmd_stop(&self) -> String {
        let run = self.st.lock().unwrap().run.clone();
        let tid = self.info()["threadId"].as_str().map(str::to_string);
        match (run, tid) {
            (Some(ids), Some(tid)) => match self.engine.interrupt(&tid) {
                Ok(()) => format!("stop: asked {} to stop the turn for {}.", self.engine.name(), ids.join(", ")),
                Err(e) => format!("stop: {} would not stop it ({e}).", self.engine.name()),
            },
            _ => "stop: nothing is running.".into(),
        }
    }

    /// Undo Ambition's last change that is not undone yet.
    fn undo(&self) -> String {
        for (h, subject) in self.f.undoable() {
            let plan = match self.f.reverse(&h) {
                Ok(p) => p,
                Err(why) => return format!("could not undo {h} ({}): {why}. Nothing changed.", util::cut(subject.trim_start_matches("Ambition: "), 64)),
            };
            if plan.is_empty() {
                continue; // that change only wrote replies: nothing to take back
            }
            for (rel, text) in &plan {
                match text {
                    None => {
                        let _ = std::fs::remove_file(self.f.path(rel));
                        self.known.lock().unwrap().remove(rel);
                    }
                    Some(t) => self.write(rel, t),
                }
            }
            let what = subject.trim_start_matches("Ambition: ");
            self.f.commit(&format!("Ambition: undo {h}: {what}"));
            return format!("undid {h}: {} ({})", util::cut(what, 110), plan.keys().cloned().collect::<Vec<_>>().join(", "));
        }
        "nothing left to undo.".into()
    }

    fn run_command(&self, c: &C::Command, rel: &str, above: &str, during_run: bool) -> String {
        match c.name.as_str() {
            "help" => self.help_text(),
            "model" => self.cmd_model(&c.args),
            "new" => self.cmd_new(),
            "thread" => self.cmd_thread(),
            "stop" => self.cmd_stop(),
            "undo" if during_run => "undo: the agent is working. Write ^/undo again when its turn ends.".into(),
            "undo" => self.undo(),
            name => {
                let mine = Cm::find(&self.f.root);
                let Some(p) = mine.get(name) else {
                    let all: Vec<&str> = Cm::BUILTINS.iter().map(|(n, _)| *n).chain(mine.keys().map(String::as_str)).collect();
                    let near = diff::closest(name, all);
                    return format!("/{name}: no such command.{} ^/help lists them.", near.map(|n| format!(" Did you mean ^/{n}?")).unwrap_or_default());
                };
                let (out, code) = Cm::run(&self.f.root, p, &c.args, rel, c.line + 1, above);
                let out = if out.is_empty() { "(no output)".to_string() } else { out };
                if code != 0 && code != -1 {
                    format!("{out}\n(exit {code})")
                } else {
                    out
                }
            }
        }
    }

    /// The paragraph (or last list item) just above line index `at`, as text.
    fn unit_above(rel: &str, text: &str, at: usize) -> String {
        let lines: Vec<&str> = text.split('\n').collect();
        C::above(rel, &lines, at.min(lines.len()), &BTreeSet::new()).map(|(s, e)| lines[s..e].join("\n")).unwrap_or_default()
    }

    /// Run commands and return their replies by file: (line index for the
    /// reply, the command's own line, reply lines). Each comes with the index
    /// where it stood in the file once its caret lines were out.
    fn do_commands(&self, order: &[(String, C::Command, usize)], base: &BTreeMap<String, String>, during_run: bool)
        -> BTreeMap<String, Vec<(usize, usize, Vec<String>)>> {
        let mut out: BTreeMap<String, Vec<(usize, usize, Vec<String>)>> = BTreeMap::new();
        for (rel, c, pos) in order {
            let text = self.read(rel).unwrap_or_default();
            // An earlier command may have changed the file since the positions were taken.
            let here = base.get(rel).map(|b| diff::remap(b, &text, *pos)).unwrap_or(*pos);
            let reply = self.run_command(c, rel, &Self::unit_above(rel, &text, here), during_run);
            out.entry(rel.clone()).or_default().push((*pos, c.line, C::reply_lines(&c.lead, &reply, 12)));
            let line = format!("^/{} {}", c.name, c.args);
            self.f.history(&format!("## {} · {rel} · ^/{} · command\n{}\n{}", now(), c.name, line.trim_end(),
                C::reply_lines("", &reply, usize::MAX).join("\n")));
            self.told.lock().unwrap().push(format!("{} in {rel} → {}", line.trim_end(), util::cut(&reply, 200)));
        }
        out
    }

    /// Commands written while the agent works: they run now, not after.
    fn mid_run_commands(&self) {
        let rels: Vec<String> = std::mem::take(&mut self.st.lock().unwrap().mid).into_iter().collect();
        for rel in rels {
            let _g = self.io.lock().unwrap();
            let text = self.read(&rel).unwrap_or_default();
            let cmds = C::commands(&rel, &text);
            if cmds.is_empty() {
                continue;
            }
            let ranges: Vec<(usize, usize)> = cmds.iter().map(|c| (c.line, c.line + 1)).collect();
            let (stripped, pos) = C::remove(&text, &ranges);
            self.write(&rel, &stripped);
            let order: Vec<(String, C::Command, usize)> = cmds.into_iter().zip(pos).map(|(c, p)| (rel.clone(), c, p)).collect();
            let base = BTreeMap::from([(rel.clone(), stripped.clone())]);
            let replies = self.do_commands(&order, &base, true);
            let mut text = self.read(&rel).unwrap_or_default();
            let mut adds: Vec<(usize, usize, Vec<String>)> = replies.get(&rel).cloned().unwrap_or_default()
                .into_iter().map(|(at, line, lines)| (diff::remap(&stripped, &text, at), line, lines)).collect();
            adds.sort_by_key(|x| (x.0, x.1));
            for (at, _, lines) in adds.into_iter().rev() {
                text = C::insert(&text, at, &lines);
            }
            self.write(&rel, &text);
            self.f.commit(&format!("Ambition: command during a run in {rel}"));
            log(&format!("command during a run in {rel}"));
        }
    }
}

/// Wait until saves stop coming for a moment.
fn settle(rx: &Receiver<()>) {
    loop {
        match rx.recv_timeout(QUIET) {
            Ok(()) => continue,
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

pub fn load(p: &std::path::Path) -> Value {
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

pub fn save(p: &std::path::Path, v: &Value) {
    let tmp = p.with_extension(format!("tmp{}", util::nonce()));
    if std::fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

fn model_json(m: &Model) -> Value {
    json!({"instance": m.instance, "provider": m.provider, "model": m.id, "label": m.label, "effortId": m.effort_id, "efforts": m.efforts})
}

fn model_from(v: &Value) -> Option<Model> {
    Some(Model {
        instance: v["instance"].as_str()?.to_string(),
        provider: v["provider"].as_str().unwrap_or("").to_string(),
        id: v["model"].as_str()?.to_string(),
        label: v["label"].as_str().unwrap_or("").to_string(),
        effort_id: v["effortId"].as_str().unwrap_or("").to_string(),
        efforts: v["efforts"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect(),
    })
}

/// The largest char boundary at or below i.
fn floor(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The smallest char boundary at or above i.
fn ceil(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_carry_the_section_they_are_varied_by() {
        assert!(RULES.contains(ATTENTION_AROUND), "RULES.md and ATTENTION_AROUND have drifted apart");
        assert!(!RULES.contains("{attention}"));
    }
}
