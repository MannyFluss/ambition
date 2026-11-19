//! The hook: what one press sends the agent. Where you are, the `^` line, a
//! flat 8 lines each way, and what changed in the document since its last
//! turn. Nothing else is attached. The agent reads the rest itself, with
//! `ambition more up|down [n]`, which picks up where the last view ended.

use crate::carets as C;
use crate::diff;
use crate::util;

/// Lines shown each way around the caret.
pub const EACH_WAY: usize = 8;
/// `ambition more` without a number.
pub const MORE: usize = 40;
const LINE_CUT: usize = 600;
const DIFF_CHARS: usize = 3000;

/// 1-based inclusive lines `lo..=hi`: `each` lines each way around 0-based
/// line `i`, inside a file of `n` lines.
pub fn window(n: usize, i: usize, each: usize) -> (usize, usize) {
    ((i + 1).saturating_sub(each).max(1), (i + 1 + each).min(n.max(1)))
}

/// The file's lines, without the empty one after a final newline.
pub fn lines(text: &str) -> Vec<&str> {
    let mut l: Vec<&str> = text.split('\n').collect();
    if l.last() == Some(&"") {
        l.pop();
    }
    l
}

/// Lines `lo..=hi` (1-based), numbered as `  7| text`.
pub fn numbered(all: &[&str], lo: usize, hi: usize) -> Vec<String> {
    let width = all.len().to_string().len();
    (lo..=hi.min(all.len()))
        .map(|k| {
            let l = all[k - 1];
            let l = if l.chars().count() > LINE_CUT {
                let cut: String = l.chars().take(LINE_CUT).collect();
                format!("{cut} … (cut here: {} more characters on this line)", l.chars().count() - LINE_CUT)
            } else {
                l.to_string()
            };
            format!("{k:>width$}| {l}")
        })
        .collect()
}

/// "lines 4-20 of 40", or "all 12 lines".
pub fn span_of(lo: usize, hi: usize, n: usize) -> String {
    if lo <= 1 && hi >= n {
        format!("all {n} line{}", if n == 1 { "" } else { "s" })
    } else {
        format!("lines {lo}-{hi} of {n}")
    }
}

/// What changed in one file between two versions, as a short diff for the
/// agent: replies, status lines and blank-only changes left out. None when
/// nothing did.
pub fn changes(rel: &str, old: &str, new: &str) -> Option<String> {
    let (was, now) = (C::clear_marks(rel, old), C::clear_marks(rel, new));
    let a: Vec<&str> = was.split('\n').collect();
    let b: Vec<&str> = now.split('\n').collect();
    let d = diff::unified(&a, &b, 1);
    if !d.iter().any(|l| (l.starts_with('+') || l.starts_with('-')) && !l[1..].trim().is_empty()) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for (k, l) in d.iter().enumerate() {
        if l.starts_with("@@") {
            if k > 0 {
                out.push("  …".into());
            }
        } else {
            out.push(format!("  {l}"));
        }
    }
    let text = out.join("\n");
    if text.len() <= DIFF_CHARS {
        return Some(text);
    }
    let mut i = DIFF_CHARS;
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    let cut = text[..i].rsplit_once('\n').map(|x| x.0).unwrap_or("");
    Some(format!("{cut}\n  … cut here: the folder's history has all of it"))
}

/// What changed since the document's last turn.
pub enum Since {
    /// Its first turn ever.
    First,
    Nothing,
    Diff(String),
}

