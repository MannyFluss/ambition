//! `~/.ambition/config`, and which folder a file belongs to.
//!
//! The config is plain lines of `key = value`, with `#` comments:
//!
//! ```text
//! allow = ~/notes              presses in this folder run without asking
//! allow = app:com.hnc.Discord  presses in this app's text fields too
//! hotkey = ctrl+option+return  the key that presses, outside your editors
//! editor = dev.zed.Zed         an app whose own binding presses instead
//! ask = ^                      the marks: a prompt, a reply, a command
//! reply = ^>
//! command = ^/
//! ```
//!
//! `allow` and `editor` take one value per line. `ambition allow` and the
//! dialog's Always add `allow` lines. With no `editor` line, the editor list
//! is Zed alone; with any, it is exactly those. `hotkey = none` turns the
//! global key off.
//!
//! A file's folder (its "place") is the nearest granted folder that holds it;
//! else its git root (unless that is the whole home folder); else the folder
//! it is in. That folder is where its
//! thread runs, what its history covers, and what one grant allows. Files in
//! Ambition's own folder (`~/.ambition`) are always allowed; a field's
//! mirror there belongs to its app's folder, `~/.ambition/fields/<app>`.

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
    values("allow").iter().filter(|v| !v.starts_with(APP)).map(|v| real(&util::expand(v))).collect()
}

/// An app's grant is `allow = app:<bundle id>`.
pub const APP: &str = "app:";

/// The apps with a standing grant, by bundle id.
pub fn app_grants() -> Vec<String> {
    values("allow").iter().filter_map(|v| v.strip_prefix(APP)).map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect()
}

pub fn app_allowed(bundle: &str) -> bool {
    app_grants().iter().any(|b| b == bundle)
}

/// Add a standing grant for an app's text fields.
pub fn allow_app(bundle: &str) -> Result<String, String> {
    let b = bundle.trim();
    if b.is_empty() || b.contains(char::is_whitespace) {
        return Err(format!("“{bundle}” is not an app's bundle id, like com.apple.Notes."));
    }
    if app_allowed(b) {
        return Ok(format!("{APP}{b} was allowed already."));
    }
    append(&format!("allow = {APP}{b}"))?;
    Ok(format!("allowed {APP}{b}: presses in its text fields run without asking."))
}

/// The apps whose own key binding presses (`ambition fire`), so the global
/// key steps aside while one is in front. Bundle ids.
pub fn editors() -> Vec<String> {
    let v = values("editor");
    if v.is_empty() {
        vec!["dev.zed.Zed".to_string()]
    } else {
        v
    }
}

/// The marks from the config, each the last line for its key, else the
/// default. Marks that are blank, hold a space, or clash fall back to all
/// three defaults.
pub fn marks() -> crate::carets::Marks {
    let d = crate::carets::Marks::default();
    let pick = |k: &str, dflt: &str| values(k).pop().unwrap_or_else(|| dflt.to_string());
    let m = crate::carets::Marks { ask: pick("ask", &d.ask), reply: pick("reply", &d.reply), command: pick("command", &d.command) };
    let ok = [&m.ask, &m.reply, &m.command].iter().all(|x| !x.is_empty() && !x.contains(char::is_whitespace))
        && m.ask != m.reply && m.ask != m.command && m.reply != m.command;
    if ok {
        m
    } else {
        d
    }
}

/// The global key: its words from the config, or the default.
pub fn hotkey_words() -> String {
    values("hotkey").pop().unwrap_or_else(|| DEFAULT_HOTKEY.to_string())
}

pub const DEFAULT_HOTKEY: &str = "ctrl+option+return";

/// A key and its modifiers, as macOS numbers them: a virtual key code, and
/// Carbon's modifier bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub key: u32,
    pub mods: u32,
}

pub const CMD: u32 = 1 << 8;
pub const SHIFT: u32 = 1 << 9;
pub const OPTION: u32 = 1 << 11;
pub const CONTROL: u32 = 1 << 12;

