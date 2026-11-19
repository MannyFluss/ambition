//! Text fields: any focused text field in any app, pressed with the global
//! key (macOS, `mac.rs`). This part knows nothing of macOS: what a field is
//! called, where its mirror lives, and how a change is cut into pieces that a
//! field can take.
//!
//! A field is mirrored as a file, `~/.ambition/fields/<app>/<name>.txt`, and
//! from there a press is the same as in any file: the same marks, hook,
//! thread, history and `^/undo`. The mirror takes the field's text at each
//! press, and Ambition writes the mirror's changes back into the field, but
//! only while the field still says what Ambition last read or wrote there.

use crate::util;
use std::path::{Path, PathBuf};

/// A field as a press found it.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// The app's name, as the Dock shows it.
    pub app: String,
    /// Its bundle id: what a grant names.
    pub bundle: String,
    /// The window's title.
    pub title: String,
    /// The page's address, when the field is in a web page.
    pub url: Option<String>,
    /// The field's own identifier in the app, when it has one.
    pub ident: Option<String>,
    /// The field's text, with line breaks as `\n`.
    pub text: String,
    /// The line the cursor was on, 0-based.
    pub line: usize,
}

/// A field Ambition can write to while it is open.
pub trait Surface: Send + Sync {
    /// The field's text now.
    fn read(&self) -> Result<String, String>;
    /// Make the field say `new`, if it still says `old` and is still where
    /// it was pressed (the same app, window and field, in front). With
    /// `mark`, the changed text is highlighted for a moment, then the cursor
    /// goes to its end. What the field says afterwards, or why nothing was
    /// written.
    fn write(&self, old: &str, new: &str, mark: bool) -> Result<String, String>;
    /// Highlight `span` again (UTF-16 offsets in `text`), if the field still
    /// says `text` and is where it was.
    fn highlight(&self, text: &str, span: (usize, usize)) -> Result<(), String>;
    /// Whether it can highlight at all. Where it cannot (Zed), a second press
    /// is a press like any other.
    fn can_highlight(&self) -> bool {
        true
    }
    /// The visible text of the field's window.
    fn window_text(&self) -> Result<String, String>;
    /// A screenshot of the field's window, saved at `out`.
    fn look(&self, out: &Path) -> Result<(), String>;
}

/// Line breaks other than `\n` become `\n`. Each is one UTF-16 unit, so
/// offsets stay the same.
pub fn normalize(text: &str) -> String {
    text.chars().map(|c| if matches!(c, '\r' | '\u{2028}' | '\u{2029}') { '\n' } else { c }).collect()
}

/// Whether a field's text is empty to the eye: only blanks and the
/// zero-width characters web editors keep in an empty box.
pub fn blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}'))
}

