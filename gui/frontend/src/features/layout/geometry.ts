import type { Screen } from "@/lib/bridge";
import { DEFAULT_SCREEN_HEIGHT, DEFAULT_SCREEN_WIDTH } from "@/lib/constants";

// Pure canvas math for the layout editor. Nothing here touches the DOM
// or React — every function maps world/viewport coordinates so the
// components above stay thin.
//
// Screens are stored and edited in REAL pixels (the server needs exact
// positions), but the canvas presents them at a fixed model scale so a
// 1920×1080 screen reads as a comfortable 192×108 unit block instead of
// dwarfing the viewport. Everything on the canvas — positions, sizes,
// grid, snap, zoom — is in model units; real↔model conversion happens
// only at the data boundary (toModel, and the drag commit in Canvas).

export const MODEL_SCALE = 0.1; // 1 model unit = 10 real pixels

// Zoom is expressed as a percentage of model 1:1 — 100% is the natural
// size (a 1080p screen is a 192×108 block), and the range is fixed and
// balanced (10%–400%) no matter how big or spread out the layout is.
// "Fit" is a separate action that picks whatever scale shows everything.
export const MIN_ZOOM = 0.1; // 10%
export const MAX_ZOOM = 4; // 400%
export const SNAP_DIST = 12; // model px within which edges snap together
export const WORLD_SPAN = 2000; // grid extends ±this around the origin (model units)

// Real-pixel screen → model-space screen (positions and size only).
export function toModel(s: Screen): Screen {
  return { ...s, x: s.x * MODEL_SCALE, y: s.y * MODEL_SCALE, width: s.width * MODEL_SCALE, height: s.height * MODEL_SCALE };
}

export interface View {
  scale: number;
  pan: { x: number; y: number };
}

export interface Bounds {
  minX: number;
  minY: number;
  w: number;
  h: number;
}

export function clampScale(s: number): number {
  return Math.min(Math.max(s, MIN_ZOOM), MAX_ZOOM);
}

// The world-space bounding box of a set of screens.
export function boundsOf(screens: Screen[]): Bounds {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const s of screens) {
    minX = Math.min(minX, s.x);
    minY = Math.min(minY, s.y);
    maxX = Math.max(maxX, s.x + s.width);
    maxY = Math.max(maxY, s.y + s.height);
  }
  if (screens.length === 0) return { minX: 0, minY: 0, w: 1, h: 1 };
  return { minX, minY, w: maxX - minX, h: maxY - minY };
}

// The scale that fits the whole desktop into a viewport, capped at 1 —
// that capped value is the "100%" reference the zoom percent uses.
export function fitScale(viewport: { w: number; h: number }, bounds: Bounds, pad = 60): number {
  return clampScale(
    Math.min((viewport.w - pad * 2) / bounds.w, (viewport.h - pad * 2) / bounds.h, 1),
  );
}

// A view centered on the layout's bounding box at `scale`.
export function centeredView(scale: number, viewport: { w: number; h: number }, bounds: Bounds): View {
  return {
    scale,
    pan: {
      x: viewport.w / 2 - (bounds.minX + bounds.w / 2) * scale,
      y: viewport.h / 2 - (bounds.minY + bounds.h / 2) * scale,
    },
  };
}

// Zoom by a factor, keeping the world point under `anchor` (client
// coordinates inside the viewport) fixed on screen.
export function zoomAt(view: View, factor: number, anchor: { x: number; y: number }): View {
  const scale = clampScale(view.scale * factor);
  const k = scale / view.scale;
  return {
    scale,
    pan: {
      x: anchor.x - (anchor.x - view.pan.x) * k,
      y: anchor.y - (anchor.y - view.pan.y) * k,
    },
  };
}

// Set the zoom to a percentage of model 1:1 (100 = natural size),
// keeping the viewport center fixed.
export function viewAtPercent(view: View, viewport: { w: number; h: number }, pct: number): View {
  const scale = clampScale(pct / 100);
  return zoomAt(view, scale / view.scale, { x: viewport.w / 2, y: viewport.h / 2 });
}

// Pull x/y to the nearest aligned edge of any other screen.
export function snapTo(
  others: Screen[],
  x: number,
  y: number,
  w: number,
  h: number,
): { x: number; y: number } {
  let bestX = x;
  let bestY = y;
  let dX = SNAP_DIST + 1;
  let dY = SNAP_DIST + 1;
  for (const o of others) {
    // left→left, left→right, right→left, right→right
    const xs = [o.x, o.x + o.width, o.x - w, o.x + o.width - w];
    const ys = [o.y, o.y + o.height, o.y - h, o.y + o.height - h];
    for (const cand of xs) {
      const d = Math.abs(cand - x);
      if (d < dX) {
        dX = d;
        bestX = cand;
      }
    }
    for (const cand of ys) {
      const d = Math.abs(cand - y);
      if (d < dY) {
        dY = d;
        bestY = cand;
      }
    }
  }
  return { x: bestX, y: bestY };
}

// Where a freshly added screen goes: one full screen to the left of the
// current desktop, so it never covers anything.
export function placementFor(screens: Screen[]): Screen {
  const b = boundsOf(screens);
  return {
    name: "screen",
    width: DEFAULT_SCREEN_WIDTH,
    height: DEFAULT_SCREEN_HEIGHT,
    x: b.minX - DEFAULT_SCREEN_WIDTH,
    y: b.minY,
  };
}

// The grid background: a quiet dot grid, not lines. Lines cross the
// screens and fight with them; dots read as texture and keep the screens
// as the loudest thing. The cell is the smallest "nice" step whose
// on-screen pitch stays comfortable (20–40 px), so the grid adapts at
// every zoom instead of turning into a wall of lines or a mush. Steps
// are in MODEL units (a 1080p screen is 192 units wide, so 32 gives a
// clear 32 px pitch at 100%).
const GRID_STEPS = [8, 16, 32, 64, 128, 256, 512];

export interface GridStyle {
  backgroundImage: string;
  backgroundSize: string;
}

export function gridStyle(scale: number): GridStyle {
  const s = Math.max(scale, 0.0001);
  let cell = GRID_STEPS[GRID_STEPS.length - 1];
  for (const step of GRID_STEPS) {
    if (step * s >= 20) {
      cell = step;
      break;
    }
  }
  // The dot stays ~1.8 screen px by counter-scaling its world radius.
  const r = Math.min(3, Math.max(1, 1.8 / s));
  return {
    backgroundImage: `radial-gradient(circle, rgba(255,255,255,0.15) ${r}px, transparent ${r + 0.5}px)`,
    backgroundSize: `${cell}px ${cell}px`,
  };
}