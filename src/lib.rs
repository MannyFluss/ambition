//! Ambition: a document you talk to.
//!
//! Point it at a folder. In any file there, a line that starts with `^` is a
//! prompt; save, and the answer lands in the file. A line that starts with
//! `^/` is a command, answered at once, in code. `^>` lines are replies, and
//! your next save clears them.
//!
//! - `app`: one folder, watched, and what one save does.
//! - `carets`: reading caret lines.
//! - `folder`: the hidden `.ambition/` folder and its private history.
//! - `commands`: your own `^/` commands.
//! - `engine` and `t3`: what answers prompts. T3 Code today.
//! - `lsp`: the language server, for colors, hover and completion in editors.

pub mod app;
pub mod carets;
pub mod commands;
pub mod diff;
pub mod engine;
pub mod folder;
pub mod http;
pub mod lsp;
pub mod pkce;
pub mod t3;
pub mod util;
