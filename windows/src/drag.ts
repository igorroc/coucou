// Drag overlay entry: the floating Navi that follows the cursor while the user
// drags it across the desktop, plus a highlight around the window underneath.
//
// This window is transparent and click-through — it never receives mouse events.
// Everything is driven by Rust (the 60 Hz cursor poll + Win32 WindowFromPoint),
// which emits `drag-hover` with the cursor position, the window description and
// its rectangle, all in this window's logical coordinates.

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
  // The macOS drag shows a surprised face; keep it for the whole gesture.
  engine.setPermanentEmote("surprised");

  const boot = await Bridge.boot();
  if (boot?.settings.naviColor) engine.bodyColor = hexToRGB(boot.settings.naviColor);

  const ctx = canvas.getContext("2d");
  let cursorX = -9999;
  let cursorY = -9999;
  let raf = 0;
  let last = performance.now();

  function frame(nowMs: number) {
    raf = requestAnimationFrame(frame);
    const dt = Math.min(0.05, (nowMs - last) / 1000);
    last = nowMs;
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

  await onEvent<DragHover>("drag-hover", (h) => {
    cursorX = h.x;
    cursorY = h.y;

    if (h.rect) {
      highlight.style.display = "block";
      highlight.style.left = `${h.rect[0]}px`;
      highlight.style.top = `${h.rect[1]}px`;
      highlight.style.width = `${h.rect[2]}px`;
      highlight.style.height = `${h.rect[3]}px`;
    } else {
      highlight.style.display = "none";
    }

    const text = [h.app, h.title].filter(Boolean).join(" · ");
    label.textContent = text;
    label.style.display = text ? "block" : "none";
    label.style.left = `${Math.round(h.x) + 14}px`;
    label.style.top = `${Math.round(h.y) + 16}px`;
  });

  // Rust shows/hides the window around a drag and drives the loop from there:
  // a hidden overlay must not keep animating (0% CPU when idle).
  await onEvent<null>("drag-show", () => start());
  await onEvent<null>("drag-hide", () => stop());
}

void main();
