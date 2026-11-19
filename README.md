<p align="center">
  <img src="media/demo.gif" width="760" alt="A note in Zed. Under a long paragraph, the line ^ make this shorter is typed, and control-option-return is pressed on it. The prompt becomes a status line with a timer, and the paragraph is rewritten in place. Then ^/undo is typed and pressed, the original paragraph comes back, and a ^> line says what was undone.">
</p>

# Ambition

Press ⌃⌥↩ in any file, or in any text field on a Mac (a browser's, a chat
box, Notes, Google Docs), and the answer lands where you pressed. There is
no chat box: the document is the conversation. Behind it, each file and
each field has its own agent thread in
[T3 Code](https://github.com/pingdotgg/t3code), with everything a T3 thread
can do (a shell, the web, other agents).

## A press, and three marks

    any line                a request: the agent does what the text there needs
    ^ make this shorter     a prompt: look at this, and do this
    ^/undo                  a command: answered at once, in code, with no model
    ^> the answer is 4      a reply: your next press there clears it

A press on a line with no mark asks the agent to finish, fix or tighten the
text there, or to answer a question written plainly. It errs on the side of
caution: the smallest change that does the job, or, when it is not sure,
one short `^>` question and no change. A `^` says "look at this
specifically". The marks are lines in the config (`ask = ^`, `reply = ^>`,
`command = ^/`); those are the defaults.

## A key, not a save

Saving sends nothing, and Ambition never saves. Only a press does
anything, and it sends only the line under the cursor. A `^` line that
arrives any other way (a `git pull`, a sync, another program) is text until
you press on it.

Within a fraction of a second of a press, a status line stands where you
pressed: `^ ◌ p3 · working`. When the turn ends, the answer replaces it,
and on a Mac the changed text is highlighted for a moment, then the cursor
goes to its end. A second press there, before you change anything, shows
the change again.

The first press in a folder, or in an app's text fields, asks first, before
anything else happens. On a Mac a dialog offers Deny, Allow once and
Always; elsewhere, run `ambition allow <folder>`. A folder is the file's
git root, else the folder it is in, and one grant covers everything inside
it. Your whole home folder or a disk is never granted. An app's grant is
`allow = app:<bundle id>`, and covers its text fields.

## Get it

One file:

    curl -fsSL https://github.com/MannyFluss/ambition/releases/latest/download/ambition-$(uname -m)-$(uname -s).tar.gz | tar xz

Put `ambition` on your PATH and start it: `ambition`. It sleeps until a
press reaches it, so it can stay running; on macOS a launchd job keeps it
up (below). With Rust: `cargo install --git https://github.com/MannyFluss/ambition`.

