# Snekkie

A tabbed SSH and serial terminal in Python — a PuTTY replacement you can
actually read the source of.

> Snekkie was called **pyterm** until recently. Saved sessions and settings
> from before the rename are copied over automatically the first time it
> starts: config now lives in `%APPDATA%\snekkie` (`~/.config/snekkie` on
> Linux), and the old directory is left untouched as a fallback.

## Install

Python 3.10 or newer.

### Windows

Two ways to run it:

- **From source:** double-click **`run-windows.bat`**. First run builds the
  virtual environment and installs dependencies (~150 MB, mostly PySide6);
  every run after that just launches the app. Requires Python.
- **Standalone .exe:** double-click **`build-windows.bat`** once to produce
  `dist\snekkie.exe` (also needs Python, just for the build step). After
  that, `snekkie.exe` runs on its own — no Python required, safe to pin to
  the taskbar or copy to another machine. Re-run `build-windows.bat` after
  pulling changes to refresh it.

Manual way, in PowerShell from the project folder:

```powershell
python -m venv .venv
.venv\Scripts\Activate.ps1
pip install -r requirements.txt
python -m snekkie
```

If `Activate.ps1` is blocked by the execution policy, either unblock it for
that one shell:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
```

or skip PowerShell and use `cmd`, where `.venv\Scripts\activate.bat` has no
such restriction.

**Serial ports:** no permissions setup needed, but your USB console cable
needs a driver. Check Device Manager ▸ *Ports (COM & LPT)* — if the adapter
shows a yellow warning triangle, install the FTDI, Prolific, or Cisco driver
for it first. The Refresh button in the Serial tab lists whatever Windows
currently sees. COM10 and above work fine.

**SSH agent:** paramiko talks to Pageant, so keys you already have loaded in
PuTTY's agent work if you pick *SSH agent / default keys* as the auth method.

### Linux / macOS

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
python -m snekkie
```

On Linux you'll need to be in the `dialout` group to open serial ports
(`sudo usermod -aG dialout $USER`, then log out and back in).

## What works today

- **One window** — a new-session panel sits permanently on the left; no
  popup dialog to dismiss before you can see the terminal
- **SSH** — password, private key, or agent auth; host key verification
  against `~/.ssh/known_hosts` with a fingerprint prompt for unknown hosts
- **Serial** — port auto-detection, full 5–8 / N-E-O-M-S / 1-1.5-2 control,
  RTS/CTS and XON/XOFF, and **send break** (Cisco password recovery)
- **Tabs** — multiple sessions, movable, duplicate, reconnect in place
- **Saved sessions** — JSON profiles, PuTTY-style load/save/delete
- **Real VT100/ANSI emulation** — 16/256/truecolour, bold, underline,
  reverse, scroll regions, cursor addressing. `nano` and `htop` behave.
- **Scrollback** — scrollbar, mouse wheel (three lines a notch, and
  trackpad-sized fractions of a notch accumulate rather than being rounded
  away), or Shift+PgUp/PgDn for whole pages
- **PuTTY mouse habits** — selecting copies, right-click pastes
- **Selection spans the scrollback** — drag past the top or bottom edge and
  the view auto-scrolls, the way it does in a browser, so a `show run`
  longer than the window can be selected in one go. Copies reach lines that
  have scrolled off entirely, and `Ctrl+Shift+A` takes the whole buffer.
- **Tab reaches the far end** — it completes commands instead of moving
  focus, and Shift+Tab sends CSI Z
- **Session logging** — raw byte log to a file per profile
- **Preferences** (`Ctrl+,`) — colour theme, default font and size for new
  sessions, and default scrollback. Sidebar can be hidden with `Ctrl+B`.
- **Saved themes** — build a scheme with the custom
  foreground/background/cursor/selection pickers, *Save as…* it under a name,
  and it joins the six built-in presets in the theme list. Saved themes live
  in `settings.json` and can be deleted from the same dialog; the built-in
  presets can't be overwritten.
- **Device syntax highlighting** — pick a device type in the sidebar
  (currently just Cisco IOS, or None) and its keywords, IP addresses, `no`
  negations, and prompt line get coloured wherever they appear on screen —
  what you type and what the device sends back both go through the buffer
  the same way, so both light up.

