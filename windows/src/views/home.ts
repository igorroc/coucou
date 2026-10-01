// Home dashboard — the expanded island's first page. Rewritten from the old
// overview (ticker + integration card + pills) to the "Noma"-style layout:
// identity header, a 2×2 card grid, a strip of integration pills and the
// command bar. Sessions come from real hook data; meeting/tasks/suggestions are
// fixtures (see mocks/home.ts).

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { createMiniBot, pruneMiniBots } from "../mochi/minibots";
import { Bridge } from "../core/bridge";
import { State, type AgentTask, type HomeSession, type SessionStatus } from "../core/state";
import type { ViewActions, ViewHost } from "./views";
import {
  HOME_IDENTITY,
  MOCK_MEETING,
  MOCK_SUGGESTIONS,
  MOCK_TASKS,
  type MockTask,
  type MockTaskStatus,
} from "../mocks/home";

const SESSION_BADGE: Record<SessionStatus, { label: string; cls: string }> = {
  action: { label: "Ação necessária", cls: "action" },
  active: { label: "Em andamento", cls: "active" },
  done: { label: "Concluída", cls: "done" },
  error: { label: "Falhou", cls: "error" },
};

const TASK_BADGE: Record<MockTaskStatus, { label: string; cls: string }> = {
  progress: { label: "Em andamento", cls: "active" },
  review: { label: "Em revisão", cls: "review" },
  pending: { label: "Pendente", cls: "pending" },
};

function homeCard(iconEl: Node, title: string, body: HTMLElement, color: string): HTMLElement {
  return h(
    "div",
    { class: "home-card" },
    h(
      "div",
      { class: "hc-head" },
      h("i", { class: "hc-icon", style: `color:${color};background:${color}26` }, iconEl),
      h("b", { text: title }),
    ),
    body,
  );
}

function sessionRow(s: HomeSession): HTMLElement {
  const st = SESSION_BADGE[s.status];
  return h(
    "div",
    { class: "hc-row" },
    h("span", { class: "hc-sq" }, svg(ICONS.terminal, 13, { stroke: 1.7 })),
    h(
      "div",
      { class: "hc-main" },
      h("div", { class: "hc-title", text: s.title }),
      h("div", { class: "hc-sub", text: s.project }),
    ),
    h("span", { class: `hc-badge ${st.cls}`, text: st.label }),
  );
}

function taskRow(t: MockTask): HTMLElement {
  const st = TASK_BADGE[t.status];
  return h(
    "div",
    { class: "hc-row" },
    h("span", { class: "hc-key", style: `--c:${t.color}` }, h("i"), h("span", { text: t.key })),
    h("div", { class: "hc-main" }, h("div", { class: "hc-title", text: t.title })),
    h("span", { class: `hc-badge ${st.cls}`, text: st.label }),
  );
}

function buildMeeting(body: HTMLElement) {
  const m = MOCK_MEETING;
  body.append(
    h("span", { class: "hc-meet-icon" }, svg(ICONS.video, 16)),
    h(
      "div",
      { class: "hc-main" },
      h("div", { class: "hc-title", text: m.title }),
      h("div", { class: "hc-sub", text: `${m.day}, ${m.time}` }),
      h("div", { class: "hc-sub", text: m.provider }),
    ),
    h(
      "button",
      { class: "hc-enter", type: "button", onclick: () => void Bridge.openUrl(m.url) },
      h("span", { text: "Entrar" }),
      svg(ICONS.arrowUpRight, 10),
    ),
  );
}

/** Full-width command bar: Enter or the send button opens the chat tab. */
function buildCommandBar(actions: ViewActions): HTMLElement {
  const input = h("input", {
    type: "text",
    class: "home-input",
    placeholder: "Pergunte ou digite um comando…",
    spellcheck: "false",
    autocomplete: "off",
  }) as HTMLInputElement;
  const send = h("button", { class: "home-send", type: "button", title: "Send" }, svg(ICONS.arrowUp, 12));

  const submit = () => {
    const q = input.value.trim();
    if (!q) return;
    input.value = "";
    actions.ask(q);
  };

  const bar = h(
    "div",
    { class: "home-bar" },
    h(
      "button",
      { class: "home-attach", type: "button", title: "Attach a file", onclick: () => actions.browseFile() },
      svg(ICONS.paperclip, 15, { stroke: 1.6 }),
    ),
    input,
    h("div", { class: "home-sep" }),
    h(
      "button",
      { class: "home-mic", type: "button", title: "Voice input isn't available yet", disabled: true },
      svg(ICONS.mic, 15, { stroke: 1.6 }),
    ),
    send,
  );

  send.addEventListener("click", submit);
  input.addEventListener("mousedown", () => actions.focusWindow(true));
  input.addEventListener("focus", () => actions.focusWindow(true));
  input.addEventListener("blur", () => actions.focusWindow(false));
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      submit();
    }
    e.stopPropagation(); // Escape closes the island, not the field
  });
  return bar;
}

function lighten(hex: string, amount: number): string {
  const v = parseInt(hex.replace("#", ""), 16);
  const c = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map((x) =>
    Math.min(255, Math.round(x + amount * 255)),
  );
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}

