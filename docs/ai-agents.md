# AI agent access (MCP)

Snekkie can let an AI agent on your computer work in the sessions you have open. You connect to a router, switch or server as usual, then ask the agent what you want done: it reads the output and types commands through Snekkie, and you see everything it does in the tab.

This uses the [Model Context Protocol](https://modelcontextprotocol.io) (MCP), so it is not tied to one agent. Claude Code, Claude Desktop, OpenAI Codex and self-hosted agents, including ones running a local model, all work, as does any other agent that can start a local (stdio) MCP server. It is **off by default**.

## Turn it on

1. Open **Preferences → Network** and tick **Let AI agents on this computer use open sessions (MCP)**, then **Save changes**.
2. Add Snekkie to your agent once, as a local (stdio) MCP server. The same section shows the exact command line for this copy of Snekkie and has **Copy Claude Code command**, **Copy Codex command** and **Copy JSON config** buttons.

Every agent starts the same program with one argument:

```
"C:\Users\you\AppData\Local\Programs\Snekkie\snekkie.exe" --mcp
```

On Linux and macOS the command is the path to `snekkie` (or the AppImage) with `--mcp`; a Flatpak install uses `flatpak run <app id> --mcp`. Preferences fills in the right one for the copy you are running, wherever it is installed. If you later move a portable copy, add it to your agent again.

### The notification-area icon (Windows)

Snekkie has one icon in the notification area (under **Show hidden icons** beside the clock). Right-click it, or select it and press the menu key, for its menu.

While Snekkie is open, the menu has **Show Snekkie**, **Start AI agent server (MCP)** or **Stop AI agent server (MCP)**, **Close AI agent bridge** and **Quit Snekkie and AI agent bridge** (while an agent's bridge is running), and **Quit Snekkie**. Starting or stopping the server there changes the same Preferences setting. Both Quit entries ask first if sessions are still connected; answering No leaves Snekkie and the bridge running. Clicking the icon brings Snekkie forward.

The `snekkie --mcp` bridge an agent starts is a separate program with no window, and it keeps running after you quit Snekkie, until its agent closes it. So the icon stays while a bridge runs, with **Open Snekkie** and **Close AI agent bridge**. Clicking it opens Snekkie. **Close AI agent bridge** ends every agent's bridge. The agent then reports Snekkie's tools as disconnected; reconnect it from the agent (in Claude Code, `/mcp`) when you want it again.

**Claude Code:**

```
claude mcp add snekkie -- "C:\Users\you\AppData\Local\Programs\Snekkie\snekkie.exe" --mcp
```

**OpenAI Codex:**

```
codex mcp add snekkie -- "C:\Users\you\AppData\Local\Programs\Snekkie\snekkie.exe" --mcp
```

or add it to `~/.codex/config.toml`:

```toml
[mcp_servers.snekkie]
command = 'C:\Users\you\AppData\Local\Programs\Snekkie\snekkie.exe'
args = ["--mcp"]
```

**Claude Desktop and many other agents** use an `mcpServers` entry (for Claude Desktop, in `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "snekkie": {
      "command": "C:\\Users\\you\\AppData\\Local\\Programs\\Snekkie\\snekkie.exe",
      "args": ["--mcp"]
    }
  }
}
```

**Self-hosted and local-model agents:** add a stdio (command-line) MCP server with the program path as its command and `--mcp` as its argument. Many accept the JSON entry above. Snekkie has no HTTP endpoint, so an agent that can only reach MCP servers by URL needs its own stdio adapter. Snekkie itself sends nothing off your computer, but the agent passes what it reads to whichever model it uses, local or hosted.

The agent can be started before Snekkie. Until Snekkie is running with access on, its tool calls say so instead of failing to connect.

## What an agent can do

| Tool | What it does |
| --- | --- |
| `list_sessions` | Lists open tabs: name, type, destination, connected or not, and which tab you have selected |
| `read_output` | Reads the latest lines of a tab's screen and scrollback |
| `send_command` | Types one command line, presses Enter and returns the output once it settles at a prompt |
| `send_keys` | Sends exact keystrokes without Enter: a space at `--More--`, `y` at a confirmation, Ctrl+C |

Agents cannot open, close or reconnect sessions, and never receive the passwords you connect with: you connect, and the agent works in what is open. They can read whatever a tab shows, including configuration output. Once you turn access off or quit Snekkie, no further calls reach your sessions.

`send_command` returns when the output has been quiet briefly at something that looks like a prompt, after 3 seconds of quiet anywhere, or after its timeout (15 seconds by default, at most 120). Long-running output may continue after it returns; the agent can read the rest with `read_output`. If paging is on at the device, the agent sees `--More--` and can answer it, or you can turn on **Automatically page through show commands**.

## Seeing what an agent does

- **Connected:** while agent access is on, the left of the status bar shows **Agent access on**, or the connected agent by name, such as **● Claude Code connected**. With more than one, it shows how many; hover to list them. An agent counts as connected from when it starts until it exits, and reappears by itself after Snekkie restarts or access is turned back on.
- **Reading:** the session's tab and the terminal glow cyan, and a soft band of light sweeps down the output. The status bar says, for example, **Claude Code is reading edge-rtr**.
- **Typing:** the tab and the terminal glow amber and pulse once for each command, and three dots move under the tab's name. The status bar says, for example, **Claude Code is typing in edge-rtr**. After `send_command` types, the cue turns cyan while the agent reads the result.

The tab glows even when you are looking at another tab, so you can see which session an agent is in. The terminal glow shows for the tab you have selected. Cues fade a second or two after the agent stops.

**Animate agent activity** is on by default and separate from the Animations switch. It appears under **Preferences → Network** and again under **Preferences → Animations → AI agent activity**; both are the same setting. Turn it off for steady colored outlines with no sweep or pulse; the status bar messages stay the same.

## Safety

- Commands run on real equipment, exactly as if you typed them. Review what the agent proposes, especially in configuration mode, and use your agent's own approval settings for tool calls.
- Everything an agent sends goes through the same path as typing: it appears in the tab, and session logging records it as input, with the usual password redaction and Private input.
- Snekkie listens only on the loopback interface (127.0.0.1), on a random port, and requires a token created fresh each time access is turned on. The port and token are kept in `mcp.json` in Snekkie's settings folder, which other accounts cannot read (administrators aside). Other computers, other users and web pages cannot connect.
- Offline mode does not affect agent access: it controls outgoing connections, and agent access makes none.

If more than one copy of Snekkie is running with access on, agents reach the one that turned it on most recently.
