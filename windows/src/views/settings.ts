// Settings view — the Noma-style panel that lives inside the island. A sidebar
// of categories, cards of controls, and a footer. Preferences save immediately
// (there is no draft); the footer's "Salvar alterações" simply confirms and
// returns home, and "Restaurar padrão" restores the defaults.
//
// This replaces the old separate settings window (settings.html + src/settings),
// so every write-to-disk flow (hooks, plugin, secrets) is confirmed here.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import {
  Bridge,
  type ChatStatus,
  type HookStatus,
  type McpInfo,
  type NewsCategory,
  type OpencodeStatus,
} from "../core/bridge";
import { State, DEFAULT_SETTINGS, type Settings } from "../core/state";
import { COMPACT_W_MAX, COMPACT_W_MIN, clampCompactWidth } from "../core/layout";
import type { ViewActions, ViewHost } from "./views";

type SectionId = "general" | "assistant" | "integrations" | "appearance" | "chat";

/** Mutate State.settings and persist. Rust echoes `settings-changed` back, which
 *  applies the live effects (sound, geometry, integrations). */
function persist() {
  void Bridge.saveSettings(State.settings);
  State.loadIntegrationTasks();
  State.notify();
}

// ── Reusable controls ─────────────────────────────────────────────────────────

function switchEl(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const el = h("button", {
    class: on ? "switch on" : "switch",
    type: "button",
    "aria-pressed": on,
  });
  el.addEventListener("click", () => {
    const next = !el.classList.contains("on");
    el.classList.toggle("on", next);
    onChange(next);
  });
  return el;
}

function statusDot(ok: boolean): HTMLElement {
  return h("i", { class: "sc-dot", style: `background:${ok ? "var(--green)" : "var(--red)"}` });
}

function field(label: string, ...controls: Node[]): HTMLElement {
  return h("div", { class: "sc-field" }, h("label", { text: label }), ...controls);
}

function textInput(placeholder: string, value = "", type = "text"): HTMLInputElement {
  return h("input", {
    type,
    class: "sc-input",
    placeholder,
    value,
    autocomplete: "off",
    spellcheck: "false",
  }) as HTMLInputElement;
}

/** Text fields need WebView keyboard focus while they are being typed in. */
function attachFocus(input: HTMLInputElement | HTMLTextAreaElement, actions: ViewActions) {
  input.addEventListener("focus", () => actions.focusWindow(true));
  input.addEventListener("blur", () => actions.focusWindow(false));
  input.addEventListener("keydown", (e) => e.stopPropagation());
}

function renderDiff(text: string): HTMLElement {
  const box = h("div", { class: "sc-diff" });
  for (const line of text.split("\n")) {
    const cls = line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "ctx";
    box.append(h("div", { class: cls, text: line }));
  }
  return box;
}

/** A Noma-style section card: icon, title, subtitle, then its body. */
function card(icon: string, title: string, sub: string, body: HTMLElement): HTMLElement {
  return h(
    "div",
    { class: "settings-card" },
    h(
      "div",
      { class: "sc-head" },
      h("i", { class: "sc-icon" }, svg(icon, 15, { stroke: 1.7 })),
      h("div", { class: "sc-titles" }, h("b", { text: title }), h("span", { text: sub })),
    ),
    body,
  );
}

// ── Integrations ──────────────────────────────────────────────────────────────

interface IntegrationDef {
  id: string;
  name: string;
  color: string;
  fields: { key: string; label: string; placeholder: string; secret: boolean }[];
}

const INTEGRATIONS: IntegrationDef[] = [
  { id: "integration_stripe", name: "Stripe", color: "#0570DE",
    fields: [{ key: "stripe-api-key", label: "Secret key", placeholder: "sk_live_…", secret: true }] },
  { id: "integration_github", name: "GitHub", color: "#F4505E",
    fields: [{ key: "github-token", label: "Token", placeholder: "ghp_…", secret: true }] },
  { id: "integration_vercel", name: "Vercel", color: "#7C5CFF",
    fields: [{ key: "vercel-token", label: "Token", placeholder: "…", secret: true }] },
  { id: "integration_n8n", name: "n8n", color: "#F29B38",
    fields: [
      { key: "n8n-url", label: "URL", placeholder: "https://n8n.example.com", secret: false },
      { key: "n8n-api-key", label: "API key", placeholder: "…", secret: true },
    ] },
  { id: "integration_resend", name: "Resend", color: "#22C55E",
    fields: [{ key: "resend-api-key", label: "API key", placeholder: "re_…", secret: true }] },
  { id: "integration_notion", name: "Notion", color: "#8C8C8C",
    fields: [{ key: "notion-api-key", label: "Token", placeholder: "ntn_…", secret: true }] },
  { id: "integration_calcom", name: "Cal.com", color: "#C9956A",
    fields: [{ key: "calcom-api-key", label: "API key", placeholder: "cal_…", secret: true }] },
];

