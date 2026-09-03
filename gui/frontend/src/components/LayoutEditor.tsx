import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Lock, Move } from "lucide-react";
import { api, type LayoutConfig, type Screen } from "@/lib/bridge";
import { DEFAULT_PORT, DEFAULT_SCREEN_HEIGHT, DEFAULT_SCREEN_WIDTH } from "@/lib/constants";
import { cn } from "@/lib/utils";
import Toolbar, { MIN_ZOOM, MAX_ZOOM } from "@/components/layout/Toolbar";
import SidePanel from "@/components/layout/SidePanel";

const SNAP_DIST = 12; // world px within which edges snap together
const WORLD_SPAN = 12000; // grid extends ±this around the origin

type DragState = {
  index: number;
  startX: number;
  startY: number;
  base: { x: number; y: number; w: number; h: number };
  el: HTMLDivElement;
};

interface Props {
  initial: LayoutConfig | null;
}

// Edge snapping: pull x/y to the nearest aligned edge of any other screen.
function snapTo(
  screens: Screen[],
  index: number,
  x: number,
  y: number,
  w: number,
  h: number,
): { x: number; y: number } {
  let bestX = x;
  let bestY = y;
  let dX = SNAP_DIST + 1;
  let dY = SNAP_DIST + 1;
  for (let i = 0; i < screens.length; i++) {
    if (i === index) continue;
    const o = screens[i];
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

function clampScale(s: number) {
  return Math.min(Math.max(s, MIN_ZOOM), MAX_ZOOM);
}

export default function LayoutEditor({ initial }: Props) {
  const [screens, setScreens] = useState<Screen[]>(initial?.screens ?? []);
  // The port is not edited here — it belongs to the Server page. It is
  // kept so Save writes the config back unchanged.
  const port = String(initial?.port ?? DEFAULT_PORT);
  const [selected, setSelected] = useState(0);
  // Zoom is *relative*: scale is the real CSS scale, and the scale that
  // makes the whole desktop fit is shown as 100%. Screens keep their real
  // proportions, so at 100% the full layout and grid are visible.
  const [scale, setScale] = useState(0.5);
  const fitScaleRef = useRef(1); // the scale shown as 100%
  const [pan, setPan] = useState({ x: 40, y: 40 });
  const [gridCell, setGridCell] = useState(100); // world px per minor grid cell
  const [lock, setLock] = useState(false);
  const [snap, setSnap] = useState(true);
  const [spacePan, setSpacePan] = useState(false); // space held → drag pans
  const [dirty, setDirty] = useState(false);
  const [savedMsg, setSavedMsg] = useState("");
  const [error, setError] = useState("");

  const viewportRef = useRef<HTMLDivElement>(null);
  const scaleRef = useRef(scale);
  const panRef = useRef(pan);
  const lockRef = useRef(lock);
  const snapRef = useRef(snap);
  const spaceRef = useRef(spacePan);
  const screensRef = useRef(screens);
  const dragRef = useRef<DragState | null>(null);
  const panDragRef = useRef<{ startX: number; startY: number; pan: { x: number; y: number } } | null>(null);
  const fittedRef = useRef(false); // initial auto-fit done (with a real size)?
  // The user owns the view once they zoom/pan/fit by hand — auto-fit then
  // never fights them again, even if the window is resized.
  const userAdjustedRef = useRef(false);
  const markAdjusted = useCallback(() => {
    userAdjustedRef.current = true;
  }, []);

  scaleRef.current = scale;
  panRef.current = pan;
  lockRef.current = lock;
  snapRef.current = snap;
  spaceRef.current = spacePan;
  screensRef.current = screens;

  const applyScale = useCallback((s: number, p: { x: number; y: number }) => {
    scaleRef.current = s;
    panRef.current = p;
    setScale(s);
    setPan(p);
  }, []);

  // The minor grid keeps its screen density readable: the cell size (in
  // world px) is the smallest multiple of 100 that still renders >= 16px
  // on screen, so the grid never turns into mush while zooming out.
  useEffect(() => {
    const s = Math.max(scale, 0.0001);
    let cell = 100;
    while (cell * s < 16 && cell < 5000) cell += 100;
    setGridCell(cell);
  }, [scale]);

  // Everything inside the world div is scaled by `scale`. To stay crisp
  // and readable at any zoom, the UI chrome counter-scales: labels, screen
  // borders and the 1px grid lines are drawn at 1/scale world px so they
  // always render at native size on screen. This is what keeps a fitted
  // (zoomed-out) layout legible instead of shrinking to mush.
  const invScale = 1 / Math.max(scale, 0.0001);
  const gridLine = `${invScale}px`; // 1 screen px line, in world px
  const gridBackgroundSize =
    `${500}px ${500}px, ${500}px ${500}px, ${gridCell}px ${gridCell}px, ${gridCell}px ${gridCell}px`;
  // Deliberately quiet: faint major lines every 500 world px and a barely
  // there minor grid. The screens do the talking — loud grid lines drown
  // them out.
  const gridBackgroundImage =
    `linear-gradient(to right, rgba(255,255,255,0.15) ${gridLine}, transparent ${gridLine}),` +
    `linear-gradient(to bottom, rgba(255,255,255,0.15) ${gridLine}, transparent ${gridLine}),` +
    `linear-gradient(to right, rgba(255,255,255,0.05) ${gridLine}, transparent ${gridLine}),` +
    `linear-gradient(to bottom, rgba(255,255,255,0.05) ${gridLine}, transparent ${gridLine})`;

  const percent = (scale / fitScaleRef.current) * 100;
  const minPercent = (MIN_ZOOM / fitScaleRef.current) * 100;
  const maxPercent = (MAX_ZOOM / fitScaleRef.current) * 100;

  // -------------------------------------------------------------------------
  // View helpers
  // -------------------------------------------------------------------------

  const bounds = useCallback(() => {
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const s of screensRef.current) {
      minX = Math.min(minX, s.x);
      minY = Math.min(minY, s.y);
      maxX = Math.max(maxX, s.x + s.width);
      maxY = Math.max(maxY, s.y + s.height);
    }
    if (screensRef.current.length === 0) return { minX: 0, minY: 0, w: 1, h: 1 };
    return { minX, minY, w: maxX - minX, h: maxY - minY };
  }, []);

  const viewportSize = useCallback(() => {
    const el = viewportRef.current;
    return el ? { w: el.clientWidth, h: el.clientHeight } : { w: 800, h: 600 };
  }, []);

  // Center the view on the layout's bounding box at scale `s`.
  const centerOnBounds = useCallback(
    (s: number) => {
      const v = viewportSize();
      const b = bounds();
      applyScale(s, {
        x: v.w / 2 - (b.minX + b.w / 2) * s,
        y: v.h / 2 - (b.minY + b.h / 2) * s,
      });
    },
    [applyScale, bounds, viewportSize],
  );

  // Fit: the whole desktop becomes 100%.
  const fit = useCallback(() => {
    const v = viewportSize();
    const b = bounds();
    const pad = 60;
    const z = clampScale(Math.min((v.w - pad * 2) / b.w, (v.h - pad * 2) / b.h, 1));
    fitScaleRef.current = z;
    centerOnBounds(z);
  }, [centerOnBounds, viewportSize, bounds]);

  // Zoom by a factor, keeping the viewport center fixed.
  const zoomBy = useCallback(
    (factor: number) => {
      markAdjusted();
      const v = viewportSize();
      const old = scaleRef.current;
      const s = clampScale(old * factor);
      const p = panRef.current;
      applyScale(s, {
        x: v.w / 2 - ((v.w / 2 - p.x) / old) * s,
        y: v.h / 2 - ((v.h / 2 - p.y) / old) * s,
      });
    },
    [applyScale, viewportSize],
  );

  // Set the zoom percentage (relative to fit). 100 = the stored fit view.
  const setPercent = useCallback(
    (pct: number) => {
      markAdjusted();
      const s = clampScale((pct / 100) * fitScaleRef.current);
      const v = viewportSize();
      const old = scaleRef.current;
      const p = panRef.current;
      applyScale(s, {
        x: v.w / 2 - ((v.w / 2 - p.x) / old) * s,
        y: v.h / 2 - ((v.h / 2 - p.y) / old) * s,
      });
    },
    [applyScale, viewportSize],
  );

  // Fit once the canvas is really measurable. WebKit can report a wrong
  // (tiny) viewport during startup, so we retry until a sane size shows
  // up and never fight the user afterwards.
  const fitWhenReady = useCallback(() => {
    if (fittedRef.current) return;
    const el = viewportRef.current;
    if (!el || screensRef.current.length === 0) return;
    if (el.clientWidth < 50 || el.clientHeight < 50) return;
    fittedRef.current = true;
    fit();
  }, [fit]);

  useLayoutEffect(() => {
    fitWhenReady();
  }, [fitWhenReady]);

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const retry = (ms: number) => window.setTimeout(fitWhenReady, ms);
    const t1 = retry(150);
    const t2 = retry(600);
    // WebKit can report a transient viewport size while the window settles
    // (huge when the WM is still sizing it, tiny in early frames). Refit on
    // every real size change until the user takes over the view, so the
    // first render is always the fitted one no matter when we measured.
    let lastW = -1;
    let lastH = -1;
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      if (w === lastW && h === lastH) {
        fitWhenReady();
        return;
      }
      lastW = w;
      lastH = h;
      if (userAdjustedRef.current) return;
      fittedRef.current = true;
      fit();
    });
    ro.observe(el);
    return () => {
      window.clearTimeout(t1);
      window.clearTimeout(t2);
      ro.disconnect();
    };
  }, [fitWhenReady, fit]);

  // -------------------------------------------------------------------------
  // Wheel zoom (anchored at the cursor) — needs a non-passive listener.
  // -------------------------------------------------------------------------

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      fittedRef.current = true; // a deliberate view change
      markAdjusted();
      const rect = el.getBoundingClientRect();
      const cx = e.clientX - rect.left;
      const cy = e.clientY - rect.top;
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      const old = scaleRef.current;
      const s = clampScale(old * factor);
      const p = panRef.current;
      // Keep the world point under the cursor fixed.
      applyScale(s, {
        x: cx - ((cx - p.x) / old) * s,
        y: cy - ((cy - p.y) / old) * s,
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [applyScale]);

  // -------------------------------------------------------------------------
  // Space as the leader key: while held, dragging anywhere pans the
  // canvas instead of moving screens (drag switch).
  // -------------------------------------------------------------------------

  useEffect(() => {
    const isTyping = (t: EventTarget | null) => {
      const el = t as HTMLElement | null;
      return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable);
    };
    const onDown = (e: KeyboardEvent) => {
      if (e.code !== "Space" || e.repeat || isTyping(e.target)) return;
      e.preventDefault();
      setSpacePan(true);
    };
    const onUp = (e: KeyboardEvent) => {
      if (e.code !== "Space" || isTyping(e.target)) return;
      setSpacePan(false);
    };
    window.addEventListener("keydown", onDown);
    window.addEventListener("keyup", onUp);
    return () => {
      window.removeEventListener("keydown", onDown);
      window.removeEventListener("keyup", onUp);
    };
  }, []);

  // -------------------------------------------------------------------------
  // Panning — middle button always, left button while space is held.
  // -------------------------------------------------------------------------

  const startPan = (e: React.PointerEvent<HTMLDivElement>) => {
    fittedRef.current = true;
    markAdjusted();
    panDragRef.current = { startX: e.clientX, startY: e.clientY, pan: panRef.current };
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onViewportPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button === 1) {
      e.preventDefault();
      startPan(e);
    } else if (e.button === 0 && spaceRef.current) {
      // Space leader key: left-drag pans.
      e.preventDefault();
      startPan(e);
    } else if (e.button === 0) {
      setSelected(-1); // click empty canvas deselects
    }
  };

  const onViewportPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = panDragRef.current;
    if (!d) return;
    applyScale(scaleRef.current, {
      x: d.pan.x + (e.clientX - d.startX),
      y: d.pan.y + (e.clientY - d.startY),
    });
  };

  const onViewportPointerUp = () => {
    panDragRef.current = null;
  };

  // -------------------------------------------------------------------------
  // Screen dragging — direct DOM writes during the gesture, state on release.
  // -------------------------------------------------------------------------

  const onScreenPointerDown = (e: React.PointerEvent<HTMLDivElement>, i: number) => {
    if (e.button !== 0) return;
    if (spaceRef.current) return; // space leader key: let the canvas pan instead
    if (lockRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    markAdjusted();
    const s = screensRef.current[i];
    const el = e.currentTarget;
    el.setPointerCapture(e.pointerId);
    setSelected(i);
    dragRef.current = {
      index: i,
      startX: e.clientX,
      startY: e.clientY,
      base: { x: s.x, y: s.y, w: s.width, h: s.height },
      el,
    };
  };

  const onScreenPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const z = scaleRef.current;
    let nx = d.base.x + (e.clientX - d.startX) / z;
    let ny = d.base.y + (e.clientY - d.startY) / z;
    if (snapRef.current) {
      const snapped = snapTo(screensRef.current, d.index, nx, ny, d.base.w, d.base.h);
      nx = snapped.x;
      ny = snapped.y;
    }
    nx = Math.round(nx);
    ny = Math.round(ny);
    d.el.style.left = `${nx}px`;
    d.el.style.top = `${ny}px`;
  };

  const onScreenPointerUp = () => {
    const d = dragRef.current;
    if (!d) return;
    dragRef.current = null;
    const nx = parseFloat(d.el.style.left);
    const ny = parseFloat(d.el.style.top);
    if (Number.isNaN(nx) || Number.isNaN(ny)) return;
    setScreens((prev) => prev.map((s, i) => (i === d.index ? { ...s, x: nx, y: ny } : s)));
    setDirty(true);
  };

  // -------------------------------------------------------------------------
  // Keyboard nudging
  // -------------------------------------------------------------------------

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (lockRef.current || selected < 0) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      const dir: Record<string, [number, number]> = {
        ArrowLeft: [-1, 0],
        ArrowRight: [1, 0],
        ArrowUp: [0, -1],
        ArrowDown: [0, 1],
      };
      const d = dir[e.key];
      if (!d) return;
      e.preventDefault();
      markAdjusted();
      const step = e.shiftKey ? 10 : 1;
      setScreens((prev) =>
        prev.map((s, i) => (i === selected ? { ...s, x: s.x + d[0] * step, y: s.y + d[1] * step } : s)),
      );
      setDirty(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected]);

  // -------------------------------------------------------------------------
  // Screen CRUD
  // -------------------------------------------------------------------------

  const addScreen = () => {
    const b = bounds();
    const s: Screen = {
      name: "screen",
      width: DEFAULT_SCREEN_WIDTH,
      height: DEFAULT_SCREEN_HEIGHT,
      x: b.minX - DEFAULT_SCREEN_WIDTH,
      y: 0,
    };
    setScreens((prev) => [...prev, s]);
    setSelected(screensRef.current.length);
    setDirty(true);
  };

  const duplicateScreen = () => {
    const s = screensRef.current[selected];
    if (!s) return;
    setScreens((prev) => [...prev, { ...s, name: `${s.name} copy`, x: s.x + 40, y: s.y + 40 }]);
    setSelected(screensRef.current.length);
    setDirty(true);
  };

  const deleteScreen = () => {
    if (selected <= 0) return; // index 0 is this machine's own screen
    const idx = selected;
    setScreens((prev) => prev.filter((_, i) => i !== idx));
    setSelected(-1);
    setDirty(true);
  };

  const patchScreen = (i: number, patch: Partial<Screen>) => {
    setScreens((prev) => prev.map((s, j) => (j === i ? { ...s, ...patch } : s)));
    setDirty(true);
  };

  // -------------------------------------------------------------------------
  // Save (the running server picks the file up live — no restart)
  // -------------------------------------------------------------------------

  const save = async () => {
    setError("");
    setSavedMsg("");
    try {
      await api().SaveConfig({ port: parseInt(port, 10), screens });
      setDirty(false);
      setSavedMsg("saved — applied live");
      window.setTimeout(() => setSavedMsg(""), 3000);
    } catch (e) {
      setError(String(e));
    }
  };

  // Explicit "Fit" clicks (toolbar button and the % readout) hand the view
  // to the user: the auto-fit stops watching the window size afterwards.
  const onFitClick = useCallback(() => {
    markAdjusted();
    fit();
  }, [fit, markAdjusted]);

  const sel = selected >= 0 ? screens[selected] : null;
  const panning = !!panDragRef.current;

  return (
    <div className="flex h-full flex-col">
      <Toolbar
        percent={percent}
        minPercent={Math.min(minPercent, 5)}
        maxPercent={maxPercent}
        onPercentChange={setPercent}
        onZoomBy={zoomBy}
        onFit={onFitClick}
        snap={snap}
        onSnapChange={setSnap}
        lock={lock}
        onLockChange={setLock}
        onAdd={addScreen}
        onSave={save}
        dirty={dirty}
        savedMsg={savedMsg}
        error={error}
      />

      <div className="flex min-h-0 flex-1">
        {/* Canvas */}
        <div
          ref={viewportRef}
          className={cn("relative min-w-0 flex-1 overflow-hidden bg-muted/40", spacePan && "cursor-grab")}
          style={panning ? { cursor: "grabbing" } : undefined}
          onPointerDown={onViewportPointerDown}
          onPointerMove={onViewportPointerMove}
          onPointerUp={onViewportPointerUp}
          onPointerCancel={onViewportPointerUp}
        >
          <div
            className="absolute top-0 left-0"
            style={{
              transform: `translate(${pan.x}px, ${pan.y}px) scale(${scale})`,
              transformOrigin: "0 0",
            }}
          >
            {/* Grid in world coordinates. Both the cell sizes and the
                line widths are set inline so the grid is 1 screen px at
                any zoom (no CSS var / sub-pixel pitfalls). */}
            <div
              className="absolute"
              style={{
                left: -WORLD_SPAN,
                top: -WORLD_SPAN,
                width: WORLD_SPAN * 2,
                height: WORLD_SPAN * 2,
                backgroundImage: gridBackgroundImage,
                backgroundSize: gridBackgroundSize,
              }}
            />
            {screens.map((s, i) => (
              <div
                key={i}
                onPointerDown={(e) => onScreenPointerDown(e, i)}
                onPointerMove={onScreenPointerMove}
                onPointerUp={onScreenPointerUp}
                onPointerCancel={onScreenPointerUp}
                className={cn(
                  "absolute rounded-md transition-shadow select-none",
                  i === 0 ? "border-dashed" : "border-solid",
                  "border-primary/70",
                  selected === i && "ring-2 ring-ring",
                  lock ? "cursor-default" : "cursor-grab active:cursor-grabbing",
                )}
                style={{
                  left: s.x,
                  top: s.y,
                  width: s.width,
                  height: s.height,
                  // Counter-scaled so the border is always 2 screen px.
                  borderWidth: `${2 * invScale}px`,
                  // Frosted, translucent fill: the grid behind is blurred
                  // away so the screen reads as a solid surface.
                  backdropFilter: `blur(${5 * invScale}px)`,
                  WebkitBackdropFilter: `blur(${5 * invScale}px)`,
                  background: i === 0 ? "rgba(255, 255, 255, 0.1)" : "rgba(255, 255, 255, 0.07)",
                  boxShadow: `0 ${4 * invScale}px ${18 * invScale}px rgba(0, 0, 0, 0.35)`,
                }}
                title={`${s.name} — ${s.width}×${s.height} at ${s.x},${s.y}`}
              >
                <span
                  className="pointer-events-none absolute top-1 left-1.5 flex items-center gap-1 text-[11px] font-semibold"
                  style={{ transform: `scale(${invScale})`, transformOrigin: "top left" }}
                >
                  {s.name || "(unnamed)"}
                  {i === 0 && (
                    <span className="rounded bg-primary px-1 py-px text-[9px] font-bold text-primary-foreground">
                      YOUR MACHINE
                    </span>
                  )}
                  {lock && <Lock className="h-3 w-3 text-muted-foreground" />}
                </span>
                <span
                  className="pointer-events-none absolute right-1.5 bottom-1 font-mono text-[10px] text-muted-foreground"
                  style={{ transform: `scale(${invScale})`, transformOrigin: "bottom right" }}
                >
                  {s.width}×{s.height}
                </span>
              </div>
            ))}
          </div>

          {spacePan && !panning && (
            <div className="pointer-events-none absolute bottom-3 left-1/2 -translate-x-1/2">
              <span className="flex items-center gap-1.5 rounded-md bg-background/80 px-3 py-1 text-xs text-muted-foreground">
                <Move className="h-3 w-3" /> drag to pan — release space to edit
              </span>
            </div>
          )}

          {lock && (
            <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
              <span className="rounded-md bg-background/80 px-3 py-1 text-xs text-muted-foreground">
                layout locked
              </span>
            </div>
          )}
        </div>

        <SidePanel
          screen={sel}
          index={selected}
          lock={lock}
          onPatch={patchScreen}
          onDuplicate={duplicateScreen}
          onDelete={deleteScreen}
        />
      </div>
    </div>
  );
}