/// The message for one press. `text` is the document as it is on disk, the
/// status line on 0-based line `i`; `cli` is how the agent runs Ambition;
/// `words` is empty for a press on a line with no mark (a request), whose
/// line is just above the status line. `note` says where the document is
/// when it is a text field or an open buffer, and which copy to edit.
#[allow(clippy::too_many_arguments)]
pub fn press(path: &str, id: &str, words: &str, text: &str, i: usize, since: &Since, cli: &str, note: Option<&str>)
    -> (String, (usize, usize)) {
    let all = lines(text);
    let (lo, hi) = window(all.len(), i, EACH_WAY);
    let reply = &C::marks().reply;
    let mut parts = if words.is_empty() {
        vec![format!("[Ambition] A press with no mark in {path}, on line {i}: a request. Its status line, {id}, is line {} \
            now. Read around line {i} and do what the text there seems to need, following the rules in this thread's first \
            message: the smallest change that does the job, in this document only, or one `{reply}` question when you \
            are not sure what is wanted.", i + 1)]
    } else {
        let quoted: Vec<String> = words.split('\n').map(|l| format!("  {l}")).collect();
        vec![
            format!("[Ambition] A press in {path}, line {}: prompt {id}, now its status line there. Follow the rules in \
                     this thread's first message: your answer goes in the document.", i + 1),
            quoted.join("\n"),
        ]
    };
    if let Some(n) = note {
        parts.push(n.to_string());
    }
    parts.push(format!("{path}, {}, as it is on disk now:\n{}", span_of(lo, hi, all.len()), numbered(&all, lo, hi).join("\n")));
    match since {
        Since::First => {}
        Since::Nothing => parts.push("Nothing else in this document changed since your last turn.".into()),
        Since::Diff(d) => parts.push(format!("What changed in this document since your last turn (+ is new, - is gone):\n{d}")),
    }
    let (up, down) = (lo > 1, hi < all.len());
    if up || down {
        let mut ways = Vec::new();
        if up {
            ways.push(format!("`{cli} more up [n]`"));
        }
        if down {
            ways.push(format!("`{cli} more down [n]`"));
        }
        parts.push(format!("For more of the document: {}.", ways.join(" or ")));
    }
    (parts.join("\n\n"), (lo, hi))
}

/// `ambition more`: `n` more lines above or below the window shown so far,
/// numbered, and the window after it.
pub fn more(path: &str, text: &str, seen: (usize, usize), up: bool, n: usize) -> (String, (usize, usize)) {
    let all = lines(text);
    let len = all.len();
    let (lo, hi) = (seen.0.min(len.max(1)), seen.1.min(len));
    if up {
        if lo <= 1 {
            return (format!("{path}: line 1 is the top; you have seen everything above line {hi}."), (1, hi));
        }
        let from = lo.saturating_sub(n).max(1);
        let rows = numbered(&all, from, lo - 1);
        (format!("{path}, lines {from}-{} of {len}:\n{}", lo - 1, rows.join("\n")), (from, hi))
    } else {
        if hi >= len {
            return (format!("{path}: the document ends at line {len}; you have seen everything below line {lo}."), (lo, len));
        }
        let to = (hi + n).min(len);
        let rows = numbered(&all, hi + 1, to);
        (format!("{path}, lines {}-{to} of {len}:\n{}", hi + 1, rows.join("\n")), (lo, to))
    }
}

/// `/Users/you/notes/a.md` as the agent is told it.
pub fn shown(p: &std::path::Path) -> String {
    util::tilde(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_lines_each_way() {
        assert_eq!(window(40, 11, 8), (4, 20));
        assert_eq!(window(5, 0, 8), (1, 5));
        let text: String = (1..=30).map(|k| format!("line {k}\n")).collect();
        let (msg, w) = press("~/a.md", "p1", "make it so", &text, 14, &Since::First, "ambition", None);
        assert_eq!(w, (7, 23));
        assert!(msg.contains("lines 7-23 of 30") && msg.contains(" 7| line 7") && msg.contains("23| line 23") && !msg.contains("| line 24"));
        assert!(msg.contains("`ambition more up [n]` or `ambition more down [n]`"));
        let (more_up, w2) = more("~/a.md", &text, w, true, 4);
        assert!(more_up.contains("lines 3-6 of 30") && more_up.contains(" 3| line 3") && !more_up.contains("| line 7"));
        assert_eq!(w2, (3, 23));
        let (_, w3) = more("~/a.md", &text, w2, false, 100);
        assert_eq!(w3, (3, 30));
        assert!(more("~/a.md", &text, w3, false, 5).0.contains("ends at line 30"));
    }

    #[test]
    fn a_request_and_a_field() {
        let text = "Dear Sam,\nthanks for the hlep yesterday\n^ ◌ p2 · working\n";
        let (msg, _) = press("~/.ambition/fields/mail/x.txt", "p2", "", text, 2, &Since::First, "ambition", Some("This document is a text field in Mail."));
        assert!(msg.starts_with("[Ambition] A press with no mark in ~/.ambition/fields/mail/x.txt, on line 2: a request."), "{msg}");
        assert!(msg.contains("one `^>` question") && msg.contains("This document is a text field in Mail."), "{msg}");
        assert!(msg.contains("2| thanks for the hlep yesterday"), "{msg}");
    }

    #[test]
    fn changes_leave_out_marks() {
        assert!(changes("a.md", "a\nb\n", "a\n^> r\nb\n").is_none());
        let d = changes("a.md", "a\nb\n", "a\nc\n").unwrap();
        assert!(d.contains("-b") && d.contains("+c"), "{d}");
    }
}
