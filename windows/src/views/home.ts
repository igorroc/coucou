// Home dashboard — the expanded island's first page. Rewritten from the old
// overview (ticker + integration card + pills) to the "Noma"-style layout:
// identity header, the news carousel, the calendar + Jira grid, a strip of
// suggestion pills and the command bar. Tasks (Jira), the next appointment and
// the suggestions all come from Rust; only the labels' fallback lives in
// mocks/home.ts.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { createMiniBot, pruneMiniBots } from "../navi/minibots";
import {
  Bridge,
  IS_TAURI,
  type CalendarEvent,
  type CalendarNext,
  type McpInfo,
  type NewsItem,
} from "../core/bridge";
import { State, type AgentTask, type SuggestedAction } from "../core/state";
import type { ViewActions, ViewHost } from "./views";
import { MOCK_SUGGESTIONS } from "../mocks/home";

function homeCard(
  iconEl: Node,
  title: string,
  body: HTMLElement,
  color: string,
  action?: HTMLElement,
): HTMLElement {
  return h(
    "div",
    { class: "home-card" },
    h(
      "div",
      { class: "hc-head" },
      h("i", { class: "hc-icon", style: `color:${color};background:${color}26` }, iconEl),
      h("b", { text: title }),
      action ?? null,
    ),
    body,
  );
}

/** Atlassian statusCategory.colorName → the chip colour. */
const JIRA_COLORS: Record<string, string> = {
  blue: "#3B9EFF",
  green: "#22C55E",
  yellow: "#F5A524",
  red: "#F4505E",
  "blue-gray": "#8C8C8C",
};

function jiraColor(colorName: string): string {
  return JIRA_COLORS[colorName] ?? "#6B7079";
}

/** One row in "Minhas tarefas": Jira (work) or a Google Task (personal). */
interface TaskRow {
  source: "jira" | "google";
  /** Chip label: the Jira key, or the Google task list's name. */
  label: string;
  title: string;
  /** Jira status colour name; unused for Google. */
  color: string;
  /** Chip tooltip: the Jira status, or the Google due date. */
  meta: string;
  /** External link opened on click; empty when there is nothing to open. */
  url: string;
}

/** "Hoje" / "Amanhã" / "05 de out." for a task's due date; "" when unparseable. */
function formatDue(due: string): string {
  const dt = new Date(due.length <= 10 ? `${due}T00:00:00` : due);
  return Number.isNaN(dt.getTime()) ? "" : dayLabel(dt);
}

/** The card's rows: Jira issues first, then the personal Google Tasks. */
function buildTaskRows(): TaskRow[] {
  const rows: TaskRow[] = [];
  for (const t of State.jira?.tasks ?? []) {
    rows.push({ source: "jira", label: t.key, title: t.summary, color: t.color, meta: t.status, url: t.url });
  }
  for (const t of State.googleTasks?.tasks ?? []) {
    const when = t.due ? formatDue(t.due) : "";
    rows.push({ source: "google", label: t.list, title: t.title, color: "", meta: when ? `Vence ${when}` : "Sem data", url: t.url });
  }
  return rows;
}

/** A task row. The chip carries the source: the Jira key with its status square,
 *  or a Google Tasks check with the list name. Clicking opens the task. */
function taskRow(t: TaskRow): HTMLElement {
  const chip =
    t.source === "jira"
      ? h(
          "span",
          { class: "hc-key", style: `--c:${jiraColor(t.color)}`, title: t.meta || t.label },
          h("i", { class: "hc-key-sq", title: t.meta }),
          h("span", { text: t.label }),
        )
      : h(
          "span",
          { class: "hc-key google", title: t.meta },
          h("i", { class: "hc-key-ic" }, svg(ICONS.checkCircle, 12, { stroke: 2.1 })),
          h("span", { text: t.label || "Pessoal" }),
        );
  const row = h(
    "div",
    { class: "hc-row task" },
    chip,
    h("div", { class: "hc-main" }, h("div", { class: "hc-title", title: t.title, text: t.title })),
  );
  if (t.url) {
    row.setAttribute("role", "button");
    row.tabIndex = 0;
    row.addEventListener("click", () => void Bridge.openUrl(t.url));
    row.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).key === "Enter") void Bridge.openUrl(t.url);
    });
  }
  return row;
}

/** "Hoje" / "Amanhã" / "qua., 08 de out." for a local date. */
function dayLabel(dt: Date): string {
  const now = new Date();
  const a = new Date(dt.getFullYear(), dt.getMonth(), dt.getDate());
  const b = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const diff = Math.round((a.getTime() - b.getTime()) / 86_400_000);
  if (diff === 0) return "Hoje";
  if (diff === 1) return "Amanhã";
  if (diff === -1) return "Ontem";
  return dt.toLocaleDateString("pt-BR", { weekday: "short", day: "2-digit", month: "short" });
}

