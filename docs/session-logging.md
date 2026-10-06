# Local session logging

Available in 2.6.0 source builds. Published 2.5.0 provides raw received-byte logging and status reporting; the additions below await packaged publication.

## Enable logging for a profile

On the sidebar's **Advanced** page, set **Log session to** or use Browse. Expand **Logging options** to choose the format, rotation and **Log passwords**. Save the session profile to retain these choices, then open a session from that profile to apply them. Reconnect retries with the logging settings already attached to that tab. An empty path disables logging. Each profile controls its own log; no upload or cloud service is used.

**Automatic log filename** fills a path in Snekkie's configuration directory, under `logs`, using `{host}-{date}-{session}.log`. `{host}` is the hostname/IP or serial device, with unsafe filename characters replaced. `{date}` uses the local connection date; `{session}` includes the process and session ID so simultaneous tabs get different files. These substitutions also work in a path you enter. Existing files are appended.

## Formats and timestamps

- **Text log (input + output)** is the default for new profiles. It records sent commands as **TX** and received output as **RX**, removes terminal ANSI control sequences, and decodes UTF-8 across transport chunks. Line timestamps use UTC ISO 8601 and identify when the first bytes of that line were submitted/received. Completed lines are written promptly; partial lines are written when the session closes. Control bytes in input are represented with labels such as `<0x03>` for Ctrl+C. Local echo is not recorded a second time as output.
- **Raw output + input log** retains received bytes in the selected file and writes readable TX records in a companion file ending in `.input.log`. With password logging off, ordinary output is preserved byte-for-byte, while recognized credential lines and private echo are redacted. Incomplete raw lines are held until newline or close to prevent split-chunk redaction bypasses. With password logging on, raw output is written without that line filtering. Explicit Private input still suppresses it.

Older profiles retain their raw format and previous rotation behavior when loaded. Both directions are now recorded for a configured log, including a companion input file in raw mode. Logging preferences are separate from syntax colors, terminal themes, clipboard selections and text exports.

## Passwords and Private input

**Log passwords defaults off**, including for old or malformed profiles. Snekkie recognizes common password, passphrase, passcode, secret and PIN prompts. It hides sensitive input and the corresponding device echo from the logs while leaving transport and terminal display behavior intact. Recognized credential-bearing command/output lines using `password`, `passwd`, `passphrase`, `secret` or `community` are also redacted.

Use the status bar's **Private input** switch for unusual prompts or other sensitive work. It suppresses both input and output logging until switched off, and masks a pending echo through its next newline. This switch overrides **Log passwords**, applies only to the current session, and resets on reconnect. **Password input protected** indicates a recognized sensitive prompt or active private mode.

If you explicitly enable **Log passwords**, passwords, SSH authentication passwords and key passphrases may be written locally in plain text. The active session displays **Password logging ON**. The checkbox is saved locally, but profile exports and imports turn password logging off so transfer cannot enable it on another installation.

Prompt recognition and keyword redaction cannot identify every secret or vendor syntax. Enable Private input before entering sensitive data at an unrecognized prompt. Logs and terminal text exports are different: Private input does not remove sensitive text already shown on screen or copied/exported.

## Rotation, failures and location

New profiles default to rotation at **10 MiB** and daily rotation. **0 MiB** disables size rotation; uncheck **Rotate daily** to disable date rotation. Text records are kept together, so one very large record can exceed the size threshold; raw output rotates at received-chunk boundaries when password logging is enabled. Rotation creates an archive with a UTC timestamp and sequence suffix, preserves the prior file before clearing the active log, and never overwrites an existing archive. Raw input companion files rotate independently. Archives are retained.

The status bar shows **Logging**, **Logging off** or **Logging failed**. Hover for the resolved path or error, or choose **Open log folder**. Logging runs on a background worker with a bounded queue. A write, flush, rotation or queue failure warns once and disables logging while the terminal and connection continue. Reconnect retries. Normal application exit drains pending records before completing.

Choose separate paths for different sessions. Automatic filenames do this for you.
