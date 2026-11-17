//! Caret lines in a file: finding them, taking them out, and writing status
//! and reply lines.
//!
//! ```text
//! ^ tighten this          a prompt. Lines in a row that start with ^ are one
//!                         prompt.
//! ^/undo                  a command: answered at once, in code, never sent
//!                         to the agent. One line each.
//! ^ ◌ p3 · …              a status line Ambition writes while p3 runs.
//!                         Never a prompt, so a prompt cannot fire twice.
//! ^> the answer is 4      a reply. The next save clears it; delete the ^>
//!                         to keep the text.
//! ```
//!
//! In notes (`.md`, `.txt`, or no extension) a caret counts only at the very
//! start of a line, so indented examples never fire. In code files it may
//! follow spaces and a comment marker: `# ^ …`, `// ^ …`, `-- ^ …`, `; ^ …`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const WORKING: char = '◌';

/// A note is prose: carets only at column 0, and a caret under a list means
/// its last item.
pub fn is_note(rel: &str) -> bool {
    match Path::new(rel).extension().and_then(|e| e.to_str()) {
        None => true,
        Some(e) => matches!(e.to_ascii_lowercase().as_str(), "md" | "txt" | "markdown"),
    }
}

/// The byte index of a caret line's `^`, which is also the length of what
/// comes before it (its lead): nothing in a note; spaces and one comment
/// marker in code.
pub fn caret_at(rel: &str, line: &str) -> Option<usize> {
    if line.starts_with('^') {
        return Some(0);
    }
    if is_note(rel) {
        return None;
    }
    let b = line.as_bytes();
    let blanks = |mut i: usize| {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        i
    };
    let mut i = blanks(0);
    if b.get(i) == Some(&b'^') {
        return Some(i);
    }
    let marker = ["//", "--", "#", ";"].iter().find(|m| line[i..].starts_with(**m))?;
    i = blanks(i + marker.len());
    (b.get(i) == Some(&b'^')).then_some(i)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Prompt(String),
    Command { name: String, args: String },
    Reply,
    Status(String),
    /// A caret line that is none of these, such as `^/ x` or `^ ◌ x`.
    Other,
}

/// What kind of caret line this is, with its lead, or None for a plain line.
pub fn kind(rel: &str, line: &str) -> Option<(String, Kind)> {
    let at = caret_at(rel, line)?;
    let lead = line[..at].to_string();
    let after = &line[at + 1..];
    if after.starts_with('>') {
        return Some((lead, Kind::Reply));
    }
    if let Some(rest) = after.strip_prefix('/') {
        return Some((lead, command(rest).unwrap_or(Kind::Other)));
    }
    let working = format!(" {WORKING} ");
    if let Some(rest) = after.strip_prefix(working.as_str()) {
        if let Some(id) = status_id(rest) {
            return Some((lead, Kind::Status(id)));
        }
    }
    if after.trim_start().starts_with(WORKING) {
        return Some((lead, Kind::Other));
    }
    let text = after.trim();
    Some((lead, if text.is_empty() { Kind::Other } else { Kind::Prompt(text.to_string()) }))
}

fn name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// `name args` after `^/`: a letter, then letters, digits, `_` or `-`, then
/// the end of the line or a space or tab.
fn command(rest: &str) -> Option<Kind> {
    let first = rest.chars().next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let end = rest.find(|c: char| !name_char(c)).unwrap_or(rest.len());
    let tail = &rest[end..];
    if !(tail.is_empty() || tail.starts_with(' ') || tail.starts_with('\t')) {
        return None;
    }
    Some(Kind::Command { name: rest[..end].to_lowercase(), args: tail.trim().to_string() })
}

/// `p3` or `p3.2` at the start of rest, not followed by `.` or a digit.
fn status_id(rest: &str) -> Option<String> {
    let b = rest.as_bytes();
    if b.first() != Some(&b'p') {
        return None;
    }
    let digits = |mut i: usize| {
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        (i > s).then_some(i)
    };
    let mut i = digits(1)?;
    if b.get(i) == Some(&b'.') {
        if let Some(j) = digits(i + 1) {
            i = j;
        }
    }
    if matches!(b.get(i), Some(b'.') | Some(b'0'..=b'9')) {
        return None;
    }
    Some(rest[..i].to_string())
}

/// One prompt: 0-based lines `start..end`, its lead, and its words, one line
/// of words per caret line.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub start: usize,
    pub end: usize,
    pub lead: String,
    pub text: String,
}

