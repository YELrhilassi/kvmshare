import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Lock } from "lucide-react";
import { api, type LayoutConfig, type Screen } from "@/lib/bridge";
import { DEFAULT_PORT, DEFAULT_SCREEN_HEIGHT, DEFAULT_SCREEN_WIDTH } from "@/lib/constants";
import { useRunning } from "@/lib/hooks";
import { cn } from "@/lib/utils";
import Toolbar, { MAX_ZOOM, MIN_ZOOM } from "@/components/layout/Toolbar";
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

export default function LayoutEditor({ initial }: Props) {
  const running = useRunning();

  const [screens, setScreens] = useState<Screen[]>(initial?.screens ?? []);
  const [port, setPort] = useState(String(initial?.port ?? DEFAULT_PORT));
  const [selected, setSelected] = useState(0);
  const [zoom, setZoom] = useState(0.4);
  const [pan, setPan] = useState({ x: 40, y: 40 });
  const [lock, setLock] = useState(false);
  const [snap, setSnap] = useState(true);
  const [dirty, setDirty] = useState(false);
  const [savedMsg, setSavedMsg] = useState("");
  const [error, setError] = useState("");

  const viewportRef = useRef<HTMLDivElement>(null);
  const zoomRef = useRef(zoom);
  const panRef = useRef(pan);
  const lockRef = useRef(lock);
  const snapRef = useRef(snap);
  const screensRef = useRef(screens);
  const dragRef = useRef<DragState | null>(null);
  const panDragRef = useRef<{ startX: number; startY: number; pan: { x: number; y: number } } | null>(null);
  const didInitFit = useRef(false);

  zoomRef.current = zoom;
  panRef.current = pan;
  lockRef.current = lock;
  snapRef.current = snap;
  screensRef.current = screens;

  const applyView = useCallback((z: number, p: { x: number; y: number }) => {
    zoomRef.current = z;
    panRef.current = p;
    setZoom(z);
    setPan(p);
  }, []);

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
    return { minX, minY, w: maxX - minX, h: maxY - minY };
  }, []);

  const viewportSize = useCallback(() => {
    const el = viewportRef.current;
    return el ? { w: el.clientWidth, h: el.clientHeight } : { w: 800, h: 600 };
  }, []);

  const fit = useCallback(() => {
    const v = viewportSize();
    const b = bounds();
    const pad = 60;
    const z = Math.min((v.w - pad * 2) / b.w, (v.h - pad * 2) / b.h);
    const clamped = Math.min(Math.max(z, MIN_ZOOM), 1);
    applyView(clamped, {
      x: (v.w - b.w * clamped) / 2 - b.minX * clamped,
      y: (v.h - b.h * clamped) / 2 - b.minY * clamped,
    });
  }, [applyView, bounds, viewportSize]);

  const reset100 = useCallback(() => {
    const v = viewportSize();
    const b = bounds();
    applyView(1, {
      x: v.w / 2 - (b.minX + b.w / 2),
      y: v.h / 2 - (b.minY + b.h / 2),
    });
  }, [applyView, bounds, viewportSize]);

  // Initial fit once the config is in.
  useLayoutEffect(() => {
    if (didInitFit.current || screensRef.current.length === 0) return;
    didInitFit.current = true;
    fit();
  }, [fit]);

  // -------------------------------------------------------------------------
  // Wheel zoom (anchored at the cursor) — needs a non-passive listener.
  // -------------------------------------------------------------------------

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const cx = e.clientX - rect.left;
      const cy = e.clientY - rect.top;
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      const z = Math.min(Math.max(zoomRef.current * factor, MIN_ZOOM), MAX_ZOOM);
      const p = panRef.current;
      // Keep the world point under the cursor fixed.
      applyView(z, {
        x: cx - ((cx - p.x) / zoomRef.current) * z,
        y: cy - ((cy - p.y) / zoomRef.current) * z,
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [applyView]);

  // -------------------------------------------------------------------------
  // Panning (middle-button drag on empty canvas)
  // -------------------------------------------------------------------------

  const onViewportPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button === 1) {
      e.preventDefault();
      panDragRef.current = { startX: e.clientX, startY: e.clientY, pan: panRef.current };
      viewportRef.current?.setPointerCapture(e.pointerId);
    } else if (e.button === 0) {
      setSelected(-1); // click empty canvas deselects
    }
  };

  const onViewportPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = panDragRef.current;
    if (!d) return;
    applyView(zoomRef.current, {
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
    if (lockRef.current) return;
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
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
    const z = zoomRef.current;
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
    if (selected <= 0) return; // index 0 is the server's own screen
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
  // Save
  // -------------------------------------------------------------------------

  const save = async () => {
    setError("");
    const p = parseInt(port, 10);
    if (Number.isNaN(p) || p < 1024 || p > 65535) {
      setError("Port must be between 1024 and 65535");
      return;
    }
    try {
      await api().SaveConfig({ port: p, screens });
      setDirty(false);
      setSavedMsg(running.server ? "saved — server restarted" : "saved");
      window.setTimeout(() => setSavedMsg(""), 3000);
    } catch (e) {
      setError(String(e));
    }
  };

  const sel = selected >= 0 ? screens[selected] : null;

  return (
    <div className="flex h-full flex-col">
      <Toolbar
        zoom={zoom}
        onZoomChange={(z) => applyView(z, panRef.current)}
        onZoomBy={(f) => applyView(Math.min(Math.max(zoomRef.current * f, MIN_ZOOM), MAX_ZOOM), panRef.current)}
        onFit={fit}
        onReset={reset100}
        snap={snap}
        onSnapChange={setSnap}
        lock={lock}
        onLockChange={setLock}
        onAdd={addScreen}
        onSave={save}
        dirty={dirty}
        savedMsg={savedMsg}
      />

      <div className="flex min-h-0 flex-1">
        {/* Canvas */}
        <div
          ref={viewportRef}
          className={cn("relative min-w-0 flex-1 overflow-hidden bg-muted/40")}
          onPointerDown={onViewportPointerDown}
          onPointerMove={onViewportPointerMove}
          onPointerUp={onViewportPointerUp}
          onPointerCancel={onViewportPointerUp}
        >
          <div
            className="absolute left-0 top-0"
            style={{
              transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
              transformOrigin: "0 0",
            }}
          >
            {/* Grid in world coordinates */}
            <div
              className="absolute grid-layer"
              style={{
                left: -WORLD_SPAN,
                top: -WORLD_SPAN,
                width: WORLD_SPAN * 2,
                height: WORLD_SPAN * 2,
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
                  "absolute rounded-md border-2 transition-shadow select-none",
                  i === 0 ? "border-dashed border-primary/70" : "border-primary/50",
                  selected === i && "ring-2 ring-ring",
                  lock ? "cursor-default" : "cursor-grab active:cursor-grabbing",
                )}
                style={{
                  left: s.x,
                  top: s.y,
                  width: s.width,
                  height: s.height,
                  background: i === 0 ? "hsl(var(--primary) / 0.06)" : "hsl(var(--primary) / 0.03)",
                }}
                title={`${s.name} — ${s.width}×${s.height} at ${s.x},${s.y}`}
              >
                <span className="pointer-events-none absolute top-1 left-1.5 flex items-center gap-1 text-[11px] font-semibold">
                  {s.name || "(unnamed)"}
                  {i === 0 && (
                    <span className="rounded bg-primary px-1 py-px text-[9px] font-bold text-primary-foreground">
                      SERVER
                    </span>
                  )}
                  {lock && <Lock className="h-3 w-3 text-muted-foreground" />}
                </span>
                <span className="pointer-events-none absolute right-1.5 bottom-1 font-mono text-[10px] text-muted-foreground">
                  {s.width}×{s.height}
                </span>
              </div>
            ))}
          </div>

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
          port={port}
          error={error}
          serverRunning={running.server}
          onPatch={patchScreen}
          onDuplicate={duplicateScreen}
          onDelete={deleteScreen}
          onPortChange={(v) => {
            setPort(v);
            setDirty(true);
          }}
        />
      </div>
    </div>
  );
}