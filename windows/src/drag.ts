// Drag overlay entry: the floating Navi that follows the cursor while the user
// drags it across the desktop, plus a highlight around the window underneath.
//
// This window is transparent and click-through — it never receives mouse events.
// Everything is driven by Rust (the 60 Hz cursor poll + Win32 WindowFromPoint),
// which emits `drag-hover` with the cursor position, the window description and
// its rectangle, all in this window's logical coordinates. On a successful drop
// it emits `drag-halo`: the character is gone (back to the notch) and a
// rotating rainbow ring lingers on the attached window before fading out.

import "./drag.css";
import { BotEngine, hexToRGB } from "./navi/engine";
import { Bridge, onEvent } from "./core/bridge";

/** Canvas square, CSS px — the body ends up ~Ø54, matching the macOS ghost. */
const CANVAS = 96;
/** Gap between the cursor and the bottom of the character. */
const GAP = 18;

interface DragHover {
  x: number;
  y: number;
  app: string;
  title: string;
  rect: [number, number, number, number] | null;
}

interface DragHalo {
  rect: [number, number, number, number];
  app: string;
  title: string;
}

async function main() {
  const root = document.getElementById("drag-root");
  if (!root) return;

  const highlight = document.createElement("div");
  highlight.id = "drag-highlight";
  const label = document.createElement("div");
  label.id = "drag-label";
  const canvas = document.createElement("canvas");
  canvas.id = "drag-bot";
  root.append(highlight, label, canvas);

  const engine = new BotEngine();
  engine.setState("idle", true);
  engine.setPermanentEmote("surprised");

  const boot = await Bridge.boot();
  if (boot?.settings.naviColor) engine.bodyColor = hexToRGB(boot.settings.naviColor);

  const ctx = canvas.getContext("2d");
  let cursorX = -9999;
  let cursorY = -9999;
  let targetX = -9999;
  let targetY = -9999;
  let raf = 0;
  let last = performance.now();

  function frame(nowMs: number) {
    raf = requestAnimationFrame(frame);
    const dt = Math.min(0.05, (nowMs - last) / 1000);
    last = nowMs;

    // Spring the character towards the cursor instead of teleporting.
    const k = 1 - Math.pow(0.0015, dt);
    cursorX += (targetX - cursorX) * k;
    cursorY += (targetY - cursorY) * k;

    engine.update(dt);
    if (!ctx) return;

    const dpr = Math.min(2, window.devicePixelRatio || 1);
    if (canvas.width !== CANVAS * dpr) {
      canvas.width = CANVAS * dpr;
      canvas.height = CANVAS * dpr;
      canvas.style.width = `${CANVAS}px`;
      canvas.style.height = `${CANVAS}px`;
    }
    canvas.style.transform = `translate(${cursorX - CANVAS / 2}px, ${cursorY - GAP - CANVAS}px)`;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, CANVAS, CANVAS);
    engine.draw(ctx, CANVAS, CANVAS);
  }

  const start = () => {
    if (raf) return;
    last = performance.now();
    raf = requestAnimationFrame(frame);
  };
  const stop = () => {
    if (!raf) return;
    cancelAnimationFrame(raf);
    raf = 0;
  };

  function placeLabel(x: number, y: number, text: string) {
    label.textContent = text;
    label.style.display = text ? "block" : "none";
    label.style.left = `${Math.round(x) + 14}px`;
    label.style.top = `${Math.round(y) + 16}px`;
  }

  function placeHighlight(rect: [number, number, number, number]) {
    highlight.style.display = "block";
    highlight.style.left = `${rect[0]}px`;
    highlight.style.top = `${rect[1]}px`;
    highlight.style.width = `${rect[2]}px`;
    highlight.style.height = `${rect[3]}px`;
  }

  function reset() {
    stop();
    highlight.className = "";
    highlight.style.display = "none";
    label.style.display = "none";
    canvas.style.display = "none";
  }

  await onEvent<DragHover>("drag-hover", (h) => {
    targetX = h.x;
    targetY = h.y;
    if (highlight.classList.contains("halo")) return; // halo owns the overlay
    canvas.style.display = "";
    if (h.rect) placeHighlight(h.rect);
    else highlight.style.display = "none";
    placeLabel(h.x, h.y, [h.app, h.title].filter(Boolean).join(" · "));
  });

  await onEvent<DragHalo>("drag-halo", (h) => {
    // The character is back in the notch: show only the ring, then fade out.
    stop();
    canvas.style.display = "none";
    placeHighlight(h.rect);
    highlight.className = "halo";
    placeLabel(h.rect[0] + 12, h.rect[1] + 12, [h.app, h.title].filter(Boolean).join(" · "));
    requestAnimationFrame(() => highlight.classList.add("on"));
  });

  // Rust shows/hides the window around a drag and drives the loop from there:
  // a hidden overlay must not keep animating (0% CPU when idle).
  await onEvent<null>("drag-show", () => {
    reset();
    start();
  });
  await onEvent<null>("drag-hide", () => reset());
}

void main();
