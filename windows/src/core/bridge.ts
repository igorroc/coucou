// Thin wrapper over the Tauri commands/events. Every call is a no-op when the
// page is opened in a plain browser, so the island can be iterated on with
// `npm run dev` alone.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Settings } from "./state";

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[coucou] ${cmd} failed`, err);
    return null;
  }
}

export interface BootInfo {
  settings: Settings;
  /** Logical screen rect of the monitor the island lives on. */
  screen: { x: number; y: number; width: number; height: number; scale: number };
  version: string;
  hookPath: string;
}

export const Bridge = {
  boot: () => call<BootInfo>("boot"),

  saveSettings: (settings: Settings) => call<void>("save_settings", { settings }),

  /** Shrink the window down to the invisible wake strip (hidden) or back to full. */
  setCollapsed: (collapsed: boolean) => call<void>("set_collapsed", { collapsed }),

  /**
   * Pushes the island shape in window coordinates. Rust flips click-through from
   * its own cursor poll, so the flag is never a frame behind a click.
   */
  setIslandRect: (x: number, y: number, width: number, height: number) =>
    call<void>("set_island_rect", { x, y, width, height }),

  /** Give the window keyboard focus (chat field) and take it away again. */
  focusWindow: (focused: boolean) => call<void>("focus_window", { focused }),

  reposition: () => call<void>("reposition"),

  openUrl: (url: string) => call<void>("open_url", { url }),

  /** "Open terminal" → opens the folder in VS Code when `code` is on PATH. */
  openInVSCode: (path: string | null) => call<boolean>("open_in_vscode", { path }),

  quit: () => call<void>("quit_app"),

  /** Writes to %LOCALAPPDATA%\Coucou\coucou.log, next to the Rust lines. */
  log: (message: string) => call<void>("log_line", { message }),

  // ── Claude Code hooks ─────────────────────────────────────────────────────
  hooksStatus: () => call<HookStatus>("hooks_status"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  hooksPreview: (install: boolean) => callOrThrow<HookPreview>("hooks_preview", { install }),
  /**
   * Writes ~/.claude/settings.json — only ever after an explicit click, and only
   * when the file still matches the preview the user looked at.
   */
  hooksApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("hooks_apply", { install, fingerprint }),

  // ── opencode plugin ─────────────────────────────────────────────────────
  opencodeStatus: () => call<OpencodeStatus>("opencode_status"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  opencodePreview: (install: boolean) => callOrThrow<OpencodePreview>("opencode_preview", { install }),
  /**
   * Writes ~/.config/opencode/plugins/coucou.js — only ever after an explicit
   * click, and only when the file still matches the preview the user looked at.
   */
  opencodeApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("opencode_apply", { install, fingerprint }),

  approvalDecision: (requestId: string, decision: "allow" | "deny") =>
    call<void>("approval_decision", { requestId, decision }),
  /** "The card is up" — until this lands the relay only waits a moment. */
  approvalAck: (requestId: string) => call<void>("approval_ack", { requestId }),
  /** "Nobody can act on this" — Claude Code asks in the terminal right away. */
  approvalDecline: (requestId: string) => call<void>("approval_decline", { requestId }),

  // ── Chat, files, secrets ──────────────────────────────────────────────────
  /** One chat turn. The API key and any file bytes never leave Rust. */
  chatSend: (query: string, context: ChatContext | null) =>
    callOrThrow<{ text: string }>("chat_send", { query, context }),
  chatReset: () => call<void>("chat_reset"),
  /** Resolved opencode binary + key presence for the Settings → Chat section. */
  chatStatus: () => call<ChatStatus>("chat_status"),
  /** Every conversation opencode has on disk, for the history list. */
  chatListSessions: () => call<SessionInfo[]>("chat_list_sessions"),
  /** MCP servers the notch can use (chat folder + global opencode config). */
  mcpList: () => call<McpInfo[]>("mcp_list"),
  /** Jira issues assigned to me, via the Atlassian MCP; cached for one hour. */
  jiraTasks: (force: boolean) => call<JiraTasks>("jira_tasks", { force }),
  /**
   * Reopens an old conversation: returns its turns and makes the next send
   * continue it in opencode.
   */
  chatOpenSession: (id: string) => callOrThrow<HistoryMessage[]>("chat_open_session", { id }),
  /** Copies a dropped file into the inbox. */
  ingestFile: (path: string) => callOrThrow<DroppedFile>("ingest_file", { path }),
  /**
   * Native Explorer picker for the drop zone (click-to-browse fallback).
   * Returns the picked path, or null when the user cancels.
   */
  browseFile: () => callOrThrow<string | null>("browse_file"),
  /** Only ever tells you whether a key exists — never its value. */
  secretPresent: (key: string) => call<boolean>("secret_present", { key }),
  secretSet: (key: string, value: string) => callOrThrow<void>("secret_set", { key, value }),
  secretClear: (key: string) => callOrThrow<void>("secret_clear", { key }),

  // ── Integrations ──────────────────────────────────────────────────────────
  refreshIntegration: (id: string) => call<void>("refresh_integration", { id }),
  /** Opens the configured n8n instance in the browser. */
  openN8n: () => call<void>("open_n8n"),

  /** Tray → Pause. Stops the integration pollers, not just the island. */
  setPaused: (paused: boolean) => call<void>("set_paused", { paused }),
};

export interface IntegrationUpdate {
  id: string;
  data: Record<string, unknown>;
  error: string | null;
  event: { success: boolean; label: string; detail: string | null } | null;
}

export type ChatContext =
  | { kind: "file"; name: string; path: string }
  | { kind: "window"; appName: string; title: string; url?: string };

export interface DroppedFile {
  name: string;
  path: string;
  size: number;
}

export interface ChatStatus {
  binConfigured: string;
  binResolved: string | null;
  claudeKeyPresent: boolean;
}

/** One conversation in the history list (opencode_sessions.rs). */
export interface SessionInfo {
  id: string;
  title: string;
  directory: string;
  projectId: string;
  projectName: string;
  projectPath: string;
  updatedAt: number;
}

export interface HistoryMessage {
  role: "user" | "assistant";
  content: string;
}

/** One MCP server opencode can reach (opencode_chat::McpInfo). */
export interface McpInfo {
  name: string;
  kind: string;
  target: string;
  enabled: boolean;
  source: string;
}

/** One Jira issue assigned to the user (jira::JiraTask). */
export interface JiraTask {
  key: string;
  summary: string;
  status: string;
  category: string;
  color: string;
  project: string;
}

/** Dashboard tasks payload (jira::JiraTasks). */
export interface JiraTasks {
  tasks: JiraTask[];
  /** Unix seconds of the fetch; 0 when never fetched. */
  fetchedAt: number;
  /** True when served from the on-disk cache. */
  cached: boolean;
  error: string | null;
}

export interface HookStatus {
  installed: boolean;
  settingsPath: string;
  hookPath: string;
  hookReady: boolean;
}

export interface HookPreview {
  diff: string;
  backup: string;
  settingsPath: string;
  /** Hand back to hooksApply so only the reviewed diff is ever written. */
  fingerprint: string;
}

export interface OpencodeStatus {
  installed: boolean;
  pluginPath: string;
  relayReady: boolean;
  bundledVersion: number;
  installedVersion: number | null;
  needsUpdate: boolean;
}

export interface OpencodePreview {
  diff: string;
  backup: string;
  pluginPath: string;
  /** Hand back to opencodeApply so only the reviewed diff is ever written. */
  fingerprint: string;
}

/** Same as `call`, but surfaces the error so the UI can show what went wrong. */
async function callOrThrow<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error("not running inside Coucou");
  return invoke<T>(cmd, args);
}

export type BridgeEvent =
  | { name: "cursor"; payload: { x: number; y: number } }
  | { name: "tray"; payload: string }
  | { name: "hotkey"; payload: string }
  | { name: "hook"; payload: Record<string, unknown> }
  | { name: "screen-changed"; payload: null };

export interface DragDropPayload {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
}

/** Files dragged onto the island. Only reaches us when the window takes the mouse. */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (!IS_TAURI) return () => {};
  return getCurrentWebview().onDragDropEvent((event) => {
    handler(event.payload as DragDropPayload);
  });
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (!IS_TAURI) return () => {};
  return listen<T>(name, (e) => handler(e.payload));
}