**On a Mac, for text fields**, build `Ambition.app`:
`macos/build-app.sh` puts it in `~/Applications`. It is the same binary
in a small app bundle, signed with a certificate the script makes on your
Mac, so macOS keeps its grants from one build to the next (the first run
asks for your password once, to trust that certificate). Then, in System
Settings › Privacy & Security, turn on Ambition under Accessibility (to
read and write text fields) and under Screen & System Audio Recording (for
the agent's `ambition look`).

It needs two things beside it:

- **git**, for the private history that makes undo work.
- **[T3 Code](https://github.com/pingdotgg/t3code)**, open. You can open any
  file's thread there to read exactly what each press sent.

Then bind the key in your editor.

## In your editor

**Zed** (tried on 1.29). Add the task in
[`editors/zed/tasks.json`](editors/zed/tasks.json) to
`~/.config/zed/tasks.json`, put the full path of your `ambition` in its
`command`, and add the binding in
[`editors/zed/keymap.json`](editors/zed/keymap.json) to
`~/.config/zed/keymap.json`. The task does not save, and runs through
`/bin/sh` rather than your login shell, which can take most of a second.
Its terminal stays out of sight; if a press seems to do nothing, open the
terminal panel (⌃\`): its tab "Ambition: press" says why.

Then add the extension, with `editors/zed/install.sh`. Its language server
is how Ambition reaches your open buffer: a press reads the buffer as it
is, unsaved, and the answer goes back in as an edit of only the text that
changes, the way typing would. Nothing is saved, your cursor and the rest
stay where they were, and Zed's ⌘Z takes it back (twice: once for the
answer, once for the status line). Without it, Ambition reads and writes
the file on disk. The extension also gives colors, completion, and a hover
that says what a press will send. Turn on the two features it uses, which
Zed keeps off by default, and trust the folder when Zed asks (in Restricted
Mode it runs no language server):

    {
      "semantic_tokens": "combined",
      "inlay_hints": { "enabled": true },
      "languages": {
        "Markdown": { "semantic_tokens": "combined", "inlay_hints": { "enabled": true } }
      }
    }

**Neovim** (0.10 or later): put [`editors/nvim/ambition.lua`](editors/nvim/ambition.lua)
in `~/.config/nvim/plugin/`. It binds ⌃⌥↩ (`<C-M-CR>`), adds
`:AmbitionPress`, and starts the language server, which takes Ambition's
edits into the buffer as Zed's does (tried in Zed only so far). A terminal
Neovim sees ⌃⌥↩ only if the terminal sends it; the file says what to do if
not.

**Anything else**: bind a key that runs `ambition fire FILE:LINE` with the
cursor's line, and start `ambition lsp` as a language server, so presses
reach your unsaved buffer. Only Zed and Neovim have been tried.

## In any text field (macOS)

Outside your editors, ⌃⌥↩ is Ambition's everywhere: macOS hands the chord
to Ambition, so the app under it never sees it (no stray Return). It steps
aside while an editor on the list is in front, so the editor's own binding
presses there; the list is Zed alone unless the config names others
(`editor = <bundle id>`, one line each). `hotkey = ctrl+option+return` sets
the chord, and `hotkey = none` turns it off.

A field is kept as a file, its mirror, in `~/.ambition/fields/<app>/`, and
from there a press is the same as in any file: the same marks, thread,
history and `^/undo`. A field is its page's address in a browser;
elsewhere, the app, its window title without counts like "(3)", and the
field's own identifier. The mirror takes the field's text at each press,
and the answer goes back into the field only while it says what Ambition
left there, and only while that app, window and field are in front. If you
typed there, or moved away, the answer waits: a notice at the top of the
screen says so, and a press in the field, as Ambition left it, puts it in.

Writing goes through Accessibility, one changed piece at a time. Where a
field takes no text that way (Chrome and Electron apps such as Discord),
Ambition selects the piece and pastes it, and puts your clipboard back.
Meanwhile your keys and clicks wait and are played after, a fraction of a
second later, so nothing can move the cursor or the focus halfway through;
if a paste lands anywhere but where it belongs, Ambition takes it back at
once with ⌘Z. Tried on macOS 26 in TextEdit, Notes, Safari, Discord and
Google Docs in Chrome. **Google Docs** shows its text only with its own
screen reader and braille support on: in a document, press ⌘⌥Z, then ⌘⌥H
(Tools › Accessibility settings).

In a field's turn the agent can also run `ambition read` (the window's
text) and `ambition look` (a screenshot of the window). An app's grant
covers both; Allow once covers that press's turn.

## What a press sends

The agent gets what Ambition calls the hook: where you are (the file's
path, or the field's app, window and address), your `^` line (or, for a
press with no mark, the line number), the 8 lines above and below it, and
what changed there since that document's last turn. Nothing else is
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
the agent acts instead of asking for permission, and it never commits or
changes your repositories with git.

**Show, don't claim.** The rules are one file, the hook was chosen by an
experiment you can rerun, and each thread shows every message it was sent.
The screenshots in [proof/](proof/) come from driving the real Zed.

## Everything it keeps

All of it is in `~/.ambition/` (or `$AMBITION_HOME`). Nothing is kept in
your folders; the agents still edit your files.

    config            your settings: `allow`, `hotkey`, `editor`, and the marks
    threads           each file's thread: its folder, link, model, last turn
    state.json        the presses on their way now, for the language server
    models.json       the models T3 offered when last asked, and the one chosen last
    commands/         your own ^/ commands
    notes.md          what threads keep for later threads
    AGENTS.md         optional: every thread reads it
    coordinator.md    where you talk to the coordinator
    history/          per folder: its private git history (<folder>-<hash>.git)
                      and the record of every prompt and reply (<folder>-<hash>.md)
    fields/           each text field's mirror, by app
    buffers/          each open editor buffer's copy, while it is pressed
    looks/            the last 20 screenshots `ambition look` took
    inbox, lock       where the running Ambition listens, and one at a time

The first press in a folder keeps only its document in the history, so
nothing waits on a copy of a big folder; the whole folder is kept right
after that first turn ends. So the first turn's `^/undo` covers its
document only.

A thread also reads the `AGENTS.md` files on the way from its folder down
to its file. A new thread starts when one of them changes, or the rules do,
with a handoff from the thread before.

To keep it running on macOS, a launchd job starts the binary inside
`Ambition.app` at login and restarts it if it stops: copy
[`macos/ambition.plist`](macos/ambition.plist) to
`~/Library/LaunchAgents/com.you.ambition.plist`, put your home folder in
it, and load it once with `launchctl load` on that file. The job starts the
app's binary rather than the app registering itself, so one job keeps
exactly one Ambition up; a second one stops at once. Started from inside
the app, macOS knows it as Ambition, and the app's grants apply.

## Inside

- One inbox, one door: `ambition fire`, and the agent's `ambition more`,
  `ambition ask`, `read` and `look`, all post a line of JSON to it
  ([src/inbox.rs](src/inbox.rs)). The language server keeps one line of it
  open, for buffers ([src/editor.rs](src/editor.rs)). The global key,
  Accessibility and the dialog are [src/mac/](src/mac/).
- The engine is T3 Code, behind one small trait
  ([src/engine.rs](src/engine.rs)). Another engine is one more `impl`.
- `cargo test` runs the real binary against a fake T3, press by press, with
  a stand-in for the macOS dialog. CI runs it on macOS and Linux on every
  push. This release was used on macOS; it compiles for Linux and Windows,
  but nobody has used it there yet.

---

MIT. Made by [MannyFluss](https://github.com/MannyFluss).
