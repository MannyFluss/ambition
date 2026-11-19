//! The running Ambition. It sleeps on its inbox (`inbox.rs`) and does what
//! each request asks. Saving a file does nothing: a press does, and a press
//! sends only the caret on the line it names.
//!
//! One press of a `^` prompt:
//!
//! 1. The file's folder is found (`config::place`). The first press in a
//!    folder with no grant asks first.
//! 2. The file's old `^>` replies go, and the prompt leaves the file: a
//!    status line stands where it was, `^ ◌ p3 · <its words> · working`, or
//!    `· waiting for p2` while another turn runs in that folder. One turn at
//!    a time runs in a folder; other folders run side by side.
//! 3. When its turn comes, the folder is committed to its private history
//!    ("Ambition: pressed p3 …"), and the prompt goes to the file's own
//!    thread with the hook (`hook.rs`).
//! 4. When the turn ends, the status line goes, and the turn is one commit
//!    ("Ambition: agent answered p3 …"), so `^/undo` takes it back.
//!
//! A press on a `^/command` runs it at once, in code (`commands.rs`); its
//! output takes the command's place as `^>` lines. A press on a line with no
//! mark is a request: its status line goes just below that line, and the
//! agent reads around it and does what the text seems to need.
//!
//! A text field (`field.rs`) is pressed the same way, through its mirror: the
//! field's text goes into the mirror, the press runs there, and each change
//! Ambition makes to the mirror (the status line, the answer) is written
//! back into the field while the field still says what Ambition left in it.
//!
//! What Ambition keeps, all in `~/.ambition/`: `threads` (each document's
//! thread), `state.json` (what is on its way now, for the language server),
//! `models.json`, `config`, `commands/`, `notes.md`, `coordinator.md`, an
//! optional `AGENTS.md`, and `history/`.

use crate::carets as C;
use crate::commands as Cm;
use crate::config;
use crate::diff;
use crate::engine::{Engine, Model, Selection, Status};
use crate::field::{self, Snapshot, Surface};
use crate::folder::{self, Folder};
use crate::hook::{self, Since};
use crate::inbox;
use crate::ui::{self, Answer};
use crate::util::{self, now};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const RULES: &str = include_str!("../RULES.md");
pub const COORDINATOR: &str = "coordinator.md";
const EFFORT_WORDS: [&str; 9] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra", "ultrathink"];

const COORDINATOR_DOC: &str = "\
# The coordinator

This is where you talk to Ambition's coordinator, the thread that knows the
others. Write a line that starts with ^ and press Ambition's key on it, as in
any file.

Threads ask it things with `ambition ask coordinator`. It can read the list of
threads (~/.ambition/threads) and any thread's history, and pass messages
between threads. It cannot stop or start them.
";

pub fn log(msg: &str) {
    println!("{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
}

/// One prompt on its way to the agent.
#[derive(Clone, Debug)]
pub struct Ask {
    pub id: String,
    pub doc: PathBuf,
    pub rel: String,
    pub p: C::Prompt,
}

/// A message for a thread that no press sent: a thread asking the
/// coordinator, or the coordinator passing one on.
struct Msg {
    doc: PathBuf,
    rel: String,
    text: String,
    reply: Option<Sender<Result<String, String>>>,
}

enum Job {
    Press(Ask),
    Message(Msg),
}

impl Job {
    fn label(&self) -> String {
        match self {
            Job::Press(a) => a.id.clone(),
            Job::Message(m) => format!("a message to {}", m.rel),
        }
    }

    fn doc(&self) -> PathBuf {
        match self {
            Job::Press(a) => a.doc.clone(),
            Job::Message(m) => m.doc.clone(),
        }
    }
}

#[derive(Default)]
struct Q {
    jobs: VecDeque<Job>,
    /// What runs in the folder now: a prompt id, a message, or a command.
    busy: Option<String>,
    /// Its document and, once it reached one, its thread.
    doc: Option<PathBuf>,
    tid: Option<String>,
}

/// A folder with presses in it: its private history and its line of turns.
pub struct Place {
    pub f: Folder,
    q: Mutex<Q>,
}

/// A press on its way: waiting for its folder, or with the agent now.
#[derive(Clone, Debug)]
pub struct Turn {
    pub id: String,
    pub doc: PathBuf,
    pub folder: PathBuf,
    pub working: bool,
    pub since: f64,
    pub thread: String,
    pub words: String,
    pub lead: String,
    /// The lines the agent has seen, 1-based: what `ambition more` adds to.
    pub seen: (usize, usize),
    /// Where the document's text is: the file, or an open buffer's copy.
    pub text_at: PathBuf,
}

/// A field that was pressed: how to reach it, what it said when Ambition last
/// read or wrote it, what Ambition wrote last (for a second press), and an
/// answer it could not take yet.
struct Live {
    surface: Arc<dyn Surface>,
    snap: Snapshot,
    shown: String,
    span: Option<(usize, usize)>,
    pending: Option<(String, String)>,
    /// What the agent is told about where this document is.
    note: String,
}

pub struct App {
    pub engine: Arc<dyn Engine>,
    pub run_limit: Duration,
    /// How an agent runs this Ambition: `ambition`, or the whole command
    /// when this one is not the `ambition` on the PATH.
    pub cli: String,
    places: Mutex<BTreeMap<PathBuf, Arc<Place>>>,
    turns: Mutex<Vec<Turn>>,
    /// Held for each of Ambition's own read-modify-writes of a file.
    io: Mutex<()>,
    /// Held for each read-modify-write of Ambition's own files.
    store: Mutex<()>,
    /// One dialog at a time.
    dialog: Mutex<()>,
    projects: Mutex<BTreeMap<PathBuf, String>>,
    /// The fields and open buffers pressed since Ambition started, by
    /// document.
    fields: Mutex<BTreeMap<PathBuf, Live>>,
    /// The editors whose language servers are connected.
    pub editors: crate::editor::Editors,
}

pub fn load(p: &Path) -> Value {
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

pub fn save(p: &Path, v: &Value) {
    let tmp = p.with_extension(format!("tmp{}", util::nonce()));
    if std::fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

fn own(name: &str) -> PathBuf {
    util::state_dir().join(name)
}

/// The key a document has in `threads`.
fn key(doc: &Path) -> String {
    doc.to_string_lossy().to_string()
}

pub fn coordinator_doc() -> PathBuf {
    util::state_real().join(COORDINATOR)
}

/// How an agent should run this binary.
fn cli() -> String {
    let exe = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(if cfg!(windows) { "ambition.exe" } else { "ambition" }))
            .find(|p| p.is_file())
            .and_then(|p| std::fs::canonicalize(p).ok())
    });
    let quote = |s: &str| if s.contains(|c: char| c.is_whitespace() || "'\"$\\".contains(c)) { format!("'{}'", s.replace('\'', "'\\''")) } else { s.to_string() };
    let mut s = match exe {
        Some(e) if Some(&e) != on_path.as_ref() => quote(&e.to_string_lossy()),
        _ => "ambition".to_string(),
    };
    if let Some(h) = std::env::var_os("AMBITION_HOME") {
        s = format!("AMBITION_HOME={} {s}", quote(&h.to_string_lossy()));
    }
    s
}

