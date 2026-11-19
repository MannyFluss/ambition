//! Ambition: a document you talk to.
//!
//! In any file or text field, press a key: the agent reads around the
//! cursor and the answer lands there. A line that starts with `^` is a
//! prompt, which says what to look at; a line that starts with `^/` is a
//! command, answered at once, in code. `^>` lines are replies, and your next
//! press there clears them. Saving never sends anything.
//!
//! - `app`: the running Ambition, and what one press does.
//! - `inbox`: the one door every press comes in by.
//! - `hook`: what a press sends the agent.
//! - `config`: grants, and which folder a file belongs to.
//! - `carets`: reading caret lines.
//! - `folder`: a folder's private history, kept in `~/.ambition/history`.
//! - `commands`: your own `^/` commands.
//! - `engine` and `t3`: what answers prompts. T3 Code today.
//! - `lsp`: the language server, for colors, hover and completion in editors.
//! - `field`: text fields, mirrored as files.
//! - `editor`: open editor buffers, through the language server.
//! - `ui`: the one dialog, and notices.
//! - `mac`: the global key, Accessibility, and the dialog, on macOS.

pub mod app;
pub mod carets;
pub mod commands;
pub mod config;
pub mod diff;
pub mod editor;
pub mod engine;
pub mod field;
pub mod folder;
pub mod hook;
pub mod http;
pub mod inbox;
pub mod lsp;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod pkce;
pub mod t3;
pub mod ui;
pub mod util;
