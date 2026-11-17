//! Line diffs (Myers), a unified view of them, and a closeness score for
//! "did you mean".

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tag {
    Equal,
    Replace,
    Delete,
    Insert,
}

/// One run of a diff from `a` to `b`: `a[i1..i2]` becomes `b[j1..j2]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Op {
    pub tag: Tag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

/// Past this many differences the diff gives up and calls the rest one
/// replacement. Ambition's files are notes and code, so this is rare.
const MAX_D: usize = 4000;

/// The runs that turn `a` into `b`, like Python's `SequenceMatcher.get_opcodes`.
pub fn ops<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Op> {
    // Trim the common start and end first: most edits touch a small middle.
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut steps = myers(am, bm);
    for s in steps.iter_mut() {
        s.1 += pre;
        s.2 += pre;
    }
    let mut out = Vec::new();
    let mut push = |tag: Tag, i1, i2, j1, j2| {
        if i1 == i2 && j1 == j2 {
            return;
        }
        out.push(Op { tag, i1, i2, j1, j2 });
    };
    push(Tag::Equal, 0, pre, 0, pre);
    // steps: (kind, i, j) per element: 'e' equal, 'd' delete a[i], 'i' insert b[j]
    let mut k = 0;
    while k < steps.len() {
        let (kind, i, j) = steps[k];
        if kind == 'e' {
            let mut n = 0;
            while k + n < steps.len() && steps[k + n].0 == 'e' {
                n += 1;
            }
            push(Tag::Equal, i, i + n, j, j + n);
            k += n;
            continue;
        }
        let (mut di, mut dj) = (0, 0);
        while k < steps.len() && steps[k].0 != 'e' {
            if steps[k].0 == 'd' {
                di += 1;
            } else {
                dj += 1;
            }
            k += 1;
        }
        let tag = match (di > 0, dj > 0) {
            (true, true) => Tag::Replace,
            (true, false) => Tag::Delete,
            _ => Tag::Insert,
        };
        push(tag, i, i + di, j, j + dj);
    }
    let (ae, be) = (a.len() - suf, b.len() - suf);
    push(Tag::Equal, ae, a.len(), be, b.len());
    out
}

/// Myers' O(ND) diff. Returns one step per element: ('e', i, j) when a[i] ==
/// b[j], ('d', i, j) to drop a[i], ('i', i, j) to take b[j]; i and j are
/// where each sequence stands when the step happens.
fn myers<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(char, usize, usize)> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 || m == 0 {
        let mut out = Vec::new();
        (0..a.len()).for_each(|i| out.push(('d', i, 0)));
        (0..b.len()).for_each(|j| out.push(('i', a.len(), j)));
        return out;
    }
    let max = (n + m) as usize;
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    for d in 0..=max.min(MAX_D) as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) { v[idx + 1] } else { v[idx - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                found = Some(d);
                break;
            }
            k += 2;
        }
        if found.is_some() {
            break;
        }
    }
    let Some(dmax) = found else {
        // Too different: drop all of a, take all of b.
        let mut out: Vec<_> = (0..a.len()).map(|i| ('d', i, 0)).collect();
        out.extend((0..b.len()).map(|j| ('i', a.len(), j)));
        return out;
    };
    // Walk back through the trace.
    let mut steps = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=dmax).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let idx = (k + off) as usize;
        let prev_k = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            steps.push(('e', x as usize, y as usize));
        }
        if x == prev_x {
            y -= 1;
            steps.push(('i', x as usize, y as usize));
        } else {
            x -= 1;
            steps.push(('d', x as usize, y as usize));
        }
        x = prev_x;
        y = prev_y;
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        steps.push(('e', x as usize, y as usize));
    }
    steps.reverse();
    steps
}

/// Where an insertion point between lines (before line `pos` of `before`)
/// moves to once the text has become `after`. Lines that were put back right
/// at that point stay above it.
pub fn remap(before: &str, after: &str, pos: usize) -> usize {
    if before == after {
        return pos;
    }
    let a: Vec<&str> = before.split('\n').collect();
    let b: Vec<&str> = after.split('\n').collect();
    for o in ops(&a, &b) {
        if o.i1 == o.i2 {
            continue; // an insertion right here: it stays above
        }
        if o.i1 <= pos && pos < o.i2 {
            return if o.tag == Tag::Equal { o.j1 + (pos - o.i1) } else { o.j1 + (pos - o.i1).min(o.j2 - o.j1) };
        }
    }
    b.len()
}

