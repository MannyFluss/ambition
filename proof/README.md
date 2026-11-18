# Proof

Ambition 0.2, used in the real Zed (1.28.1) on macOS on 2026-11-18, the way
a person uses it: typing in a file, saving with ⌘S, pressing ⌃⌥↩. Keys were
sent with `osascript` and the screen captured with `screencapture`. The
agent was real: T3 Code with Claude Haiku 5.5. Everything ran in scratch
folders, with Ambition's state in a scratch `$AMBITION_HOME`. The Zed task
was the one in `editors/zed/tasks.json`, except that it named the 0.2
binary by its full path, because the `ambition` on that Mac's PATH was
still 0.1.

1. [01-saving-sends-nothing.png](01-saving-sends-nothing.png): `^/model haiku 5.5`
   typed and saved with ⌘S. Nothing happened: Ambition's log stayed empty.
2. [02-first-press-in-a-folder-asks.png](02-first-press-in-a-folder-asks.png):
   the first press in the folder brings up the dialog: Deny, Allow once,
   Always.
3. [03-always-and-the-command-answers.png](03-always-and-the-command-answers.png):
   after Always, the command answered in place, and `allow = …/notes` was in
   the config.
4. [04-a-prompt-typed.png](04-a-prompt-typed.png): a prompt under the
   paragraph; the language server lights the lines a press will send.
5. [05-the-press-puts-a-status-line.png](05-the-press-puts-a-status-line.png):
   the press: the prompt is now its status line, `p1 · … · working`.
6. [06-the-answer-lands.png](06-the-answer-lands.png): 12 seconds later the
   paragraph is shorter, in place, and the status line is gone.
7. [07-undo.png](07-undo.png): `^/undo` pressed: the paragraph is back, and a
   `^>` line says what was undone.
8. [08-a-busy-folder-makes-a-press-wait.png](08-a-busy-folder-makes-a-press-wait.png):
   a press in todo.md while notes.md's turn ran in the same folder:
   `waiting for p2`. It ran the moment p2 ended.
9. [09-todo-md-has-its-own-thread.png](09-todo-md-has-its-own-thread.png) and
   [10-notes-md-has-another.png](10-notes-md-has-another.png): `^/thread` in
   each file gives a different thread (…`48646b8e` and …`a3138dc1`), in one
   T3 project.
10. [11-another-folder-asks-again.png](11-another-folder-asks-again.png): a
    press in a second folder asks again; a grant covers one folder.
11. [12-deny-sends-nothing.png](12-deny-sends-nothing.png): Deny. The file is
    unchanged, nothing was granted, and the task's tab says why.
12. [13-a-thread-asks-the-coordinator.png](13-a-thread-asks-the-coordinator.png):
    a prompt asked todo.md's agent to ask the coordinator. The agent ran
    `ambition ask coordinator`, a coordinator thread started, read the list
    of threads and answered, and the answer landed in the file.

The demo at the top of the README, `media/demo.gif`, was recorded the same
day, the same way.
