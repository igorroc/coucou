// Chat view — a single two-column layout: the chat list on the left, the
// conversation and composer on the right. Ported from PromptView / ChatBubble,
// with the history list (opencode's own sessions) merged in.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, IS_TAURI, type ChatContext, type SessionInfo } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type AgentTask, type ChatMessage } from "../core/state";
import { createMiniBot } from "../mochi/minibots";
import type { ViewHost } from "./views";

let nextId = 1;

/** Keeps message ids unique when history was restored from an old session. */
function freshId(): number {
  const max = State.chatHistory.reduce((m, x) => Math.max(m, x.id), 0);
  nextId = Math.max(nextId, max + 1);
  return nextId++;
}

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

/** "10:24" today, "Ontem · 16:03" yesterday, "12 de mai. · 14:08" older. */
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

/** The model answering right now: opencode's override (or its default) or the Claude model. */
function currentModel(): string {
  if (State.settings.chatProvider === "opencode") {
    return State.settings.opencodeModel.trim() || "Padrão do opencode";
  }
  return State.settings.model || "";
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  // ── Left column: the chat list ─────────────────────────────────────────────
  const list = h("div", { class: "chat-list" });
  const newBtn = h("button", { class: "chat-new", title: "New chat" }, svg(ICONS.pencil, 14, { stroke: 1.7 }));
  const listCol = h(
    "div",
    { class: "chat-col-list" },
    h("div", { class: "chat-list-head" }, h("b", { text: "Chats" }), newBtn),
    list,
  );

  // ── Right column: the conversation ─────────────────────────────────────────
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Pergunte ou digite um comando…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const bar = h("div", { class: "chat-bar" }, input, send);
  const mochiTask: AgentTask = {
    id: "chat_mochi", name: "Mochi", color: "#F5F6F8", source: "opencode",
    state: "idle", stepIndex: 0, steps: [], isIntegration: false,
  };
  const avatar = createMiniBot(mochiTask, 30);
  const titleEl = h("div", { class: "chat-conv-title", text: State.assistantName });
  const modelEl = h("div", { class: "chat-conv-model", text: currentModel() });
  const convCol = h(
    "div",
    { class: "chat-col-conv" },
    h(
      "div",
      { class: "chat-conv-head" },
      h("span", { class: "chat-conv-avatar" }, avatar),
      h(
        "div",
        { class: "chat-conv-id" },
        titleEl,
        modelEl,
      ),
    ),
    h("div", { class: "chat-conv-body" }, chipRow, log),
    bar,
  );

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, listCol, convCol),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let renderKeyLast = "";
  let sessions: SessionInfo[] = [];
  let listLoading = false;
  let listError: string | null = null;
  let listSignature = "";

  function paintList() {
    const key = `${listLoading}:${listError ?? ""}:${sessions.map((s) => `${s.id}:${s.updatedAt}`).join("|")}`;
    if (key === listSignature) return;
    listSignature = key;
    clear(list);

    if (listLoading && sessions.length === 0) {
      list.append(h("div", { class: "hist-empty", text: "Loading chats…" }));
      return;
    }
    if (listError) {
      list.append(h("div", { class: "hist-empty", text: listError }));
      return;
    }
    if (sessions.length === 0) {
      list.append(h("div", { class: "hist-empty", text: "No chats yet." }));
      return;
    }
    for (const s of sessions) list.append(sessionRow(s));
  }

  function sessionRow(s: SessionInfo): HTMLElement {
    const del = h(
      "button",
      { class: "hist-del", type: "button", title: "Excluir chat" },
      svg(ICONS.trash, 13),
    );
    del.addEventListener("click", (e) => {
      e.stopPropagation();
      void deleteSession(s);
    });
    const row = h(
      "div",
      { class: "hist-row", role: "button", tabindex: "0", onclick: () => void openSession(s) },
      h("span", { class: "hist-avatar" }, svg(ICONS.bubble, 15, { stroke: 0 })),
      h(
        "div",
        { class: "hist-main" },
        h("div", { class: "hist-title", text: s.title || "Chat" }),
        h("div", { class: "hist-sub", text: `${s.projectName} · ${formatWhen(s.updatedAt)}` }),
      ),
      del,
    );
    row.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).key === "Enter") void openSession(s);
    });
    row.classList.toggle("on", State.chatSessionId === s.id);
    return row;
  }

  async function deleteSession(s: SessionInfo) {
    Sound.play("blip");
    sessions = sessions.filter((x) => x.id !== s.id);
    if (State.chatSessionId === s.id) {
      State.chatHistory = [];
      State.chatSessionId = null;
      State.droppedFile = null;
      State.promptContext = null;
      void Bridge.chatReset();
      State.notify();
      onHeightChange();
    }
    listSignature = "";
    paintList();
    try {
      await Bridge.chatDeleteSession(s.id);
    } catch (e) {
      listError = String(e).replace(/^Error:\s*/, "");
      listSignature = "";
      paintList();
    }
    void refreshSessions();
  }

  async function refreshSessions() {
    if (!IS_TAURI || listLoading) return;
    listLoading = true;
    listError = null;
    paintList();
    try {
      sessions = (await Bridge.chatListSessions()) ?? [];
    } catch (e) {
      listError = String(e).replace(/^Error:\s*/, "");
    } finally {
      listLoading = false;
      paintList();
    }
  }

  async function openSession(s: SessionInfo) {
    Sound.play("blip");
    try {
      const messages = await Bridge.chatOpenSession(s.id);
      State.chatHistory = messages.map((m, i) => ({ id: i + 1, role: m.role, content: m.content }));
      State.chatSessionId = s.id;
      State.droppedFile = null;
      State.promptContext = null;
      State.notify();
      onHeightChange();
      listSignature = ""; // refresh the active row highlight
      paintList();
    } catch (e) {
      listError = String(e).replace(/^Error:\s*/, "");
      listSignature = "";
      paintList();
    }
  }

  function newChat() {
    Sound.play("blip");
    State.chatHistory = [];
    State.chatSessionId = null;
    State.droppedFile = null;
    State.promptContext = null;
    void Bridge.chatReset();
    State.notify();
    onHeightChange();
    listSignature = "";
    paintList();
    input.focus();
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    Sound.play("send");

    State.chatHistory.push({ id: freshId(), role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    const context: ChatContext | null =
      State.chatHistory.length === 1 && file ? { kind: "file", name: file.name, path: file.path } : null;

    try {
      const reply = await Bridge.chatSend(query, context);
      State.chatHistory.push({ id: freshId(), role: "assistant", content: reply.text });
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      State.notify();
      onHeightChange();
      input.focus();
      // A new conversation may have just been created: refresh the list.
      if (!State.chatSessionId) void refreshSessions();
    }
  }

  newBtn.addEventListener("click", newChat);
  send.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const name = State.assistantName;
      if (titleEl.textContent !== name) titleEl.textContent = name;
      const model = currentModel();
      if (modelEl.textContent !== model) modelEl.textContent = model;

      const file = State.droppedFile;
      const wantChip = file?.name ?? "";
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const thinking = State.stateOverride === "thinking";
      const count = State.chatHistory.length + (thinking ? 0.5 : 0);
      // Compare the ends too: loading an old session can leave the count the
      // same while every message is different.
      const first = State.chatHistory[0]?.content ?? "";
      const last = State.chatHistory.at(-1)?.content ?? "";
      const renderKey = `${count}|${first}|${last}`;
      if (renderKey !== renderKeyLast) {
        renderKeyLast = renderKey;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = State.chatHistory.length === 0 ? "Pergunte ou digite um comando…" : "Continue…";
      input.disabled = sending;
      paintList();
    },
    focus() {
      // Reload the list every time the chat comes to screen so a session
      // started elsewhere shows up.
      void refreshSessions();
      window.setTimeout(() => {
        input.focus();
        input.select();
      }, 60);
    },
    /** A question handed over from the home command bar. */
    ask(query: string) {
      input.value = query;
      void submit();
    },
  };
}
