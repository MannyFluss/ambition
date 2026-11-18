//! A folder with presses in it, and what Ambition keeps for it, outside it.
//!
//! ```text
//! ~/.ambition/history/<folder>-<hash>.git   the private history: a bare git
//!                                           repo whose work tree is the folder
//! ~/.ambition/history/<folder>-<hash>.md    every prompt, command and reply
//! ```
//!
//! `^/undo` works from the private history, so your own repo is never
//! touched. It stays outside the folder: inside, editors took it for a
//! repository of yours (Zed hung on it). Ambition writes nothing in the
//! folder but the answers.

use crate::{carets as C, diff};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a git run printed, and whether it worked.
pub struct Out {
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A bigger file is never read for carets.
pub const MAX_BYTES: u64 = 512 * 1024;

/// Ambition's own files in its own folder, kept out of that folder's history.
const OWN: &str = "/history/\n/state.json\n/threads\n/models.json\n/inbox\n/config\n/lock\n/*.tmp*\n";

pub struct Folder {
    pub root: PathBuf,
    /// The private history (see above).
    pub git_dir: PathBuf,
    /// The record of every prompt, command and reply here.
    pub log: PathBuf,
}

/// Commit subjects of the changes ^/undo may take back.
pub const UNDOABLE: [&str; 2] = ["Ambition: agent ", "Ambition: command "];

impl Folder {
    pub fn new(root: &Path) -> Folder {
        let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "folder".into());
        let name = name.trim_start_matches('.').to_string();
        let hash = crate::util::sha(&root.to_string_lossy());
        let base = crate::util::state_dir().join("history").join(format!("{name}-{}", &hash[..8]));
        Folder { root: root.to_path_buf(), git_dir: base.with_extension("git"), log: base.with_extension("md") }
    }