impl App {
    pub fn new(engine: Arc<dyn Engine>) -> App {
        let run_limit = std::env::var("AMBITION_RUN_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(1800.0);
        App {
            engine,
            run_limit: Duration::from_secs_f64(run_limit),
            cli: cli(),
            places: Mutex::new(BTreeMap::new()),
            turns: Mutex::new(Vec::new()),
            io: Mutex::new(()),
            store: Mutex::new(()),
            dialog: Mutex::new(()),
            projects: Mutex::new(BTreeMap::new()),
            fields: Mutex::new(BTreeMap::new()),
            editors: Default::default(),
        }
    }

    /// The rules every new thread gets first, for one document.
    pub fn rules(&self, doc: &Path, folder: &Path) -> String {
        RULES.replace("{document}", &util::tilde(doc)).replace("{folder}", &util::tilde(folder))
    }

    // ---------- starting ----------

    /// Everything before the first press: Ambition's folder, prompts a stop
    /// cut off, and the engine's models.
    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        let home = util::state_dir();
        std::fs::create_dir_all(&home).map_err(|e| format!("cannot make {}: {e}", home.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
        }
        self.restore();
        let coord = home.join(COORDINATOR);
        if !coord.exists() {
            let _ = std::fs::write(&coord, COORDINATOR_DOC);
        }
        let readme = Cm::dir().join("README.md");
        if !readme.exists() {
            let _ = std::fs::create_dir_all(Cm::dir());
            let _ = std::fs::write(&readme, Cm::README);
        }
        self.publish();
        let me = self.clone();
        std::thread::spawn(move || match me.refresh_models() {
            Ok(m) => log(&format!("{}: {} models", me.engine.name(), m.len())),
            Err(e) => log(&format!("{} is not ready: {e}", me.engine.name())),
        });
        Ok(())
    }

    /// Prompts whose turn a stop cut off go back where they were, so a press
    /// sends them again.
    fn restore(&self) {
        let d = load(&own("state.json"));
        if d["pid"].as_u64() == Some(std::process::id() as u64) {
            return;
        }
        for t in d["turns"].as_array().into_iter().flatten() {
            let (Some(doc), Some(id)) = (t["doc"].as_str(), t["id"].as_str()) else { continue };
            let doc = PathBuf::from(doc);
            let rel = doc.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let Ok(text) = std::fs::read_to_string(&doc) else { continue };
            if !C::statuses(&rel, &text).contains_key(id) {
                continue;
            }
            let lead = t["lead"].as_str().unwrap_or("");
            let words = t["words"].as_str().unwrap_or("");
            let mut new = if words.is_empty() { Vec::new() } else { C::prompt_lines(lead, words) };
            new.extend(C::reply_lines(lead, &format!("{id} was cut off when Ambition stopped. Press its line again to send it."), 2));
            let _ = std::fs::write(&doc, C::replace_status(&rel, &text, id, &new));
            log(&format!("{id} in {} was cut off: its prompt is back", util::tilde(&doc)));
        }
    }

    /// Answer one inbox request.
    pub fn handle(self: &Arc<Self>, req: &Value) -> Value {
        let cwd = req["cwd"].as_str().map(PathBuf::from).and_then(|p| std::fs::canonicalize(p).ok());
        let r = match req["op"].as_str().unwrap_or("") {
            "press" => match (req["file"].as_str(), req["line"].as_u64()) {
                (Some(f), Some(l)) => {
                    log(&format!("a press at {}:{l}", util::tilde(Path::new(f))));
                    self.press(f, l as usize)
                }
                _ => Err("a press names a file and a line".into()),
            },
            "more" => {
                let up = req["dir"].as_str() != Some("down");
                let n = req["n"].as_u64().map(|n| n as usize).unwrap_or(hook::MORE).max(1);
                self.more(up, n, cwd.as_deref())
            }
            "ask" => self.ask(req["to"].as_str().unwrap_or(""), req["message"].as_str().unwrap_or(""), cwd.as_deref()),
            "read" => self.agent_read(req["what"].as_str().unwrap_or(""), cwd.as_deref()),
            "look" => self.agent_look(cwd.as_deref()),
            "ping" => Ok(format!("ambition {}", env!("CARGO_PKG_VERSION"))),
            other => Err(format!("Ambition has no request “{other}”")),
        };
        match r {
            Ok(t) => inbox::ok(t),
            Err(e) => inbox::no(e),
        }
    }

    // ---------- Ambition's own files ----------

    fn entry(&self, doc: &Path) -> Value {
        load(&own("threads"))[key(doc)].clone()
    }

    fn update_entry(&self, doc: &Path, f: impl FnOnce(&mut Value)) {
        let _g = self.store.lock().unwrap();
        let p = own("threads");
        let mut all = load(&p);
        let mut v = all[key(doc)].clone();
        if !v.is_object() {
            v = json!({});
        }
        f(&mut v);
        all[key(doc)] = v;
        save(&p, &all);
    }

    /// What is on its way now, for the language server and for a restart.
    fn publish(&self) {
        let rows: Vec<Value> = self.turns.lock().unwrap().iter().map(|t| json!({
            "id": t.id, "doc": key(&t.doc), "folder": key(&t.folder), "state": if t.working { "working" } else { "waiting" },
            "since": t.since, "thread": t.thread, "words": t.words, "lead": t.lead})).collect();
        let _g = self.store.lock().unwrap();
        let p = own("state.json");
        let mut d = load(&p);
        d["pid"] = json!(std::process::id());
        d["updated"] = json!(util::epoch());
        d["turns"] = json!(rows);
        save(&p, &d);
    }

    fn next_id(&self) -> String {
        let _g = self.store.lock().unwrap();
        let p = own("state.json");
        let mut d = load(&p);
        let n = d["n"].as_u64().unwrap_or(0) + 1;
        d["n"] = json!(n);
        save(&p, &d);
        format!("p{n}")
    }

    fn set_turn(&self, id: &str, f: impl FnOnce(&mut Turn)) {
        if let Some(t) = self.turns.lock().unwrap().iter_mut().find(|t| t.id == id) {
            f(t);
        }
    }

    // ---------- folders ----------

    fn place(&self, root: &Path) -> Result<Arc<Place>, String> {
        let mut m = self.places.lock().unwrap();
        if let Some(p) = m.get(root) {
            return Ok(p.clone());
        }
        let f = Folder::new(root);
        f.init()?;
        let p = Arc::new(Place { f, q: Mutex::new(Q::default()) });
        m.insert(root.to_path_buf(), p.clone());
        Ok(p)
    }

    /// Ask for a folder's grant: a dialog, where there is a screen, else
    /// `ambition allow`.
    fn grant(&self, doc: &Path, root: &Path) -> Result<(), String> {
        let how = format!("To let presses there run without asking: ambition allow {}", util::tilde(root));
        if config::too_wide(root) {
            return Err(format!("{} is your whole home folder or a disk, which Ambition does not grant. Move the file into a \
                folder, or allow one: ambition allow <folder>.", util::tilde(root)));
        }
        let _one = self.dialog.lock().unwrap();
        if config::place(doc).1 {
            return Ok(()); // the dialog before this one granted it
        }
        let text = format!("A press in {} would send its line, and the lines around it, to an agent with a shell, working in {}.\n\n\
            Always lets every press in that folder run without asking.", util::tilde(doc), util::tilde(root));
        let answer = ui::ask(&format!("Let Ambition work in {}?", util::tilde(root)), &text);
        self.decide(answer, &util::tilde(root), &how, || config::allow(root))
    }

    /// Ask for an app's grant, for its text fields. The first thing a press
    /// in a field does, before it reads anything.
    pub fn grant_app(&self, app: &str, bundle: &str) -> Result<(), String> {
        let _one = self.dialog.lock().unwrap();
        if config::app_allowed(bundle) {
            return Ok(());
        }
        let how = format!("To let presses in {app} run without asking: ambition allow {}{bundle}", config::APP);
        let text = format!("A press in {app} would send the text around your cursor to an agent with a shell.\n\n\
            Always lets every press in {app}'s text fields run without asking, and lets the agent read and look at the window \
            it was pressed in.");
        let answer = ui::ask(&format!("Let Ambition work in {app}?"), &text);
        self.decide(answer, app, &how, || config::allow_app(bundle))
    }

    /// What a grant dialog's answer means for the press.
    fn decide(&self, answer: Answer, place: &str, how: &str, always: impl FnOnce() -> Result<String, String>) -> Result<(), String> {
        match answer {
            Answer::Always => {
                always()?;
                log(&format!("granted {place}"));
                Ok(())
            }
            Answer::Once => Ok(()),
            Answer::Deny => Err(format!("denied: nothing was sent from {place}.")),
            Answer::Unanswered => Err("nobody answered the dialog, so nothing was sent.".into()),
            Answer::Failed(e) => Err(format!("{place} is not allowed yet, and Ambition could not ask ({e}), so nothing was sent. {how}")),
        }
    }

    // ---------- a press ----------

    /// A press on `line` (1-based) of `file`.
    pub fn press(self: &Arc<Self>, file: &str, line: usize) -> Result<String, String> {
        let doc = std::fs::canonicalize(file).map_err(|_| format!("{file}: no such file."))?;
        // The grant comes first, before anything is read or kept.
        let (root, allowed) = config::place(&doc);
        if !allowed {
            self.grant(&doc, &root)?;
        }
        let meta = std::fs::metadata(&doc).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(format!("{} is not a file.", util::tilde(&doc)));
        }
        if meta.len() > folder::MAX_BYTES || !folder::is_text(&doc) {
            return Err(format!("{} is not a text file of up to 512 KB, so Ambition does not read it.", util::tilde(&doc)));
        }
        // An editor's open buffer, unsaved, is the document; else the file.
        let buffer = self.editors.buffer(&doc);
        let text = match &buffer {
            Some(b) => b.read()?,
            None => std::fs::read_to_string(&doc).map_err(|e| e.to_string())?,
        };
        if let Some(b) = &buffer {
            let surface: Arc<dyn Surface> = Arc::new(crate::editor::Buffer::clone_of(b));
            if let Some(r) = self.again(&doc, &text, surface) {
                return r;
            }
        }
        let name = doc.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let lines: Vec<&str> = text.split('\n').collect();
        let n = hook::lines(&text).len();
        if line == 0 || line > n.max(1) {
            return Err(format!("{name} has {n} lines, so there is no line {line} to send."));
        }
        C::reload();
        let what = C::pressed(&name, lines[line - 1], line)?;
        let place = self.place(&root)?;
        let rel = place.f.rel(&doc).unwrap_or(name.clone());
        self.busy_here(&place, &doc)?;
        match buffer {
            Some(b) => {
                let copy = util::state_dir().join("buffers").join(&util::sha(&key(&doc))[..12]).join(&name);
                if let Some(d) = copy.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                std::fs::write(&copy, &text).map_err(|e| format!("cannot write {}: {e}", util::tilde(&copy)))?;
                place.f.mirror(&rel, Some(copy.clone()));
                let client = b.client().to_string();
                let note = format!("{} is open in {client}, with changes not saved to disk. Do not edit that file on disk: edit \
                    its copy, {}, and Ambition puts your changes in the open buffer when your turn ends, unsaved.",
                    util::tilde(&doc), util::tilde(&copy));
                let snap = Snapshot { app: client, title: rel.clone(), text: text.clone(), line: line - 1, ..Default::default() };
                self.fields.lock().unwrap().insert(doc.clone(), Live { surface: Arc::new(b), snap, shown: text, span: None, pending: None, note });
            }
            None => {
                place.f.mirror(&rel, None);
                self.fields.lock().unwrap().remove(&doc);
            }
        }
        self.dispatch(&place, &doc, &rel, line - 1, what)
    }