/// A unified diff of two texts' lines with `context` lines around each
/// change, without the file headers: hunks start with `@@`.
pub fn unified(a: &[&str], b: &[&str], context: usize) -> Vec<String> {
    let all = ops(a, b);
    let changes: Vec<&Op> = all.iter().filter(|o| o.tag != Tag::Equal).collect();
    if changes.is_empty() {
        return Vec::new();
    }
    // Group changes whose context would touch.
    let mut groups: Vec<Vec<&Op>> = Vec::new();
    for c in changes {
        match groups.last_mut() {
            Some(g) if c.i1 <= g.last().unwrap().i2 + 2 * context => g.push(c),
            _ => groups.push(vec![c]),
        }
    }
    let mut out = Vec::new();
    for g in groups {
        let (first, last) = (g[0], *g.last().unwrap());
        let (s, e) = (first.i1.saturating_sub(context), (last.i2 + context).min(a.len()));
        let sj = first.j1.saturating_sub(first.i1 - s);
        let ej = (last.j2 + (e - last.i2)).min(b.len());
        out.push(format!("@@ -{},{} +{},{} @@", s + 1, e - s, sj + 1, ej - sj));
        let mut i = s;
        for c in &g {
            while i < c.i1 {
                out.push(format!(" {}", a[i]));
                i += 1;
            }
            out.extend(a[c.i1..c.i2].iter().map(|l| format!("-{l}")));
            out.extend(b[c.j1..c.j2].iter().map(|l| format!("+{l}")));
            i = c.i2;
        }
        while i < e {
            out.push(format!(" {}", a[i]));
            i += 1;
        }
    }
    out
}

/// How alike two words are, 0 to 1: twice their longest common subsequence
/// over their total length, like difflib's ratio.
pub fn ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for x in &a {
        let mut prev = 0;
        for (j, y) in b.iter().enumerate() {
            let tmp = row[j + 1];
            row[j + 1] = if x == y { prev + 1 } else { row[j + 1].max(row[j]) };
            prev = tmp;
        }
    }
    2.0 * row[b.len()] as f64 / (a.len() + b.len()) as f64
}

/// The closest of `words` to `word`, if it is close enough (0.6, as difflib).
pub fn closest<'a>(word: &str, words: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(&str, f64)> = None;
    for w in words {
        let r = ratio(word, w);
        if r >= 0.6 && best.is_none_or(|(_, b)| r > b) {
            best = Some((w, r));
        }
    }
    best.map(|(w, _)| w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[&str], b: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for o in ops(a, b) {
            match o.tag {
                Tag::Equal => out.extend(a[o.i1..o.i2].iter().map(|s| s.to_string())),
                _ => out.extend(b[o.j1..o.j2].iter().map(|s| s.to_string())),
            }
        }
        out
    }

    #[test]
    fn ops_rebuild_the_target() {
        let cases: Vec<(&str, &str)> = vec![
            ("a b c d", "a x c d"),
            ("a b c", "a b c"),
            ("", "x y"),
            ("x y", ""),
            ("a b c d e f", "b c e f g"),
            ("1 2 3 4 5 6 7 8", "1 9 3 4 10 11 7 8 12"),
        ];
        for (x, y) in cases {
            let a: Vec<&str> = x.split_whitespace().collect();
            let b: Vec<&str> = y.split_whitespace().collect();
            assert_eq!(apply(&a, &b), b.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "{x} -> {y}");
            for o in ops(&a, &b) {
                if o.tag == Tag::Equal {
                    assert_eq!(&a[o.i1..o.i2], &b[o.j1..o.j2]);
                }
            }
        }
    }

    #[test]
    fn a_replace_is_one_run() {
        let a = ["one", "two", "three"];
        let b = ["one", "TWO", "three"];
        let o = ops(&a, &b);
        assert_eq!(o.iter().filter(|o| o.tag != Tag::Equal).count(), 1);
        assert_eq!(o[1], Op { tag: Tag::Replace, i1: 1, i2: 2, j1: 1, j2: 2 });
    }

    #[test]
    fn unified_view() {
        let a = ["a", "b", "c", "d", "e", "f", "g"];
        let b = ["a", "b", "C", "d", "e", "f", "g", "h"];
        let u = unified(&a, &b, 1);
        assert_eq!(u, vec!["@@ -2,3 +2,3 @@", " b", "-c", "+C", " d", "@@ -7,1 +7,2 @@", " g", "+h"]);
    }

    #[test]
    fn positions_follow_a_change() {
        // A one-line paragraph grows back to three: what came after it moves down two.
        let before = "# T\n\nshort.\n\nThe end.\n\n";
        let after = "# T\n\nline one,\nline two,\nline three.\n\nThe end.\n\n";
        assert_eq!(remap(before, after, 6), 8);
        assert_eq!(remap(before, after, 4), 6);
        assert_eq!(remap(before, after, 1), 1);
        assert_eq!(remap(before, before, 3), 3);
        // Lines put back exactly at the point stay above it.
        assert_eq!(remap("a\nc", "a\nb\nc", 1), 2);
    }

    #[test]
    fn closeness() {
        assert_eq!(closest("slwo", ["slow", "help", "stop"]), Some("slow"));
        assert_eq!(closest("zzz", ["slow", "help"]), None);
        assert!(ratio("abc", "abc") == 1.0);
    }
}
