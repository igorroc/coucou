// Entry point: boot the bridge, wire the island, start the greeting.

import "./style.css";
import { Bridge, IS_TAURI, onEvent } from "./core/bridge";
import { Sound } from "./core/sound";
import { State, type Settings } from "./core/state";
import { Island } from "./island/island";
import { registerHookHandlers } from "./island/hooks";
import { registerIntegrationHandlers, refreshConfigured } from "./island/integrations";

async function main() {
  const root = document.getElementById("root");
  if (!root) return;

  void Sound.preload();

  const island = new Island(root);

  const boot = await Bridge.boot();
  if (boot) {
    State.settings = { ...State.settings, ...boot.settings };
  }
  island.applySettings();
  State.loadIntegrationTasks();

  /** Available MCP servers — shown on the dashboard and in Settings → Integrations. */
  const refreshMcps = async () => {
    State.setMcps((await Bridge.mcpList()) ?? []);
  };
  void refreshMcps();

  await onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y));

  /** Pause has to reach Rust too, or the pollers keep calling out. */
  const setPaused = (on: boolean) => {
    if (State.paused === on) return;
    State.paused = on;
    void Bridge.setPaused(on);
  };

  await onEvent<string>("tray", (what) => {
    switch (what) {
      case "settings":
        setPaused(false);
        island.alert("settings");
        break;
      case "open":
        setPaused(false);
        island.alert(State.restoreView());
        break;
      case "pause":
        setPaused(!State.paused);
        if (State.paused) island.fsm.forceHidden();
        else island.reveal();
        break;
    }
  });

  // Ctrl+Space, system-wide: expand the island, or compact it.
  await onEvent<string>("hotkey", () => island.toggle());

  await onEvent<null>("screen-changed", () => void Bridge.reposition());

  // The Rust media watcher (SMTC) reports playback start/stop.
  await onEvent<{ playing: boolean }>("media", ({ playing }) => island.setDancing(playing));

  // Drag-attach: Rust drives the drag; the island hides its bot and, on a drop
  // over a window, opens the chat with that window as context.
  await onEvent<null>("drag-start", () => island.onDragStart());
  await onEvent<{ context: { app: string; title: string } | null }>("drag-end", ({ context }) =>
    island.onDragEnd(context),
  );
  await onEvent<null>("bot-tap", () => island.onBotTap());

  // The settings window writes preferences; apply them here without a restart.
  await onEvent<Settings>("settings-changed", (s) => {
    State.settings = { ...State.settings, ...s };
    island.applySettings();
    State.loadIntegrationTasks();
    void refreshConfigured();
    void refreshMcps();
  });

  registerHookHandlers(island);
  registerIntegrationHandlers(island);

  island.launch();

  // In a plain browser there is no wake strip behind the cursor: make the whole
  // page wake the island so the visuals can be checked with `npm run dev`.
  if (!IS_TAURI) {
    document.addEventListener("click", () => Sound.resume(), { once: true });
  }
}

void main();