    /// A press in a document whose turn is running now would overwrite the
    /// agent's work in its copy.
    fn busy_here(&self, place: &Place, doc: &Path) -> Result<(), String> {
        let q = place.q.lock().unwrap();
        match (&q.busy, &q.doc) {
            (Some(b), Some(d)) if d == doc && self.fields.lock().unwrap().contains_key(doc) => {
                Err(format!("{b} is still working in this document. Press again when it is done."))
            }
            _ => Ok(()),
        }
    }

    /// A press on a field or buffer that says what Ambition left in it, with
    /// an answer that could not land, or a change to show again.
    fn again(&self, doc: &Path, text: &str, surface: Arc<dyn Surface>) -> Option<Result<String, String>> {
        let found = {
            let mut f = self.fields.lock().unwrap();
            match f.get_mut(doc) {
                Some(l) if l.shown == text && (l.pending.is_some() || (l.span.is_some() && surface.can_highlight())) => {
                    l.surface = surface.clone();
                    Some((l.pending.clone(), l.span))
                }
                _ => None,
            }
        };
        match found? {
            (Some((what, t)), _) => Some(self.push(doc, &t, true, Some(&what)).map(|_| format!("{what} is in now."))),
            (None, Some(span)) => Some(surface.highlight(text, span).map(|_| "showed what Ambition wrote last.".to_string())),
            _ => None,
        }
    }

    fn dispatch(self: &Arc<Self>, place: &Arc<Place>, doc: &Path, rel: &str, i: usize, what: C::Pressed) -> Result<String, String> {
        match what {
            C::Pressed::Prompt => self.prompt_press(place, doc, rel, i, false),
            C::Pressed::Request => self.prompt_press(place, doc, rel, i, true),
            C::Pressed::Command(c) => self.command_press(place, doc, rel, i, &c),
        }
    }

    /// A press on a prompt, or, with `request`, on a line with no mark.
    fn prompt_press(self: &Arc<Self>, place: &Arc<Place>, doc: &Path, rel: &str, i: usize, request: bool) -> Result<String, String> {
        let mut q = place.q.lock().unwrap();
        let io = self.io.lock().unwrap();
        let text = place.f.read(rel).unwrap_or_default();
        let p = if request {
            // No prompt leaves the text: the status line goes just below the line pressed.
            C::Prompt { start: i + 1, end: i + 1, lead: String::new(), text: String::new() }
        } else {
            match C::prompts(rel, &text).into_iter().find(|p| p.start <= i && i < p.end) {
                Some(p) => p,
                None => return Err("the prompt changed while it was pressed; press it again.".into()),
            }
        };
        let id = self.next_id();
        let ahead = q.busy.clone();
        let state = match &ahead {
            Some(a) => format!("waiting for {a}"),
            None => "working".into(),
        };
        let (cleared, gone) = C::clear_replies(rel, &text, true);
        let start = diff::remap(&text, &cleared, p.start);
        let status = C::status_line(&p.lead, &id, &status_words(&p.text, &state));
        let shown = if request {
            C::insert(&cleared, start, &[status])
        } else {
            // The status line takes the prompt's place, exactly there.
            let mut ls: Vec<&str> = cleared.split('\n').collect();
            let end = (start + (p.end - p.start)).min(ls.len());
            ls.splice(start.min(end)..end, [status.as_str()]);
            ls.join("\n")
        };
        place.f.write(rel, &shown);
        // In a field, the status line must reach the field before anything is sent.
        if let Err(e) = self.push(doc, &shown, false, None) {
            place.f.write(rel, &text);
            return Err(format!("nothing was sent: Ambition could not write in the field ({e})."));
        }
        log(&format!("{id}: its status line is in {}", util::tilde(doc)));
        drop(io);
        if !gone.is_empty() {
            place.f.history(&format!("## {} · {rel} · cleared\n{}", now(), gone.join("\n")));
        }
        let asked = if request { format!("(a press with no mark, on line {})", i + 1) } else { C::prompt_lines("", &p.text).join("\n") };
        place.f.history(&format!("## {} · {rel} · {id} · asked\n{asked}", now()));
        self.turns.lock().unwrap().push(Turn {
            id: id.clone(), doc: doc.to_path_buf(), folder: place.f.root.clone(), working: ahead.is_none(), since: util::epoch(),
            thread: String::new(), words: p.text.clone(), lead: p.lead.clone(), seen: (0, 0), text_at: place.f.file(rel),
        });
        q.jobs.push_back(Job::Press(Ask { id: id.clone(), doc: doc.to_path_buf(), rel: rel.to_string(), p }));
        let go = q.busy.is_none();
        if go {
            q.busy = Some(id.clone());
        }
        drop(q);
        self.publish();
        if go {
            self.spawn(place);
        }
        Ok(match ahead {
            Some(a) => format!("{id} waits for {a}: one turn at a time runs in {}.", util::tilde(&place.f.root)),
            None => format!("{id} sent to the thread for {rel}."),
        })
    }

    fn command_press(self: &Arc<Self>, place: &Arc<Place>, doc: &Path, rel: &str, i: usize, name: &str) -> Result<String, String> {
        // A free folder is held while the command runs, so no turn starts
        // meanwhile and its change is one commit that ^/undo can take back.
        let held = {
            let mut q = place.q.lock().unwrap();
            let free = q.busy.is_none() && q.jobs.is_empty();
            if free {
                q.busy = Some(format!("^/{name}"));
                q.doc = Some(doc.to_path_buf());
            }
            free
        };
        let found = {
            let _io = self.io.lock().unwrap();
            let text = place.f.read(rel).unwrap_or_default();
            C::commands(rel, &text).into_iter().find(|c| c.line == i).map(|c| {
                let (cleared, _) = C::clear_replies(rel, &text, true);
                let at = diff::remap(&text, &cleared, i);
                let (stripped, pos) = C::remove(&cleared, &[(at, at + 1)]);
                place.f.write(rel, &stripped);
                if held {
                    place.f.track(rel);
                    place.f.commit(&format!("Ambition: pressed {} in {rel}", C::cmd(name)));
                }
                (c, stripped, pos[0])
            })
        };
        let Some((c, stripped, at)) = found else {
            if held {
                self.release(place);
            }
            return Err("the command changed while it was pressed; press it again.".into());
        };
        let reply = self.run_command(&c, place, doc, rel, &unit_above(rel, &stripped, at), held);
        let done = {
            let _io = self.io.lock().unwrap();
            let text = place.f.read(rel).unwrap_or_default();
            let here = diff::remap(&stripped, &text, at);
            let done = C::insert(&text, here, &C::reply_lines(&c.lead, &reply, 12));
            place.f.write(rel, &done);
            done
        };
        let line = format!("{} {}", C::cmd(&c.name), c.args);
        place.f.history(&format!("## {} · {rel} · ^/{} · command\n{}\n{}", now(), c.name, line.trim_end(),
            C::reply_lines("", &reply, usize::MAX).join("\n")));
        if held {
            place.f.commit(&format!("Ambition: command /{} in {rel}", c.name));
        }
        self.push_or_tell(doc, &done, &format!("{}'s answer", C::cmd(&c.name)));
        if held {
            place.f.keep_whole();
            self.release(place);
        }
        Ok(reply)
    }

