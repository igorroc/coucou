// App state — mirror of AppState.swift (the parts the island needs).

import type { BotEmoteName, BotStateName, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../mochi/engine";

export type AgentSource = "claudeCode" | "opencode" | "n8n";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
}

export interface ApprovalInfo {
  requestId: string;
  sessionId: string;
  tool: string;
  command: string;
  /** Which pill the request belongs to — the decision card returns here. */
  agentId: string;
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
}

export type PromptContext =
  | { kind: "window"; appName: string; title: string; url?: string }
  | { kind: "file"; name: string; path?: string };

export interface ResultItem {
  label: string;
  detail: string;
  url?: string;
}

export interface SearchResult {
  title: string;
  items: ResultItem[];
  note?: string;
}

/** Badge shown on a session row in the home dashboard. */
export type SessionStatus = "action" | "active" | "done" | "error";

/**
 * One coding-agent session, keyed by `session_id`. Read-only view model for the
 * home dashboard — the island's own `tasks` keep driving the bot, badges and
 * approvals exactly as before.
 */
export interface HomeSession {
  id: string;
  agent: "claudeCode" | "opencode";
  /** Project folder the session runs in. */
  project: string;
  /** First user prompt, else the project name. */
  title: string;
  status: SessionStatus;
  lastStep?: string;
  cwd?: string;
  updatedAt: number;
}

const task = (
  id: string, name: string, color: string, source: AgentSource,
): AgentTask => ({
  id, name, color, state: "idle", stepIndex: 0, steps: [], source, isIntegration: true,
});

/** AgentTask.integrationAgents — same ids, names and colours as macOS. */
export const INTEGRATION_AGENTS: AgentTask[] = [
  task("integration_claude", "VS Code", "#F5F6F8", "claudeCode"),
  task("integration_opencode", "opencode", "#FF6B5B", "opencode"),
  task("integration_resend", "Resend", "#22C55E", "n8n"),
  task("integration_n8n", "n8n", "#F29B38", "n8n"),
  task("integration_vercel", "Vercel", "#7C5CFF", "n8n"),
  task("integration_github", "GitHub", "#F4505E", "n8n"),
  task("integration_notion", "Notion", "#8C8C8C", "n8n"),
  task("integration_calcom", "Cal.com", "#C9956A", "n8n"),
  task("integration_stripe", "Stripe", "#0570DE", "n8n"),
];

export const TOGGLEABLE_INTEGRATION_IDS = [
  "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  "integration_notion", "integration_calcom", "integration_stripe",
];

/** What an integration poller last reported. */
export interface IntegrationInfo {
  data: Record<string, unknown>;
  error: string | null;
  loaded: boolean;
  configured: boolean;
}

export interface Settings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  /** When true the island never auto-closes: no home → petit, no petit → hidden. */
  keepVisible: boolean;
  /** Width of the island in compact mode, in logical px (Settings → General). */
  compactWidth: number;
  absenceInterval: number;
  activeIntegrations: string[];
  screen: "primary" | "cursor";
  autostart: boolean;
  hooksInstalled: boolean;
  /** Claude model used by the chat. */
  model: string;
  /** Chat backend: "claude" (Anthropic API) or "opencode" (local CLI). */
  chatProvider: string;
  /** Explicit opencode.exe path; empty = auto-detect. */
  opencodeBin: string;
  /** provider/model override for opencode chat; empty = its default. */
  opencodeModel: string;
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  keepVisible: false,
  compactWidth: 288,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  model: "claude-opus-5",
  chatProvider: "claude",
  opencodeBin: "",
  opencodeModel: "",
};

type Listener = () => void;

class AppState {
  mode: IslandMode = "hidden";
  view: IslandViewName = "overview";

  tasks: AgentTask[] = [];
  focusId: string | null = null;

  /** Coding-agent sessions by `session_id`, for the home dashboard. */
  sessions: HomeSession[] = [];

  stateOverride: BotStateName | null = null;

  /** Cursor in logical screen pixels, origin top-left (like AppState.mousePosition). */
  mouse = { x: 0, y: 0 };
  /** Cursor relative to the island's top-left corner. */
  mouseInIsland = { x: 0, y: 0 };

  isPinned = false;
  paused = false;