function formatWhen(start: string, allDay: boolean): string {
  if (allDay) {
    const [y, m, d] = start.split("-").map(Number);
    return `${dayLabel(new Date(y, (m ?? 1) - 1, d ?? 1))} · dia inteiro`;
  }
  const dt = new Date(start);
  if (Number.isNaN(dt.getTime())) return start;
  const hh = String(dt.getHours()).padStart(2, "0");
  const mm = String(dt.getMinutes()).padStart(2, "0");
  return `${dayLabel(dt)}, ${hh}:${mm}`;
}

/** One event row: a meeting icon only when there is a join link (otherwise a
 *  plain calendar icon), title + when/place on one line, and an "Entrar" button
 *  that appears on hover — only for events that actually have a join link. */
function calendarRow(e: CalendarEvent): HTMLElement {
  const online = e.url !== "";
  const place = e.location || (online ? e.provider : "");
  return h(
    "div",
    { class: "hc-event" },
    h(
      "span",
      { class: online ? "hc-meet-icon" : "hc-meet-icon cal" },
      svg(online ? ICONS.video : ICONS.calendar, 15, online ? {} : { stroke: 1.6 }),
    ),
    h(
      "div",
      { class: "hc-main" },
      h("div", { class: "hc-title", title: e.title, text: e.title }),
      h(
        "div",
        { class: "hc-event-meta" },
        h("span", { class: "hc-event-when", text: formatWhen(e.start, e.allDay) }),
        place ? h("span", { class: "hc-event-place", text: place }) : null,
      ),
    ),
    online
      ? h(
          "button",
          {
            class: "hc-enter",
            type: "button",
            title: "Entrar",
            "aria-label": "Entrar",
            onclick: () => void Bridge.openUrl(e.url),
          },
          svg(ICONS.arrowUpRight, 12),
        )
      : null,
  );
}

/** "Próximos eventos" — the upcoming Google Calendar events, or a status line.
 *  The list scrolls inside the card when there are more than fit. */
function renderCalendarCard(body: HTMLElement, cal: CalendarNext | null) {
  clear(body);
  if (!cal) {
    body.append(h("div", { class: "hc-empty", text: IS_TAURI ? "Carregando agenda…" : "Conecte o Google Calendar para ver seus compromissos." }));
    return;
  }
  const events = cal.events ?? [];
  if (events.length === 0) {
    body.append(h("div", { class: "hc-empty", text: cal.error ?? "Nenhum compromisso nos próximos 7 dias." }));
    return;
  }
  const list = h("div", { class: "hc-events" });
  for (const e of events) list.append(calendarRow(e));
  body.append(list);
  if (cal.error) body.append(h("div", { class: "hc-empty", text: cal.error }));
}

/** Category id → accent colour for the news chip. */
const NEWS_COLORS: Record<string, string> = {
  tecnologia: "#3B9EFF",
  ia: "#A78BFA",
  dev: "#22D3EE",
  fintech: "#34D399",
  economia: "#F5A524",
  negocios: "#F472B6",
  mundo: "#8C8C8C",
  brasil: "#22C55E",
  ciencia: "#7C5CFF",
  esportes: "#F4505E",
};

/** "há 12 min" / "há 3 h" / "30 de set." for a `published_at` timestamp. */
function formatNewsDate(s: string): string {
  if (!s) return "";
  const dt = new Date(s.replace(" ", "T").replace(" UTC", "Z"));
  if (Number.isNaN(dt.getTime())) return s;
  const mins = Math.max(1, Math.round((Date.now() - dt.getTime()) / 60_000));
  if (mins < 60) return `há ${mins} min`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `há ${hours} h`;
  return dt.toLocaleDateString("pt-BR", { day: "2-digit", month: "short" });
}

/** One news slide: category, one-line title, two-line summary, source · date. */
function newsSlide(n: NewsItem): HTMLElement {
  const color = NEWS_COLORS[n.categoryId] ?? "#22D3EE";
  return h(
    "div",
    {
      class: "news-slide",
      title: n.url ? "Abrir no navegador" : "",
      onclick: () => {
        if (n.url) void Bridge.openUrl(n.url);
      },
    },
    h("div", { class: "news-cat", style: `color:${color}`, text: n.category.toUpperCase() }),
    h("div", { class: "news-title", title: n.title, text: n.title }),
    h("div", { class: "news-summary", text: n.summary || "—" }),
    h(
      "div",
      { class: "news-meta" },
      h("span", { text: n.source || "—" }),
      h("span", { class: "news-dot", text: "•" }),
      h("span", { text: formatNewsDate(n.publishedAt) }),
    ),
  );
}

