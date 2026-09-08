import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Stage, Layer, Rect, Text, Group, Circle } from "react-konva";
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

// One screen as a Konva Group: a soft glass panel with a header strip
// (accent dot + name, like a window title bar), a "you" badge on the
// own screen, and the resolution pinned to the bottom corner. Draggable
// unless locked. Text lives in a header so it reads at any zoom; the
// pill-free layout keeps screens looking like screens, not tags.
const OWN_ACCENT = "#34d399"; // emerald — this machine
const OTHER_ACCENT = "#7dd3fc"; // sky — other machines

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

  const name = screen.name || "screen";
  const accent = own ? OWN_ACCENT : OTHER_ACCENT;
  // The header is a fixed fraction of the screen height so a small
  // screen never drowns in chrome, and never so tall it eats the body.
  const headerH = Math.min(28, Math.max(20, h * 0.14));
  const res = `${screen.width}×${screen.height}`;

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
      {/* Body: a dark glass panel with a subtle top glow; the own screen
          carries a faint emerald tint so it reads as "this machine" at
          a glance, not just from its badge. */}
      <Rect
        width={w}
        height={h}
        cornerRadius={10}
        fillLinearGradientStartPoint={{ x: 0, y: 0 }}
        fillLinearGradientEndPoint={{ x: 0, y: h }}
        fillLinearGradientColorStops={
          own
            ? [0, "rgba(52,211,153,0.12)", 0.35, "rgba(30,33,42,0.92)", 1, "rgba(17,19,26,0.94)"]
            : [0, "rgba(125,211,252,0.08)", 0.35, "rgba(30,33,42,0.9)", 1, "rgba(17,19,26,0.93)"]
        }
        stroke={selected ? accent : own ? "rgba(52,211,153,0.5)" : "rgba(148,163,184,0.32)"}
        strokeWidth={selected ? 2.5 : 1.25}
        shadowColor={selected ? accent : "rgba(0,0,0,0.5)"}
        shadowBlur={selected ? 26 : 14}
        shadowOpacity={selected ? 0.5 : 1}
        shadowOffsetY={4}
      />
      {/* Header strip: a slightly lighter band with a hairline below it,
          framing the name like a window title bar. */}
      <Rect
        width={w}
        height={headerH}
        cornerRadius={[10, 10, 0, 0]}
        fill="rgba(255,255,255,0.05)"
        listening={false}
      />
      <Rect y={headerH - 1} width={w} height={1} fill="rgba(255,255,255,0.06)" listening={false} />
      {/* Accent dot + name, left-aligned in the header. */}
      <Circle x={13} y={headerH / 2} radius={3.5} fill={accent} listening={false} />
      <Text
        text={name}
        fontSize={13}
        fontStyle="600"
        fill="rgba(255,255,255,0.96)"
        x={22}
        y={0}
        width={Math.max(0, w - 22 - (own ? 40 : 8))}
        height={headerH}
        verticalAlign="middle"
        ellipsis
        listening={false}
      />
      {/* "you" pill, top-right in the header. */}
      {own && (
        <>
          <Rect
            x={w - 36}
            y={(headerH - 15) / 2}
            width={26}
            height={15}
            cornerRadius={7.5}
            fill="rgba(52,211,153,0.18)"
            listening={false}
          />
          <Text
            text="you"
            fontSize={9}
            fontStyle="700"
            fill={OWN_ACCENT}
            align="center"
            width={26}
            x={w - 36}
            y={(headerH - 15) / 2}
            height={15}
            verticalAlign="middle"
            listening={false}
          />
        </>
      )}
      {/* Resolution: a small pill pinned to the bottom-right, readable
          against anything behind it. */}
      <Rect
        x={Math.max(4, w - 72)}
        y={Math.max(headerH + 6, h - 22)}
        width={Math.min(64, w - 8)}
        height={16}
        cornerRadius={8}
        fill="rgba(10,10,14,0.65)"
        listening={false}
      />
      <Text
        text={res}
        fontSize={9.5}
        fontFamily="monospace"
        fill="rgba(255,255,255,0.68)"
        align="center"
        width={w}
        y={Math.max(headerH + 7, h - 21)}
        listening={false}
      />
    </Group>
  );
}