/// `ctrl+option+return`, or `⌃⌥↩`. Ok(None) for `none`. A chord needs at
/// least one of control, option or command, so it never eats plain typing.
pub fn chord(words: &str) -> Result<Option<Chord>, String> {
    let w = words.trim().to_lowercase();
    if matches!(w.as_str(), "none" | "off" | "") {
        return Ok(None);
    }
    let mut mods = 0;
    let mut key = None;
    let mut parts: Vec<String> = Vec::new();
    for tok in w.split(|c: char| c == '+' || c == '-' || c.is_whitespace()).filter(|t| !t.is_empty()) {
        // Glyphs may come run together: ⌃⌥↩.
        let mut rest = tok.to_string();
        while let Some(c) = rest.chars().next().filter(|c| "⌃⌥⌘⇧↩".contains(*c)) {
            parts.push(c.to_string());
            rest = rest[c.len_utf8()..].to_string();
        }
        if !rest.is_empty() {
            parts.push(rest);
        }
    }
    for p in &parts {
        match p.as_str() {
            "ctrl" | "control" | "⌃" => mods |= CONTROL,
            "alt" | "opt" | "option" | "⌥" => mods |= OPTION,
            "cmd" | "command" | "⌘" => mods |= CMD,
            "shift" | "⇧" => mods |= SHIFT,
            k => {
                if key.is_some() {
                    return Err(format!("hotkey: “{words}” names two keys."));
                }
                key = Some(keycode(k).ok_or(format!("hotkey: Ambition does not know the key “{k}”."))?);
            }
        }
    }
    let key = key.ok_or(format!("hotkey: “{words}” names no key, only modifiers."))?;
    if mods & (CONTROL | OPTION | CMD) == 0 {
        return Err(format!("hotkey: “{words}” needs control, option or command, so it never takes plain typing."));
    }
    Ok(Some(Chord { key, mods }))
}

/// macOS virtual key codes (ANSI layout) for the keys a chord may use.
fn keycode(k: &str) -> Option<u32> {
    const LETTERS: [u32; 26] = [0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6];
    const DIGITS: [u32; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    const F: [u32; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];
    let mut cs = k.chars();
    if let (Some(c), None) = (cs.next(), cs.next()) {
        if c.is_ascii_lowercase() {
            return Some(LETTERS[(c as u8 - b'a') as usize]);
        }
        if c.is_ascii_digit() {
            return Some(DIGITS[(c as u8 - b'0') as usize]);
        }
    }
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=12).contains(n)) {
        return Some(F[n - 1]);
    }
    Some(match k {
        "return" | "enter" | "↩" => 36,
        "space" => 49,
        "tab" => 48,
        "escape" | "esc" => 53,
        "." | "period" => 47,
        "," | "comma" => 43,
        "/" | "slash" => 44,
        ";" | "semicolon" => 41,
        "'" | "quote" => 39,
        "[" => 33,
        "]" => 30,
        "\\" | "backslash" => 42,
        "`" | "grave" => 50,
        "=" | "equal" => 24,
        _ => return None,
    })
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
    // A field's mirror belongs to its app's folder.
    if let Ok(rest) = file.strip_prefix(own.join("fields")) {
        if let Some(app) = rest.components().next().filter(|_| rest.components().count() > 1) {
            return (own.join("fields").join(app), true);
        }
    }
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
    append(&format!("allow = {}", util::tilde(&f)))?;
    Ok(format!("allowed {}: presses there run without asking.", util::tilde(&f)))
}

/// Add one line to the config, made with its header if it is not there yet.
fn append(line: &str) -> Result<(), String> {
    let p = path();
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")));
    let mut text = std::fs::read_to_string(&p).unwrap_or_else(|_| HEADER.to_string());
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    std::fs::write(&p, text).map_err(|e| format!("cannot write {}: {e}", p.display()))
}

const HEADER: &str = "\
# Ambition's settings. One `key = value` per line.
#
# allow = <folder>        presses in this folder run without asking first.
# allow = app:<bundle id> presses in this app's text fields, too (macOS).
#                         One line each; `ambition allow` and Always add them.
# hotkey = ctrl+option+return   the key that presses in any text field (macOS).
#                               `none` turns it off.
# editor = dev.zed.Zed    an app whose own binding presses instead, so the
#                         hotkey steps aside while it is in front. One line
#                         each; with none, the list is Zed alone.
# ask = ^                 the marks: a prompt, a reply, a command. These are
# reply = ^>              the defaults.
# command = ^/

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

    #[test]
    fn chords() {
        let ctrl_opt_return = Some(Chord { key: 36, mods: CONTROL | OPTION });
        assert_eq!(chord("ctrl+option+return"), Ok(ctrl_opt_return));
        assert_eq!(chord("⌃⌥↩"), Ok(ctrl_opt_return));
        assert_eq!(chord(" Control-Alt-Enter "), Ok(ctrl_opt_return));
        assert_eq!(chord(DEFAULT_HOTKEY), Ok(ctrl_opt_return));
        assert_eq!(chord("cmd+shift+k"), Ok(Some(Chord { key: 40, mods: CMD | SHIFT })));
        assert_eq!(chord("ctrl+f5"), Ok(Some(Chord { key: 96, mods: CONTROL })));
        assert_eq!(chord("none"), Ok(None));
        assert!(chord("shift+return").is_err(), "plain typing with shift is never taken");
        assert!(chord("ctrl+option").is_err());
        assert!(chord("ctrl+a+b").is_err());
        assert!(chord("ctrl+nope").is_err());
    }
}
