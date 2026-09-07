import { useCallback, useEffect, useRef, useState } from "react";
import { Move } from "lucide-react";
import type { Screen } from "@/lib/bridge";
import { MODEL_SCALE, snapTo, type View } from "@/features/layout/geometry";
import { cn } from "@/lib/utils";
import GridLayer from "@/features/layout/canvas/GridLayer";
import ScreenNode from "@/features/layout/canvas/ScreenNode";

// Transient gesture state lives in refs (no re-renders per pointer
// move); only things that need to paint — space hint, pan cursor —
// are state.
interface DragState {
  index: number;
  startX: number;
  startY: number;
  base: { x: number; y: number; w: number; h: number };
  el: HTMLDivElement;
  others: Screen[]; // snapshot for edge snapping
}

interface PanState {
  startX: number;
  startY: number;
  pan: { x: number; y: number };
}

interface CanvasProps {
  view: View;
  viewRef: React.MutableRefObject<View>;
  viewportRef: React.RefObject<HTMLDivElement | null>;
  screens: Screen[];
  selected: number;
  lock: boolean;
  snap: boolean;
  onSelect: (i: number) => void;
  onMove: (i: number, x: number, y: number) => void;
  zoomAtPoint: (factor: number, clientX: number, clientY: number) => void;
  panBy: (dx: number, dy: number) => void;
}

function isTyping(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable);
}

