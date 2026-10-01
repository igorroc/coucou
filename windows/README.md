<div align="center">

<img src="src-tauri/icons/128x128.png" width="96" alt="Navi Assistant icon">

# Navi Assistant for Windows

**Navi doesn't get a notch on a PC — so it lives at the top of your screen instead.**

Approve Claude Code permissions, watch your session work, drop a file, chat with Claude, keep an eye on your services — without leaving what you're doing.

![Windows 10/11](https://img.shields.io/badge/Windows-10%2F11-0078D4?logo=windows)
![Tauri 2](https://img.shields.io/badge/Tauri-2-FFC131?logo=tauri&logoColor=black)
![Rust](https://img.shields.io/badge/Rust-backend-000?logo=rust)
![License: MIT](https://img.shields.io/badge/license-MIT-green)

</div>

<img src="screenshots/greeting.png" width="640" alt="Navi waving hello at launch">

---

## Install

The downloadable installer is **temporarily unavailable**. Microsoft Defender
wrongly flags the unsigned installer as malware (`Trojan:Win32/Wacatac.H!ml`, a
machine-learning false positive). A report is under review at Microsoft, and the
installer will be published again once it is cleared and code-signed.

Until then, [build it yourself](#build-it-yourself): it takes a few minutes and
installs for the current user only — no admin prompt.

## Using it

<img src="screenshots/compact.png" width="292" alt="The compact island, with the integration pills as mini Navis">
<img src="screenshots/overview.png" width="640" alt="The overview: the focused integration on the left, the other pills on the right">
<img src="screenshots/approval.png" width="640" alt="A Claude Code permission request, with Deny and Allow">
<img src="screenshots/chat.png" width="640" alt="Chatting with Claude from the island">
<img src="screenshots/drop.png" width="640" alt="Navi turned into a box, waiting for a file">

| What you do | What happens |
|---|---|
| Move the mouse to the very top-centre of the screen | Navi peeks out |
| Click the small island | It opens |
| Click Navi | It gets annoyed. Three times in a row and it goes dizzy |
| Rest the pointer on Navi for two seconds | Hearts |
| Drag a file onto the island, or open the + tab and click the drop zone to browse | Navi turns into a box, swallows it, then offers to answer questions about it |
| Right-click the island | Context menu: Minimize, or Close (hides the island, keeps Navi Assistant in the tray) |
| `Esc` | Closes the island |
| Tray icon | Open, Settings…, Pause, Quit |

Everything else happens on its own: a Claude Code permission request opens the
island with **Deny / Allow**, a finished session shows what it did, and
your integrations sit in the coloured pills next to Navi.

## Claude Code

<img src="screenshots/settings.png" width="562" alt="The settings window">

Open **Settings… → Claude Code → Install hooks…**. You get the exact diff of what
will change in `%USERPROFILE%\.claude\settings.json`, the path of the dated backup
that will be taken, and nothing is written until you click. Your own hooks are
never touched, and uninstalling removes only Navi Assistant's entries.

The relay is a tiny executable, `navi-assistant-hook.exe`, copied to
`%LOCALAPPDATA%\Navi Assistant\bin\` at launch. It is given 300 ms to reach Navi Assistant and
exits cleanly if the app is closed, slow or crashed — **a Claude Code session is
never blocked or slowed down by Navi Assistant.** If nobody answers a permission request
in time, Navi Assistant stays quiet and Claude Code asks in the terminal as usual.

It works from any terminal — Windows Terminal, PowerShell, VS Code, Git Bash.

## opencode

Open **Settings… → opencode → Install plugin…**. This copies `navi-assistant.js` (in
`opencode-plugin/` next to the source) to
`%USERPROFILE%\.config\opencode\plugins\navi-assistant.js` — opencode auto-loads
global plugins, so no `opencode.json` edit is needed. You get a preview of
what will change, a dated backup of any previous copy, and nothing is written
until you click. Uninstall removes only Navi Assistant's file.

The plugin forwards session, tool and permission events through the same
`navi-assistant-hook.exe` relay and named pipe Claude Code uses, so the island shows
opencode sessions in their own pill with the same **Deny / Allow** approval
card. The same guarantee holds: **an opencode session is never blocked by
Navi Assistant** — if nobody answers in time, opencode asks in the TUI as usual.
Restart opencode after installing so it picks the plugin up.

## Chat and keys

**Settings… → Claude** takes your Anthropic API key. Keys live in the **Windows
Credential Manager**, never on disk and never in the interface — the island can
only ask whether a key exists. Same for every integration key.

When the chat runs through your local **opencode**, its conversations live in
opencode's own store, and the chat view shows them in a list beside the
conversation — grouped by project, newest first. Clicking one reopens it and the
next message continues that same opencode session; the pencil button starts a
new one.

Conversations started from the notch are kept in their own folder,
`%LOCALAPPDATA%\Navi Assistant\chat`, so they never mix with your repositories: Navi Assistant
sets it up as its own git worktree the first time, which is what tells opencode
to file them under a separate "Navi" project. A file dropped on the island is
attached to the message but the session still lands in that folder.

That folder also gets its own `opencode.json`, provisioned at launch with the
MCP servers the notch needs — currently **Jira (Atlassian)** and **Intercom**.
Navi Assistant only ever adds those entries: any other key or server you put in that
file is left untouched. Remote MCPs use OAuth, so the first use may ask you to
sign in — run `opencode` inside the folder once to complete the browser login.

No telemetry. The only network requests Navi Assistant makes are to the services you
configure yourself.

## Build it yourself

You need [Rust](https://rustup.rs), [Node 20+](https://nodejs.org), and the
**MSVC build tools** (Visual Studio Build Tools with "Desktop development with
C++"). WebView2 ships with Windows 10/11.

```powershell
cd windows
npm install
npm run tauri dev      # live-reloading development build
npm run pack           # builds the installer and drops it in windows/release/
```

`npm run dev` alone serves the front end in an ordinary browser, which is enough
to work on the island's looks. It also serves `dev/upload-preview.html`, which
replays the whole file-drop choreography on a loop — the one part of the UI that
otherwise needs a real drag from Explorer to see. Neither page ships in the app.

`npm run pack` leaves two files in `windows/release/`, the same names the release
workflow publishes:

```
Navi Assistant-Windows-X.Y.Z-setup.exe    the versioned installer
Navi Assistant-Windows-setup.exe          the same file under the rolling name
```

Installing is optional — `target/release/navi-assistant.exe` runs on its own. There is no
window in the taskbar and no console: the island at the top of the screen and the
Navi in the notification area are the whole app, and Quit lives in its menu.

The 28 sounds are the macOS app's own files; they are never duplicated in this
folder. The path is declared once, in `SOUNDS_DIR` at the top of
`vite.config.ts` — when they move to `shared/sounds/`, change that one line.

The app icon and the tray icon are drawn in code, like Navi itself:

```powershell
npm run icons          # regenerates src-tauri/icons from scripts/gen-icons.mjs
```

### Layout

```
windows/
  src/                 island front end (TypeScript, no framework)
    navi/             Navi and the launch greeting, in Canvas 2D
    island/            state machine, hooks, integrations
    views/             every island view
    settings/          the settings window
  src-tauri/           Rust backend: window, named pipe, Claude API, pollers
  hook/                navi-assistant-hook.exe, the Claude Code relay
  opencode-plugin/     navi-assistant.js, the opencode plugin (same relay, same pipe)
  scripts/             icon generator
```

### Log

`%LOCALAPPDATA%\Navi Assistant\navi-assistant.log` — hook events, permission decisions, poller
problems, and every chat turn sent through opencode. It stays on your machine.

When a chat fails, the log keeps the real reason: the opencode error event with
its provider/model/name, plus the stderr (or the raw output when a turn came back
empty). The question and outputs are clipped, so the log won't hold a full
transcript. The raw opencode log lives in
`%USERPROFILE%\.local\share\opencode\log\` if you need the provider's own view.

## What's different from the Mac version

- No notch, so the island lives at the top centre of the screen and retracts into
  the top edge instead of hiding in a notch.
- Permission approval works from **any** terminal; the Mac build only listens to
  VS Code sessions.
- Not in this version: sending a file by email, dragging Navi onto a window to
  attach it as context, and jumping to a specific terminal window — "Open
  terminal" opens the working folder in VS Code when `code` is on your `PATH`.
- Cal.com shows the next bookings as a list rather than the Mac's calendar.