    // ---------- the folder's line ----------

    fn spawn(self: &Arc<Self>, place: &Arc<Place>) {
        let (me, p) = (self.clone(), place.clone());
        std::thread::spawn(move || me.work(p));
    }

    fn release(self: &Arc<Self>, place: &Arc<Place>) {
        let mut q = place.q.lock().unwrap();
        q.busy = None;
        q.doc = None;
        if let Some(j) = q.jobs.front() {
            q.busy = Some(j.label());
            drop(q);
            self.spawn(place);
        }
    }

    fn enqueue(self: &Arc<Self>, place: &Arc<Place>, job: Job) {
        let mut q = place.q.lock().unwrap();
        let label = job.label();
        q.jobs.push_back(job);
        if q.busy.is_none() {
            q.busy = Some(label);
            drop(q);
            self.spawn(place);
        }
    }

    /// The folder's turns, one at a time, until its line is empty.
    fn work(self: Arc<Self>, place: Arc<Place>) {
        loop {
            let job = {
                let mut q = place.q.lock().unwrap();
                match q.jobs.pop_front() {
                    Some(j) => {
                        q.busy = Some(j.label());
                        q.doc = Some(j.doc());
                        q.tid = None;
                        j
                    }
                    None => {
                        q.busy = None;
                        q.doc = None;
                        q.tid = None;
                        return;
                    }
                }
            };
            let id = job.label();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                Job::Press(a) => self.turn(&place, a),
                Job::Message(m) => self.message_turn(&place, m),
            }));
            if let Err(e) = r {
                log(&format!("{id} failed: {e:?}"));
            }
            self.turns.lock().unwrap().retain(|t| t.id != id);
            self.publish();
            // After the first turn's answer, never before it.
            place.f.keep_whole();
        }
    }

    // ---------- a turn ----------

    fn turn(&self, place: &Place, a: Ask) {
        let f = &place.f;
        let working = C::status_line(&a.p.lead, &a.id, &status_words(&a.p.text, "working"));
        // The folder as it is, without this status line, is one commit: the
        // writer's own typing, which ^/undo of the turn leaves alone.
        let started = {
            let _io = self.io.lock().unwrap();
            self.catch_up(&a.doc, &a.rel, &a.id, f);
            let text = f.read(&a.rel).unwrap_or_default();
            match C::statuses(&a.rel, &text).get(&a.id).copied() {
                None => None,
                Some(i) => {
                    let mut lines: Vec<&str> = text.split('\n').collect();
                    lines.remove(i);
                    let t0 = lines.join("\n");
                    f.write(&a.rel, &t0);
                    f.track(&a.rel);
                    f.commit(&format!("Ambition: pressed {} in {}", a.id, a.rel));
                    lines.insert(i, &working);
                    let t1 = lines.join("\n");
                    f.write(&a.rel, &t1);
                    Some((i, t1))
                }
            }
        };
        let Some((i, start)) = started else {
            f.history(&format!("## {} · {} · {} · dropped\nIts status line was gone when its turn came, so nothing was sent.", now(), a.rel, a.id));
            return;
        };
        // A press that waited said so in its field; now it says working.
        let _ = self.push(&a.doc, &start, false, None);
        self.set_turn(&a.id, |t| {
            t.working = true;
            t.since = util::epoch();
        });
        self.publish();

        let since = match self.entry(&a.doc)["last"].as_str() {
            None => Since::First,
            Some(last) => {
                let old = f.show(last, &a.rel).unwrap_or_default();
                let new = f.show("HEAD", &a.rel).unwrap_or_default();
                hook::changes(&a.rel, &old, &new).map(Since::Diff).unwrap_or(Since::Nothing)
            }
        };
        let note = self.fields.lock().unwrap().get(&a.doc).map(|l| l.note.clone());
        let (msg, seen) = hook::press(&util::tilde(&a.doc), &a.id, &a.p.text, &start, i, &since, &self.cli, note.as_deref());
        self.set_turn(&a.id, |t| t.seen = seen);

        let (mut tid, mut link, mut run, mut status, mut err) = (String::new(), String::new(), String::new(), Status::Failed, String::new());
        match self.ask_agent(place, &a.doc, &a.rel, &msg, true, Some(&a.id)) {
            Ok((t, l, r, s)) => (tid, link, run, status) = (t, l, r, s),
            Err(e) => {
                log(&format!("{}: {e}", self.engine.name()));
                err = e;
            }
        }
        let said = if !tid.is_empty() && !run.is_empty() && status == Status::Completed {
            self.engine.reply(&tid, &run).unwrap_or_default().trim().to_string()
        } else {
            String::new()
        };

        let _io = self.io.lock().unwrap();
        let Some(mut text) = f.read(&a.rel) else {
            f.history(&format!("## {} · {} · {} · agent ({}): the file is gone", now(), a.rel, a.id, status.word()));
            f.commit(&format!("Ambition: agent answered {} in {}: {}", a.id, a.rel, util::cut(&a.p.text, 70)));
            return;
        };
        let was: BTreeSet<String> = C::replies(&a.rel, &start).into_iter().collect();
        let new_replies = C::replies(&a.rel, &text).into_iter().any(|l| !was.contains(&l));
        let changed = text != start || others_changed(f, &a.rel);
        let note = if !err.is_empty() {
            Some(format!("{} did not reach the agent: {err}", a.id))
        } else {
            match &status {
                Status::Completed => None,
                Status::NeedsInput => Some(format!("{} is waiting for your answer in {}: {link}", a.id, self.engine.name())),
                Status::Running => Some(format!("{} is still working after {} minutes: {link}. ^/stop stops it.", a.id, self.run_limit.as_secs() / 60)),
                Status::Interrupted => Some(format!("{} was stopped before it finished. {link}", a.id)),
                s => Some(format!("{}: the agent's turn ended as {}. {link}", a.id, s.word())),
            }
        };
        if let Some(n) = note {
            text = C::replace_status(&a.rel, &text, &a.id, &C::reply_lines(&a.p.lead, &n, 12));
        } else if !said.is_empty() && !new_replies {
            // The answer is in the thread but not in the file. The rules have the
            // agent end its turn by repeating its ^> lines; when those never
            // reached the file, they go where the prompt stood. With no ^> lines,
            // the last message counts only when the turn changed no file at all.
            let unmark = |l: &str| C::unreply(l);
            let marked: Vec<String> = said.split('\n').filter_map(unmark).collect();
            let body = if !marked.is_empty() {
                marked.join("\n")
            } else if !changed {
                said.split('\n').map(|l| unmark(l).unwrap_or_else(|| l.to_string())).collect::<Vec<_>>().join("\n")
            } else {
                String::new()
            };
            if !body.trim().is_empty() {
                text = C::replace_status(&a.rel, &text, &a.id, &C::reply_lines(&a.p.lead, &body, 12));
            }
        }
        f.write(&a.rel, &C::remove_marks(&a.rel, &text, &a.id));
        let reps = C::replies(&a.rel, &f.read(&a.rel).unwrap_or_default());
        f.history(&format!("## {} · {} · {} · agent ({}){}{}", now(), a.rel, a.id, status.word(),
            if link.is_empty() { String::new() } else { format!("\nThread: {link}") },
            if reps.is_empty() { String::new() } else { format!("\nReplies in the file now:\n{}", reps.join("\n")) }));
        f.commit(&format!("Ambition: agent answered {} in {}: {}", a.id, a.rel, util::cut(&a.p.text, 70)));
        let done = f.read(&a.rel).unwrap_or_default();
        drop(_io);
        self.push_or_tell(&a.doc, &done, &format!("{}'s answer", a.id));
        let head = f.head();
        self.update_entry(&a.doc, |v| {
            v["last"] = json!(head);
            v["turns"] = json!(v["turns"].as_u64().unwrap_or(0) + 1);
            v["updated"] = json!(now());
        });
        log(&format!("answered {} in {} ({})", a.id, util::tilde(&a.doc), status.word()));
    }

    fn message_turn(&self, place: &Place, m: Msg) {
        let f = &place.f;
        {
            let _io = self.io.lock().unwrap();
            f.track(&m.rel);
            f.commit(&format!("Ambition: saved before a message to {}", m.rel));
        }
        let start = m.doc == coordinator_doc();
        let out = match self.ask_agent(place, &m.doc, &m.rel, &m.text, start, None) {
            Ok((tid, _, run, Status::Completed)) => Ok(self.engine.reply(&tid, &run).unwrap_or_default().trim().to_string()),
            Ok((_, link, _, s)) => Err(format!("the turn ended as {}: {link}", s.word())),
            Err(e) => Err(e),
        };
        {
            let _io = self.io.lock().unwrap();
            let what = match &out {
                Ok(a) => format!("Answer: {}", util::cut(a, 400)),
                Err(e) => format!("Not answered: {e}"),
            };
            f.history(&format!("## {} · {} · message\n{}\n{what}", now(), m.rel, util::cut(&m.text, 400)));
            f.commit(&format!("Ambition: agent answered a message in {}", m.rel));
        }
        if let Some(tx) = m.reply {
            let _ = tx.send(out);
        }
    }

    // ---------- the thread ----------

    /// The AGENTS.md files a document's thread reads: Ambition's own, then
    /// each on the way down from its folder to it.
    fn agents(&self, doc: &Path, folder: &Path) -> Vec<PathBuf> {
        let mut out = vec![util::state_real().join("AGENTS.md")];
        let mut dirs = Vec::new();
        let mut d = doc.parent();
        while let Some(x) = d {
            dirs.push(x.to_path_buf());
            if x == folder || !x.starts_with(folder) {
                break;
            }
            d = x.parent();
        }
        out.extend(dirs.into_iter().rev().map(|d| d.join("AGENTS.md")));
        let mut seen = BTreeSet::new();
        out.into_iter().filter(|p| p.is_file() && seen.insert(p.clone())).collect()
    }

    fn agents_sha(&self, doc: &Path, folder: &Path) -> String {
        let text: String = self.agents(doc, folder).iter()
            .map(|p| format!("{}\n{}\n", p.display(), std::fs::read_to_string(p).unwrap_or_default())).collect();
        util::sha(&text)
    }

    fn project_id(&self, folder: &Path) -> Result<String, String> {
        let mut m = self.projects.lock().unwrap();
        if let Some(p) = m.get(folder) {
            return Ok(p.clone());
        }
        let p = self.engine.project(folder)?;
        m.insert(folder.to_path_buf(), p.clone());
        Ok(p)
    }

    /// Why the document's next turn needs a new thread, or None to use the
    /// one it has.
    fn why_new(&self, doc: &Path, folder: &Path) -> Option<&'static str> {
        let d = self.entry(doc);
        let Some(tid) = d["thread"].as_str() else { return Some("first") };
        if d["new"].as_bool() == Some(true) {
            return Some("asked");
        }
        if d["rules"].as_str() != Some(util::sha(RULES).as_str()) {
            return Some("rules");
        }
        if d["agents"].as_str() != Some(self.agents_sha(doc, folder).as_str()) {
            return Some("agents");
        }
        match self.engine.thread(tid) {
            Ok(None) => Some("gone"),
            _ => None, // busy or away: keep the thread it has
        }
    }

    fn tail_history(f: &Folder, rel: &str) -> String {
        let text = std::fs::read_to_string(&f.log).unwrap_or_default();
        let mut entries: Vec<&str> = Vec::new();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("\n## ") {
            entries.push(&rest[..i]);
            rest = &rest[i + 1..];
        }
        entries.push(rest);
        let tag = format!(" · {rel} · ");
        let keep: Vec<&str> = entries.into_iter()
            .filter(|e| e.starts_with("## ") && e.lines().next().unwrap_or("").contains(&tag) && !e.contains(" · asked\n")).collect();
        let out = keep[keep.len().saturating_sub(6)..].join("\n").trim().to_string();
        if out.len() <= 2500 {
            return out;
        }
        let mut i = out.len() - 2500;
        while !out.is_char_boundary(i) {
            i += 1;
        }
        format!("…{}", &out[i..])
    }

    /// The first words of a new thread: the rules, and what came before.
    pub fn handoff(&self, f: &Folder, doc: &Path, rel: &str, why: &str, what: &str) -> String {
        let d = self.entry(doc);
        let old = d["thread"].as_str().unwrap_or("");
        let reason = match why {
            "first" => "This is the first thread for this document.".to_string(),
            "asked" => "Someone pressed `^/new`, which starts a new thread.".into(),
            "rules" => "Ambition's rules changed after the last thread began, and a thread reads them only when it starts.".into(),
            "agents" => "An AGENTS.md for this document changed after the last thread began, and a thread reads them only when it starts.".into(),
            _ => format!("{} no longer has the last thread.", self.engine.name()),
        };
        let mut parts = vec![
            format!("[Ambition] A new thread starts here, for {}, in the folder {}. {reason}", util::tilde(doc), util::tilde(&f.root)),
            self.rules(doc, &f.root),
        ];
        if doc == coordinator_doc() {
            parts.push("You are the coordinator: the end of “The coordinator” above is for you.".into());
        }
        let agents = self.agents(doc, &f.root);
        if !agents.is_empty() {
            let list: Vec<String> = agents.iter().map(|p| format!("- {}", util::tilde(p))).collect();
            parts.push(format!("These AGENTS.md files apply to this document, the nearest last. Read the ones you have not \
                loaded before you answer:\n{}", list.join("\n")));
        }
        if self.cli != "ambition" {
            parts.push(format!("Here, run Ambition as `{}`: use that wherever these rules say `ambition`.", self.cli));
        }
        if !old.is_empty() && why != "gone" {
            parts.push(format!("The thread before this one: {} (id {old}). Read it when you need more than this message gives.",
                d["link"].as_str().filter(|l| !l.is_empty()).unwrap_or(old)));
        }
        let notes = util::state_real().join("notes.md");
        if notes.exists() {
            parts.push(format!("The notes threads keep for later ones are in {}.", util::tilde(&notes)));
        }
        let h = Self::tail_history(f, rel);
        if !h.is_empty() {
            parts.push(format!("The end of this document's history, from {}:\n{h}", util::tilde(&f.log)));
        }
        parts.push(format!("The {what} for you to answer follows."));
        parts.join("\n\n")
    }

    /// Send a message to a document's thread and wait for the turn. A new
    /// thread starts when the document needs one and `start` allows it.
    /// (thread id, link, run id, how it ended)
    fn ask_agent(&self, place: &Place, doc: &Path, rel: &str, text: &str, start: bool, turn: Option<&str>)
        -> Result<(String, String, String, Status), String> {
        let root = &place.f.root;
        let d = self.entry(doc);
        let old = d["thread"].as_str().unwrap_or("").to_string();
        let why = self.why_new(doc, root);
        let (tid, link, run) = match why {
            Some(w) if start => {
                let project = self.project_id(root)?;
                // The thread before this one's model; else the model chosen last;
                // else the model of the last thread Ambition started; else the
                // project's. Never a model picked from a list by chance: with
                // none of these, T3 Code starts the thread on its own default.
                let sel = (!old.is_empty() && w != "gone").then(|| self.engine.model_of(&old).ok().flatten()).flatten()
                    .or_else(|| self.last_model())
                    .or_else(|| self.last_started())
                    .or_else(|| self.engine.project_model(&project).ok().flatten());
                let first = format!("{}\n\n{text}", self.handoff(&place.f, doc, rel, w, if turn.is_some() { "press" } else { "message" }));
                let name = d["field"]["app"].as_str().map(|app| format!("{app}: {}", d["field"]["title"].as_str().unwrap_or(rel)));
                let title = if doc == coordinator_doc() {
                    "Ambition · coordinator".to_string()
                } else {
                    format!("Ambition · {}", util::cut(name.as_deref().unwrap_or(rel), 70))
                };
                let (tid, run) = self.engine.launch(&project, &title, &first, sel.as_ref())?;
                let link = self.engine.thread(&tid).ok().flatten().map(|t| t.link).unwrap_or_default();
                if let Some(s) = &sel {
                    let _g = self.store.lock().unwrap();
                    let mut d = load(&Self::models_file());
                    d["started"] = sel_json(s);
                    save(&Self::models_file(), &d);
                }
                if !old.is_empty() && w != "gone" {
                    let _ = self.engine.settle(&old);
                }
                let label = self.label(sel.as_ref());
                let agents = self.agents_sha(doc, root);
                self.update_entry(doc, |v| {
                    v["folder"] = json!(key(root));
                    v["thread"] = json!(tid);
                    v["link"] = json!(link);
                    v["model"] = json!(label);
                    v["started"] = json!(now());
                    v["rules"] = json!(util::sha(RULES));
                    v["agents"] = json!(agents);
                    v["new"] = json!(false);
                    v["previous"] = if old.is_empty() { Value::Null } else { json!(old) };
                });
                place.f.history(&format!("## {} · {rel} · thread\nA new thread ({w}): {}", now(), if link.is_empty() { &tid } else { &link }));
                log(&format!("new thread {tid} for {} ({w})", util::tilde(doc)));
                (tid, link, run)
            }
            Some("first") | Some("gone") => return Err(format!("{} has no thread; only a press in it starts one.", util::tilde(doc))),
            _ => {
                let run = self.engine.send(&old, text)?;
                (old, d["link"].as_str().unwrap_or("").to_string(), run)
            }
        };
        place.q.lock().unwrap().tid = Some(tid.clone());
        if let Some(id) = turn {
            self.set_turn(id, |t| t.thread = link.clone());
            self.publish();
        }
        let status = self.engine.wait(&tid, &run, self.run_limit)?;
        Ok((tid, link, run, status))
    }

    // ---------- fields ----------

    /// A press in a text field. `snap` is what the field said; `surface`
    /// reaches it again. The caller has asked for the app's grant
    /// (`grant_app`) before reading the field.
    pub fn field_press(self: &Arc<Self>, snap: Snapshot, surface: Arc<dyn Surface>) -> Result<String, String> {
        C::reload();
        let (dir, name) = field::mirror(&snap);
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", util::tilde(&dir)))?;
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let doc = dir.join(&name);
        if let Some(r) = self.again(&doc, &snap.text, surface.clone()) {
            if let Some(l) = self.fields.lock().unwrap().get_mut(&doc) {
                l.snap = snap.clone();
            }
            return r;
        }
        if field::blank(&snap.text) {
            let mut m = format!("the field in {} looks empty to Ambition, so there is nothing to send.", snap.app);
            if snap.url.as_deref().is_some_and(|u| u.contains("docs.google.com")) {
                m.push_str(" Google Docs shows its text only with its screen reader and braille support on: in the document, \
                    press ⌘⌥Z, then ⌘⌥H (Tools › Accessibility settings).");
            }
            return Err(m);
        }
        let lines: Vec<&str> = snap.text.split('\n').collect();
        let i = snap.line.min(lines.len().saturating_sub(1));
        let what = C::pressed(&name, lines[i], i + 1)?;
        let place = self.place(&dir)?;
        self.busy_here(&place, &doc)?;
        {
            let _io = self.io.lock().unwrap();
            place.f.write(&name, &snap.text);
        }
        let entry = json!({"app": snap.app, "bundle": snap.bundle, "title": snap.title, "url": snap.url, "where": field::describe(&snap)});
        let note = format!("This document is {}. Ambition mirrors that field in {}: edit the file, and Ambition puts your \
            changes in the field when your turn ends. For the rest of the window: `{cli} read` (its text) or `{cli} look` \
            (a screenshot).", field::describe(&snap), util::tilde(&doc), cli = self.cli);
        self.fields.lock().unwrap().insert(doc.clone(), Live { surface, shown: snap.text.clone(), snap, span: None, pending: None, note });
        self.update_entry(&doc, |v| v["field"] = entry);
        self.dispatch(&place, &doc, &name, i, what)
    }

    /// Write a mirror's new text into its field, when the document is one
    /// (else nothing happens). `mark` highlights what changed. When `keep`
    /// names it and the write cannot happen, the text waits for the next
    /// press in that field.
    fn push(&self, doc: &Path, new: &str, mark: bool, keep: Option<&str>) -> Result<(), String> {
        let (surface, shown) = match self.fields.lock().unwrap().get(doc) {
            Some(l) => (l.surface.clone(), l.shown.clone()),
            None => return Ok(()),
        };
        if shown == new {
            return Ok(());
        }
        let r = surface.write(&shown, new, mark);
        let mut f = self.fields.lock().unwrap();
        let Some(l) = f.get_mut(doc) else { return r.map(|_| ()) };
        match r {
            Ok(now) => {
                l.span = mark.then(|| field::changed(&shown, &now));
                l.shown = now;
                l.pending = None;
                Ok(())
            }
            Err(e) => {
                if let Some(what) = keep {
                    l.pending = Some((what.to_string(), new.to_string()));
                }
                Err(e)
            }
        }
    }

    /// Write an answer into its field, or say why it is not there.
    fn push_or_tell(&self, doc: &Path, new: &str, what: &str) {
        if let Err(e) = self.push(doc, new, true, Some(what)) {
            let place = self.fields.lock().unwrap().get(doc).map(|l| l.snap.app.clone()).unwrap_or_default();
            ui::tell(&format!("{what} is ready, but not in {place}: {e} A press in that field puts it in, if the field is as \
                Ambition left it. It is also in {}.", util::tilde(doc)));
        }
    }

    /// Before a field's turn starts: when the field changed while the press
    /// waited, and still holds its status line, the mirror takes the field.
    fn catch_up(&self, doc: &Path, rel: &str, id: &str, f: &Folder) {
        let Some((surface, shown)) = self.fields.lock().unwrap().get(doc).map(|l| (l.surface.clone(), l.shown.clone())) else { return };
        let Ok(now) = surface.read() else { return };
        if now != shown && C::statuses(rel, &now).contains_key(id) {
            f.write(rel, &now);
            if let Some(l) = self.fields.lock().unwrap().get_mut(doc) {
                l.shown = now;
            }
        }
    }

    /// The field whose turn an agent's command came from. Only a granted
    /// press starts a turn, so a running turn is the grant for `read` and
    /// `look`: Always for that app, or Allow once for this press.
    fn turn_field(&self, cwd: Option<&Path>) -> Result<(Arc<dyn Surface>, Snapshot), String> {
        let Some((doc, _)) = self.asker(cwd) else {
            return Err("no press is with an agent here now, so there is no window to read.".into());
        };
        match self.fields.lock().unwrap().get(&doc) {
            Some(l) => Ok((l.surface.clone(), l.snap.clone())),
            None => Err(format!("this turn's document, {}, is a file, not a text field: `ambition read` and `ambition look` \
                work in a field's turn.", util::tilde(&doc))),
        }
    }

    /// `ambition read`: the visible text of the field's window, or with
    /// `field`, the whole field as it is now.
    pub fn agent_read(&self, what: &str, cwd: Option<&Path>) -> Result<String, String> {
        let (s, snap) = self.turn_field(cwd)?;
        if what == "field" {
            return s.read();
        }
        let t = s.window_text()?;
        Ok(format!("The visible text of {}'s window “{}”, from Accessibility:\n\n{t}", snap.app, snap.title))
    }

    /// `ambition look`: a screenshot of the field's window.
    pub fn agent_look(&self, cwd: Option<&Path>) -> Result<String, String> {
        let (s, snap) = self.turn_field(cwd)?;
        let dir = util::state_dir().join("looks");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut old: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        old.sort();
        for p in old.iter().take(old.len().saturating_sub(19)) {
            let _ = std::fs::remove_file(p);
        }
        let out = dir.join(format!("{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S-%3f")));
        s.look(&out)?;
        Ok(format!("A screenshot of {}'s window “{}” is at {}. Open the image to see it.", snap.app, snap.title, out.display()))
    }

    // ---------- what agents ask for ----------

    /// The busy folder an agent's command came from, by its working folder:
    /// (that turn's document, its thread link).
    fn asker(&self, cwd: Option<&Path>) -> Option<(PathBuf, String)> {
        let places: Vec<Arc<Place>> = self.places.lock().unwrap().values().cloned().collect();
        let busy: Vec<(PathBuf, PathBuf)> = places.iter().filter_map(|p| {
            let q = p.q.lock().unwrap();
            q.tid.as_ref().and(q.doc.clone()).map(|d| (p.f.root.clone(), d))
        }).collect();
        let hit: Vec<&(PathBuf, PathBuf)> = match cwd {
            Some(c) => busy.iter().filter(|(r, _)| c.starts_with(r)).collect(),
            None => Vec::new(),
        };
        let pick = if hit.len() == 1 { Some(hit[0]) } else if busy.len() == 1 { Some(&busy[0]) } else {
            // Nested folders: the deepest one that holds it.
            hit.iter().max_by_key(|(r, _)| r.components().count()).copied()
        };
        pick.map(|(_, d)| (d.clone(), self.entry(d)["link"].as_str().unwrap_or("").to_string()))
    }

    /// `ambition more`: more of the document whose turn is running.
    pub fn more(&self, up: bool, n: usize, cwd: Option<&Path>) -> Result<String, String> {
        let mut turns = self.turns.lock().unwrap();
        let working: Vec<usize> = (0..turns.len()).filter(|&k| turns[k].working && turns[k].seen != (0, 0)).collect();
        let hit: Vec<usize> = working.iter().copied().filter(|&k| cwd.is_some_and(|c| c.starts_with(&turns[k].folder))).collect();
        let k = match (hit.len(), working.len()) {
            (1, _) => hit[0],
            (_, 1) => working[0],
            (_, 0) => return Err("no press is with an agent now, so there is no document to show more of.".into()),
            _ => return Err("several presses are with agents now: run it from your document's folder.".into()),
        };
        let t = &mut turns[k];
        let text = std::fs::read_to_string(&t.text_at).map_err(|e| format!("cannot read {}: {e}", util::tilde(&t.doc)))?;
        let (out, seen) = hook::more(&util::tilde(&t.doc), &text, t.seen, up, n);
        t.seen = seen;
        Ok(out)
    }

    /// `ambition ask`: a message for the coordinator, answered when its turn
    /// ends; or, from the coordinator, one passed on to a document's thread.
    pub fn ask(self: &Arc<Self>, to: &str, message: &str, cwd: Option<&Path>) -> Result<String, String> {
        let message = message.trim();
        if message.is_empty() || to.is_empty() {
            return Err("ask whom, and what: ambition ask coordinator \"<message>\"".into());
        }
        let from = self.asker(cwd);
        let who = match &from {
            Some((d, _)) if *d == coordinator_doc() => "the coordinator".to_string(),
            Some((d, l)) => format!("the thread for {}{}", util::tilde(d), if l.is_empty() { String::new() } else { format!(" ({l})") }),
            None => "an agent Ambition could not place (it ran `ambition ask` outside any busy folder)".into(),
        };
        let quoted: Vec<String> = message.split('\n').map(|l| format!("  {l}")).collect();
        let quoted = quoted.join("\n");
        if to.eq_ignore_ascii_case("coordinator") {
            let doc = coordinator_doc();
            if from.as_ref().is_some_and(|(d, _)| *d == doc) {
                return Err("you are the coordinator: answer in your own turn.".into());
            }
            if !doc.exists() {
                let _ = std::fs::write(&doc, COORDINATOR_DOC);
            }
            let place = self.place(&util::state_real())?;
            let text = format!("[Ambition] A message for the coordinator, from {who}:\n\n{quoted}\n\n\
                Your last message in this turn is your answer: Ambition prints it for the thread that asked.");
            let (tx, rx) = channel();
            self.enqueue(&place, Job::Message(Msg { doc, rel: COORDINATOR.into(), text, reply: Some(tx) }));
            return match rx.recv_timeout(self.run_limit + Duration::from_secs(120)) {
                Ok(Ok(a)) if a.is_empty() => Ok("(the coordinator answered with nothing)".into()),
                Ok(r) => r,
                Err(_) => Err("the coordinator did not answer in time.".into()),
            };
        }
        let doc = std::fs::canonicalize(util::expand(to)).map_err(|_| format!("{to}: no such file. Name a document, or `coordinator`."))?;
        let d = self.entry(&doc);
        let (Some(_), Some(root)) = (d["thread"].as_str(), d["folder"].as_str()) else {
            return Err(format!("{} has no thread yet, and only a press in it starts one.", util::tilde(&doc)));
        };
        if from.as_ref().is_some_and(|(f, _)| *f == doc) {
            return Err("that is your own document.".into());
        }
        let place = self.place(Path::new(root))?;
        let rel = place.f.rel(&doc).unwrap_or_else(|| key(&doc));
        let text = format!("[Ambition] A message for this document's thread, from {who}:\n\n{quoted}\n\n\
            Answer in this thread. Change the document only if the message asks you to, and write no `^>` lines this turn.");
        self.enqueue(&place, Job::Message(Msg { doc: doc.clone(), rel, text, reply: None }));
        Ok(format!("passed on to the thread for {}: it reads it when its folder is free, and answers in its own thread{}.",
            util::tilde(&doc), d["link"].as_str().filter(|l| !l.is_empty()).map(|l| format!(", {l}")).unwrap_or_default()))
    }

    // ---------- models ----------

    fn models_file() -> PathBuf {
        own("models.json")
    }

    /// The model of the last thread Ambition started: as kept in
    /// `models.json`, else asked of the newest thread in `threads`.
    fn last_started(&self) -> Option<Selection> {
        if let Some(s) = sel_from(&load(&Self::models_file())["started"]) {
            return Some(s);
        }
        let all = load(&own("threads"));
        let mut rows: Vec<(&str, &str)> = all.as_object()?.values()
            .filter_map(|v| Some((v["started"].as_str()?, v["thread"].as_str()?))).collect();
        rows.sort();
        rows.iter().rev().find_map(|(_, tid)| self.engine.model_of(tid).ok().flatten())
    }

    fn refresh_models(&self) -> Result<Vec<Model>, String> {
        let ms = self.engine.models(None)?;
        let rows: Vec<Value> = ms.iter().map(model_json).collect();
        let _g = self.store.lock().unwrap();
        let mut d = load(&Self::models_file());
        d["models"] = json!(rows);
        d["at"] = json!(util::epoch());
        save(&Self::models_file(), &d);
        Ok(ms)
    }

    fn cached_models(&self) -> Vec<Model> {
        load(&Self::models_file())["models"].as_array().into_iter().flatten().filter_map(model_from).collect()
    }

    /// The model chosen last, which new threads start on.
    fn last_model(&self) -> Option<Selection> {
        sel_from(&load(&Self::models_file())["last"])
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

    /// The model the document's thread uses, else the one a new thread gets.
    fn current(&self, doc: &Path) -> Option<Selection> {
        if let Some(tid) = self.entry(doc)["thread"].as_str() {
            if let Ok(Some(s)) = self.engine.model_of(tid) {
                return Some(s);
            }
        }
        self.last_model()
    }

    fn cmd_model(&self, place: &Place, doc: &Path, args: &str) -> String {
        let engine = self.engine.name();
        let cur = self.current(doc);
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
                    return self.switch(place, doc, &m, Some(words[0]));
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
            Some(m) => self.switch(place, doc, &m, effort),
            None => {
                let names: Vec<String> = hits.iter().take(12).map(|m| m.label.clone()).collect();
                format!("model: “{q}” fits {} models: {}. Name one more exactly. Nothing changed.", hits.len(), names.join(" · "))
            }
        }
    }

    fn switch(&self, place: &Place, doc: &Path, m: &Model, effort: Option<&str>) -> String {
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
        let tid = self.entry(doc)["thread"].as_str().map(str::to_string);
        let pid = match self.project_id(&place.f.root) {
            Ok(p) => p,
            Err(e) => return format!("model: could not reach {} ({e}). Nothing changed.", self.engine.name()),
        };
        if let Err(e) = self.engine.use_model(tid.as_deref(), &pid, &sel) {
            return format!("model: {} would not switch ({e}). Nothing changed.", self.engine.name());
        }
        {
            let _g = self.store.lock().unwrap();
            let mut d = load(&Self::models_file());
            let mut ms: Vec<Value> = d["models"].as_array().cloned().unwrap_or_default();
            if !ms.iter().any(|x| x["model"] == json!(m.id)) {
                ms.push(model_json(m));
            }
            d["models"] = json!(ms);
            d["last"] = sel_json(&sel);
            save(&Self::models_file(), &d);
        }
        let label = self.label(Some(&sel));
        if tid.is_some() {
            self.update_entry(doc, |v| v["model"] = json!(label));
            format!("model: {label}. This file's thread uses it from its next turn, and new threads start on it.")
        } else {
            format!("model: {label}. New threads start on it, this file's first among them.")
        }
    }

    // ---------- commands ----------

    fn help_text(&self) -> String {
        let mut rows: Vec<String> = Cm::BUILTINS.iter().map(|(n, d)| format!("/{n}: {d}")).collect();
        let mine = Cm::find();
        let own: Vec<(&String, &PathBuf)> = mine.iter().filter(|(n, _)| !Cm::is_builtin(n)).collect();
        for (n, p) in &own {
            let d = Cm::describe(p);
            rows.push(format!("/{n}: {}", if d.is_empty() { p.file_name().unwrap().to_string_lossy().to_string() } else { d }));
        }
        if own.is_empty() {
            rows.push(format!("Your own: a script in {}/ (its README.md says how).", util::tilde(&Cm::dir())));
        }
        rows.join("\n")
    }

    fn cmd_thread(&self, doc: &Path) -> String {
        let d = self.entry(doc);
        let Some(tid) = d["thread"].as_str() else { return "thread: none yet. A press on a ^ prompt in this file starts one.".into() };
        let th = match self.engine.thread(tid) {
            Ok(None) => return format!("thread: {} no longer has it. This file's next prompt starts a new one, with a handoff.", self.engine.name()),
            Ok(t) => t,
            Err(_) => None,
        };
        let link = d["link"].as_str().filter(|l| !l.is_empty()).unwrap_or(tid);
        let model = d["model"].as_str().map(str::to_string).unwrap_or_else(|| self.label(self.current(doc).as_ref()));
        let mut out = format!("thread: {link} · model: {model}");
        let mut more = Vec::new();
        if let Some(t) = th {
            more.push(format!("status: {}, {} turn{}", t.status, t.turns, if t.turns == 1 { "" } else { "s" }));
        }
        let folder = PathBuf::from(d["folder"].as_str().unwrap_or(""));
        match self.why_new(doc, &folder) {
            Some("asked") => more.push("^/new was pressed, so this file's next prompt starts a new thread".into()),
            Some("rules") => more.push("Ambition's rules changed, so this file's next prompt starts a new thread".into()),
            Some("agents") => more.push("an AGENTS.md changed, so this file's next prompt starts a new thread".into()),
            _ => {}
        }
        if !more.is_empty() {
            out.push('\n');
            out.push_str(&more.join(" · "));
        }
        out
    }

    fn cmd_new(&self, doc: &Path) -> String {
        let d = self.entry(doc);
        let Some(tid) = d["thread"].as_str().map(str::to_string) else {
            return "new: this file has no thread yet; its first prompt starts one.".into();
        };
        self.update_entry(doc, |v| v["new"] = json!(true));
        format!("new: this file's next prompt starts a new thread, with a handoff from {}.", d["link"].as_str().filter(|l| !l.is_empty()).unwrap_or(&tid))
    }

    fn cmd_stop(&self, place: &Place, held: bool) -> String {
        let (busy, tid) = {
            let q = place.q.lock().unwrap();
            (q.busy.clone(), q.tid.clone())
        };
        match (held, busy, tid) {
            (false, Some(b), Some(t)) => match self.engine.interrupt(&t) {
                Ok(()) => format!("stop: asked {} to stop the turn for {b}.", self.engine.name()),
                Err(e) => format!("stop: {} would not stop it ({e}).", self.engine.name()),
            },
            (false, Some(b), None) => format!("stop: {b} has not reached its thread yet. Press ^/stop again in a moment."),
            _ => format!("stop: nothing is running in {}.", util::tilde(&place.f.root)),
        }
    }

    /// Undo Ambition's last change in the folder that is not undone yet.
    fn undo(&self, place: &Place) -> String {
        let f = &place.f;
        let _io = self.io.lock().unwrap();
        for (h, subject) in f.undoable() {
            let plan = match f.reverse(&h) {
                Ok(p) => p,
                Err(why) => return format!("could not undo {h} ({}): {why}. Nothing changed.", util::cut(subject.trim_start_matches("Ambition: "), 64)),
            };
            if plan.is_empty() {
                continue; // that change only wrote replies: nothing to take back
            }
            for (rel, text) in &plan {
                match text {
                    None => {
                        let _ = std::fs::remove_file(f.path(rel));
                    }
                    Some(t) => {
                        f.write(rel, t);
                    }
                }
            }
            let what = subject.trim_start_matches("Ambition: ");
            f.commit(&format!("Ambition: undo {h}: {what}"));
            return format!("undid {h}: {} ({})", util::cut(what, 110), plan.keys().cloned().collect::<Vec<_>>().join(", "));
        }
        "nothing left to undo.".into()
    }

    fn run_command(&self, c: &C::Command, place: &Place, doc: &Path, rel: &str, above: &str, held: bool) -> String {
        match c.name.as_str() {
            "help" => self.help_text(),
            "model" => self.cmd_model(place, doc, &c.args),
            "new" => self.cmd_new(doc),
            "thread" => self.cmd_thread(doc),
            "stop" => self.cmd_stop(place, held),
            "undo" if !held => format!("undo: an agent is working in {}. Press ^/undo again when its turn ends.", util::tilde(&place.f.root)),
            "undo" => self.undo(place),
            name => {
                let mine = Cm::find();
                let Some(p) = mine.get(name) else {
                    let all: Vec<&str> = Cm::BUILTINS.iter().map(|(n, _)| *n).chain(mine.keys().map(String::as_str)).collect();
                    let near = diff::closest(name, all);
                    return format!("/{name}: no such command.{} ^/help lists them.", near.map(|n| format!(" Did you mean ^/{n}?")).unwrap_or_default());
                };
                let (out, code) = Cm::run(&place.f.root, p, &c.args, rel, c.line + 1, above);
                let out = if out.is_empty() { "(no output)".to_string() } else { out };
                if code != 0 && code != -1 {
                    format!("{out}\n(exit {code})")
                } else {
                    out
                }
            }
        }
    }
}

