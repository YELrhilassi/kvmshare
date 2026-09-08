import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Stage, Layer, Rect, Text, Group } from "react-konva";
import type { KonvaEventObject } from "konva/lib/Node";
import type { Screen } from "@/lib/bridge";
import { gridStyle, MODEL_SCALE, snapTo, WORLD_SPAN, type View } from "@/features/layout/geometry";
import { cn } from "@/lib/utils";

// The layout editor canvas, on a real canvas (Konva). The Stage zooms
// and pans as one; each screen is a draggable Rect. Wheel zooms anchored
// at the cursor, space+drag (or middle-drag) pans, dragging a screen
// moves it with edge snapping. The document stays in real pixels; the
// canvas shows model units and converts only at the data boundary.

interface CanvasProps {
  view: View;
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
  const [size, setSize] = useState({ w: 800, h: 600 });
  const spaceRef = useRef(false);
  const lockRef = useRef(lock);
  const snapRef = useRef(snap);
  const screensRef = useRef(screens);
  const panRef = useRef<{ x: number; y: number } | null>(null);

  useEffect(() => {
    lockRef.current = lock;
  }, [lock]);
  useEffect(() => {
    snapRef.current = snap;
  }, [snap]);
  useEffect(() => {
    screensRef.current = screens;
  });

  // The Stage must fill its container; track it with a ResizeObserver.
  useLayoutEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const measure = () => setSize({ w: el.clientWidth || 800, h: el.clientHeight || 600 });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [viewportRef]);

  // Wheel zoom, anchored at the cursor (the old div version needed a
  // non-passive listener; Konva handles it natively).
  const onWheel = useCallback(
    (e: KonvaEventObject<WheelEvent>) => {
      e.evt.preventDefault();
      const stage = e.target.getStage();
      if (!stage) return;
      const pos = stage.getPointerPosition();
      if (!pos) return;
      zoomAtPoint(Math.exp(-e.evt.deltaY * 0.0012), pos.x, pos.y);
    },
    [zoomAtPoint],
  );

  // Space as the leader key: while held, dragging pans instead of moving
  // screens.
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

  // Pan: middle button always, left while space is held. Pointer moves
  // on the empty stage are pan gestures; clicks on empty canvas deselect.
  const onStagePointerDown = useCallback(
    (e: KonvaEventObject<PointerEvent>) => {
      if (e.evt.button === 1 || (e.evt.button === 0 && spaceRef.current)) {
        const stage = e.target.getStage();
        const pos = stage?.getPointerPosition();
        if (!pos) return;
        panRef.current = { x: pos.x, y: pos.y };
        return;
      }
      if (e.evt.button === 0 && e.target === e.target.getStage()) {
        onSelect(-1);
      }
    },
    [onSelect],
  );

  const onStagePointerMove = useCallback(
    (e: KonvaEventObject<PointerEvent>) => {
      const start = panRef.current;
      if (!start) return;
      const stage = e.target.getStage();
      const pos = stage?.getPointerPosition();
      if (!pos) return;
      panBy(pos.x - start.x, pos.y - start.y);
      panRef.current = { x: pos.x, y: pos.y };
    },
    [panBy],
  );

  const onStagePointerUp = useCallback(() => {
    panRef.current = null;
  }, []);

  // Keyboard nudging of the selected screen.
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
      const step = e.shiftKey ? 5 : 1;
      const s = screensRef.current[selected];
      if (!s) return;
      onMove(selected, s.x + (d[0] * step) / MODEL_SCALE, s.y + (d[1] * step) / MODEL_SCALE);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected, onMove]);

  const onScreenDragStart = useCallback(
    (_e: KonvaEventObject<DragEvent>, i: number) => {
      onSelect(i);
    },
    [onSelect],
  );

  // Snap on release: Konva reports the position in model units (the
  // Stage is scaled), so snapping happens in model space, then converts
  // to real pixels once for the document.
  const onScreenDragEnd = useCallback(
    (e: KonvaEventObject<DragEvent>, i: number) => {
      const node = e.target;
      let nx = node.x();
      let ny = node.y();
      const s = screensRef.current[i];
      if (s) {
        const w = s.width * MODEL_SCALE;
        const h = s.height * MODEL_SCALE;
        if (snapRef.current) {
          const others = screensRef.current
            .filter((_, j) => j !== i)
            .map((o) => ({
              ...o,
              x: o.x * MODEL_SCALE,
              y: o.y * MODEL_SCALE,
              width: o.width * MODEL_SCALE,
              height: o.height * MODEL_SCALE,
            }));
          const snapped = snapTo(others, nx, ny, w, h);
          nx = snapped.x;
          ny = snapped.y;
        }
      }
      onMove(i, Math.round(nx) / MODEL_SCALE, Math.round(ny) / MODEL_SCALE);
      node.position({ x: nx, y: ny });
    },
    [onMove],
  );

  return (
    <div
      ref={viewportRef}
      className={cn("relative min-w-0 flex-1 overflow-hidden bg-muted/40", spacePan && "cursor-grab")}
    >
      {/* The dot grid lives in the background (CSS, like before); the
          Stage is transparent above it, so screens are the loudest thing. */}
      <div
        className="absolute inset-0"
        style={{
          backgroundImage: gridStyle(view.scale).backgroundImage,
          backgroundSize: gridStyle(view.scale).backgroundSize,
        }}
      />
      <Stage
        width={size.w}
        height={size.h}
        scaleX={view.scale}
        scaleY={view.scale}
        x={view.pan.x}
        y={view.pan.y}
        onWheel={onWheel}
        onPointerDown={onStagePointerDown}
        onPointerMove={onStagePointerMove}
        onPointerUp={onStagePointerUp}
        onPointerLeave={onStagePointerUp}
      >
        <Layer listening={false}>
          <Rect
            x={-WORLD_SPAN}
            y={-WORLD_SPAN}
            width={WORLD_SPAN * 2}
            height={WORLD_SPAN * 2}
            fill="transparent"
          />
        </Layer>
        <Layer>
          {screens.map((s, i) => (
            <ScreenRect
              key={i}
              screen={s}
              selected={selected === i}
              own={i === 0}
              lock={lock || spacePan}
              onSelect={() => onSelect(i)}
              onDragStart={(e) => onScreenDragStart(e, i)}
              onDragEnd={(e) => onScreenDragEnd(e, i)}
            />
          ))}
        </Layer>
      </Stage>

      {spacePan && !panRef.current && (
        <div className="pointer-events-none absolute bottom-3 left-1/2 -translate-x-1/2">
          <span className="flex items-center gap-1.5 rounded-full bg-background/85 px-3 py-1 text-xs text-muted-foreground shadow-sm">
            panning — release space to edit
          </span>
        </div>
      )}
    </div>
  );
}