Passwords are deliberately never written to disk.

## Layout

```
snekkie/
├── profiles.py            saved sessions (JSON, no secrets)
├── settings.py            app-wide preferences (theme, default font)
├── emulation.py           pyte wrapper — the screen model
├── transport/
│   ├── __init__.py        Transport ABC + registry
│   ├── ssh.py             paramiko
│   └── serialport.py      pyserial
└── ui/
    ├── keys.py            Qt key event → xterm byte sequence
    ├── terminal.py        renders the screen, collects input
    ├── session.py         one tab: transport + reader thread + widget
    ├── dialogs.py         SSH/Serial/Advanced setting forms
    ├── sidebar.py         new-session panel + saved-session list
    ├── preferences.py     theme/font preferences dialog
    └── window.py          splitter, tabs, menus, status bar
```

Three layers, and they only touch each other through narrow interfaces:

1. **Transport** — anything that produces a byte stream. Doesn't know a
   terminal exists.
2. **Emulation** — bytes in, screen buffer out. Doesn't know Qt exists.
3. **UI** — draws the buffer, sends keystrokes. Doesn't parse escape codes.

### Threading

Each session runs a `ReaderThread` that blocks on `transport.read()` and
emits bytes via a Qt signal. Signals queue across threads, so pyte and the
widget are only ever touched from the GUI thread — pyte is not thread-safe.

Rendering is throttled to one repaint per 25 ms, and repaints only the rows
pyte reports as actually changed (`Screen.dirty`) plus wherever the cursor
was and now is — typing a character invalidates one line, not the whole
viewport. Without the interval throttle a `show run` dump would trigger a
repaint per packet and the UI would crawl; without the dirty-row tracking
every keystroke would cost a full-screen redraw once the screen filled up.

Dirty-row tracking alone doesn't help a *full* screen, though: once there is
history, every new line scrolls, so every row's text changes and pyte marks
the whole screen dirty. Redrawing all of it per line is what made a
maximised window crawl — 37 ms a line at 1920x1080. So `_try_blit_scroll`
copies the screen's pixels up with `QWidget.scroll()` and repaints only the
newly exposed strip. It works out the shift by checking the current buffer
against a per-row record of what is actually drawn: if every surviving row
matches the row *n* below where it used to be, the copy is provably right.
Anything that breaks the match — an in-place edit, a row still waiting to be
painted, a resize — falls back to repainting. That turned a 72-row repaint
into a 2-row one, about 6x faster.

`paintEvent` is the other hot path, so it avoids per-run allocation: fonts
are cached by (bold, italic, underline), resolved `QColor`s are memoised,
each row's text is built once and sliced per run, blank cells are fetched
with `line.get` to skip pyte's `__missing__`, and the syntax-highlight pass
is cached by row *text* — keyed that way so a line keeps its colours when it
scrolls up a row, which a row-indexed cache would miss on every line of a
dump.

`tests/test_render.py` guards all of this by comparing incrementally painted
pixels against a full redraw, and separately asserts the blit actually
engages: a broken shift check still renders correctly, just slowly, so the
pixel tests alone would not catch it.

### Selection coordinates

pyte keeps the document in three pieces: `history.top` (scrolled off above),
`buffer` (the visible rows), and `history.bottom` (below, when scrolled
back). Concatenated they are the document, and `Terminal.document_rows()`
addresses it by absolute row.

Selection endpoints are stored in those absolute coordinates, not screen
rows. It has to be that way for dragging: the view scrolls out from under
the drag, so a screen row number stops meaning anything mid-gesture, and the
copy needs to reach lines that are no longer displayed. `paintEvent` maps
back the other way, treating the screen as a window starting at `view_top`.

### Garbage collection

Scrollback is a lot of small long-lived objects — one dict per row plus a
`Char` per written cell, so 5000 lines is around 370k of them. A
generation-2 pass walks all of it on the GUI thread, which freezes the whole
app, tab bar and menus included, for ~90 ms on a filled 1920x1080 window.
That is the "everything gets sluggish once there's history" symptom, and no
amount of render tuning touches it.

