// Autonomous "is everything ok?" peek: while the island is closed, Mochi slips
// down from the top edge, looks around, and rises back out of sight. It has its
// own canvas and engine, so it does not need the island to be open.

import { Ease } from "../core/anim";
import { BotEngine } from "./engine";

export const PEEK_W = 84;
export const PEEK_H = 124;

/** Resting centre of Mochi, in canvas px (the canvas top sits on the screen edge).
 *  Kept just under the body radius so the head comes out of the edge, not below it. */
const REST_CY = 24;
/** Fully retracted centre, above the top edge so the body is clipped away. */
const HIDDEN_CY_FACTOR = -1.4;

const T = { drop: 0.6, look: 1.15, rise: 0.55 };
const TOTAL = T.drop + T.look + T.rise;

export class Peek {
  private engine = new BotEngine();
  private t = 0;
  private running = false;
  private looked = false;

  onDone: (() => void) | null = null;

  get active(): boolean {
    return this.running;
  }

  start() {
    this.engine.resetMorph();
    this.engine.setState("idle", true);
    this.engine.blink();
    this.t = 0;
    this.running = true;
    this.looked = false;
  }

  cancel() {
    this.running = false;
  }

  /** 0 = retracted above the edge, 1 = fully down. */
  private descent(): number {
    if (this.t < T.drop) return Ease.out(this.t / T.drop);
    if (this.t < T.drop + T.look) return 1;
    const k = Math.min(1, (this.t - T.drop - T.look) / T.rise);
    return 1 - Ease.inOut(k);
  }

  update(dt: number) {
    if (!this.running) return;
    this.t += dt;
    if (!this.looked && this.t >= T.drop) {
      this.looked = true;
      this.engine.triggerEmote("lookAround", T.look);
    }
    this.engine.update(dt);
    if (this.t >= TOTAL) {
      this.running = false;
      this.onDone?.();
    }
  }

  draw(x: CanvasRenderingContext2D) {
    const R = PEEK_W * 0.3;
    const baseCy = PEEK_H / 2 + R * 0.06;
    const hiddenCy = R * HIDDEN_CY_FACTOR;
    const cy = hiddenCy + (REST_CY - hiddenCy) * this.descent();
    x.save();
    x.translate(0, cy - baseCy);
    this.engine.draw(x, PEEK_W, PEEK_H);
    x.restore();
  }
}
