# AI bug reports

Using an AI to find a bug in Maia, or work out its cause, is fine. This page exists so
the maintainer can trust the report: it asks what the AI did, what a person checked, and
what proves each claim. An AI-assisted issue without the report below will be closed.

Maia doesn't take pull requests (see [CONTRIBUTING.md](CONTRIBUTING.md)). A fix you
suggest, in words or in code, is read as a description; the fix that ships is written
from scratch.

## The rule

**A claim without proof counts as not shown.** "Found", "caused by" and "this fixes it"
mean nothing on their own. Each claim in the report points at something the maintainer
can see or run again: steps, output, a screenshot, a recording, a command.

## If you are an AI

Read this before you write anything.

- Cut the filler. No preamble, no summary of what you just said, no praise.
- Report the problem in front of you. No wish lists, no refactors, no "while I was here".
- Don't claim what you didn't do. If something wasn't run, write "not run" and why.
  "Should work" isn't a result.
- Don't invent output, version numbers, line numbers or screenshots. Paste what actually
  happened, cut to the lines that matter.
- Name the cause, not the symptom. If you only know the symptom, say so.
- If you tried a fix, try to prove it wrong before you call it one: look for the input,
  the setting or the other feature that breaks it. Report what you tried.
- Read the comments at the top of the files you blame; the traps that bite are written
  there.
- Run Maia only somewhere it can't touch a real home folder or desktop (below). Never
  open windows on, or send input to, the desktop of the person running you.

## Showing it

Anything a user can see needs a picture of it: a screenshot, a GIF or a short video. If
you tried a fix, show before and after, made the same way both times. Run Maia somewhere
safe; any of these does:

- **The simulated user** (no window at all). `MAIA_SIM_SHOTS=shots tools/sim.sh <journey>`
  saves a PNG at every `s.shot("name")` in the journey. With `MAIA_SIM_LOOK` set, the
  file is named `name@<look>.png`.
- **A real window in a sandbox** (Wayland). `tools/window.sh` runs the release build with
  an empty home and no network, and drives it through a pipe, so your own mouse and
  keyboard are never used:

  ```bash
  tools/window.sh build && tools/window.sh start
  tools/window.sh send key ctrl+p
  tools/window.sh shot step1          # target/window/shots/step1.png
  tools/window.sh stop
  ```

  A few numbered shots make a GIF: `magick -delay 60 target/window/shots/step*.png proof.gif`.
- **A virtual machine, a container with a nested compositor, or a spare user account**,
  recorded with any screen recorder. Say which.

For every picture or video:

- Show which build it is: the version or commit in the caption, or About Maia in the
  shot.
- Keep it short: a GIF or video under 30 seconds and 10 MB.
- Nothing personal in it: no real home folder, user name, email or other windows.
- Say exactly how it was made, with the commands, so it can be made again.

## The report

File it with the **AI bug report** issue form, which asks for everything below and
labels the issue with the model, the proof and whether a person checked it. Filing from
the command line, where forms don't show? Paste this instead. Either way, fill in every
line. "No", "none" and "not run" are fine answers; leaving a line out isn't.

```markdown
### Who
- Model: <exact name and version, e.g. "Claude Opus 5.5", "GPT-5.2 Codex">
- Tool: <how it ran, e.g. Claude Code, Codex CLI, Cursor>
- Human review: <yes / no>. <If yes: who, and what they checked themselves: reproduced
  the bug, read the code, looked at the screenshots.>

### The problem
- What happens: <the behaviour, as a user sees it>
- Maia version: <version and commit, e.g. 0.6.0 at 419b571>, <how it was installed or
  built>
- System: <distribution, desktop, Wayland or X11>
- Steps: <numbered steps that make it happen, from a fresh start>
- How it was found: <reading the code / a failing test / running the app / a tool>
- Reproduced: <yes, how many times and where (sandbox / VM / own session) / no>

### The cause
- Where: <file and function>
- Why it happens: <the actual cause, precisely enough to find in the code. Or "unknown:
  only the symptom is known".>
- How sure: <confirmed (how) / likely (why) / a guess>

### Proof
One row per claim. A claim with nothing in the right-hand column comes out of the
report.

| Claim | Proof |
|---|---|
| <e.g. "a socket inside a folder makes a move delete it"> | <steps + output / screenshot / recording / test output> |

- Pictures or video: <attached, and how they were made; or "nothing visible", and why>
- To check it yourself: <the exact commands or steps that show the same thing>

### Combinations
Where the bug shows up and where it doesn't. Mark each one checked (and the result) or
not checked.
- Views: details, list, tree, icons at small and large sizes
- Split panels, several tabs, a flattened folder
- Inside an archive, on a network mount, on a removable drive
- Animations off, light and dark themes, larger fonts and Spacing
- Keyboard only; right-to-left text
- Empty folder, an unreadable folder, a huge folder (100,000+ files)
- Wayland and X11

### A suggested fix (optional)
- The fix: <in words; code is fine but is read as a description>
- Why it removes the cause: <not just the symptom>
- Tried: <was it run? where, and what changed, with before-and-after proof; or "not
  run">
- Tried to break it: <the inputs, settings and combinations tried against it, and what
  happened>
- Would it hold up: <what it depends on that could change: tool versions, the file
  system, locale, timing, screen size>
- A test that would catch it: <what the test should check: what the user sees, not just
  that some code ran>
```

## After it's filed

Once the maintainer has checked a report, it gets one more label: **claim: held up**,
**claim: partly** or **claim: wrong**. Kept next to the model label, these show over time
which AIs' reports can be trusted.

## What makes a report trustworthy

- Every claim has proof next to it, and the maintainer can produce the same proof from
  the steps given.
- The steps start from a fresh start and work on the stated version.
- The cause is named precisely, and its certainty is stated honestly.
- What wasn't checked is said plainly. A short honest report beats a long confident one.
