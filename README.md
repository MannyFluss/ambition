<p align="center">
  <img src="media/demo.gif" width="760" alt="A note in Zed. Under a long paragraph, the line ^ make this shorter is typed, and control-option-return is pressed on it. The prompt becomes a status line with a timer, and the paragraph is rewritten in place. Then ^/undo is typed and pressed, the original paragraph comes back, and a ^> line says what was undone.">
</p>

# Ambition

Write a line that starts with `^` in any file, press ⌃⌥↩ on it, and the
answer lands in the file. There is no chat box: the document is the
conversation. Behind it, each file has its own agent thread in
[T3 Code](https://github.com/pingdotgg/t3code), with everything a T3 thread
can do (a shell, the web, other agents).

## Three marks

    ^ make this shorter     a prompt: a press on it sends it to the agent
    ^/undo                  a command: answered at once, in code, with no model
    ^> the answer is 4      a reply: your next press in that file clears it

That is all the syntax there is.

## A key, not a save

Saving sends nothing. Only a press does: your editor's Ambition key saves
the file and runs `ambition fire FILE:LINE`, and only the `^` line under
the cursor is sent. A `^` line that arrives any other way (a `git pull`, a
sync, another program) is text until you press on it.

The first press in a folder asks first. On macOS a dialog offers Deny,
Allow once and Always; elsewhere, run `ambition allow <folder>`. A folder is
the file's git root, else the folder it is in, and one grant covers
everything inside it. Your whole home folder or a disk is never granted.

## Get it

One file:

    curl -fsSL https://github.com/MannyFluss/ambition/releases/latest/download/ambition-$(uname -m)-$(uname -s).tar.gz | tar xz

Put `ambition` on your PATH and start it: `ambition`. It sleeps until a
press reaches it, so it can stay running; on macOS a launchd job keeps it
up (below). With Rust: `cargo install --git https://github.com/MannyFluss/ambition`.

It needs two things beside it:

- **git**, for the private history that makes undo work.
- **[T3 Code](https://github.com/pingdotgg/t3code)**, open. You can open any
  file's thread there to read exactly what each press sent.

Then bind the key in your editor.

## In your editor

**Zed** (tried on 1.28). Add the task in
[`editors/zed/tasks.json`](editors/zed/tasks.json) to
`~/.config/zed/tasks.json` and the binding in
[`editors/zed/keymap.json`](editors/zed/keymap.json) to
`~/.config/zed/keymap.json`. The task saves the file first, and its terminal
stays out of sight. If a press seems to do nothing, open the terminal panel
(⌃\`): its tab "Ambition: press" says why. Zed runs tasks in your login
shell, so `ambition` must be on that shell's PATH.

For colors, completion, and a hover that says what a press will send,
`editors/zed/install.sh` adds the extension. Turn on the two features it
uses, which Zed keeps off by default, and trust the folder when Zed asks (in
Restricted Mode it runs no language server):

    {
      "semantic_tokens": "combined",
      "inlay_hints": { "enabled": true },
      "languages": {
        "Markdown": { "semantic_tokens": "combined", "inlay_hints": { "enabled": true } }
      }
    }

**Neovim** (0.10 or later): put [`editors/nvim/ambition.lua`](editors/nvim/ambition.lua)
in `~/.config/nvim/plugin/`. It binds ⌃⌥↩ (`<C-M-CR>`), adds
`:AmbitionPress`, and starts the language server. A terminal Neovim sees
⌃⌥↩ only if the terminal sends it; the file says what to do if not.

**Anything else**: bind a key that saves, then runs `ambition fire
FILE:LINE` with the cursor's line. For the language server, start `ambition
lsp`. Only Zed and Neovim have been tried.

## What a press sends

The agent gets what Ambition calls the hook: where you are (the file's
path), your `^` line, the 8 lines above and below it as they are on disk,
and what changed in the file since that file's last turn. Nothing else is
attached. When the agent needs more, it runs `ambition more up` or
`ambition more down`, with a number of lines it chooses, and reads further
from where the hook stopped. What the agent is told is one file you can
read, [RULES.md](RULES.md), exactly as each thread receives it.

That was chosen by [examples/context.rs](examples/context.rs), 16 prompts
whose right answers are known (a table under the caret, a section under a
heading, a sentence between two paragraphs, a question inside a function, an
entry 140 lines from the one to copy), each in a fresh T3 thread, twice per
model:

|                                          | Claude Sonnet 5.5 | Claude Haiku 5.5 | a turn, Haiku |
|------------------------------------------|-------------------|------------------|---------------|
| the paragraph above                      | 32 of 32          | 31 of 32         | 9.7 s         |
| above and below                          | 32 of 32          | 32 of 32         | 10.8 s        |
| file and line only                       | 32 of 32          | 32 of 32         | 17.7 s        |
| the whole file                           | 32 of 32          | 32 of 32         | 10.3 s        |
| **8 lines each way, and `ambition more`** | **32 of 32**     | **32 of 32**     | **12.4 s**    |

The first four rows are Ambition 0.1, on 2026-11-16, which sent on save;
the code for them is at tag v0.1.0. The paragraph above missed the one
prompt that asks for a sentence between two paragraphs, and file and line
alone was slowest because the agent read the file first. The last row is
this release, on 2026-11-18, through the real press: its time runs from the
press to the status line going, so it includes Ambition's own commits and
T3 starting a thread, which 0.1's rows did not. (Sonnet 5.5's median that
day was 5.9 s.) The agents asked for more in 3 of 64 runs, all to summarize
a long section below the caret; for the entry 140 lines away they used
their own tools on the file. The hook stays the same size however long the
file grows.

## Threads

- **One per file.** Each file's thread goes on across presses and days,
  and T3 groups a folder's threads under one project. `^/thread` gives the
  link; `^/new` starts a fresh one with a handoff.
- **One turn at a time per folder.** A press while another turn runs in the
  same folder waits in line, and its status line says `waiting for p2`.
  Other folders run side by side. Commands run at once, except `^/undo`,
  which asks you to press it again when the turn ends.
- **The coordinator.** One more thread knows the others. A thread reaches it
  with `ambition ask coordinator "…"` and gets its answer back; you talk to
  it in `~/.ambition/coordinator.md`, with `^` lines like anywhere else. It
  can read the list of threads and any thread's history, and pass messages
  on. It cannot stop or start threads.

## Commands

    ^/help                  the commands, with what each does
    ^/model                 the models T3 offers, the one in use marked
    ^/model opus high       switch this file's thread (and effort); new threads start on it
    ^/new                   this file's next prompt starts a new thread, with a handoff
    ^/thread                the link to this file's thread
    ^/stop                  stop the turn running in this folder
    ^/undo                  take back Ambition's last change in this folder

Your own commands are scripts in `~/.ambition/commands/`, one file each,
for every folder: `count.sh` answers `^/count`. The paragraph above the
command arrives on stdin, and what the script prints becomes the reply. Ask
for one in plain words, and the agent writes it there.

## Why it works this way

**Your attention is the scarce thing.** The caret marks where you are
looking, and that is all the context you give. The hook is small and the
same every time; the agent reads further only when it needs to.

**A press means you were there.** Nothing fires because a file changed, so
nothing another program writes can start an agent with a shell. Running
`ambition fire` takes code already running as you, and even then only the
line it names is sent.

**The document is the interface.** The answer lands where you were
looking: as an edit, or as `^>` lines under your prompt.

**Quiet.** There is no file watcher. Between presses Ambition sleeps on its
inbox, a socket only you can reach; `^/` commands answer in code, with no
model.

**Nothing is final.** Ambition keeps a private git history of each folder,
apart from your own repo, and `^/undo` takes back the agent's last turn. So
the agent acts instead of asking for permission.

**Show, don't claim.** The rules are one file, the hook was chosen by an
experiment you can rerun, and each thread shows every message it was sent.
The screenshots in [proof/](proof/) come from driving the real Zed.

## Everything it keeps

All of it is in `~/.ambition/` (or `$AMBITION_HOME`). Nothing is kept in
your folders; the agents still edit your files.

    config            your settings: `allow = <folder>`, one line per grant
    threads           each file's thread: its folder, link, model, last turn
    state.json        the presses on their way now, for the language server
    models.json       the models T3 offered when last asked, and the one chosen last
    commands/         your own ^/ commands
    notes.md          what threads keep for later threads
    AGENTS.md         optional: every thread reads it
    coordinator.md    where you talk to the coordinator
    history/          per folder: its private git history (<folder>-<hash>.git)
                      and the record of every prompt and reply (<folder>-<hash>.md)
    inbox, lock       where the running Ambition listens, and one at a time

A thread also reads the `AGENTS.md` files on the way from its folder down
to its file. A new thread starts when one of them changes, or the rules do,
with a handoff from the thread before.

To keep it running on macOS, a launchd job at
`~/Library/LaunchAgents/com.you.ambition.plist`, loaded once with `launchctl
load` on that file:

    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0"><dict>
      <key>Label</key><string>com.you.ambition</string>
      <key>ProgramArguments</key><array><string>/usr/local/bin/ambition</string></array>
      <key>RunAtLoad</key><true/>
      <key>KeepAlive</key><true/>
    </dict></plist>

## Inside

- One inbox, one door: `ambition fire`, and the agent's `ambition more` and
  `ambition ask`, all post a line of JSON to it ([src/inbox.rs](src/inbox.rs)).
  A global hotkey can post there the same way.
- The engine is T3 Code, behind one small trait
  ([src/engine.rs](src/engine.rs)). Another engine is one more `impl`.
- `cargo test` runs the real binary against a fake T3, press by press, with
  a stand-in for the macOS dialog. CI runs it on macOS and Linux on every
  push. This release was used on macOS; it compiles for Linux and Windows,
  but nobody has used it there yet.

## Where it is going

The same key in every text field on a Mac (a browser's, Notes'), with a
field's text kept like a file so that undo and "what changed" work the same.
That needs Accessibility and Screen Recording grants, so it comes as its own
release, 0.3.

---

MIT. Made by [MannyFluss](https://github.com/MannyFluss).