    /// A path relative to the folder, with `/`.
    pub fn rel(&self, p: &Path) -> Option<String> {
        let r = p.strip_prefix(&self.root).ok()?.to_string_lossy().replace('\\', "/");
        (!r.is_empty()).then_some(r)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Make the private history, if it is not there yet.
    pub fn init(&self) -> Result<(), String> {
        let git = self.git_dir.clone();
        if !git.join("HEAD").exists() {
            if let Some(parent) = git.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let out = Command::new("git")
                .args(["init", "--quiet", "--bare"])
                .arg(&git)
                .output()
                .map_err(|e| format!("Ambition needs git, and could not run it: {e}"))?;
            if !out.status.success() {
                return Err(format!("git init failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            let own = crate::util::state_real();
            let exclude = if self.root == own { OWN } else { "" };
            let _ = fs::create_dir_all(git.join("info"));
            let _ = fs::write(git.join("info").join("exclude"), format!("/.ambition/\n{exclude}"));
            for (k, v) in [
                ("user.name", "Ambition"),
                ("user.email", "ambition@localhost"),
                ("commit.gpgsign", "false"),
                ("core.autocrlf", "false"),
                ("core.quotePath", "false"),
                ("gc.auto", "256"),
            ] {
                self.git(&["config", k, v]);
            }
            let nohooks = git.join("no-hooks");
            let _ = fs::create_dir_all(&nohooks);
            self.git(&["config", "core.hooksPath", &nohooks.to_string_lossy()]);
        }
        if self.git(&["rev-parse", "--verify", "-q", "HEAD"]).stdout.is_empty() {
            self.commit("Ambition: the first press in this folder");
        }
        Ok(())
    }

    /// git, with the private history as its repo and the folder as its work tree.
    pub fn git(&self, args: &[&str]) -> Out {
        self.git_in(args, None)
    }

    pub fn git_in(&self, args: &[&str], input: Option<&[u8]>) -> Out {
        let mut c = Command::new("git");
        c.arg(format!("--git-dir={}", self.git_dir.display()))
            .arg(format!("--work-tree={}", self.root.display()))
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let Ok(mut child) = c.spawn() else {
            return Out { ok: false, stdout: Vec::new(), stderr: b"cannot run git".to_vec() };
        };
        if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
            let _ = stdin.write_all(data);
        }
        match child.wait_with_output() {
            Ok(o) => Out { ok: o.status.success(), stdout: o.stdout, stderr: o.stderr },
            Err(e) => Out { ok: false, stdout: Vec::new(), stderr: e.to_string().into_bytes() },
        }
    }

    pub fn git_text(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.git(args).stdout).trim_end().to_string()
    }

    /// The text files Ambition reads for carets: what git would track here
    /// (so `.gitignore` is honored), not hidden, at most `MAX_BYTES`, and text.
    pub fn files(&self) -> Vec<String> {
        let out = self.git(&["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
        let mut seen = std::collections::BTreeSet::new();
        for rel in out.stdout.split(|&b| b == 0) {
            let Ok(rel) = std::str::from_utf8(rel) else { continue };
            if rel.is_empty() || !self.watchable(rel) {
                continue;
            }
            seen.insert(rel.to_string());
        }
        seen.into_iter().collect()
    }

    /// Whether a file (relative to the folder) is one Ambition reads: not in a
    /// hidden folder or hidden itself, a regular file, small, and text.
    pub fn watchable(&self, rel: &str) -> bool {
        if rel.split('/').any(|p| p.starts_with('.') || p.is_empty()) {
            return false;
        }
        let p = self.path(rel);
        match fs::metadata(&p) {
            Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
            _ => return false,
        }
        is_text(&p)
    }

    pub fn read(&self, rel: &str) -> Option<String> {
        let bytes = fs::read(self.path(rel)).ok()?;
        String::from_utf8(bytes).ok()
    }

    /// Write a file, unless it already says this.
    pub fn write(&self, rel: &str, text: &str) -> bool {
        if self.read(rel).as_deref() == Some(text) {
            return false;
        }
        let p = self.path(rel);
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        fs::write(p, text).is_ok()
    }

    /// Append an entry to the folder's log.
    pub fn history(&self, entry: &str) {
        let p = &self.log;
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        if !p.exists() {
            let _ = fs::write(p, format!("# Ambition history for {}\n\nEvery prompt, command and reply, newest last.\n",
                crate::util::tilde(&self.root)));
        }
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(p) {
            let _ = write!(f, "\n{}\n", entry.trim_end_matches('\n'));
        }
    }

    /// Commit what changed in the folder to the private history. Returns the
    /// short hash, or "" when nothing changed.
    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "--update", "--", "."]);
        let tracked: std::collections::BTreeSet<String> =
            self.git_text(&["ls-files", "-z"]).split('\0').map(str::to_string).collect();
        let new: Vec<String> = self.files().into_iter().filter(|f| !tracked.contains(f)).collect();
        if !new.is_empty() {
            let list = new.join("\0");
            self.git_in(&["add", "--force", "--pathspec-from-file=-", "--pathspec-file-nul"], Some(list.as_bytes()));
        }
        if self.git(&["diff", "--cached", "--quiet"]).ok
            && !self.git_text(&["rev-parse", "--verify", "-q", "HEAD"]).is_empty()
        {
            return String::new();
        }
        if !self.git(&["commit", "--quiet", "--allow-empty", "-m", message]).ok {
            return String::new();
        }
        self.git_text(&["rev-parse", "--short", "HEAD"])
    }

    pub fn head(&self) -> String {
        self.git_text(&["rev-parse", "HEAD"])
    }

    /// A file as it was at a commit, or None.
    pub fn show(&self, rev: &str, rel: &str) -> Option<String> {
        let out = self.git(&["show", &format!("{rev}:{rel}")]);
        if !out.ok {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }

    /// The files a commit changed: [(status letter, path)].
    pub fn changed_in(&self, rev: &str) -> Vec<(String, String)> {
        let out = self.git_text(&["diff-tree", "-r", "--no-commit-id", "--name-status", "--no-renames", "--root", rev]);
        out.lines()
            .filter_map(|row| {
                let (st, rel) = row.split_once('\t')?;
                Some((st.to_string(), rel.to_string()))
            })
            .collect()
    }

    /// What changed between two commits: [(status letter, path)].
    pub fn changed_between(&self, a: &str, b: &str) -> Vec<(String, String)> {
        let out = self.git_text(&["diff", "--name-status", "--no-renames", a, b]);
        out.lines()
            .filter_map(|row| {
                let (st, rel) = row.split_once('\t')?;
                Some((st.to_string(), rel.to_string()))
            })
            .collect()
    }

    /// Ambition's last change that ^/undo has not taken back yet:
    /// (short hash, subject).
    pub fn undoable(&self) -> Vec<(String, String)> {
        let log = self.git_text(&["log", "-n", "200", "--format=%h %s"]);
        let mut undone = std::collections::BTreeSet::new();
        for l in log.lines() {
            if let Some(rest) = l.split_once(' ').map(|x| x.1) {
                if let Some(h) = rest.strip_prefix("Ambition: undo ") {
                    undone.insert(h.split(':').next().unwrap_or("").to_string());
                }
            }
        }
        log.lines()
            .filter_map(|l| {
                let (h, subject) = l.split_once(' ')?;
                (!undone.contains(h) && UNDOABLE.iter().any(|p| subject.starts_with(p)))
                    .then(|| (h.to_string(), subject.to_string()))
            })
            .collect()
    }

    /// What undoing commit `h` does to the files now: {rel: new text, or None
    /// to delete}, or why it cannot. Reply and status lines are left out of
    /// the comparison: a press clears replies anyway, and status lines are
    /// Ambition's own.
    pub fn reverse(&self, h: &str) -> Result<BTreeMap<String, Option<String>>, String> {
        let mut out = BTreeMap::new();
        for (_, rel) in self.changed_in(h) {
            let clear = |t: Option<String>| t.map(|t| C::clear_marks(&rel, &t));
            let before = clear(self.show(&format!("{h}^"), &rel));
            let after = clear(self.show(h, &rel));
            let cur = self.read(&rel);
            if before == after {
                continue;
            }
            match (before, after) {
                (None, Some(after)) => {
                    // The change made this file.
                    if let Some(c) = &cur {
                        if C::clear_marks(&rel, c) != after {
                            return Err(format!("{rel} has changed since that run made it"));
                        }
                    }
                    out.insert(rel, None);
                }
                (Some(before), None) => {
                    if cur.is_some() {
                        return Err(format!("{rel} exists again"));
                    }
                    out.insert(rel, Some(before));
                }
                (Some(before), Some(after)) => {
                    let cur = cur.unwrap_or_default();
                    let text = match merge_file(&cur, &after, &before) {
                        Some(t) => t,
                        None => unpatch(&cur, &after, &before).ok_or(format!("you have changed the same lines of {rel} since"))?,
                    };
                    out.insert(rel, Some(text));
                }
                (None, None) => {}
            }
        }
        Ok(out)
    }
}

/// `git merge-file`: the text with the change from `base` to `theirs` applied
/// to `ours`, or None on a conflict.
pub fn merge_file(ours: &str, base: &str, theirs: &str) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("ambition-merge-{}-{}", std::process::id(), crate::util::nonce()));
    fs::create_dir_all(&dir).ok()?;
    let paths: Vec<PathBuf> = [("ours", ours), ("base", base), ("theirs", theirs)]
        .iter()
        .map(|(n, t)| {
            let p = dir.join(n);
            let _ = fs::write(&p, t);
            p
        })
        .collect();
    let out = Command::new("git").arg("merge-file").arg("-p").args(&paths).output();
    let _ = fs::remove_dir_all(&dir);
    let out = out.ok()?;
    if out.status.success() {
        String::from_utf8(out.stdout).ok()
    } else {
        None
    }
}