pub fn prompts(rel: &str, text: &str) -> Vec<Prompt> {
    let mut out: Vec<Prompt> = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if let Some((lead, Kind::Prompt(words))) = kind(rel, line) {
            match out.last_mut() {
                Some(p) if p.end == i => {
                    p.end = i + 1;
                    p.text.push('\n');
                    p.text.push_str(&words);
                }
                _ => out.push(Prompt { start: i, end: i + 1, lead, text: words }),
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub line: usize,
    pub lead: String,
    pub name: String,
    pub args: String,
}

pub fn commands(rel: &str, text: &str) -> Vec<Command> {
    text.split('\n')
        .enumerate()
        .filter_map(|(i, line)| match kind(rel, line) {
            Some((lead, Kind::Command { name, args })) => Some(Command { line: i, lead, name, args }),
            _ => None,
        })
        .collect()
}

pub fn is_reply(rel: &str, line: &str) -> bool {
    matches!(kind(rel, line), Some((_, Kind::Reply)))
}

pub fn replies(rel: &str, text: &str) -> Vec<String> {
    text.split('\n').filter(|l| is_reply(rel, l)).map(str::to_string).collect()
}

/// {id: line index} of the status lines in a file.
pub fn statuses(rel: &str, text: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (i, line) in text.split('\n').enumerate() {
        if let Some((_, Kind::Status(id))) = kind(rel, line) {
            out.insert(id, i);
        }
    }
    out
}

/// The lines without the indexes in `gone`. Where taking out a run of lines
/// leaves a blank line just above it and just below it, the one below goes
/// too, so caret lines that come and go leave no growing gap.
pub fn drop_lines<'a>(lines: &[&'a str], gone: &BTreeSet<usize>) -> Vec<&'a str> {
    let (mut out, mut i, n) = (Vec::new(), 0, lines.len());
    while i < n {
        if !gone.contains(&i) {
            out.push(lines[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && gone.contains(&j) {
            j += 1;
        }
        if out.last().is_some_and(|l: &&str| l.trim().is_empty()) && j < n && lines[j].trim().is_empty() {
            j += 1;
        }
        i = j;
    }
    out
}

/// The text with every `^>` line taken out, and the lines taken. `tidy` also
/// closes the gap a reply leaves between two blank lines (see `drop_lines`);
/// undo compares texts without it.
pub fn clear_replies(rel: &str, text: &str, tidy: bool) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.split('\n').collect();
    let gone: BTreeSet<usize> = (0..lines.len()).filter(|&i| is_reply(rel, lines[i])).collect();
    let keep: Vec<&str> = if tidy {
        drop_lines(&lines, &gone)
    } else {
        lines.iter().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, l)| *l).collect()
    };
    (keep.join("\n"), gone.iter().map(|&i| lines[i].to_string()).collect())
}

/// Take out a prompt's status line and any markers it has (`p3.2` …), without
/// leaving a doubled blank line where it stood.
pub fn remove_marks(rel: &str, text: &str, pid: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let dotted = format!("{pid}.");
    let gone: BTreeSet<usize> = (0..lines.len())
        .filter(|&i| matches!(kind(rel, lines[i]), Some((_, Kind::Status(id))) if id == pid || id.starts_with(&dotted)))
        .collect();
    drop_lines(&lines, &gone).join("\n")
}

/// Paragraphs: 0-based `(start, end)` line ranges, end exclusive.
pub fn blocks(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    let lines: Vec<&str> = text.split('\n').collect();
    for i in 0..=lines.len() {
        let blank = lines.get(i).is_none_or(|l| l.trim().is_empty());
        match (blank, start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// The text with these `(start, end)` line ranges cut out. Returns the text and,
/// for each range in the order given, the line index where it stood in the
/// new text.
pub fn remove(text: &str, ranges: &[(usize, usize)]) -> (String, Vec<usize>) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&k| ranges[k].0);
    let mut out: Vec<&str> = Vec::new();
    let mut pos = vec![0; ranges.len()];
    let (mut i, mut next) = (0, 0);
    while i < lines.len() {
        if next < order.len() && i == ranges[order[next]].0 {
            pos[order[next]] = out.len();
            i = ranges[order[next]].1.max(i + 1);
            next += 1;
            continue;
        }
        out.push(lines[i]);
        i += 1;
    }
    while next < order.len() {
        pos[order[next]] = out.len();
        next += 1;
    }
    (out.join("\n"), pos)
}

/// Insert lines at a line index, clamped to the file, and before its final
/// newline.
pub fn insert(text: &str, at: usize, new: &[String]) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut at = at.min(lines.len());
    if at == lines.len() && lines.last() == Some(&"") {
        at -= 1;
    }
    let mut out: Vec<&str> = lines[..at].to_vec();
    out.extend(new.iter().map(String::as_str));
    out.extend_from_slice(&lines[at..]);
    out.join("\n")
}

pub fn status_line(lead: &str, pid: &str, msg: &str) -> String {
    format!("{lead}^ {WORKING} {pid} · {msg}")
}

