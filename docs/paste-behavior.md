# Paste preview, pacing and right-click behavior

Paste review, pacing and the right-click preference are available in Snekkie 2.6.0. Version 2.5.0 used PuTTY-style direct paste.

In **Settings → Preferences → General**, choose **Right-click pastes (PuTTY style)**:

| Setting | Plain right-click on terminal output | Ctrl+right-click |
| --- | --- | --- |
| Enabled (default) | Paste; multiline text opens review when enabled | Open the context menu |
| Disabled | Open the context menu; choose **Paste** to paste | Open the context menu |

Choose **OK** to apply and save the choice. It applies to all open and new tabs and survives restart. **Cancel** discards edits. Older settings files keep direct paste enabled.

Opening, navigating or dismissing the menu sends no terminal input and does not paste. Copy-on-select continues to work in either mode. **Ctrl+Shift+V** and **Edit → Paste** remain available in both modes, with the same line-ending handling.

## Review multiline paste

Snekkie 2.6.0 enables **Preview multiline paste** by default. Clipboard text containing CR or LF opens **Review paste**, regardless of whether it came from the keyboard, Edit menu, right-click or context-menu Paste. Single-line paste stays immediate. Plain Ctrl+V remains the device's control key; use **Ctrl+Shift+V** for clipboard paste.

Review the destination and line count, edit **Paste text**, and choose the delay between lines. **Send paste** submits the reviewed text. **Cancel** or Escape sends nothing. Enter in the editor only adds a newline. Switching tabs cannot redirect the reviewed paste; reconnecting or closing its destination invalidates it.

CRLF, LF and CR all become CR on the wire. Empty lines are retained. A last line without a newline is sent as text, without submitting it; press Enter at the device or add a final newline in the preview if submission is intended.

## Pace and stop

The default delay is **100 ms**; the preview accepts **0–5,000 ms**. Preferences → General saves **Paste delay between lines** and the preview checkbox for future pastes. Changing the delay in one preview only affects that paste. Canceling Preferences discards preference edits.

With a positive delay, the status bar shows the target and lines submitted, plus **Stop paste**. Stop discards remaining lines; lines already submitted to the transport cannot be recalled. Progress reports submission, not acknowledgment from the device. Delayed frames do not catch up by sending several lines at once.

Typing and mouse input on the destination are blocked during its queued paste so commands cannot interleave. Other tabs remain usable. Queues pause while a dialog is open. Disconnect, reconnect, closing the tab or quitting discards unsent lines; nothing is replayed into a replacement connection. One queue runs at a time.

**0 ms** sends the entire text immediately, without a stoppable queue. Turning off multiline preview restores immediate clipboard paste and bypasses pacing. [Command snippets](command-snippets.md) use the same controller and saved delay after their own review, while still adding a final CR to submit the last command. Bracketed-paste support for terminal applications remains separate work.
