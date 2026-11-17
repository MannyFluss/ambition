# Ambition, in ten saves

Ambition turns any text file in the folder it watches into a conversation
with an agent. There is no chat box. You write a line, you save, and the
answer appears in the file, or the file itself changes. Behind it is one
long-running T3 Code thread with a full toolkit (shell, web, git, other
agents), and a small program that watches the folder.

How to play:

- Put this file inside the folder Ambition watches. Play in a copy, so this
  one stays clean.
- Each step has a caret line indented by two spaces. Indented carets never
  fire. To run a step, put the cursor just before its ^ and press backspace
  until the ^ touches the left edge, then save. One step per save.
- With the editor extension on, a line that reaches the left edge changes
  color: that is Ambition seeing it.

## 1 · Three marks

    ^ a prompt         asks the agent, about the text around it
    ^/help             a command: runs at once, no AI involved
    ^> an answer       the agent's reply; your next save clears it

That is all the syntax there is.

## 2 · An instant command

Arm it and save. It answers in about a second, because no model is involved.

  ^/help

## 3 · Your first prompt

Arm the line below and save. While the agent works, a ◌ status line with
your words stands where your prompt was.

The quick brown fox jumps over the lazy dog, who has seen this sentence so
many times that it no longer bothers to move.

  ^ rewrite this paragraph as a two-line poem

## 4 · Take it back, and talk back

Every change Ambition makes is kept in a private history of this folder, so
nothing is final. This takes back the agent's last change, whatever file it
was in:

  ^/undo

Now ask for an answer instead of an edit:

  ^ in one line: what is the strangest thing about how this file works?

To keep an answer, delete its ^> before your next save. To answer it, write
a new prompt under it.

## 5 · Above, below, around

The caret is where you are looking, and the agent reads your words against
what is just above it and just below it.

  ^ fill in this table with three things to pack for a rainy day
| Thing | Why |
|-------|-----|
|       |     |
|       |     |
|       |     |

## 6 · One long memory

All your files talk to the same thread, so it remembers across files and
days.

  ^ remember the word "lantern" for me. I'll ask for it later.

  ^/thread

Open that link in T3 Code: you will see the exact message each save sent,
and every step the agent took. You can type in that thread too.

## 7 · Choose the mind

  ^/model

The list comes from T3, as it is right now. Switch by name, with an effort
if you like: ^/model haiku for speed, ^/model opus high for depth. The
thread keeps its memory across switches, even into another provider's
model.

## 8 · Write your own command

A command is any script in .ambition/commands/. Its first comment line is
its description, the paragraph above it arrives on stdin, and whatever it
prints becomes the answer. You do not have to write it yourself:

  ^ write me a command called shout that prints the paragraph above in capitals

Then try it under any paragraph with ^/shout. If a shell can do it, it can
be a command.

## 9 · Past prose

The agent has a real shell, git and the web. It acts for real, and ^/undo
only covers files in this folder, so ask for what you want done. Carets work
in code files too, after a comment marker: # ^ in Python, // ^ in JavaScript.

  ^ write uptime_haiku.py, which turns this computer's uptime into a haiku, then run it and give me its output

## 10 · Did it remember?

  ^ what word did I ask you to remember?

Prompts to steal for your own files: "argue with this", "what is missing
here?", "continue in the same voice", "make this a checklist", "explain this
code like I wrote it at 3am", "find the bug", "turn this into a script".