/// Turn each changed run of `after` back into `before`, wherever it sits in
/// `cur` now. For edits next to later changes, which merge-file calls a
/// conflict. None when a run can no longer be found.
pub fn unpatch(cur: &str, after: &str, before: &str) -> Option<String> {
    let a: Vec<&str> = after.split('\n').collect();
    let b: Vec<&str> = before.split('\n').collect();
    let mut c: Vec<&str> = cur.split('\n').collect();
    let runs: Vec<diff::Op> = diff::ops(&a, &b).into_iter().filter(|o| o.tag != diff::Tag::Equal).collect();
    for o in runs.iter().rev() {
        let chunk = &a[o.i1..o.i2];
        let put = &b[o.j1..o.j2];
        if !chunk.is_empty() {
            let hits: Vec<usize> =
                (0..=c.len().saturating_sub(chunk.len())).filter(|&k| c.len() >= chunk.len() && &c[k..k + chunk.len()] == chunk).collect();
            let k = *hits.iter().min_by_key(|&&k| (k as isize - o.i1 as isize).abs())?;
            c.splice(k..k + chunk.len(), put.iter().copied());
        } else if o.i1 == 0 {
            c.splice(0..0, put.iter().copied());
        } else {
            // The change deleted lines: put them back after the line before them.
            let prev = a[o.i1 - 1];
            let hits: Vec<usize> = (0..c.len()).filter(|&k| c[k] == prev).collect();
            let k = *hits.iter().min_by_key(|&&k| (k as isize - (o.i1 as isize - 1)).abs())?;
            c.splice(k + 1..k + 1, put.iter().copied());
        }
    }
    Some(c.join("\n"))
}

/// No NUL byte in the first 8 KB, and valid UTF-8 there.
pub fn is_text(p: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(p) else { return false };
    let mut buf = [0u8; 8192];
    let Ok(n) = f.read(&mut buf) else { return false };
    let head = &buf[..n];
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(), // cut mid-character at the 8 KB edge
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpatch_moves_with_the_text() {
        let before = "a\nb\nc\nd\ne";
        let after = "a\nB\nc\nd\ne";
        let cur = "x\na\nB\nc\nd\ne\ny";
        assert_eq!(unpatch(cur, after, before).unwrap(), "x\na\nb\nc\nd\ne\ny");
        let after_del = "a\nb\nd\ne";
        let cur_del = "a\nb\nd\ne\nz";
        assert_eq!(unpatch(cur_del, after_del, before).unwrap(), "a\nb\nc\nd\ne\nz");
        assert_eq!(unpatch("q\nr", "a\nB", "a\nb"), None);
    }
}
