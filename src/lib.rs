//! Ambition: a document you talk to.
//!
//! In any file, a line that starts with `^` is a prompt; press a key on it,
//! and the answer lands in the file. A line that starts with `^/` is a
//! command, answered at once, in code. `^>` lines are replies, and your next
//! press in that file clears them. Saving never sends anything.
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

pub mod app;
pub mod carets;
pub mod commands;
pub mod config;
pub mod diff;
pub mod engine;
pub mod folder;
pub mod hook;
pub mod http;
pub mod inbox;
pub mod lsp;
pub mod pkce;
pub mod t3;
pub mod util;