// One screen as a Konva Group (rect + labels). Draggable unless locked;
// the own screen (index 0) carries a "you" badge.
function ScreenRect({
  screen,
  selected,
  own,
  lock,
  onSelect,
  onDragStart,
  onDragEnd,
}: {
  screen: Screen;
  selected: boolean;
  own: boolean;
  lock: boolean;
  onSelect: () => void;
  onDragStart: (e: KonvaEventObject<DragEvent>) => void;
  onDragEnd: (e: KonvaEventObject<DragEvent>) => void;
}) {
  const x = screen.x * MODEL_SCALE;
  const y = screen.y * MODEL_SCALE;
  const w = screen.width * MODEL_SCALE;
  const h = screen.height * MODEL_SCALE;

  return (
    <Group
      x={x}
      y={y}
      draggable={!lock}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onClick={(e) => {
        e.cancelBubble = true;
        onSelect();
      }}
    >
      <Rect
        width={w}
        height={h}
        cornerRadius={8}
        fill={own ? "rgba(255,255,255,0.14)" : "rgba(255,255,255,0.08)"}
        stroke={selected ? "#fafafa" : own ? "rgba(255,255,255,0.5)" : "rgba(255,255,255,0.28)"}
        strokeWidth={selected ? 2 : 1}
        shadowColor="rgba(0,0,0,0.35)"
        shadowBlur={12}
        shadowOffsetY={3}
      />
      <Text
        text={screen.name || "screen"}
        fontSize={12}
        fontStyle="bold"
        fill="rgba(255,255,255,0.9)"
        align="center"
        width={w}
        y={6}
      />
      {own && (
        <Text text="you" fontSize={9} fontStyle="bold" fill="#000" align="center" width={w} y={22} />
      )}
      <Text
        text={`${screen.width}×${screen.height}`}
        fontSize={9.5}
        fontFamily="monospace"
        fill="rgba(255,255,255,0.5)"
        align="center"
        width={w}
        y={h - 16}
      />
    </Group>
  );
}