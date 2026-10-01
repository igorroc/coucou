// navi-assistant.js — Navi Assistant plugin for opencode.
//
// Forwards opencode session / prompt / tool events to Navi Assistant over the
// named pipe `\\.\pipe\navi-assistant-<sid>` via `navi-assistant-hook.exe`, the same relay
// Claude Code hooks use. Payloads reuse the Claude Code hook shape
// (`hook_event_name`, `session_id`, `cwd`, `tool_name`, `tool_input`, …)
// plus `"agent": "opencode"`, so the island routes them to the opencode pill.
//
// opencode exposes two kinds of extension points, and they are NOT
// interchangeable (opencode 1.18.x):
//   * named hooks — `chat.message`, `tool.execute.before`, `tool.execute.after`;
//   * the generic `event` bus — `session.created`, `session.idle`,
//     `session.error`, `session.deleted`, `permission.asked`, …
// Registering a bus event name as if it were a hook silently does nothing, so
// every event below goes through `event` and only real hooks are keyed directly.
//
// Permissions are deliberately NOT approved from here. `opencode run` is
// non-interactive: any permission that resolves to "ask" (notably
// `external_directory`, which defaults to ask) is auto-rejected on the spot,
// before a plugin can answer. Navi grants the directories it needs through the
// chat `opencode.json` `permission.external_directory` block instead.
//
// Hard rule (same as nb-hook / navi-assistant-hook): **never block opencode.**
//   * Every relay is fire-and-forget with a 2 s budget and is abandoned after.
//     No answer — Navi Assistant closed, timeout, relay missing — leaves
//     opencode completely unblocked.
//
// Install: copied to `~/.config/opencode/plugins/navi-assistant.js` by Navi Assistant's
// settings window (or manually). No `opencode.json` edit is needed:
// files in the global plugin directory are auto-loaded at startup.
//
// Version stamp — the Tauri installer compares this to detect outdated copies.
// Bump on any protocol change.
// NAVI_PLUGIN_VERSION is matched by src-tauri/src/opencode.rs (do not rename).
const NAVI_PLUGIN_VERSION = 2;

// ── Relay resolution ──────────────────────────────────────────────────────────

function relayCandidates() {
  const list = [];
  try {
    const local = typeof process !== "undefined" ? process.env.LOCALAPPDATA : "";
    if (local) list.push(`${local}\\Navi Assistant\\bin\\navi-assistant-hook.exe`);
  } catch { /* process.env unavailable — fall through */ }
  return list;
}

/** stdin bytes for the relay: JSON line, like the Claude Code hook payload. */
function line(obj) {
  return `${JSON.stringify(obj)}\n`;
}

/**
 * Spawn the relay synchronously with `input` on stdin.
 * Returns trimmed stdout ("" when the relay is missing, crashed or timed out).
 * Never throws — silence is the safe answer.
 */
async function relay(event, payload, timeoutMs, $) {
  const candidates = relayCandidates();
  if (candidates.length === 0) return "";
  const body = line({ ...payload, hook_event_name: event, agent: "opencode" });

  // Prefer opencode's Bun shell API ($), fall back to node child_process.
  try {
    if ($) {
      const proc = $`${candidates[0]} ${event}`.stdin(body);
      const done = await Promise.race([
        proc.then((r) => (typeof r === "string" ? r : r?.stdout ?? "")),
        new Promise((resolve) => setTimeout(() => resolve(""), timeoutMs)),
      ]);
      return String(done ?? "").trim();
    }
  } catch { /* fall through to spawnSync */ }

  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const { spawnSync } = require("node:child_process");
    const res = spawnSync(candidates[0], [event], {
      input: body,
      encoding: "utf8",
      timeout: timeoutMs,
      windowsHide: true,
    });
    return String(res.stdout ?? "").trim();
  } catch {
    return "";
  }
}

function fireAndForget(event, payload, $) {
  // Deliberately unawaited: the 2 s budget lives inside the relay call chain
  // via timeout, and nothing here may delay the agent.
  void relay(event, payload, 2000, $).catch(() => {});
}

// ── Payload helpers ───────────────────────────────────────────────────────────

/** Concatenated text of an opencode message's text parts. */
function textFromParts(parts) {
  if (!Array.isArray(parts)) return "";
  return parts
    .filter((p) => p && p.type === "text" && typeof p.text === "string")
    .map((p) => p.text)
    .join("\n")
    .trim();
}

/** Human string for a `session.error` payload, best effort. */
function errorMessage(error) {
  if (!error || typeof error !== "object") return "";
  const data = error.data && typeof error.data === "object" ? error.data : {};
  return String(data.message ?? error.message ?? error.name ?? "").slice(0, 2000);
}

// ── Plugin ────────────────────────────────────────────────────────────────────

export const NaviAssistantPlugin = async ({ $, directory }) => {
  const cwd = directory || (typeof process !== "undefined" ? process.cwd() : "");

  /** Base payload shared by every event. */
  const base = (extra = {}) => ({ cwd, ...extra });

  return {
    // User prompt submitted in a session → thinking state + step line.
    // `chat.message` is the only place the prompt text is available; the
    // `message.updated` event carries no content.
    "chat.message": async (input, output) => {
      const prompt = textFromParts(output?.parts);
      if (!prompt) return;
      fireAndForget(
        "UserPromptSubmit",
        base({ session_id: input?.sessionID ?? "", prompt: prompt.slice(0, 2000) }),
        $,
      );
    },

    // ── Tools ───────────────────────────────────────────────────────────────
    "tool.execute.before": async (input, output) => {
      const tool = (input?.tool ?? "Tool").toString();
      const args = (output && typeof output.args === "object" && output.args) || {};
      // The question tool surfaces as an island question card.
      if (tool.toLowerCase() === "question") {
        const q = args.question || args.prompt || args.text || "opencode asks a question";
        fireAndForget(
          "Notification",
          base({ session_id: input?.sessionID ?? "", message: `${q}?` }),
          $,
        );
        return;
      }
      fireAndForget(
        "PreToolUse",
        base({ session_id: input?.sessionID ?? "", tool_name: tool, tool_input: args }),
        $,
      );
    },
    "tool.execute.after": async (input) => {
      fireAndForget(
        "PostToolUse",
        base({ session_id: input?.sessionID ?? "" }),
        $,
      );
    },

    // ── Bus events (session lifecycle) ──────────────────────────────────────
    // Individual event names are NOT hooks; they only arrive through `event`.
    event: async ({ event }) => {
      const props = event?.properties ?? {};
      const sessionId = props.sessionID ?? "";
      switch (event?.type) {
        case "session.created":
          fireAndForget("SessionStart", base({ session_id: sessionId }), $);
          break;
        case "session.idle":
          fireAndForget("Stop", base({ session_id: sessionId }), $);
          break;
        case "session.error":
          fireAndForget(
            "StopFailure",
            base({ session_id: sessionId, message: errorMessage(props.error) }),
            $,
          );
          break;
        case "session.deleted":
          fireAndForget("SessionEnd", base({ session_id: sessionId }), $);
          break;
        default:
          break;
      }
    },
  };
};