/** Integration pill — a mini Mochi + label; click focuses that agent. */
function buildPill(task: AgentTask, actions: ViewActions): HTMLElement {
  const label = task.id === "integration_claude" ? "VS Code" : task.name;
  const canvas = createMiniBot(task, 24);
  const pill = h(
    "div",
    { class: "pill", onclick: () => actions.setFocus(task.id) },
    canvas,
    h("span", { class: "lbl", text: label }),
  );
  pill.style.borderColor = `${task.color}24`;
  pill.addEventListener("mouseenter", () => {
    pill.style.background = `${task.color}2e`;
    pill.style.borderColor = `${task.color}8c`;
    pill.style.boxShadow = `0 2px 10px ${task.color}59`;
    (pill.querySelector(".lbl") as HTMLElement).style.color = lighten(task.color, 0.3);
  });
  pill.addEventListener("mouseleave", () => {
    pill.style.background = "";
    pill.style.borderColor = `${task.color}24`;
    pill.style.boxShadow = "";
    (pill.querySelector(".lbl") as HTMLElement).style.color = "";
  });

  if (task.pillBadge) {
    const colors = { approval: "#F5A524", finished: "#22C55E", error: "#F4505E" } as const;
    const icons = { approval: ICONS.bang, finished: ICONS.check, error: ICONS.xmark } as const;
    const inner = h(
      "i",
      { style: `background:${colors[task.pillBadge]}` },
      svg(icons[task.pillBadge], 6, { stroke: task.pillBadge === "finished" ? 3 : 0 }),
    );
    const badge = h("div", { class: "pill-badge" }, inner);
    badge.style.boxShadow = `0 0 4px ${colors[task.pillBadge]}99`;
    pill.append(badge);
  }
  return pill;
}

export function buildHome(actions: ViewActions): ViewHost {
  const pillsRow = h("div", { class: "home-pills" });
  const identity = h(
    "div",
    { class: "home-identity" },
    h(
      "div",
      { class: "home-who" },
      h("div", { class: "home-name", text: HOME_IDENTITY.name }),
      h("div", { class: "home-sub", text: HOME_IDENTITY.subtitle }),
    ),
    pillsRow,
  );

  const sessionsRows = h("div", { class: "hc-rows" });
  const meetingBody = h("div", { class: "hc-meeting" });
  const suggestGrid = h("div", { class: "hc-suggest" });
  const taskRows = h("div", { class: "hc-rows tasks" });

  const el = h(
    "div",
    { class: "view home" },
    identity,
    h(
      "div",
      { class: "home-grid" },
      h(
        "div",
        { class: "home-col" },
        homeCard(svg(ICONS.terminal, 13, { stroke: 1.7 }), "Sessões do OpenCode", sessionsRows, "#3B9EFF"),
        homeCard(svg(ICONS.calendar, 13, { stroke: 1.7 }), "Próximo compromisso", meetingBody, "#7C5CFF"),
      ),
      h(
        "div",
        { class: "home-col" },
        homeCard(svg(ICONS.sparkle, 13, { stroke: 1.7 }), "Sugestões", suggestGrid, "#A78BFA"),
        homeCard(svg(ICONS.checkCircle, 13, { stroke: 1.7 }), "Minhas tarefas", taskRows, "#34D399"),
      ),
    ),
    buildCommandBar(actions),
  );

  buildMeeting(meetingBody);
  for (const s of MOCK_SUGGESTIONS) {
    suggestGrid.append(
      h(
        "button",
        { class: "hc-suggest-btn", type: "button", onclick: () => actions.ask(s.prompt) },
        h("i", { class: "hc-suggest-icon" }, svg(s.icon, 13, { stroke: 1.7 })),
        h("span", { class: "hc-suggest-label", text: s.label }),
        h("i", { class: "hc-chevron" }, svg(ICONS.chevronRight, 9, { stroke: 2.2 })),
      ),
    );
  }
  for (const t of MOCK_TASKS) taskRows.append(taskRow(t));

  let sessionKey = "";
  let pillKey = "";

  return {
    el,
    sync() {
      const sessions = State.opencodeSessions.slice(0, 3);
      const sKey = sessions
        .map((s) => `${s.id}:${s.status}:${s.title}:${s.lastStep ?? ""}:${s.project}`)
        .join("|");
      if (sKey !== sessionKey) {
        sessionKey = sKey;
        clear(sessionsRows);
        if (sessions.length === 0) {
          sessionsRows.append(h("div", { class: "hc-empty", text: "Nenhuma sessão do opencode agora." }));
        } else {
          for (const s of sessions) sessionsRows.append(sessionRow(s));
        }
      }

      const pills = State.tasks.filter((t) => t.id !== "integration_opencode");
      const pKey = pills.map((t) => `${t.id}:${t.state}:${t.name}:${t.pillBadge ?? ""}`).join("|");
      if (pKey !== pillKey) {
        pillKey = pKey;
        clear(pillsRow);
        for (const t of pills) pillsRow.append(buildPill(t, actions));
        pruneMiniBots();
      }
    },
  };
}
