# File transfers

Snekkie moves files two ways: quick transfers from a session tab, and a two-pane file explorer. Both use your SSH login and host-key check, and both follow Offline mode.

## Quick transfers

Right-click a connected SSH tab and choose **Upload file…** or **Download file…**. Snekkie tries SFTP first, then SCP if the device doesn't offer SFTP. Progress and Cancel appear in the status bar.

## File explorer

Open it either way:

- Choose **SFTP** as the session type in the sidebar and connect. It takes the same login as SSH and can be saved like any other session.
- Right-click a connected SSH tab and choose **Open file explorer**. You don't log in again. The connection stays open until both tabs are closed, so you can close either one first.

To see the terminal and its files together, choose **Open file explorer beside** instead. See [Side by side](#side-by-side).

Your computer is on the left, the server on the right and the transfer queue along the bottom. Each opens in its own tab. A saved session reopens at the folders you last used.

| To do this | Do this |
| --- | --- |
| Transfer | Drag files between panes, drop files from your file manager onto the server pane, use **Upload** or **Download**, or double-click a file |
| Open a folder | Double-click it. **Backspace** goes up a level |
| Select | Click, Ctrl+click, Shift+click or **Ctrl+A** |
| Manage server files | **New folder**, **Rename** (F2), **Delete** (Delete) and **Permissions** in the toolbar or the right-click menu |
| Refresh | **F5** |

### The queue

Three files transfer at once and the rest wait. Folders keep their structure. If a file already exists, Snekkie asks whether to **Overwrite**, **Skip** or **Keep both**. Tick **Do the same for the rest of this batch** to answer once.

A partly uploaded file never replaces the original: it's written under a temporary name and swapped in when complete.

### Editing a server file

Choose **Edit** to open a private copy in your default app for that file type. To use a particular editor or IDE instead, open **Preferences** > **Files** and choose its program under **Open with**. Each save uploads automatically. If the file changed on the server since you opened it, Snekkie asks before replacing it. Closing the tab deletes the copies, and asks first if any are still open. Snekkie won't open a file that would run as a program.

### Links and safety

- Links are marked and can be opened, but folder transfers and deletes don't follow them. Deleting a link removes the link, not what it points to.
- Delete always shows how many files and folders it will remove, and Enter won't confirm it.
- Names from the server that could write outside the chosen folder, or that Windows can't store, are skipped and shown in the queue.
- AI agents connected through [AI agent access](ai-agents.md) can't use the explorer.

## Side by side

Up to eight session or explorer tabs can share the window: four columns across, each holding one tab or two stacked.

- **Add a tab:** drag it down out of the tab strip and over the workspace. The part it would fill lights up, so drop near a pane's left or right edge for a new column, or its top or bottom edge to stack in that column.
- **Replace or swap:** drop in the middle of a pane to take its place. A tab already showing trades places with that pane instead. If a column or row is full, any drop on that pane replaces it.
- **Other ways to add:** right-click a tab and choose **Show side by side with**, or use **Open file explorer beside** on an SSH tab. New tabs go to the right, then underneath.
- **Type in one:** click a pane to give it the keyboard. A line along its top marks the active pane, and keys, paste and shortcuts go only there.
- **Resize:** drag the dividers. Each column sets its own stack height.
- **Other tabs:** selecting a tab that isn't showing shows it alone. Select any tab of the group to bring them all back.
- **Finish:** right-click a tab and choose **Take out of side by side** for that tab, or **End side by side** for all of them. **Swap sides** reverses the columns. Closing a tab lets the ones beside it fill the room.
- **Reorder:** dragging along the tab strip only reorders the tabs.

Files dropped from your file manager go to the active explorer, or else the first one showing. If the window is too small for the whole layout, it shows the active tab until there's room. Each pane needs about 160 points of width and 100 of height.

## Devices without SFTP

Many network devices offer only SCP. The explorer can't browse those, because SCP has no way to list folders. It says so and points you to the tab's right-click transfers.

## Not included

Dragging files out of Snekkie into your file manager, resuming interrupted transfers, folder sync and bookmarks.
