//! What Ambition shows on screen itself: one question (may a press run
//! here?) and short notices. On a Mac the running Ambition shows them (a
//! dialog, and a small notice at the top of the screen that takes no
//! focus). Elsewhere there is no screen: the question cannot be asked, and
//! notices go to the log only.
//!
//! `AMBITION_TEST_DIALOG=<file>` answers the question with the word in that
//! file (Deny when there is none) and logs each question and notice beside
//! it, for the end-to-end tests.

use std::io::Write;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Deny,
    Once,
    Always,
    /// Nobody answered in time.
    Unanswered,
    /// It could not be asked.
    Failed(String),
}

pub trait Ui: Send + Sync {
    fn ask(&self, title: &str, text: &str) -> Answer;
    fn tell(&self, text: &str);
}

static UI: OnceLock<Box<dyn Ui>> = OnceLock::new();

/// The screen, once there is one.
pub fn install(u: Box<dyn Ui>) {
    let _ = UI.set(u);
}

fn test_file() -> Option<String> {
    std::env::var("AMBITION_TEST_DIALOG").ok().filter(|f| !f.is_empty())
}

fn note(file: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{}", line.replace('\n', " "));
    }
}

/// Ask: Allow once, Always, or Deny.
pub fn ask(title: &str, text: &str) -> Answer {
    if let Some(f) = test_file() {
        note(&format!("{f}.log"), "asked");
        return match std::fs::read_to_string(&f).unwrap_or_default().trim() {
            "Always" => Answer::Always,
            "Allow once" => Answer::Once,
            "Unanswered" => Answer::Unanswered,
            _ => Answer::Deny,
        };
    }
    match UI.get() {
        Some(u) => u.ask(title, text),
        None => Answer::Failed("there is no screen to ask on".into()),
    }
}

/// Tell, without taking the focus from where you are.
pub fn tell(text: &str) {
    crate::app::log(&format!("told: {text}"));
    if let Some(f) = test_file() {
        note(&format!("{f}.told"), text);
        return;
    }
    if let Some(u) = UI.get() {
        u.tell(text);
    }
}
