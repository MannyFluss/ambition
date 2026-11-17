//! `ambition`: point it at a folder, and every file there becomes a document
//! you can talk to.

use ambition::app::{log, App};
use ambition::folder::Folder;
use ambition::{t3::T3, util};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const USAGE: &str = "\
Ambition: a document you talk to.

  ambition ~/notes     watch a folder. In any file there, start a line with ^,
                       write what you want, and save. The answer lands in the
                       file.
  ambition             watch the folder named on the one line of
                       ~/.config/ambition. The first `ambition <folder>`
                       writes that line.
  ambition lsp         the language server, for editors

In a file:

  ^ make this shorter  a prompt for the agent
  ^/help               a command: answered at once, with no model
  ^> ...               a reply: your next save clears it
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lsp") => return ambition::lsp::main(),
        Some("-h" | "--help" | "help") => return print!("{USAGE}"),
        Some("-V" | "--version" | "version") => return println!("ambition {}", env!("CARGO_PKG_VERSION")),
        Some(a) if a.starts_with('-') => fail(2, &format!("ambition: no option {a}.\n\n{USAGE}")),
        _ => {}
    }
    let config = util::home().join(".config").join("ambition");
    let folder = match args.first() {
        Some(a) => util::expand(a),
        None => read_config(&config).unwrap_or_else(|| fail(2, USAGE)),
    };
    let root = match std::fs::canonicalize(&folder) {
        Ok(p) if p.is_dir() => p,
        _ => fail(2, &format!("ambition: {} is not a folder.", folder.display())),
    };
    let home = std::fs::canonicalize(util::home()).unwrap_or_else(|_| util::home());
    if root == home || root.parent().is_none() {
        fail(2, "ambition: watch one folder, not your whole home or disk. Pick one inside it, like ~/notes.");
    }
    if !args.is_empty() && !config.exists() {
        let _ = std::fs::create_dir_all(config.parent().unwrap());
        let _ = std::fs::write(&config, format!("{}\n", util::tilde(&root)));
    }

    let f = Folder::new(&root);
    let _ = std::fs::create_dir_all(&f.dot);
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(f.dot.join("lock"));
    let lock = lock.unwrap_or_else(|e| fail(1, &format!("ambition: cannot write in {}: {e}", f.dot.display())));
    if lock.try_lock().is_err() {
        fail(1, &format!("ambition: another Ambition is already watching {}.", util::tilde(&root)));
    }

    let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let label = format!("Ambition · {name} · {}", &util::sha(&root.to_string_lossy())[..6]);
    let app = Arc::new(App::new(f, Arc::new(T3::new(&label))));
    if let Err(e) = app.start() {
        fail(1, &format!("ambition: {e}"));
    }
    let _watcher = watch(&root, app.clone());
    log(&format!("watching {}. Start a line with ^ in any file there, write what you want, and save.", util::tilde(&root)));
    app.serve();
    drop(lock);
}

/// The first line of the config file that is not blank or a # comment.
fn read_config(p: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(p).ok()?;
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).map(util::expand)
}

/// Ask the operating system to say when files change, and hand each burst of
/// changes to the app. Between saves nothing runs.
fn watch(root: &Path, app: Arc<App>) -> Box<dyn Watcher> {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut w: Box<dyn Watcher> = match notify::recommended_watcher(tx.clone()) {
        Ok(w) => Box::new(w),
        Err(e) => {
            log(&format!("no file events here ({e}); checking every second instead"));
            let cfg = notify::Config::default().with_poll_interval(Duration::from_secs(1));
            Box::new(notify::PollWatcher::new(tx, cfg).unwrap_or_else(|e| fail(1, &format!("ambition: cannot watch files: {e}"))))
        }
    };
    if let Err(e) = w.watch(root, RecursiveMode::Recursive) {
        fail(1, &format!("ambition: cannot watch {}: {e}", root.display()));
    }
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut paths = Vec::new();
            let mut take = |ev: notify::Result<notify::Event>| {
                // Reads are events too on some systems, and Ambition reads a file
                // to see what changed: answering them would loop forever.
                if let Ok(ev) = ev {
                    if !matches!(ev.kind, notify::EventKind::Access(_)) {
                        paths.extend(ev.paths);
                    }
                }
            };
            take(first);
            while let Ok(ev) = rx.recv_timeout(Duration::from_millis(60)) {
                take(ev);
            }
            paths.sort();
            paths.dedup();
            app.changed(paths);
        }
    });
    w
}

fn fail(code: i32, msg: &str) -> ! {
    eprintln!("{}", msg.trim_end());
    std::process::exit(code)
}