/// Swap a status line for new lines (none removes it).
pub fn replace_status(rel: &str, text: &str, pid: &str, new: &[String]) -> String {
    let Some(&i) = statuses(rel, text).get(pid) else { return text.to_string() };
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<&str> = lines[..i].to_vec();
    out.extend(new.iter().map(String::as_str));
    out.extend_from_slice(&lines[i + 1..]);
    out.join("\n")
}

/// Text as `^>` lines, at most `limit` of them.
pub fn reply_lines(lead: &str, body: &str, limit: usize) -> Vec<String> {
    let rows: Vec<&str> = body.trim_end_matches('\n').split('\n').collect();
    let mut out: Vec<String> =
        rows.iter().take(limit).map(|r| format!("{lead}^> {r}").trim_end().to_string()).collect();
    if rows.len() > limit {
        out.push(format!("{lead}^> … {} more lines, all in .ambition/history.md", rows.len() - limit));
    }
    out
}

/// Whether a line opens a list item: `- x`, `* x`, `+ x`, `1. x`, `1) x`.
pub fn is_item(line: &str) -> bool {
    let s = line.trim_start();
    let rest = if let Some(r) = s.strip_prefix(['-', '*', '+']) {
        r
    } else {
        let d = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        if d == 0 {
            return false;
        }
        match s[d..].strip_prefix(['.', ')']) {
            Some(r) => r,
            None => return false,
        }
    };
    let trimmed = rest.trim_start();
    rest.len() > trimmed.len() && !trimmed.is_empty()
}

/// What a caret just under the block `lines[first..last]` points at: 0-based
/// `(start, end)`. In a note, under a list, that is the last list item, not
/// the whole list; anywhere else, the whole block. `skip`: line indexes to
/// pass over (Ambition's own status lines).
pub fn unit(rel: &str, lines: &[&str], first: usize, last: usize, skip: &BTreeSet<usize>) -> (usize, usize) {
    if is_note(rel) {
        for i in (first + 1..last).rev() {
            if !skip.contains(&i) && is_item(lines[i]) {
                return (i, last);
            }
        }
    }
    (first, last)
}

/// The paragraph (or last list item) just above line index `at`, skipping
/// blank lines and the lines in `skip`. 0-based `(start, end)`, or None when
/// nothing is above it.
pub fn above(rel: &str, lines: &[&str], at: usize, skip: &BTreeSet<usize>) -> Option<(usize, usize)> {
    let mut i = at.checked_sub(1)?;
    loop {
        if lines[i].trim().is_empty() || skip.contains(&i) {
            i = i.checked_sub(1)?;
        } else {
            break;
        }
    }
    let mut j = i;
    while j > 0 && !lines[j - 1].trim().is_empty() && !skip.contains(&(j - 1)) {
        j -= 1;
    }
    Some(unit(rel, lines, j, i + 1, skip))
}

/// The paragraph just below line index `at`, skipping blank lines and the
/// lines in `skip`. 0-based `(start, end)`, or None at the end of the file.
pub fn below(lines: &[&str], at: usize, skip: &BTreeSet<usize>) -> Option<(usize, usize)> {
    let mut s = at + 1;
    while s < lines.len() && (skip.contains(&s) || lines[s].trim().is_empty()) {
        s += 1;
    }
    let mut e = s;
    while e < lines.len() && !lines[e].trim().is_empty() && !skip.contains(&e) {
        e += 1;
    }
    (e > s).then_some((s, e))
}

