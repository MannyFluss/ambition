//! `~/.ambition/config`, and which folder a file belongs to.
//!
//! The config is plain lines of `key = value`, with `#` comments. This
//! release reads one key, `allow`: a folder where presses run without asking
//! first. Write one line per folder; `ambition allow <folder>` adds one, and
//! so does the dialog's Always.
//!
//! ```text
//! allow = ~/notes
//! allow = ~/code/game
//! ```
//!
//! A file's folder (its "place") is the nearest granted folder that holds it;
//! else its git root (unless that is the whole home folder); else the folder
//! it is in. That folder is where its
//! thread runs, what its history covers, and what one grant allows. Files in
//! Ambition's own folder (`~/.ambition`) are always allowed.

use crate::util;
use std::path::{Path, PathBuf};

pub fn path() -> PathBuf {
    util::state_dir().join("config")
}

/// The values of one key, in order.
pub fn values(key: &str) -> Vec<String> {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

/// The folders with a standing grant, as they are on disk.
pub fn grants() -> Vec<PathBuf> {
    values("allow").iter().map(|v| real(&util::expand(v))).collect()
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The nearest folder at or above `dir` that holds a `.git`.
pub fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut d = Some(dir);
    while let Some(x) = d {
        if x.join(".git").exists() {
            return Some(x.to_path_buf());
        }
        d = x.parent();
    }
    None
}

/// Too wide to grant: the whole home folder, or a disk.
pub fn too_wide(folder: &Path) -> bool {
    folder == real(&util::home()) || folder.parent().is_none()
}

/// A file's place, and whether it is allowed already. `file` is canonical.
pub fn place(file: &Path) -> (PathBuf, bool) {
    let own = util::state_real();
    if file.starts_with(&own) {
        return (own, true);
    }
    if let Some(g) = grants().into_iter().filter(|g| file.starts_with(g)).max_by_key(|g| g.components().count()) {
        return (g, true);
    }
    let dir = file.parent().unwrap_or(file);
    // A git root as wide as the home folder (a dotfiles repo) is no place.
    let root = git_root(dir).filter(|r| !too_wide(r)).unwrap_or_else(|| dir.to_path_buf());
    (root, false)
}

/// Add a standing grant. The words to print, or why not.
pub fn allow(folder: &Path) -> Result<String, String> {
    let f = std::fs::canonicalize(folder).map_err(|_| format!("{} is not a folder.", folder.display()))?;
    if !f.is_dir() {
        return Err(format!("{} is not a folder.", folder.display()));
    }
    if too_wide(&f) {
        return Err("Ambition grants one folder, not your whole home or disk. Pick one inside it, like ~/notes.".into());
    }
    if grants().contains(&f) {
        return Ok(format!("{} was allowed already.", util::tilde(&f)));
    }
    let p = path();
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")));
    let mut text = std::fs::read_to_string(&p).unwrap_or_else(|_| HEADER.to_string());
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("allow = {}\n", util::tilde(&f)));
    std::fs::write(&p, text).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    Ok(format!("allowed {}: presses there run without asking.", util::tilde(&f)))
}

const HEADER: &str = "\
# Ambition's settings. One `key = value` per line.
#
# allow = <folder>   presses in this folder run without asking first.
#                    One line per folder; `ambition allow <folder>` adds one.

";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_root_is_found_above() {
        let d = std::env::temp_dir().join(format!("ambition-cfg-{}", util::nonce()));
        std::fs::create_dir_all(d.join("repo/.git")).unwrap();
        std::fs::create_dir_all(d.join("repo/a/b")).unwrap();
        let d = real(&d);
        assert_eq!(git_root(&d.join("repo/a/b")), Some(d.join("repo")));
        assert!(too_wide(Path::new("/")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