/** Full-width command bar: Enter or the send button opens the chat tab. */
function buildCommandBar(actions: ViewActions): { el: HTMLElement; focus: () => void } {
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
  return {
    el: bar,
    focus: () => {
      input.focus();
      input.select();
    },
  };
}

function lighten(hex: string, amount: number): string {
  const v = parseInt(hex.replace("#", ""), 16);
  const c = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map((x) =>
    Math.min(255, Math.round(x + amount * 255)),
  );
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}

/** Integration pill — a mini Navi + label; click focuses that agent. */
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

/** MCP pill — replaces the agent pills on the dashboard. Green dot when enabled. */
function buildMcpPill(mcp: McpInfo): HTMLElement {
  const label = mcp.name.charAt(0).toUpperCase() + mcp.name.slice(1);
  return h(
    "div",
    { class: "pill mcp", title: `${mcp.source} · ${mcp.kind} · ${mcp.target}` },
    h("i", { class: "mcp-dot", style: `background:${mcp.enabled ? "#22C55E" : "#6B7079"}` }),
    h("span", { class: "lbl", text: label }),
  );
}

export function buildHome(actions: ViewActions): ViewHost {
  const pillsRow = h("div", { class: "home-pills" });
  const homeName = h("div", { class: "home-name" });
  const homeSub = h("div", { class: "home-sub", text: "Pronto para ajudar." });
  const identity = h(
    "div",
    { class: "home-identity" },
    h(
      "div",
      { class: "home-who" },
      homeName,
      homeSub,
    ),
    pillsRow,
  );

  const meetingBody = h("div", { class: "hc-meeting" });
  const suggestRow = h("div", { class: "home-suggest" });
  const taskRows = h("div", { class: "hc-rows tasks" });
  const refreshBtn = h(
    "button",
    { class: "hc-refresh", type: "button", title: "Atualizar tarefas do Jira" },
    svg(ICONS.refresh, 13, { stroke: 1.8 }),
  );
  const calRefreshBtn = h(
    "button",
    { class: "hc-refresh", type: "button", title: "Atualizar agenda" },
    svg(ICONS.refresh, 13, { stroke: 1.8 }),
  );

  // "Notícias do dia" — a full-width carousel above the grid, one slide at a time.
  const newsBody = h("div", { class: "news-body" });
  const newsPrev = h("button", { class: "hc-refresh", type: "button", title: "Anterior" }, svg(ICONS.chevronLeft, 13, { stroke: 2 }));
  const newsNext = h("button", { class: "hc-refresh", type: "button", title: "Próxima" }, svg(ICONS.chevronRight, 13, { stroke: 2 }));
  newsPrev.disabled = true;
  newsNext.disabled = true;
  const newsNav = h("div", { class: "news-nav" }, newsPrev, newsNext);
  const newsCard = homeCard(svg(ICONS.news, 13, { stroke: 1.7 }), "Notícias do dia", newsBody, "#22D3EE", newsNav);
  newsCard.classList.add("news-card");

  const commandBar = buildCommandBar(actions);

  const el = h(
    "div",
    { class: "view home" },
    identity,
    newsCard,
    h(
      "div",
      { class: "home-grid" },
      h(
        "div",
        { class: "home-col" },
        homeCard(svg(ICONS.calendar, 13, { stroke: 1.7 }), "Próximos eventos", meetingBody, "#7C5CFF", calRefreshBtn),
      ),
      h(
        "div",
        { class: "home-col" },
        homeCard(svg(ICONS.checkCircle, 13, { stroke: 1.7 }), "Minhas tarefas", taskRows, "#34D399", refreshBtn),
      ),
    ),
    suggestRow,
    commandBar.el,
  );

  /** Icon key from Rust → the SVG path, defaulting to the sparkle. */
  function suggestIcon(key: string): string {
    return (ICONS as Record<string, string>)[key] ?? ICONS.sparkle;
  }

  /** Freshly generated, cached, or the built-in set — never empty. */
  function currentSuggestions(): SuggestedAction[] {
    if (State.assistantSuggestions.length > 0) return State.assistantSuggestions;
    const cached = State.settings.assistantSuggestions;
    if (cached && cached.length > 0) return cached;
    return MOCK_SUGGESTIONS.map((s) => ({ icon: s.icon, label: s.label, prompt: s.prompt }));
  }

  function paintSuggestions() {
    clear(suggestRow);
    for (const s of currentSuggestions()) {
      suggestRow.append(
        h(
          "button",
          {
            class: "hc-suggest-btn",
            type: "button",
            title: s.label,
            onclick: () => actions.ask(s.prompt ?? s.label),
          },
          h("i", { class: "hc-suggest-icon" }, svg(suggestIcon(s.icon), 13, { stroke: 1.7 })),
          h("span", { class: "hc-suggest-label", text: s.label }),
          h("i", { class: "hc-chevron" }, svg(ICONS.chevronRight, 9, { stroke: 2.2 })),
        ),
      );
    }
  }
  paintSuggestions();

  let suggestKey = "";
  let suggestBusy = false;
  let lastSuggestCheck = 0;

  async function loadSuggestions(force: boolean) {
    if (!IS_TAURI || suggestBusy) return;
    suggestBusy = true;
    try {
      const result = await Bridge.assistantSuggestions(force);
      if (result && result.items.length > 0) State.setAssistantSuggestions(result.items);
    } finally {
      suggestBusy = false;
    }
  }

  let pillKey = "";
  let taskKey = "";

  // "Minhas tarefas": Jira (Atlassian MCP) + personal Google Tasks (Composio).
  // Each Rust side owns its own cache, so this only has to avoid calling too
  // often while the dashboard stays on screen.
  let tasksBusy = false;
  let lastTasksCheck = 0;

  async function loadTasks(force: boolean) {
    if (!IS_TAURI || tasksBusy) return;
    tasksBusy = true;
    refreshBtn.classList.add("spin");
    try {
      const [jira, google] = await Promise.all([
        Bridge.jiraTasks(force),
        Bridge.googleTasks(force),
      ]);
      if (jira) State.setJira(jira);
      if (google) State.setGoogleTasks(google);
    } finally {
      tasksBusy = false;
      refreshBtn.classList.remove("spin");
    }
  }
  refreshBtn.addEventListener("click", () => void loadTasks(true));

  // Google Calendar via the Composio MCP; the Rust side owns the 15-minute cache.
  let calKey = "";
  let calBusy = false;
  let lastCalCheck = 0;

  async function loadCalendar(force: boolean) {
    if (!IS_TAURI || calBusy) return;
    calBusy = true;
    calRefreshBtn.classList.add("spin");
    try {
      const result = await Bridge.calendarNext(force);
      if (result) State.setCalendar(result);
    } finally {
      calBusy = false;
      calRefreshBtn.classList.remove("spin");
    }
  }
  calRefreshBtn.addEventListener("click", () => void loadCalendar(true));

  // News carousel: one slide at a time; Rust owns the 45-minute cache.
  let newsIndex = 0;
  let newsKey = "";
  let newsBusy = false;
  let lastNewsCheck = 0;

  function renderNews() {
    clear(newsBody);
    const feed = State.news;
    const items = feed?.items ?? [];
    if (!feed) {
      newsBody.append(
        h("div", { class: "hc-empty", text: IS_TAURI ? "Carregando notícias…" : "Conecte o Composio para ver as notícias." }),
      );
      return;
    }
    if (items.length === 0) {
      newsBody.append(h("div", { class: "hc-empty", text: feed.error ?? "Nenhuma notícia hoje." }));
      return;
    }
    if (newsIndex >= items.length) newsIndex = 0;
    newsBody.append(newsSlide(items[newsIndex]));
  }

  function stepNews(delta: number) {
    const items = State.news?.items ?? [];
    if (items.length < 2) return;
    newsIndex = (newsIndex + delta + items.length) % items.length;
    newsKey = "";
    renderNews();
    actions.blip();
  }
  newsPrev.addEventListener("click", () => stepNews(-1));
  newsNext.addEventListener("click", () => stepNews(1));

  async function loadNews(force: boolean) {
    if (!IS_TAURI || newsBusy) return;
    newsBusy = true;
    try {
      const result = await Bridge.newsFeed(force);
      if (result) State.setNews(result);
    } finally {
      newsBusy = false;
    }
  }

  return {
    el,
    focus() {
      commandBar.focus();
    },
    sync() {
      // Identity: the user-chosen name, and the master instruction as a one-line
      // subtitle (the full text lives in Settings → Assistente).
      homeName.textContent = State.assistantName;
      const subtitle = (State.settings.aboutAssistant || State.settings.aboutUser || "")
        .trim()
        .split("\n")[0];
      homeSub.textContent = subtitle || "Pronto para ajudar.";

      // Suggestions: repaint when the set changes, and regenerate at most once
      // an hour from the dashboard (Rust decides whether that hits the network).
      const suggestionKey = currentSuggestions()
        .map((s) => `${s.icon}:${s.label}:${s.prompt ?? ""}`)
        .join("|");
      if (suggestionKey !== suggestKey) {
        suggestKey = suggestionKey;
        paintSuggestions();
      }
      const snow = performance.now();
      if (!suggestBusy && (lastSuggestCheck === 0 || snow - lastSuggestCheck > 60_000)) {
        lastSuggestCheck = snow;
        void loadSuggestions(false);
      }

      // The row shows the MCP servers when there are any; the agent pills remain
      // as a fallback so the dashboard is never empty.
      const mcps = State.mcps;
      if (mcps.length > 0) {
        const mKey = "mcp|" + mcps.map((m) => `${m.name}:${m.enabled}:${m.kind}:${m.source}`).join("|");
        if (mKey !== pillKey) {
          pillKey = mKey;
          clear(pillsRow);
          for (const m of mcps.slice(0, 6)) pillsRow.append(buildMcpPill(m));
        }
      } else {
        const pills = State.tasks.filter((t) => t.id !== "integration_opencode");
        const pKey = "agents|" + pills.map((t) => `${t.id}:${t.state}:${t.name}:${t.color}:${t.pillBadge ?? ""}`).join("|");
        if (pKey !== pillKey) {
          pillKey = pKey;
          clear(pillsRow);
          for (const t of pills) pillsRow.append(buildPill(t, actions));
          pruneMiniBots();
        }
      }

      // Tasks: at most one automatic check a minute while the card is up (each
      // Rust side decides whether that means a network call, via its own TTL).
      const now = performance.now();
      if (!tasksBusy && (lastTasksCheck === 0 || now - lastTasksCheck > 60_000)) {
        lastTasksCheck = now;
        void loadTasks(false);
      }

      const jira = State.jira;
      const google = State.googleTasks;
      const tKey = [
        jira ? `${jira.fetchedAt}:${jira.cached}:${jira.error ?? ""}:${jira.tasks.map((t) => `${t.key}:${t.status}`).join("|")}` : "",
        google ? `${google.fetchedAt}:${google.cached}:${google.error ?? ""}:${google.tasks.map((t) => `${t.id}:${t.title}:${t.due}`).join("|")}` : "",
      ].join("#");
      if (tKey !== taskKey) {
        taskKey = tKey;
        const rows = buildTaskRows();
        clear(taskRows);
        if (!jira && !google) {
          taskRows.append(
            h("div", { class: "hc-empty", text: IS_TAURI ? "Carregando tarefas…" : "Conecte Jira e Google Tasks para ver suas tarefas." }),
          );
        } else if (rows.length === 0) {
          taskRows.append(h("div", { class: "hc-empty", text: jira?.error ?? google?.error ?? "Nenhuma tarefa no momento." }));
        } else {
          // The whole list, not just the first few: the card scrolls internally.
          for (const t of rows) taskRows.append(taskRow(t));
          const err = jira?.error ?? google?.error;
          if (err) taskRows.append(h("div", { class: "hc-empty", text: err }));
        }
      }

      // Next appointment: at most one automatic check a minute while on screen.
      const cnow = performance.now();
      if (!calBusy && (lastCalCheck === 0 || cnow - lastCalCheck > 60_000)) {
        lastCalCheck = cnow;
        void loadCalendar(false);
      }
      const cal = State.calendar;
      const cKey = cal
        ? `${cal.fetchedAt}:${cal.cached}:${cal.error ?? ""}:${(cal.events ?? []).map((e) => `${e.start}:${e.title}`).join("|")}`
        : "";
      if (cKey !== calKey) {
        calKey = cKey;
        renderCalendarCard(meetingBody, cal);
      }

      // News: at most one automatic check a minute while on screen.
      const nnow = performance.now();
      if (!newsBusy && (lastNewsCheck === 0 || nnow - lastNewsCheck > 60_000)) {
        lastNewsCheck = nnow;
        void loadNews(false);
      }
      const feed = State.news;
      const nKey = feed
        ? `${feed.fetchedAt}:${feed.cached}:${feed.error ?? ""}:${feed.items.map((i) => i.title).join("|")}`
        : "";
      const fullKey = `${nKey}:${newsIndex}`;
      if (fullKey !== newsKey) {
        newsKey = fullKey;
        const many = (feed?.items.length ?? 0) > 1;
        newsPrev.disabled = !many;
        newsNext.disabled = !many;
        renderNews();
      }
    },
  };
}