/// A status line's words: the prompt's, cut, then its state.
fn status_words(text: &str, state: &str) -> String {
    if text.is_empty() {
        state.to_string()
    } else {
        format!("{} · {state}", util::cut(text, 48))
    }
}

/// Whether a file other than `rel` differs from the last commit.
fn others_changed(f: &Folder, rel: &str) -> bool {
    let out = f.git(&["status", "--porcelain", "-z", if f.whole() { "--untracked-files=all" } else { "--untracked-files=no" }]);
    out.stdout.split(|&b| b == 0).filter_map(|s| std::str::from_utf8(s).ok()).filter(|s| s.len() > 3).any(|s| &s[3..] != rel)
}

/// The paragraph (or last list item) just above line index `at`, as text.
fn unit_above(rel: &str, text: &str, at: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    C::above(rel, &lines, at.min(lines.len()), &BTreeSet::new()).map(|(s, e)| lines[s..e].join("\n")).unwrap_or_default()
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

fn sel_json(s: &Selection) -> Value {
    json!({"instance": s.instance, "model": s.model, "effort": s.effort.as_ref().map(|(k, v)| json!([k, v]))})
}

fn sel_from(v: &Value) -> Option<Selection> {
    let model = v["model"].as_str()?.to_string();
    let effort = v["effort"].as_array().and_then(|e| Some((e.first()?.as_str()?.to_string(), e.get(1)?.as_str()?.to_string())));
    Some(Selection { instance: v["instance"].as_str().unwrap_or("").to_string(), model, effort })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_name_the_document_and_the_hook() {
        assert!(RULES.contains("{document}") && RULES.contains("{folder}"));
        assert!(RULES.contains("ambition more up") && RULES.contains("ambition ask coordinator"));
        assert!(!RULES.to_lowercase().contains("when they save"), "a save sends nothing now");
    }
}