  uploadProgress = 0;
  uploadDuration = 2.4;
  fileDragOver = false;

  promptContext: PromptContext | null = null;
  droppedFile: { name: string; path: string } | null = null;
  noteMessage: string | null = null;
  searchResult: SearchResult | null = null;
  chatHistory: ChatMessage[] = [];
  /** opencode session id of the conversation on screen, when reusing one. */
  chatSessionId: string | null = null;
  pendingApproval: ApprovalInfo | null = null;

  integrations: Record<string, IntegrationInfo> = {};

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Marks the UI dirty; the island re-renders on the next frame. */
  notify() {
    for (const fn of this.listeners) fn();
  }

  get focusTask(): AgentTask | null {
    return this.tasks.find((t) => t.id === this.focusId) ?? this.tasks[0] ?? null;
  }

  get effectiveState(): BotStateName {
    return this.stateOverride ?? this.focusTask?.state ?? "idle";
  }

  get otherTasks(): AgentTask[] {
    return this.tasks.filter((t) => t.id !== this.focusId);
  }

  /** Newest first, opencode only — the home "Sessões do OpenCode" card. */
  get opencodeSessions(): HomeSession[] {
    return this.sessions
      .filter((s) => s.agent === "opencode")
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }

  /**
   * Create or refresh a session. A missing `session_id` means the event can't be
   * attributed, so callers simply don't call this.
   */
  upsertSession(patch: {
    id: string;
    agent: HomeSession["agent"];
    project?: string;
    title?: string;
    status?: SessionStatus;
    lastStep?: string;
    cwd?: string;
  }) {
    const now = Date.now();
    let s = this.sessions.find((x) => x.id === patch.id);
    if (!s) {
      s = {
        id: patch.id,
        agent: patch.agent,
        project: patch.project ?? patch.cwd ?? "opencode",
        title: patch.title ?? patch.project ?? "Nova sessão",
        status: patch.status ?? "active",
        updatedAt: now,
      };
      this.sessions.push(s);
    }
    if (patch.project) s.project = patch.project;
    if (patch.title) s.title = patch.title;
    if (patch.status) s.status = patch.status;
    if (patch.lastStep !== undefined) s.lastStep = patch.lastStep;
    if (patch.cwd) s.cwd = patch.cwd;
    s.updatedAt = now;
    if (this.sessions.length > 12) {
      this.sessions.sort((a, b) => b.updatedAt - a.updatedAt);
      this.sessions = this.sessions.slice(0, 12);
    }
    this.notify();
  }

  removeSession(id: string) {
    const i = this.sessions.findIndex((s) => s.id === id);
    if (i < 0) return;
    this.sessions.splice(i, 1);
    this.notify();
  }

  setFocus(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    this.focusId = id;
    t.pillBadge = null;
    this.notify();
  }

  updateTask(id: string, state: BotStateName) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state;
    this.notify();
  }

  appendStep(id: string, step: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /** loadIntegrationTasks() — coding agents always on, the rest opt-in (max 4). */
  loadIntegrationTasks() {
    for (const proto of INTEGRATION_AGENTS) {
      const shouldLoad =
        proto.id === "integration_claude" ||
        proto.id === "integration_opencode" ||
        this.settings.activeIntegrations.includes(proto.id);
      const idx = this.tasks.findIndex((t) => t.id === proto.id);
      if (shouldLoad && idx < 0) this.tasks.push({ ...proto, steps: [] });
      if (!shouldLoad && idx >= 0) this.tasks.splice(idx, 1);
    }
    // Keep the declared order so pills never shuffle.
    const order = INTEGRATION_AGENTS.map((t) => t.id);
    this.tasks.sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
    if (!this.focusId) this.focusId = "integration_claude";
    this.notify();
  }

  toggleIntegration(id: string) {
    if (id === "integration_claude" || id === "integration_opencode") return;
    const active = this.settings.activeIntegrations;
    if (active.includes(id)) {
      this.settings.activeIntegrations = active.filter((x) => x !== id);
      if (this.focusId === id) this.focusId = "integration_claude";
    } else {
      if (active.length >= 4) return;
      this.settings.activeIntegrations = [...active, id];
    }
    this.loadIntegrationTasks();
  }

  defaultView(): IslandViewName {
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
