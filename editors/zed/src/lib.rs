//! Ambition for Zed: it starts `ambition lsp`, the language server built into
//! the ambition binary, which does the rest. The server acts only on files in
//! folders Ambition watches and leaves every other file alone.
//!
//! It finds `ambition` on your PATH. To use another copy, set
//! `lsp.ambition.binary.path` in Zed's settings, as for any language server.

use zed_extension_api::{self as zed, settings::LspSettings, LanguageServerId, Result, Worktree};

struct Ambition;

impl zed::Extension for Ambition {
    fn new() -> Self {
        Ambition
    }

    fn language_server_command(&mut self, _id: &LanguageServerId, worktree: &Worktree) -> Result<zed::Command> {
        let env = worktree.shell_env();
        let binary = LspSettings::for_worktree("ambition", worktree).ok().and_then(|s| s.binary);
        if let Some(path) = binary.as_ref().and_then(|b| b.path.clone()) {
            let args = binary.as_ref().and_then(|b| b.arguments.clone()).unwrap_or_else(|| vec!["lsp".into()]);
            let mut vars = env.clone();
            if let Some(extra) = binary.and_then(|b| b.env) {
                vars.extend(extra);
            }
            return Ok(zed::Command { command: path, args, env: vars });
        }
        let path = worktree
            .which("ambition")
            .ok_or("Ambition: the `ambition` command is not on your PATH. See https://github.com/MannyFluss/ambition")?;
        Ok(zed::Command { command: path, args: vec!["lsp".into()], env })
    }
}

zed::register_extension!(Ambition);