/// A window title without the counts apps put in it ("(3) Inbox", "All
/// iCloud – 116 notes"), so a new message does not make it another field.
pub fn clean_title(title: &str) -> String {
    let words: Vec<&str> = title.split_whitespace().collect();
    let number = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') && w.chars().any(|c| c.is_ascii_digit());
    let mut out = String::new();
    for (k, word) in words.iter().enumerate() {
        // "(3)" and "[12]" are counts; so is a number with a word after it
        // ("116 notes"). A number that ends the title is a name ("Untitled 2").
        let bracketed = word.len() > 2 && word.starts_with(['(', '[']) && word.ends_with([')', ']']) && number(&word[1..word.len() - 1]);
        let counted = number(word) && words.get(k + 1).is_some_and(|n| n.chars().next().is_some_and(char::is_alphabetic));
        if bracketed || counted {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// What makes a field the same field from one press to the next: its page
/// address in a browser, else the app, the window title without counts, and
/// the field's identifier.
pub fn identity(s: &Snapshot) -> String {
    match &s.url {
        Some(u) => format!("url {}", u.split('#').next().unwrap_or(u)),
        None => format!("app {} | {} | {}", s.bundle, clean_title(&s.title), s.ident.as_deref().unwrap_or("")),
    }
}

/// A short name made of letters, digits and dashes.
fn slug(s: &str, n: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() || c == '.' {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.chars().count() >= n {
            break;
        }
    }
    let t = out.trim_matches(|c| c == '-' || c == '.').to_string();
    if t.is_empty() {
        "field".into()
    } else {
        t
    }
}

/// Where a field's mirror lives: its app's folder, and its file name there.
/// The name comes from the field's identity alone, so it stays the same.
pub fn mirror(s: &Snapshot) -> (PathBuf, String) {
    let id = identity(s);
    let base = match &s.url {
        Some(u) => {
            let host = u.split("://").nth(1).unwrap_or(u).split('/').next().unwrap_or("");
            slug(host, 40)
        }
        None => slug(&clean_title(&s.title), 40),
    };
    let app = slug(if s.app.is_empty() { &s.bundle } else { &s.app }, 40);
    (fields_dir().join(app), format!("{base}-{}.txt", &util::sha(&id)[..8]))
}

pub fn fields_dir() -> PathBuf {
    util::state_dir().join("fields")
}

/// How the agent is told where a field is.
pub fn describe(s: &Snapshot) -> String {
    let mut out = format!("a text field in {}", if s.app.is_empty() { &s.bundle } else { &s.app });
    if !s.title.is_empty() {
        out.push_str(&format!(", window “{}”", s.title));
    }
    if let Some(u) = &s.url {
        out.push_str(&format!(", page {u}"));
    }
    out
}

pub fn u16len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The line holding UTF-16 offset `at`, 0-based.
pub fn line_at(text: &str, at: usize) -> usize {
    text.encode_utf16().take(at).filter(|&u| u == b'\n' as u16).count()
}

/// One piece of a change: replace `len` UTF-16 units at `at` (in the old
/// text) with `text`. Pieces begin at the start of a line, and end at the
/// start of a line or at the end, because some fields (Google Docs) place a
/// cursor exactly only there.
#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    pub at: usize,
    pub len: usize,
    pub text: String,
}

/// Lines with their line breaks, so the pieces join back exactly.
fn units(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The pieces that turn `old` into `new`, first to last. Apply them last to
/// first, so each one's offsets still hold.
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let (a, b) = (units(old), units(new));
    let mut starts = Vec::with_capacity(a.len() + 1);
    let mut at = 0;
    for u in &a {
        starts.push(at);
        at += u16len(u);
    }
    starts.push(at);
    crate::diff::ops(&a, &b)
        .into_iter()
        .filter(|o| o.tag != crate::diff::Tag::Equal)
        .map(|o| Hunk { at: starts[o.i1], len: starts[o.i2] - starts[o.i1], text: b[o.j1..o.j2].concat() })
        .collect()
}

/// What changed from `old` to `new`, as UTF-16 offsets `(start, end)` in
/// `new`: what a highlight covers. Empty when only text went.
pub fn changed(old: &str, new: &str) -> (usize, usize) {
    let (a, b): (Vec<u16>, Vec<u16>) = (old.encode_utf16().collect(), new.encode_utf16().collect());
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max = a.len().min(b.len()) - pre;
    let suf = a.iter().rev().zip(b.iter().rev()).take(max).take_while(|(x, y)| x == y).count();
    (pre, b.len() - suf)
}

/// Apply pieces to a text, last to first (what a field does with them).
pub fn apply(old: &str, hs: &[Hunk]) -> String {
    let mut t: Vec<u16> = old.encode_utf16().collect();
    for h in hs.iter().rev() {
        t.splice(h.at..h.at + h.len, h.text.encode_utf16());
    }
    String::from_utf16_lossy(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(url: Option<&str>, title: &str) -> Snapshot {
        Snapshot { app: "Discord".into(), bundle: "com.hnc.Discord".into(), title: title.into(), url: url.map(String::from), ..Default::default() }
    }

    #[test]
    fn counts_leave_the_title() {
        assert_eq!(clean_title("(3) Discord | #general"), "Discord | #general");
        assert_eq!(clean_title("All iCloud – 116 notes"), "All iCloud – notes");
        assert_eq!(clean_title("Inbox (8,957) - Gmail"), "Inbox - Gmail");
        assert_eq!(clean_title("Chapter two"), "Chapter two");
        assert_eq!(clean_title("Untitled 2"), "Untitled 2");
    }

    #[test]
    fn a_field_is_its_page_or_its_window() {
        let a = snap(Some("https://docs.google.com/document/d/abc/edit?tab=t.0#heading=h.1"), "Untitled document - Google Docs");
        let b = snap(Some("https://docs.google.com/document/d/abc/edit?tab=t.0"), "Renamed - Google Docs");
        assert_eq!(identity(&a), identity(&b), "a page is the same field whatever its title says");
        assert_eq!(mirror(&a), mirror(&b));
        let c = snap(None, "(2) Notes");
        let d = snap(None, "(5) Notes");
        assert_eq!(identity(&c), identity(&d), "a count in the title is not another field");
        let e = Snapshot { ident: Some("body".into()), ..snap(None, "Notes") };
        assert_ne!(identity(&c), identity(&e));
        let (dir, name) = mirror(&a);
        assert!(dir.ends_with("fields/discord"), "{dir:?}");
        assert!(name.starts_with("docs.google.com-") && name.ends_with(".txt"), "{name}");
        let (_, n2) = mirror(&c);
        assert!(n2.starts_with("notes-"), "{n2}");
    }

    #[test]
    fn pieces_start_and_end_at_lines() {
        let old = "one\ntwo\nthree";
        for new in ["one\n^ ◌ p1 · working\ntwo\nthree", "one\nTWO\nthree", "one\nthree", "zero\none\ntwo\nthree", "one\ntwo\nthree\nfour", "one\ntwo\nthree!", ""] {
            let hs = hunks(old, new);
            assert_eq!(apply(old, &hs), new, "{hs:?}");
            let starts: Vec<usize> = std::iter::once(0).chain(old.match_indices('\n').map(|(i, _)| i + 1)).collect();
            for h in &hs {
                assert!(starts.contains(&h.at), "{h:?} starts mid-line");
                assert!(starts.contains(&(h.at + h.len)) || h.at + h.len == u16len(old), "{h:?} ends mid-line");
            }
        }
        assert_eq!(hunks("a\nb\n", "a\nX\nb\n"), vec![Hunk { at: 2, len: 0, text: "X\n".into() }]);
    }

    #[test]
    fn what_changed_and_where_the_cursor_is() {
        assert_eq!(changed("one\ntwo", "one\nTWO"), (4, 7));
        assert_eq!(changed("a\n^ ◌ p1 · w\nb", "a\nfine\nb"), (2, 6));
        assert_eq!(changed("abc", "abc"), (3, 3));
        assert_eq!(changed("aXa", "aa"), (1, 1));
        assert_eq!(line_at("a\nb\nc", 2), 1);
        assert_eq!(line_at("a\nb\nc", 1), 0);
        assert_eq!(normalize("a\rb\u{2028}c"), "a\nb\nc");
        assert!(blank("\u{feff}\n") && blank("\u{200b}\u{200b}") && !blank(" x "));
    }
}
