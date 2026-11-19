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

## Text fields, open buffers, and a press with no mark (0.3, phase 1)

Used on macOS 26.6 on 2026-11-18, on the branch `v0.3-fields`, the way a
person uses it: keys sent with `osascript`, a real mouse click posted as a
system event, the screen captured with `screencapture`, each image cropped
to the app. The agent was real: T3 Code with Claude Haiku 5.5. Ambition ran
from a scratch `$AMBITION_HOME`, as the binary inside an ad hoc signed
`Ambition.app`, started from a shell, so it used that shell's Accessibility
and Screen Recording grants; the app's own grants come when it is
installed. Timings are from Ambition's log.

14. [14-a-field-asks-first.png](14-a-field-asks-first.png): the first press
    in an app (TextEdit) asks before anything is read: Allow once, Always,
    Deny. 63 ms from the key to the dialog (Chrome); 0.3 s in Zed, most of
    it Zed starting its task.
15. [15-the-status-line-in-a-field.png](15-the-status-line-in-a-field.png):
    the status line takes the `^` line's place in the field, 149 ms after
    Always (171 ms after the key in Discord). The chord itself never reached
    TextEdit: no newline was added.
16. [16-a-press-with-no-mark.png](16-a-press-with-no-mark.png): a press on
    "The meeting moved to thursday at 3pm, tell sam and bring the the
    slides", with no mark. The agent fixed only that: Thursday, Sam, one
    "the".
17. [17-a-second-press-highlights-again.png](17-a-second-press-highlights-again.png)
    and [18-then-the-cursor-at-the-end.png](18-then-the-cursor-at-the-end.png):
    a second press on the unchanged field highlights what was written, for
    about a second; then the cursor is at its end, nothing selected.
19. [19-discord-working.png](19-discord-working.png) and
    [20-discord-answer.png](20-discord-answer.png): Discord's message box,
    pasted into (it takes no text through Accessibility); the clipboard
    came back exactly. Nothing was sent, and the box was cleared after.
20. [21-google-docs-working.png](21-google-docs-working.png) and
    [22-google-docs-answer.png](22-google-docs-answer.png): Google Docs,
    with its screen reader and braille support on: the status line, then
    the paragraph above made shorter, in place. The blank line under the
    title stayed.
21. [23-a-click-during-the-write.png](23-a-click-during-the-write.png): a
    mouse click on the paragraph, 193 ms after the key, while Ambition was
    writing the status line. The click waited ("held 2 of your keys and
    clicks"), the status line went exactly onto the `^` line, and only then
    did the click put the cursor where it was made.
22. [24-safari-working.png](24-safari-working.png) and
    [25-safari-answer.png](25-safari-answer.png): a textarea in a Safari
    page; a field in a page is its address.
23. [26-notes-answer.png](26-notes-answer.png): a press with no mark in a
    note: "buter" and "suger" fixed, the bold title untouched (only the
    changed line is rewritten).
24. [27-moved-away-the-answer-waits.png](27-moved-away-the-answer-waits.png)
    and [28-a-press-puts-it-in.png](28-a-press-puts-it-in.png): a press in
    TextEdit, then Finder brought to the front while the agent worked.
    Nothing was written; a notice said why, without taking the focus. Back
    in TextEdit, a press put the kept answer in.
25. [29-zed-a-new-folder-asks.png](29-zed-a-new-folder-asks.png): the
    first press in a new folder in Zed asks at once. Ambition's dialog is
    the one behind; the one in front is from the 0.2 install on the same
    Mac, whose own Zed task has the same name and ran too (it was denied).
26. [30-zed-status-unsaved.png](30-zed-status-unsaved.png): the status line
    reaches the open buffer through the language server, 100 ms after
    Always; the tab's dot says unsaved, and the file on disk did not
    change.
27. [31-zed-answer-in-place.png](31-zed-answer-in-place.png): the answer,
    as an edit of only what changed ("about" taken out of line 5); still
    unsaved, the disk still the old text.
28. [32-zed-undo-once.png](32-zed-undo-once.png) and
    [33-zed-undo-twice.png](33-zed-undo-twice.png): Zed's own ⌘Z takes the
    answer back, and once more the status line, back to the saved file.
29. [34-zed-no-mark-status.png](34-zed-no-mark-status.png) and
    [35-zed-no-mark-answer.png](35-zed-no-mark-answer.png): a press with no
    mark on "is singin along" in Zed: the status line just below it, the
    cursor left where it was, then only "singin" changed. (This press ran
    from Zed's task picker, because the 0.2 install's task of the same name
    took the key there.)
30. [36-ambition-look.png](36-ambition-look.png): the agent, asked to,
    ran `ambition read` (it reported the window's first words, "Window
    check") and `ambition look`, which saved this screenshot of the window.

Not shown: Ambition.app with its own Accessibility and Screen Recording
grants (the move steps say what to click), and Neovim.
