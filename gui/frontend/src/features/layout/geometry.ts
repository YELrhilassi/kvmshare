import type { Screen } from "@/lib/bridge";
import { DEFAULT_SCREEN_HEIGHT, DEFAULT_SCREEN_WIDTH } from "@/lib/constants";

// Pure canvas math for the layout editor. Nothing here touches the DOM
// or React — every function maps world/viewport coordinates so the
// components above stay thin.

export const MIN_ZOOM = 0.05; // real (CSS) scale floor
export const MAX_ZOOM = 4; // real (CSS) scale ceiling
export const SNAP_DIST = 12; // world px within which edges snap together
export const WORLD_SPAN = 12000; // grid extends ±this around the origin

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

// Set the zoom to a percentage of the reference (fit) scale, keeping the
// viewport center fixed.
export function viewAtPercent(
  view: View,
  viewport: { w: number; h: number },
  pct: number,
  reference: number,
): View {
  const scale = clampScale((pct / 100) * reference);
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

// The grid background: minor cells that stay >= 16 screen px at any zoom
// plus a fainter 500px major line, all drawn at 1 screen px width.
export interface GridStyle {
  backgroundImage: string;
  backgroundSize: string;
  line: string;
}

export function gridStyle(scale: number): GridStyle {
  const s = Math.max(scale, 0.0001);
  let cell = 100;
  while (cell * s < 16 && cell < 5000) cell += 100;
  const inv = 1 / s;
  const line = `${inv}px`;
  const backgroundSize = `${500}px ${500}px, ${500}px ${500}px, ${cell}px ${cell}px, ${cell}px ${cell}px`;
  const backgroundImage =
    `linear-gradient(to right, rgba(255,255,255,0.15) ${line}, transparent ${line}),` +
    `linear-gradient(to bottom, rgba(255,255,255,0.15) ${line}, transparent ${line}),` +
    `linear-gradient(to right, rgba(255,255,255,0.05) ${line}, transparent ${line}),` +
    `linear-gradient(to bottom, rgba(255,255,255,0.05) ${line}, transparent ${line})`;
  return { backgroundImage, backgroundSize, line };
}