`tune_gc()` in `__main__.py` freezes what is alive at startup (Qt's widget
tree lives for the life of the process anyway) and makes full passes rare.
Cycles are still collected, just later. Scrollback itself is not cyclic, so
refcounting reclaims it either way and the deferral costs no real memory —
object counts come out the same. Worst-case stall drops about fourfold,
matching what disabling the collector achieves without leaking cycles.

Related: `Terminal.scroll_by()` never asks pyte to travel more than a
screenful per call. pyte swaps rows between screen and history assuming the
distance fits on screen, and a larger step makes it write buffer rows below
the last line, which stay there for good — a single long scrollbar drag used
to leave 550+ of them behind, feeding straight back into the problem above.

## Adding things

**A new transport (telnet, raw TCP, local shell):** write one file in
`transport/`, subclass `Transport`, decorate with `@register("telnet",
"Telnet")`, and add it to `_load()`. Add the kind to the sidebar's combo box.
Nothing else changes.

**Colour schemes:** the four theme colours (foreground/background/cursor/
selection) live in `settings.py`'s `THEMES` dict and are app-wide, set via
Preferences; anything the user saves goes in `AppSettings.saved_themes`
instead, so adding a preset later can never clobber one of theirs. The
16-colour ANSI palette used for SGR codes is still the fixed `PALETTE` dict
at the top of `ui/terminal.py` — move it into `AppSettings` too if you want
that themeable as well.

**Scroll granularity:** pyte only exposes half-screen paging, so
`Terminal.scroll_by()` borrows `history.ratio` for the duration of one
`prev_page`/`next_page` call — the distance those move is just
`ceil(screen.lines * ratio)`. That buys exact line-at-a-time scrolling for
the wheel and a single-hop seek for a scrollbar drag.

**A new device syntax:** add an entry to `SYNTAXES` in `ui/highlight.py` —
a list of `(regex, category)` pairs, where `category` is a key in `COLORS`
— and a label in `SYNTAX_LABELS`. It shows up in the sidebar's Device combo
automatically. Regexes run against each visible line's plain text, so
keep them simple; they don't see anything pyte's screen model already
threw away (like which bytes came from you vs. the far end).

**Keyboard tweaks:** `ui/keys.py` is a lookup table. If you hit an old box
where Backspace misbehaves, flip `Qt.Key_Backspace` to `b"\x08"`.

**Saved passwords:** hook the `keyring` package into
`MainWindow._credentials_for()` — never into `profiles.py`.

## Known gaps (roughly in the order I'd fix them)

1. **Connect blocks the GUI thread.** Up to 12 s on an unreachable host.
   Move `transport.connect()` into a worker and marshal the host-key and
   password prompts back with a queued signal.
2. **No X11 or port forwarding.** paramiko supports both; it's plumbing.
3. **No search in scrollback.** pyte gives you the text, so this is a
   dialog plus a highlight pass in `paintEvent`.
4. **No mouse reporting** (modes 1000/1002/1006), so clicking in `vim`
   doesn't position the cursor.
5. **`HistoryScreen` scrollback is a little quirky** on resize — a known
   pyte rough edge, and the main reason a bigger project would eventually
   swap the emulator out.
6. **No SFTP browser, no scripting/expect.** Both are natural next tabs.

## Shipping it

```bash
pip install pyinstaller
pyinstaller --noconsole --onefile --name snekkie launcher.py
```

(`launcher.py` at the repo root, not `snekkie/__main__.py` — PyInstaller runs
the entry script as a bare top-level module with no parent package, which
breaks the package's relative imports if you point it at `__main__.py`
directly.)

On Windows that produces `dist\snekkie.exe`, which runs on machines with no
Python installed. Note that one-file PyInstaller builds are a common
antivirus false positive — if Defender quarantines it, drop `--onefile` and
ship the `dist\snekkie\` folder instead.

```
```

## Development

```bash
pip install -e ".[dev]"
pytest
ruff check snekkie tests
```

The test suite covers the emulation layer (escape sequences, colour, resize,
scrollback, split UTF-8), key encoding, profile persistence, the transport
registry, the renderer (incremental repaint correctness, highlight caching),
and an end-to-end session over a real pty. The pty tests skip on Windows. CI
runs everything on Windows and Linux across Python 3.10 and 3.13.

Tagging a release as `vX.Y.Z` and pushing the tag builds a Windows .exe as a
workflow artifact.