/// "line 4" or "lines 4-9", from 0-based `(start, end)`.
pub fn span(s: usize, e: usize) -> String {
    if e <= s + 1 {
        format!("line {}", s + 1)
    } else {
        format!("lines {}-{}", s + 1, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_in_a_note() {
        assert_eq!(kind("a.md", "^ make this short"), Some((String::new(), Kind::Prompt("make this short".into()))));
        assert_eq!(kind("a.md", "^/model opus high").unwrap().1,
                   Kind::Command { name: "model".into(), args: "opus high".into() });
        assert_eq!(kind("a.md", "^> four").unwrap().1, Kind::Reply);
        assert_eq!(kind("a.md", "^ ◌ p3 · working").unwrap().1, Kind::Status("p3".into()));
        assert_eq!(kind("a.md", "^ ◌ p3.2 · marker").unwrap().1, Kind::Status("p3.2".into()));
        assert_eq!(kind("a.md", "^ ◌ p3x").unwrap().1, Kind::Status("p3".into()));
        assert_eq!(kind("a.md", "^ ◌ p3. no").unwrap().1, Kind::Other);
        assert_eq!(kind("a.md", "^ ◌ loose").unwrap().1, Kind::Other);
        assert_eq!(kind("a.md", "^/ nothing").unwrap().1, Kind::Other);
        assert_eq!(kind("a.md", "^/help.").unwrap().1, Kind::Other);
        assert_eq!(kind("a.md", "^   ").unwrap().1, Kind::Other);
        assert_eq!(kind("a.md", "  ^ indented never fires"), None);
        assert_eq!(kind("a.md", "# ^ a comment in a note is text"), None);
        assert_eq!(kind("a.md", "plain"), None);
    }

    #[test]
    fn kinds_in_code() {
        assert_eq!(kind("a.py", "    # ^ fix this").unwrap(), ("    # ".into(), Kind::Prompt("fix this".into())));
        assert_eq!(kind("a.js", "// ^> done").unwrap().1, Kind::Reply);
        assert_eq!(kind("a.lua", "-- ^/count").unwrap().1, Kind::Command { name: "count".into(), args: "".into() });
        assert_eq!(kind("a.lisp", ";^ go").unwrap(), (";".into(), Kind::Prompt("go".into())));
        assert_eq!(kind("a.py", "  ^ bare caret in code").unwrap().0, "  ");
        assert_eq!(kind("a.py", "x = 1  # ^ not at the start"), None);
        assert_eq!(kind("a.c", "- ^ one dash is no marker"), None);
    }

    #[test]
    fn prompts_group_lines_in_a_row() {
        let t = "intro\n^ one\n^ two\n\n^ three\n^> reply\n^ four";
        let p = prompts("a.md", t);
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].start, p[0].end, p[0].text.as_str()), (1, 3, "one\ntwo"));
        assert_eq!(p[1].text, "three");
        assert_eq!(p[2].start, 6);
    }

    #[test]
    fn commands_and_statuses() {
        let t = "a\n^/Help\n^/echo  a b \n^ ◌ p1 · working\n^ ◌ p1.2 · ↓ this";
        let c = commands("a.md", t);
        assert_eq!(c.iter().map(|c| (c.line, c.name.as_str(), c.args.as_str())).collect::<Vec<_>>(),
                   vec![(1, "help", ""), (2, "echo", "a b")]);
        let s = statuses("a.md", t);
        assert_eq!(s.get("p1"), Some(&3));
        assert_eq!(s.get("p1.2"), Some(&4));
    }

    #[test]
    fn clearing_replies_leaves_no_double_gap() {
        let t = "one\n\n^> a\n^> b\n\ntwo\n";
        assert_eq!(clear_replies("a.md", t, true).0, "one\n\ntwo\n");
        assert_eq!(clear_replies("a.md", t, false).0, "one\n\n\ntwo\n");
        assert_eq!(clear_replies("a.md", t, true).1, vec!["^> a", "^> b"]);
    }

    #[test]
    fn remove_and_insert() {
        let t = "a\n^ x\n^ y\nb\n^/c\nd";
        let (out, pos) = remove(t, &[(4, 5), (1, 3)]);
        assert_eq!(out, "a\nb\nd");
        assert_eq!(pos, vec![2, 1]);
        assert_eq!(insert("a\nb\n", 9, &["z".into()]), "a\nb\nz\n");
        assert_eq!(insert("a\nb", 1, &["z".into()]), "a\nz\nb");
    }

    #[test]
    fn status_marks_go_without_gaps() {
        let t = "a\n\n^ ◌ p2.2 · m\npara\n\n^ ◌ p2 · w\n\nb";
        assert_eq!(remove_marks("a.md", t, "p2"), "a\n\npara\n\nb");
    }

    #[test]
    fn replies_are_capped() {
        let body = (1..=14).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let r = reply_lines("# ", &body, 12);
        assert_eq!(r.len(), 13);
        assert_eq!(r[0], "# ^> 1");
        assert!(r[12].ends_with("2 more lines, all in .ambition/history.md"));
        assert_eq!(reply_lines("", "x\n\n", 12), vec!["^> x"]);
    }

    #[test]
    fn what_a_caret_points_at() {
        let lines: Vec<&str> = "Intro.\n\n- one\n- two\n  more\n- three\n^ x".split('\n').collect();
        let none = BTreeSet::new();
        assert_eq!(above("a.md", &lines, 6, &none), Some((5, 6)));
        assert_eq!(above("a.py", &lines, 6, &none), Some((2, 6)));
        assert_eq!(above("a.md", &lines, 0, &none), None);
        assert_eq!(below(&lines, 1, &none), Some((2, 7)));
        assert!(is_item("  12) x") && is_item("* x") && !is_item("-x") && !is_item("1. ") && !is_item("x"));
        assert_eq!(blocks("a\nb\n\n\nc\n"), vec![(0, 2), (4, 5)]);
        assert_eq!(span(3, 4), "line 4");
        assert_eq!(span(3, 6), "lines 4-6");
    }
}