export default function Canvas({
  view,
  viewRef,
  viewportRef,
  screens,
  selected,
  lock,
  snap,
  onSelect,
  onMove,
  zoomAtPoint,
  panBy,
}: CanvasProps) {
  const [spacePan, setSpacePan] = useState(false);
  const [panning, setPanning] = useState(false);
  const [dragging, setDragging] = useState(-1); // screen index being dragged
  const spaceRef = useRef(false);
  const lockRef = useRef(lock);
  const snapRef = useRef(snap);
  const screensRef = useRef(screens);
  const dragRef = useRef<DragState | null>(null);
  const panDragRef = useRef<PanState | null>(null);

  useEffect(() => {
    lockRef.current = lock;
  }, [lock]);
  useEffect(() => {
    snapRef.current = snap;
  }, [snap]);
  useEffect(() => {
    screensRef.current = screens;
  });

  // ---------------------------------------------------------------------
  // Wheel zoom (anchored at the cursor) — needs a non-passive listener.
  // The factor is exponential in delta, so a mouse notch (~±100) gives a
  // deliberate ~1.13× step while trackpad flicks zoom smoothly without
  // overshooting.
  // ---------------------------------------------------------------------

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      zoomAtPoint(Math.exp(-e.deltaY * 0.0012), e.clientX, e.clientY);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [viewportRef, zoomAtPoint]);

  // ---------------------------------------------------------------------
  // Space as the leader key: while held, dragging anywhere pans the
  // canvas instead of moving screens (drag switch).
  // ---------------------------------------------------------------------

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.code !== "Space" || e.repeat || isTyping(e.target)) return;
      e.preventDefault();
      spaceRef.current = true;
      setSpacePan(true);
    };
    const onUp = (e: KeyboardEvent) => {
      if (e.code !== "Space" || isTyping(e.target)) return;
      spaceRef.current = false;
      setSpacePan(false);
    };
    window.addEventListener("keydown", onDown);
    window.addEventListener("keyup", onUp);
    return () => {
      window.removeEventListener("keydown", onDown);
      window.removeEventListener("keyup", onUp);
    };
  }, []);

  // ---------------------------------------------------------------------
  // Keyboard nudging of the selected screen.
  // ---------------------------------------------------------------------

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (lockRef.current || selected < 0) return;
      if (isTyping(e.target)) return;
      const dir: Record<string, [number, number]> = {
        ArrowLeft: [-1, 0],
        ArrowRight: [1, 0],
        ArrowUp: [0, -1],
        ArrowDown: [0, 1],
      };
      const d = dir[e.key];
      if (!d) return;
      e.preventDefault();
      // Steps are in model units (visible on screen); the document
      // stores real pixels, so convert back.
      const step = e.shiftKey ? 5 : 1;
      const s = screensRef.current[selected];
      if (!s) return;
      onMove(selected, s.x + (d[0] * step) / MODEL_SCALE, s.y + (d[1] * step) / MODEL_SCALE);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected, onMove]);

  // ---------------------------------------------------------------------
  // Canvas panning — middle button always, left while space is held.
  // ---------------------------------------------------------------------

  const onViewportPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (e.button === 1 || (e.button === 0 && spaceRef.current)) {
        e.preventDefault();
        panDragRef.current = { startX: e.clientX, startY: e.clientY, pan: viewRef.current.pan };
        setPanning(true);
        e.currentTarget.setPointerCapture(e.pointerId);
      } else if (e.button === 0) {
        onSelect(-1); // click empty canvas deselects
      }
    },
    [onSelect, viewRef],
  );

  const onViewportPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const d = panDragRef.current;
      if (!d) return;
      panBy(e.clientX - d.startX, e.clientY - d.startY);
    },
    [panBy],
  );

  const onViewportPointerUp = useCallback(() => {
    panDragRef.current = null;
    setPanning(false);
  }, []);

  // ---------------------------------------------------------------------
  // Screen dragging — direct DOM writes during the gesture, one state
  // update on release.
  // ---------------------------------------------------------------------

  const onScreenPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>, i: number) => {
      if (e.button !== 0) return;
      if (spaceRef.current || lockRef.current) return;
      e.preventDefault();
      e.stopPropagation();
      const s = screensRef.current[i];
      const el = e.currentTarget;
      el.setPointerCapture(e.pointerId);
      onSelect(i);
      setDragging(i);
      dragRef.current = {
        index: i,
        startX: e.clientX,
        startY: e.clientY,
        // Drag math runs in model units (what the DOM shows); the
        // document stays in real pixels, so convert on commit.
        base: { x: s.x * MODEL_SCALE, y: s.y * MODEL_SCALE, w: s.width * MODEL_SCALE, h: s.height * MODEL_SCALE },
        el,
        others: screensRef.current
          .filter((_, j) => j !== i)
          .map((o) => ({ ...o, x: o.x * MODEL_SCALE, y: o.y * MODEL_SCALE, width: o.width * MODEL_SCALE, height: o.height * MODEL_SCALE })),
      };
    },
    [onSelect],
  );

  const onScreenPointerMove = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const z = viewRef.current.scale;
    let nx = d.base.x + (e.clientX - d.startX) / z;
    let ny = d.base.y + (e.clientY - d.startY) / z;
    if (snapRef.current) {
      const snapped = snapTo(d.others, nx, ny, d.base.w, d.base.h);
      nx = snapped.x;
      ny = snapped.y;
    }
    nx = Math.round(nx);
    ny = Math.round(ny);
    d.el.style.left = `${nx}px`;
    d.el.style.top = `${ny}px`;
  }, [viewRef]);

  const onScreenPointerUp = useCallback(() => {
    const d = dragRef.current;
    if (!d) return;
    dragRef.current = null;
    setDragging(-1);
    const nx = parseFloat(d.el.style.left);
    const ny = parseFloat(d.el.style.top);
    if (Number.isNaN(nx) || Number.isNaN(ny)) return;
    // Commit back to real pixels for the document.
    onMove(d.index, nx / MODEL_SCALE, ny / MODEL_SCALE);
  }, [onMove]);

  return (
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
        className="absolute top-0 left-0 will-change-transform"
        style={{
          transform: `translate(${view.pan.x}px, ${view.pan.y}px) scale(${view.scale})`,
          transformOrigin: "0 0",
        }}
      >
        <GridLayer scale={view.scale} />
        {screens.map((s, i) => (
          <ScreenNode
            key={i}
            screen={s}
            index={i}
            selected={selected === i}
            lock={lock}
            dragging={dragging === i}
            scale={view.scale}
            onPointerDown={onScreenPointerDown}
            onPointerMove={onScreenPointerMove}
            onPointerUp={onScreenPointerUp}
          />
        ))}
      </div>

      {spacePan && !panning && (
        <div className="pointer-events-none absolute bottom-3 left-1/2 -translate-x-1/2">
          <span className="flex items-center gap-1.5 rounded-full bg-background/85 px-3 py-1 text-xs text-muted-foreground shadow-sm">
            <Move className="h-3 w-3" /> panning — release space to edit
          </span>
        </div>
      )}

      {lock && (
        <div className="pointer-events-none absolute top-3 left-1/2 -translate-x-1/2">
          <span className="rounded-full bg-background/85 px-2.5 py-1 text-[11px] font-medium text-muted-foreground shadow-sm">
            locked
          </span>
        </div>
      )}
    </div>
  );
}