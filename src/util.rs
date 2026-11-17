//! Small things several parts need: the time, random tokens, the home folder.

use std::path::{Path, PathBuf};

/// Local time, to the second: `2026-11-16 22:09:40`.
pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Local time, to the minute, for thread titles.
pub fn minute() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// Seconds since 1970, as a float.
pub fn epoch() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Random bytes from the operating system.
pub fn random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("the operating system gave no random bytes");
    buf
}

/// A short random hex string, for temporary names.
pub fn nonce() -> String {
    random(6).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/notes` → `/Users/you/notes`.
pub fn expand(p: &str) -> PathBuf {
    let p = p.trim();
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return home().join(rest);
    }
    PathBuf::from(p)
}

/// `/Users/you/notes` → `~/notes`, for messages.
pub fn tilde(p: &Path) -> String {
    let h = home();
    let real = std::fs::canonicalize(&h).unwrap_or_else(|_| h.clone());
    for base in [&h, &real] {
        match p.strip_prefix(base) {
            Ok(rest) if rest.as_os_str().is_empty() => return "~".into(),
            Ok(rest) => return format!("~/{}", rest.display()),
            Err(_) => {}
        }
    }
    p.display().to_string()
}

/// The first `n` characters of a line of text, with … when cut.
pub fn cut(s: &str, n: usize) -> String {
    let one = s.split('\n').next().unwrap_or("");
    let mut out: String = one.chars().take(n).collect();
    if one.chars().count() > n || s.contains('\n') {
        out.push('…');
    }
    out
}

/// A short hash of some text: 16 hex characters of its SHA-256.
pub fn sha(text: &str) -> String {
    crate::pkce::sha256(text.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutting() {
        assert_eq!(cut("abcdef", 3), "abc…");
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("ab\ncd", 9), "ab…");
    }
}