const SECRET_KEYS = [
  "stripe-api-key", "github-token", "vercel-token",
  "n8n-url", "n8n-api-key", "resend-api-key", "notion-api-key", "calcom-api-key",
];

const MAX_ACTIVE = 4;

// ── Chat ──────────────────────────────────────────────────────────────────────

const MODELS: [string, string][] = [
  ["claude-opus-5", "Claude Opus 5"],
  ["claude-sonnet-5", "Claude Sonnet 5"],
  ["claude-haiku-4-5", "Claude Haiku 4.5"],
];

// ── Appearance ────────────────────────────────────────────────────────────────

const EXTRA_AGENT_COLORS: { id: string; name: string; color: string }[] = [
  { id: "integration_claude", name: "VS Code", color: "#F5F6F8" },
  { id: "integration_opencode", name: "opencode", color: "#FF6B5B" },
];

const MOCHI_DEFAULT_COLOR = "#EDEDEF";

// ── View ──────────────────────────────────────────────────────────────────────

export function buildSettings(actions: ViewActions): ViewHost {
  let loaded = false;
  let section: SectionId = "general";

  const pane: Record<SectionId, HTMLElement> = {
    general: h("div", { class: "sc-pane" }),
    assistant: h("div", { class: "sc-pane" }),
    integrations: h("div", { class: "sc-pane" }),
    appearance: h("div", { class: "sc-pane" }),
    chat: h("div", { class: "sc-pane" }),
  };

  const navItem = (id: SectionId, icon: string, label: string, sub: string) => {
    const btn = h(
      "button",
      { class: "sn-item", type: "button", onclick: () => go(id) },
      h("i", { class: "sn-icon" }, svg(icon, 15, { stroke: 1.7 })),
      h("span", { class: "sn-text" }, h("span", { class: "sn-label", text: label }), h("span", { class: "sn-sub", text: sub })),
    );
    (btn as HTMLElement & { __id?: SectionId }).__id = id;
    return btn;
  };

  const navItems = [
    navItem("general", ICONS.gear, "Geral", "Aparência e comportamento"),
    navItem("assistant", ICONS.sparkle, "Assistente", "Quem você é, como ela age e sugestões"),
    navItem("integrations", ICONS.stack, "Integrações", "Apps e serviços conectados"),
    navItem("appearance", ICONS.sparkle, "Aparência", "Cores do Mochi e dos agentes"),
    navItem("chat", ICONS.bubble, "Chat", "Quem responde no notch"),
  ];

  const nav = h(
    "div",
    { class: "settings-nav" },
    h("div", { class: "sn-title", text: "Configurações" }),
    ...navItems,
  );

  const scroll = h(
    "div",
    { class: "sc-scroll" },
    pane.general,
    pane.assistant,
    pane.integrations,
    pane.appearance,
    pane.chat,
  );

  const reset = h("button", {
    class: "settings-reset",
    type: "button",
    text: "Restaurar padrão",
    onclick: () => {
      State.settings = {
        ...DEFAULT_SETTINGS,
        hooksInstalled: State.settings.hooksInstalled,
        agentColors: {},
        activeIntegrations: [...DEFAULT_SETTINGS.activeIntegrations],
        assistantSuggestions: State.settings.assistantSuggestions,
      };
      persist();
      renderAll();
      actions.blip();
    },
  });
  const save = h(
    "button",
    { class: "settings-save", type: "button", onclick: () => { persist(); actions.blip(); actions.setView(State.defaultView()); } },
    svg(ICONS.check, 12, { stroke: 2.4 }),
    h("span", { text: "Salvar alterações" }),
  );

  const content = h(
    "div",
    { class: "settings-content" },
    h(
      "div",
      { class: "settings-head" },
      h("h2", { text: "Configurações" }),
      h("p", { text: "Ajuste o comportamento do Mochi." }),
    ),
    scroll,
    h("div", { class: "settings-footer" }, reset, save),
  );

  const el = h("div", { class: "view settings" }, h("div", { class: "settings-shell" }, nav, content));

  function go(id: SectionId) {
    section = id;
    actions.blip();
    paintNav();
    if (id === "integrations") void refreshIntegrations();
    if (id === "chat") void refreshChat();
    if (id === "assistant") {
      renderAssistant();
      if (newsCats.length === 0) void refreshNewsCats();
    }
  }

  function paintNav() {
    for (const item of navItems) {
      const id = (item as HTMLElement & { __id?: SectionId }).__id;
      item.classList.toggle("on", id === section);
    }
    for (const [id, el] of Object.entries(pane)) el.classList.toggle("on", id === section);
  }

  // ── Section builders ────────────────────────────────────────────────────────

  function renderGeneral() {
    clear(pane.general);
    const s = State.settings;

    const volume = h("input", {
      type: "range", min: "0", max: "0.2", step: "0.005",
      class: "sc-range",
      value: String(s.soundVolume),
    }) as HTMLInputElement;
    volume.disabled = !s.soundEnabled;
    volume.style.opacity = s.soundEnabled ? "1" : "0.4";
    volume.addEventListener("input", () => actions.setVolume(Number(volume.value)));

    const soundRow = h(
      "div",
      { class: "sc-field" },
      switchEl(s.soundEnabled, (v) => {
        actions.toggleSound();
        volume.disabled = !v;
        volume.style.opacity = v ? "1" : "0.4";
      }),
      h("label", { text: "Som" }),
      volume,
    );

    const autoClose = h("input", {
      type: "number", min: "5", max: "120", step: "1",
      class: "sc-input sc-number",
      value: String(Math.round(s.autoCloseInterval)),
    }) as HTMLInputElement;
    attachFocus(autoClose, actions);
    autoClose.addEventListener("change", () => {
      const v = Math.max(5, Math.min(120, Number(autoClose.value) || 15));
      autoClose.value = String(v);
      actions.setAutoClose(v);
    });

    const widthLabel = h("span", { class: "sc-hint", text: `${clampCompactWidth(s.compactWidth)} px` });
    const compactWidth = h("input", {
      type: "range",
      min: String(COMPACT_W_MIN), max: String(COMPACT_W_MAX), step: "4",
      class: "sc-range",
      value: String(clampCompactWidth(s.compactWidth)),
    }) as HTMLInputElement;
    compactWidth.addEventListener("input", () => {
      State.settings.compactWidth = clampCompactWidth(Number(compactWidth.value));
      widthLabel.textContent = `${State.settings.compactWidth} px`;
      persist();
    });

    const screen = h("select", { class: "sc-input" }) as HTMLSelectElement;
    screen.append(
      h("option", { value: "primary", text: "Tela principal" }),
      h("option", { value: "cursor", text: "Tela sob o cursor" }),
    );
    screen.value = s.screen;
    screen.addEventListener("change", () => {
      State.settings.screen = screen.value as Settings["screen"];
      persist();
    });

    pane.general.append(
      card(ICONS.gear, "Aparência e comportamento", "Como o Mochi se comporta no seu dia a dia.", h(
        "div",
        { class: "sc-body" },
        soundRow,
        field("Fechar sozinho", autoClose, h("span", { class: "sc-hint", text: "segundos após sair da ilha" })),
        switchRow("Manter visível", "nunca fechar sozinho", s.keepVisible, (v) => { State.settings.keepVisible = v; persist(); }),
        field("Largura compacta", compactWidth, widthLabel),
        field("Vive na", screen),
        switchRow("Iniciar com o Windows", "abrir junto com o sistema", s.autostart, (v) => { State.settings.autostart = v; persist(); }),
      )),
    );
  }

  // ── Assistant section ───────────────────────────────────────────────────────

  /** A multiline field with the same styling as `.sc-input`. */
  function textArea(placeholder: string, value: string, rows: number): HTMLTextAreaElement {
    const el = h("textarea", {
      class: "sc-input sc-textarea",
      placeholder,
      rows,
      spellcheck: "true",
      autocomplete: "off",
    }) as HTMLTextAreaElement;
    el.value = value;
    return el;
  }

  function renderAssistant() {
    clear(pane.assistant);
    const s = State.settings;

    // ── Identity card: name + "about me" + "about the assistant" ──────────────
    const name = textInput("Mochi", s.assistantName);
    attachFocus(name, actions);

    const aboutUser = textArea(
      "Quem você é, o que faz, o que sabe, o que gosta…",
      s.aboutUser,
      5,
    );
    attachFocus(aboutUser, actions);

    const aboutAssistant = textArea(
      "Como ela deve se comportar, o que priorizar, o tom, o que evitar…",
      s.aboutAssistant,
      5,
    );
    attachFocus(aboutAssistant, actions);

    const saveBtn = h("button", { class: "sc-btn primary", type: "button", text: "Salvar" });
    saveBtn.addEventListener("click", async () => {
      State.settings.assistantName = name.value.trim();
      State.settings.aboutUser = aboutUser.value.trim();
      State.settings.aboutAssistant = aboutAssistant.value.trim();
      persist();
      actions.blip();
      // Nome/instruções mudaram: as sugestões antigas não valem mais.
      await regenerateSuggestions(true);
      renderAssistant();
    });

    pane.assistant.append(
      card(ICONS.sparkle, "Identidade", "Quem é você e como o assistente deve se comportar.", h(
        "div",
        { class: "sc-body" },
        field("Nome", name),
        h(
          "div",
          { class: "sc-field" },
          h("label", { text: "Sobre mim" }),
          aboutUser,
        ),
        h(
          "div",
          { class: "sc-field" },
          h("label", { text: "Sobre a assistente" }),
          aboutAssistant,
        ),
        h("div", { class: "sc-hint", text: "As duas vão junto de cada resposta — e orientam as sugestões e as consultas que ela faz (Jira, agenda, notícias…)." }),
        h("div", { class: "sc-actions" }, saveBtn),
      )),
    );

    // ── Suggestions card ──────────────────────────────────────────────────────
    const grid = h("div", { class: "sc-suggest-preview" });
    const items = State.assistantSuggestions.length > 0
      ? State.assistantSuggestions
      : s.assistantSuggestions;
    if (State.suggestionsLoading) {
      grid.append(h("div", { class: "sc-hint", text: "Gerando sugestões…" }));
    } else if (items.length === 0) {
      grid.append(h("div", { class: "sc-hint", text: "Nenhuma sugestão ainda. Clique em “Regerar”." }));
    } else {
      for (const item of items) {
        grid.append(h(
          "div",
          { class: "sc-suggest-chip", title: item.prompt ?? "" },
          h("i", {}, svg(ICONS.sparkle, 12, { stroke: 1.7 })),
          h("span", { text: item.label }),
        ));
      }
    }

    const regen = h("button", { class: "sc-btn", type: "button", text: "Regerar sugestões" });
    regen.addEventListener("click", async () => {
      regen.disabled = true;
      grid.replaceChildren(h("div", { class: "sc-hint", text: "Gerando sugestões…" }));
      await regenerateSuggestions(true);
      regen.disabled = false;
      renderAssistant();
    });

    pane.assistant.append(
      card(ICONS.list, "Sugestões da dash", "As ações rápidas que aparecem na tela inicial, geradas a partir do nome e da instrução.", h(
        "div",
        { class: "sc-body" },
        grid,
        h("div", { class: "sc-actions" }, regen),
      )),
    );

    pane.assistant.append(newsCard());
  }

  // ── News categories card ────────────────────────────────────────────────────

  function newsCard(): HTMLElement {
    const body = h("div", { class: "sc-body" });
    body.append(h("div", { class: "sc-hint", text: "Escolha as categorias que aparecem no carrossel “Notícias do dia”, no topo da dash (até 6)." }));
    const chips = h("div", { class: "sc-chips" });
    if (newsCats.length === 0) {
      chips.append(h("span", { class: "sc-hint", text: "Carregando categorias…" }));
    } else {
      for (const c of newsCats) {
        const on = State.settings.newsCategories.includes(c.id);
        const chip = h("button", { class: on ? "sc-chip on" : "sc-chip", type: "button", text: c.label });
        chip.addEventListener("click", () => {
          const cur = State.settings.newsCategories ?? [];
          State.settings.newsCategories = cur.includes(c.id) ? cur.filter((x) => x !== c.id) : [...cur, c.id];
          chip.classList.toggle("on", State.settings.newsCategories.includes(c.id));
          persist();
        });
        chips.append(chip);
      }
    }
    body.append(chips);
    return card(ICONS.news, "Notícias do dia", "Categorias exibidas no carrossel do topo da dash.", body);
  }

  async function refreshNewsCats() {
    newsCats = (await Bridge.newsCategories()) ?? [];
    if (section === "assistant") renderAssistant();
  }

  /** Calls Rust and stores the result; falls back silently on error. */
  async function regenerateSuggestions(force: boolean) {
    State.suggestionsLoading = true;
    State.notify();
    const result = await Bridge.assistantSuggestions(force);
    if (result && result.items.length > 0) {
      State.setAssistantSuggestions(result.items, false);
    } else {
      State.suggestionsLoading = false;
      State.notify();
    }
  }

  function switchRow(label: string, sub: string, on: boolean, onChange: (v: boolean) => void): HTMLElement {
    return h(
      "div",
      { class: "sc-switch-row" },
      h("div", { class: "sc-switch-text" }, h("span", { class: "sc-switch-label", text: label }), h("span", { class: "sc-hint", text: sub })),
      switchEl(on, onChange),
    );
  }

  function renderAppearance() {
    clear(pane.appearance);
    State.settings.agentColors ??= {};
    const list = h("div", { class: "sc-colors" });

    function colorRow(name: string, fallback: string, get: () => string, set: (v: string) => void): HTMLElement {
      const input = h("input", {
        type: "color",
        class: "sc-color",
        value: get() || fallback,
        title: `Padrão ${fallback}`,
      }) as HTMLInputElement;
      const resetBtn = h("button", { class: "sc-color-reset", type: "button", text: "Reset" });
      const sync = () => { resetBtn.style.display = get() ? "" : "none"; };
      input.addEventListener("input", () => { set(input.value); sync(); persist(); });
      resetBtn.addEventListener("click", () => { set(""); input.value = fallback; sync(); persist(); });
      sync();
      return h("div", { class: "sc-color-row" }, input, h("span", { class: "sc-color-name", text: name }), resetBtn);
    }

    for (const def of [...EXTRA_AGENT_COLORS, ...INTEGRATIONS]) {
      list.append(colorRow(def.name, def.color, () => State.settings.agentColors[def.id] ?? "", (v) => {
        if (v) State.settings.agentColors[def.id] = v; else delete State.settings.agentColors[def.id];
      }));
    }
    list.append(colorRow("Mochi principal", MOCHI_DEFAULT_COLOR, () => State.settings.mochiColor, (v) => { State.settings.mochiColor = v; }));

    pane.appearance.append(
      card(ICONS.sparkle, "Mochi e cores dos agentes", "Colora o Mochi principal e cada pill. Aplicado na hora.", h("div", { class: "sc-body" }, list)),
    );
  }

  // ── Chat section ────────────────────────────────────────────────────────────

  function renderChat() {
    clear(pane.chat);
    const provider = State.settings.chatProvider === "opencode" ? "opencode" : "claude";

    const claudeBtn = h("button", { class: provider === "claude" ? "sc-seg on" : "sc-seg", type: "button", text: "Claude API" });
    const opencodeBtn = h("button", { class: provider === "opencode" ? "sc-seg on" : "sc-seg", type: "button", text: "opencode CLI" });
    claudeBtn.addEventListener("click", () => { State.settings.chatProvider = "claude"; persist(); renderChat(); });
    opencodeBtn.addEventListener("click", () => { State.settings.chatProvider = "opencode"; persist(); renderChat(); });

    const providerBody = h(
      "div",
      { class: "sc-body" },
      h("div", { class: "sc-seg-row" }, claudeBtn, opencodeBtn),
    );

    if (provider === "opencode") {
      const status = chatStatus;
      providerBody.append(h("div", {
        class: status?.binResolved ? "sc-notice ok" : "sc-notice warn",
        text: status?.binResolved
          ? `Encontrado: ${status.binResolved}`
          : "opencode não encontrado no PATH. Instale (opencode.ai) ou cole o caminho abaixo.",
      }));

      const bin = textInput("opencode.exe (opcional — detectado automaticamente)", State.settings.opencodeBin);
      attachFocus(bin, actions);
      const binSave = h("button", { class: "sc-btn", type: "button", text: "Salvar" });
      binSave.addEventListener("click", () => { State.settings.opencodeBin = bin.value.trim(); persist(); renderChat(); });

      const model = textInput("provider/modelo (opcional)", State.settings.opencodeModel);
      attachFocus(model, actions);
      model.addEventListener("change", () => { State.settings.opencodeModel = model.value.trim(); persist(); });

      providerBody.append(
        field("Binário", bin, binSave),
        field("Modelo", model),
        h("div", { class: "sc-hint", text: "A primeira mensagem de cada conversa diz quem é o Mochi; as seguintes continuam a mesma sessão do opencode." }),
      );
    }

    pane.chat.append(
      card(ICONS.bubble, "Provedor", "Quem responde a partir do notch.", providerBody),
    );

    // Claude API key + model
    const keyPresent = secrets["anthropic-api-key"] ?? false;
    const state = h("span", { class: "sc-hint", text: keyPresent ? "Chave salva no Gerenciador de Credenciais." : "Nenhuma chave ainda — o chat fica mudo." });
    const keyField = textInput(keyPresent ? "••••••••••••  (salva)" : "sk-ant-...", "", "password");
    attachFocus(keyField, actions);
    const keyDot = statusDot(keyPresent);
    const feedback = h("div", {});
    const saveKey = h("button", { class: "sc-btn primary", type: "button", text: "Salvar chave" });
    const clearKey = h("button", { class: "sc-btn danger", type: "button", text: "Remover" });
    clearKey.style.display = keyPresent ? "" : "none";

    saveKey.addEventListener("click", async () => {
      const value = keyField.value.trim();
      if (!value) return;
      clear(feedback);
      try {
        await Bridge.secretSet("anthropic-api-key", value);
        keyField.value = "";
        secrets["anthropic-api-key"] = true;
        feedback.append(h("div", { class: "sc-notice ok", text: "Salva. Nunca toca o disco." }));
        renderChat();
      } catch (err) {
        feedback.append(h("div", { class: "sc-notice err", text: `Não foi possível salvar: ${String(err)}` }));
      }
    });
    clearKey.addEventListener("click", async () => {
      clear(feedback);
      try {
        await Bridge.secretClear("anthropic-api-key");
        secrets["anthropic-api-key"] = false;
        feedback.append(h("div", { class: "sc-notice ok", text: "Chave removida." }));
        renderChat();
      } catch (err) {
        feedback.append(h("div", { class: "sc-notice err", text: `Não foi possível remover: ${String(err)}` }));
      }
    });

    const model = h("select", { class: "sc-input" }) as HTMLSelectElement;
    for (const [id, label] of MODELS) model.append(h("option", { value: id, text: label }));
    if (!MODELS.some(([id]) => id === State.settings.model)) {
      model.append(h("option", { value: State.settings.model, text: State.settings.model }));
    }
    model.value = State.settings.model;
    model.addEventListener("change", () => { State.settings.model = model.value; persist(); });

    pane.chat.append(
      card(ICONS.clipboard, "Claude", "Chave Anthropic usada pelo provedor Claude API.", h(
        "div",
        { class: "sc-body" },
        h("div", { class: "sc-field" }, keyDot, state),
        field("Chave de API", keyField, saveKey, clearKey),
        field("Modelo", model),
        feedback,
      )),
    );
  }

  // ── Integrations section ────────────────────────────────────────────────────

  function renderIntegrations() {
    clear(pane.integrations);
    pane.integrations.append(mcpCard(), hooksCard(), opencodeCard(), integrationsCard());
  }

  // ── Available MCP servers card ──────────────────────────────────────────────

  function mcpCard(): HTMLElement {
    const body = h("div", { class: "sc-body" });
    if (mcps.length === 0) {
      body.append(h("div", { class: "sc-hint", text: "Nenhum MCP encontrado. O chat do Mochi provê Jira (Atlassian) e Intercom; adicione outros em %LOCALAPPDATA%\\Coucou\\chat\\opencode.json." }));
    } else {
      for (const mcp of mcps) {
        const name = mcp.name.charAt(0).toUpperCase() + mcp.name.slice(1);
        body.append(
          h(
            "div",
            { class: "sc-int-item" },
            h(
              "div",
              { class: "sc-int-head" },
              h("i", { class: "sc-dot-c", style: `background:${mcp.enabled ? "#22C55E" : "#6B7079"}` }),
              h("span", { text: name }),
              h("span", { class: "sc-hint", style: "flex:0 0 auto;margin-left:auto", text: `${mcp.source} · ${mcp.kind}` }),
            ),
            h("span", { class: "sc-hint", text: mcp.target }),
          ),
        );
      }
    }
    return card(ICONS.stack, "MCPs disponíveis", "Servidores acessíveis ao chat do Mochi.", body);
  }

  // ── Claude Code hooks card ──────────────────────────────────────────────────

  function hooksCard(): HTMLElement {
    const body = h("div", { class: "sc-body" });

    const rebuild = async () => {
      const fresh = await Bridge.hooksStatus();
      if (fresh) hooksStatus = fresh;
      draw();
    };

    function draw() {
      clear(body);
      body.append(
        h("div", { class: "sc-hint", text: hooksStatus.installed
          ? "O Coucou está conectado às suas sessões do Claude Code: chamadas de ferramenta, perguntas e permissões aparecem na ilha."
          : "Instale os hooks para ver suas sessões do Claude Code na ilha e aprovar permissões sem sair do que está fazendo." }),
        h("div", { class: "sc-path" }, h("span", { class: "sc-path-k", text: "settings.json" }), h("span", { class: "sc-path-v", text: hooksStatus.settingsPath })),
        h("div", { class: "sc-path" }, h("span", { class: "sc-path-k", text: "Relay" }), h("span", { class: "sc-path-v", text: hooksStatus.hookPath }), statusDot(hooksStatus.hookReady)),
      );
      if (!hooksStatus.hookReady) {
        body.append(h("div", { class: "sc-notice warn", text: "coucou-hook.exe ainda não está no lugar. Reinicie o Coucou; se persistir, compile com `cargo build -p coucou-hook`." }));
      }
      const row = h("div", { class: "sc-actions" });
      const install = h("button", { class: "sc-btn primary", type: "button", text: hooksStatus.installed ? "Reinstalar hooks…" : "Instalar hooks…" });
      if (!hooksStatus.hookReady) { install.disabled = true; install.title = "O relay ainda não foi instalado."; }
      install.addEventListener("click", () => preview(true));
      row.append(install);
      if (hooksStatus.installed) {
        const uninstall = h("button", { class: "sc-btn danger", type: "button", text: "Desinstalar hooks…" });
        uninstall.addEventListener("click", () => preview(false));
        row.append(uninstall);
      }
      body.append(row);
    }

    async function preview(install: boolean) {
      let p;
      try {
        p = await Bridge.hooksPreview(install);
      } catch (err) {
        clear(body);
        body.append(
          h("div", { class: "sc-notice err", text: String(err).replace(/^Error:\s*/, "") }),
          h("div", { class: "sc-actions" }, backButton(draw)),
        );
        return;
      }
      clear(body);
      body.append(
        h("div", { class: "sc-hint", text: install
          ? "Isto é exatamente o que vai mudar no seu settings.json. Seus próprios hooks ficam intactos."
          : "Isto remove apenas as entradas do Coucou. Seus próprios hooks ficam intactos." }),
        renderDiff(p.diff),
        h("div", { class: "sc-path" }, h("span", { class: "sc-path-k", text: "Backup" }), h("span", { class: "sc-path-v", text: p.backup })),
      );
      const confirm = h("button", { class: install ? "sc-btn primary" : "sc-btn danger", type: "button", text: install ? "Fazer backup e escrever" : "Fazer backup e remover" });
      confirm.addEventListener("click", async () => {
        confirm.disabled = true;
        try {
          const backup = await Bridge.hooksApply(install, p.fingerprint);
          clear(body);
          body.append(h("div", { class: "sc-notice ok", text: `Pronto. Configurações anteriores salvas em ${backup}. Abra uma nova sessão do Claude Code para ativar.` }));
          State.settings.hooksInstalled = install;
          window.setTimeout(() => void rebuild(), 2600);
        } catch (err) {
          confirm.disabled = false;
          body.append(h("div", { class: "sc-notice err", text: `Não foi possível escrever: ${String(err)}` }));
        }
      });
      body.append(h("div", { class: "sc-actions" }, confirm, backButton(draw)));
    }

    draw();
    return card(ICONS.terminal, "Claude Code", "Sessões, perguntas e permissões na ilha.", body);
  }

  // ── opencode plugin card ────────────────────────────────────────────────────

  function opencodeCard(): HTMLElement {
    const body = h("div", { class: "sc-body" });

    const rebuild = async () => {
      const fresh = await Bridge.opencodeStatus();
      if (fresh) opencodeStatus = fresh;
      draw();
    };

    function draw() {
      clear(body);
      const status = opencodeStatus;
      const hint = status.installed
        ? status.needsUpdate
          ? `Plugin v${status.installedVersion} instalado, v${status.bundledVersion} empacotado — atualize para receber os últimos eventos.`
          : "O Coucou está conectado às suas sessões do opencode: chamadas de ferramenta, perguntas e permissões aparecem na ilha."
        : "Instale o plugin para ver suas sessões do opencode na ilha e aprovar permissões sem sair do que está fazendo.";
      body.append(
        h("div", { class: "sc-hint", text: hint }),
        h("div", { class: "sc-path" }, h("span", { class: "sc-path-k", text: "Plugin" }), h("span", { class: "sc-path-v", text: status.pluginPath })),
        h("div", { class: "sc-path" }, h("span", { class: "sc-path-k", text: "Relay" }), h("span", { class: "sc-path-v", text: status.relayReady ? "coucou-hook.exe pronto" : "coucou-hook.exe ausente" }), statusDot(status.relayReady)),
      );
      if (!status.relayReady) {
        body.append(h("div", { class: "sc-notice warn", text: "coucou-hook.exe ainda não está no lugar. Reinicie o Coucou; se persistir, compile com `cargo build -p coucou-hook`." }));
      }
      const row = h("div", { class: "sc-actions" });
      const install = h("button", { class: "sc-btn primary", type: "button", text: status.installed ? "Reinstalar plugin…" : "Instalar plugin…" });
      if (!status.relayReady) { install.disabled = true; install.title = "O relay ainda não foi instalado."; }
      install.addEventListener("click", () => preview(true));
      row.append(install);
      if (status.installed) {
        const uninstall = h("button", { class: "sc-btn danger", type: "button", text: "Desinstalar plugin…" });
        uninstall.addEventListener("click", () => preview(false));
        row.append(uninstall);
      }
      body.append(row);
    }

    async function preview(install: boolean) {
      let p;
      try {
        p = await Bridge.opencodePreview(install);
      } catch (err) {
        clear(body);
        body.append(h("div", { class: "sc-notice err", text: String(err).replace(/^Error:\s*/, "") }), h("div", { class: "sc-actions" }, backButton(draw)));
        return;
      }
      clear(body);
      body.append(
        h("div", { class: "sc-hint", text: install
          ? "Isto copia o plugin do Coucou para a pasta global de plugins do opencode. Plugins de projeto ficam intactos."
          : "Isto remove apenas o arquivo do plugin do Coucou. Seus próprios plugins ficam intactos." }),
        renderDiff(p.diff),
        p.backup ? h("div", { class: "sc-path" }, h("span", { class: "sc-path-k", text: "Backup" }), h("span", { class: "sc-path-v", text: p.backup })) : h("div", {}),
      );
      const confirm = h("button", { class: install ? "sc-btn primary" : "sc-btn danger", type: "button", text: install ? "Fazer backup e escrever" : "Fazer backup e remover" });
      confirm.addEventListener("click", async () => {
        confirm.disabled = true;
        try {
          const backup = await Bridge.opencodeApply(install, p.fingerprint);
          clear(body);
          body.append(h("div", { class: "sc-notice ok", text: backup
            ? `Pronto. Plugin anterior salvo em ${backup}. Reinicie o opencode para ativar.`
            : "Pronto. Reinicie o opencode para ativar." }));
          window.setTimeout(() => void rebuild(), 2600);
        } catch (err) {
          confirm.disabled = false;
          body.append(h("div", { class: "sc-notice err", text: `Não foi possível escrever: ${String(err)}` }));
        }
      });
      body.append(h("div", { class: "sc-actions" }, confirm, backButton(draw)));
    }

    draw();
    return card(ICONS.terminal, "opencode", "Sessões, perguntas e permissões na ilha.", body);
  }

  // ── Connected integrations card ─────────────────────────────────────────────

  function integrationsCard(): HTMLElement {
    const body = h("div", { class: "sc-body" });
    const note = h("div", { class: "sc-hint" });

    function updateNote() {
      const used = State.settings.activeIntegrations.length;
      note.textContent = `Escolha até ${MAX_ACTIVE} pills para mostrar ao lado do Mochi — ${used}/${MAX_ACTIVE} em uso. As chaves ficam no Gerenciador de Credenciais do Windows, nunca no disco.`;
    }

    // VS Code (Claude Code): an always-available pill, no key needed.
    {
      const sw = switchEl(State.settings.vscodePill, (v) => { State.settings.vscodePill = v; persist(); });
      body.append(h(
        "div",
        { class: "sc-int-item" },
        h("div", { class: "sc-int-head" }, sw, h("i", { class: "sc-dot-c", style: `background:${State.settings.agentColors?.["integration_claude"] ?? "#F5F6F8"}` }), h("span", { text: "VS Code" })),
        h("span", { class: "sc-hint", text: "Sessões do Claude Code" }),
      ));
    }

    for (const def of INTEGRATIONS) {
      const active = State.settings.activeIntegrations.includes(def.id);
      const sw = switchEl(active, () => {
        const on = State.settings.activeIntegrations.includes(def.id);
        if (on) {
          State.settings.activeIntegrations = State.settings.activeIntegrations.filter((x) => x !== def.id);
        } else {
          if (State.settings.activeIntegrations.length >= MAX_ACTIVE) {
            sw.classList.toggle("on", false);
            return;
          }
          State.settings.activeIntegrations = [...State.settings.activeIntegrations, def.id];
        }
        updateNote();
        persist();
      });

      const rows = h("div", { class: "sc-int-fields" });
      for (const f of def.fields) {
        const input = textInput(secrets[f.key] ? "••••••••  (salva)" : f.placeholder, "", f.secret ? "password" : "text");
        attachFocus(input, actions);
        const saveBtn = h("button", { class: "sc-btn", type: "button", text: "Salvar" });
        const d = statusDot(secrets[f.key] ?? false);
        saveBtn.addEventListener("click", async () => {
          const value = input.value.trim();
          try {
            await Bridge.secretSet(f.key, value);
            secrets[f.key] = value.length > 0;
            input.value = "";
            input.placeholder = value ? "••••••••  (salva)" : f.placeholder;
            d.style.background = value ? "var(--green)" : "var(--red)";
          } catch {
            d.style.background = "var(--amber)";
          }
        });
        rows.append(field(f.label, input, saveBtn, d));
      }

      body.append(h(
        "div",
        { class: "sc-int-item" },
        h(
          "div",
          { class: "sc-int-head" },
          sw,
          h("i", { class: "sc-dot-c", style: `background:${State.settings.agentColors?.[def.id] ?? def.color}` }),
          h("span", { text: def.name }),
        ),
        rows,
      ));
    }

    updateNote();
    return card(ICONS.stack, "Integrações conectadas", "Conecte seus apps para um fluxo de trabalho mais inteligente.", h("div", {}, note, body));
  }

  function backButton(onClick: () => void): HTMLElement {
    return h("button", { class: "sc-btn", type: "button", text: "Voltar", onclick: onClick });
  }

  // ── Data ────────────────────────────────────────────────────────────────────

  let hooksStatus: HookStatus = { installed: false, settingsPath: "", hookPath: "", hookReady: false };
  let opencodeStatus: OpencodeStatus = {
    installed: false, pluginPath: "", relayReady: false,
    bundledVersion: 0, installedVersion: null, needsUpdate: false,
  };
  let chatStatus: ChatStatus | null = null;
  let mcps: McpInfo[] = [];
  let newsCats: NewsCategory[] = [];
  const secrets: Record<string, boolean> = {};

  async function refreshIntegrations() {
    hooksStatus = (await Bridge.hooksStatus()) ?? hooksStatus;
    opencodeStatus = (await Bridge.opencodeStatus()) ?? opencodeStatus;
    mcps = (await Bridge.mcpList()) ?? [];
    for (const k of SECRET_KEYS) secrets[k] = (await Bridge.secretPresent(k)) ?? false;
    renderIntegrations();
  }

  async function refreshChat() {
    chatStatus = await Bridge.chatStatus();
    secrets["anthropic-api-key"] = (await Bridge.secretPresent("anthropic-api-key")) ?? false;
    renderChat();
  }

  function renderAll() {
    renderGeneral();
    renderAssistant();
    renderAppearance();
    // Integrations and Chat fill on first navigation (they fetch statuses), but
    // building them now keeps the panes non-empty if we start on them.
    renderIntegrations();
    renderChat();
  }

  async function load() {
    loaded = true;
    hooksStatus = (await Bridge.hooksStatus()) ?? hooksStatus;
    opencodeStatus = (await Bridge.opencodeStatus()) ?? opencodeStatus;
    chatStatus = await Bridge.chatStatus();
    mcps = (await Bridge.mcpList()) ?? [];
    newsCats = (await Bridge.newsCategories()) ?? [];
    for (const k of SECRET_KEYS) secrets[k] = (await Bridge.secretPresent(k)) ?? false;
    secrets["anthropic-api-key"] = (await Bridge.secretPresent("anthropic-api-key")) ?? false;
    renderAll();
    paintNav();
  }

  paintNav();
  renderAll(); // optimistic defaults; replaced by load() on first show

  return {
    el,
    sync() {
      if (!loaded) void load();
    },
  };
}
