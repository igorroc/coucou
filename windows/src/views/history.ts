// History view — the chats the user has had from the notch, read from the
// opencode data directory. Grouped by project ("Mochi" for the notch's own
// folder, otherwise the repo name), newest first. Clicking one reopens it in
// the chat tab and continues the same opencode session.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, IS_TAURI, type SessionInfo } from "../core/bridge";
import { State } from "../core/state";
import type { ViewHost } from "./views";

/** "10:24" for today, "Ontem · 16:03" yesterday, "12 de mai. · 14:08" older. */
function formatWhen(ms: number): string {
  if (!ms) return "";
  const d = new Date(ms);
  const time = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const now = new Date();
  const day = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const diff = (today.getTime() - day.getTime()) / 86_400_000;
  if (diff === 0) return time;
  if (diff === 1) return `Ontem · ${time}`;
  const month = d.toLocaleDateString("pt-BR", { day: "2-digit", month: "short" });
  return `${month} · ${time}`;
}

function chatRow(s: SessionInfo, onOpen: (s: SessionInfo) => void): HTMLElement {
  return h(
    "button",
    { class: "hist-row", type: "button", onclick: () => onOpen(s) },
    h(
      "span",
      { class: "hist-avatar" },
      svg(ICONS.bubble, 15, { stroke: 0 }),
    ),
    h(
      "div",
      { class: "hist-main" },
      h("div", { class: "hist-title", text: s.title || "Chat" }),
      h("div", { class: "hist-sub", text: `${s.projectName} · ${formatWhen(s.updatedAt)}` }),
    ),
    h("i", { class: "hist-chevron" }, svg(ICONS.chevronRight, 10, { stroke: 2.2 })),
  );
}

export function buildHistory(actions: {
  setView(v: "prompt"): void;
  blip(): void;
}): ViewHost {
  const list = h("div", { class: "hist-list" });
  const el = h(
    "div",
    { class: "view" },
    h(
      "div",
      { class: "card wash hist-card" },
      h(
        "div",
        { class: "hist-body" },
        h("div", { class: "hist-head" }, h("b", { text: "Chats" })),
        list,
      ),
    ),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let loading = false;
  let error: string | null = null;
  let sessions: SessionInfo[] = [];
  let signature = "";

  function paint() {
    const key = `${loading}:${error ?? ""}:${sessions.map((s) => `${s.id}:${s.updatedAt}`).join("|")}`;
    if (key === signature) return;
    signature = key;
    clear(list);

    if (loading && sessions.length === 0) {
      list.append(h("div", { class: "hist-empty", text: "Loading chats…" }));
      return;
    }
    if (error) {
      list.append(h("div", { class: "hist-empty", text: error }));
      return;
    }
    if (sessions.length === 0) {
      list.append(h("div", { class: "hist-empty", text: "No chats yet." }));
      return;
    }
    for (const s of sessions) list.append(chatRow(s, open));
  }

  async function refresh() {
    if (loading) return;
    loading = true;
    error = null;
    paint();
    try {
      sessions = (await Bridge.chatListSessions()) ?? [];
    } catch (e) {
      error = String(e).replace(/^Error:\s*/, "");
    } finally {
      loading = false;
      paint();
    }
  }

  async function open(s: SessionInfo) {
    actions.blip();
    try {
      const messages = await Bridge.chatOpenSession(s.id);
      State.chatHistory = messages.map((m, i) => ({ id: i + 1, role: m.role, content: m.content }));
      State.chatSessionId = s.id;
      State.droppedFile = null;
      State.promptContext = null;
      State.notify();
      actions.setView("prompt");
    } catch (e) {
      error = String(e).replace(/^Error:\s*/, "");
      signature = "";
      paint();
    }
  }

  return {
    el,
    sync() {
      paint();
    },
    // Called every time the view comes to screen: reload so a chat started or
    // finished elsewhere shows up without reopening the app.
    focus() {
      if (!IS_TAURI) {
        paint();
        return;
      }
      void refresh();
    },
  };
}
