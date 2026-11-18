# Ambition, in ten presses

Ambition turns any text file into a conversation with an agent. There is no
chat box. You write a line, you press a key on it, and the answer appears
in the file, or the file itself changes. Behind it is a T3 Code thread for
this file, with a full toolkit (shell, web, git, other agents).

How to play:

- Save a copy of this file anywhere and open it in your editor, with
  Ambition running and its key bound (the README says how). Play in the
  copy, so this one stays clean.
- Each step has a line that starts with ^. Put the cursor on it and press
  the key (⌃⌥↩ in the bindings Ambition ships). One step per press.
- Saving does nothing, and neither does typing: only a press sends a line,
  and only the line under the cursor.
- The first press in a folder asks whether Ambition may work there. Allow
  once lets this press through; Always lets every press in that folder
  through from now on.

## 1 · Three marks

    ^ a prompt         asks the agent, about the text around it
    ^/help             a command: runs at once, no AI involved
    ^> an answer       the agent's reply; your next press in this file clears it

That is all the syntax there is. (The lines just above are indented, so
they are only examples: a press on them sends nothing.)

## 2 · An instant command

Press on it. It answers in about a second, because no model is involved.

^/help

## 3 · Your first prompt

Press on the line below. While the agent works, a ◌ status line with your
words stands where your prompt was.

The quick brown fox jumps over the lazy dog, who has seen this sentence so
many times that it no longer bothers to move.

^ rewrite this paragraph as a two-line poem

## 4 · Take it back, and talk back

Every change Ambition makes is kept in a private history of this folder, so
nothing is final. This takes back the agent's last change in the folder:

^/undo

Now ask for an answer instead of an edit:

^ in one line: what is the strangest thing about how this file works?

To keep an answer, delete its ^> before your next press here. To answer it,
write a new prompt under it and press on that.

## 5 · Above, below, around

The caret is where you are looking. A press sends your line, the 8 lines
above it and the 8 below, and what changed in this file since its last
turn. The agent reads your words against what is around them.

^ fill in this table with three things to pack for a rainy day
| Thing | Why |
|-------|-----|
|       |     |
|       |     |
|       |     |

## 6 · One long memory

This file has its own thread, so it remembers across presses and days.
Other files have threads of their own.

^ remember the word "lantern" for me. I'll ask for it later.

^/thread

Open that link in T3 Code: you will see the exact message each press sent,
and every step the agent took. You can type in that thread too.

## 7 · Choose the mind

^/model

The list comes from T3, as it is right now. Switch by name, with an effort
if you like: ^/model haiku for speed, ^/model opus high for depth. This
file's thread keeps its memory across switches, even into another
provider's model, and new threads start on the model you chose last.

## 8 · Write your own command

A command is any script in ~/.ambition/commands/, and it works in every
folder. Its first comment line is its description, the paragraph above it
arrives on stdin, and whatever it prints becomes the answer. You do not have
to write it yourself:

^ write me a command called shout that prints the paragraph above in capitals

Then try it under any paragraph with ^/shout. If a shell can do it, it can
be a command.

## 9 · Past prose

The agent has a real shell, git and the web. It acts for real, and ^/undo
only covers files in this folder, so ask for what you want done. Carets work
in code files too, after a comment marker: # ^ in Python, // ^ in JavaScript.

^ write uptime_haiku.py, which turns this computer's uptime into a haiku, then run it and give me its output

## 10 · Did it remember, and who else is there?

^ what word did I ask you to remember?

Every file's thread can reach one more, the coordinator, which knows the
others. You can talk to it yourself in ~/.ambition/coordinator.md.

^ ask the coordinator which other files have threads, and tell me here

Prompts to steal for your own files: "argue with this", "what is missing
here?", "continue in the same voice", "make this a checklist", "explain this
code like I wrote it at 3am", "find the bug", "turn this into a script".
