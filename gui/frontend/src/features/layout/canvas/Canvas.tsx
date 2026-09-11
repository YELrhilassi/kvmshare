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

// One screen as a Konva Group: a flat, solid panel — no gradients, no
// shadows, no translucency — with a header band (accent tick + name),
// a centered resolution line, and a small "you" tag on the own screen.
// Solid fills and hairline strokes render crisply at every zoom and
// keep the canvas reading as a diagram, not a mock desktop. Draggable
// unless locked.
const OWN_ACCENT = "#34d399"; // emerald — this machine
const OTHER_ACCENT = "#7dd3fc"; // sky — other machines
const BODY_FILL = "#1b1e26"; // solid panel
const HEADER_FILL = "#232732"; // solid header band
const HAIRLINE = "rgba(255,255,255,0.08)";
const RADIUS = 3; // very small rounding — crisp, not bubbly

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
  const headerH = Math.min(24, Math.max(16, h * 0.12));
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
      {/* Body: one solid rect with a hairline stroke; the selection is
          the accent stroke itself — no glow, no shadow. */}
      <Rect
        width={w}
        height={h}
        cornerRadius={RADIUS}
        fill={BODY_FILL}
        stroke={selected ? accent : own ? "rgba(52,211,153,0.55)" : "rgba(148,163,184,0.35)"}
        strokeWidth={selected ? 2 : 1}
        listening
      />
      {/* Header band: solid, slightly lighter, hairline separated. */}
      <Rect
        width={w}
        height={headerH}
        cornerRadius={[RADIUS, RADIUS, 0, 0]}
        fill={HEADER_FILL}
        listening={false}
      />
      <Rect y={headerH} width={w} height={1} fill={HAIRLINE} listening={false} />
      {/* Accent tick + name, left-aligned in the header. */}
      <Rect x={8} y={headerH / 2 - 3.5} width={3} height={7} cornerRadius={1} fill={accent} listening={false} />
      <Text
        text={name}
        fontSize={11}
        fontStyle="500"
        fill="rgba(255,255,255,0.92)"
        letterSpacing={0.3}
        x={16}
        y={0}
        width={Math.max(0, w - 16 - (own ? 34 : 8))}
        height={headerH}
        verticalAlign="middle"
        ellipsis
        listening={false}
      />
      {/* "you" tag, top-right: plain text, no pill. */}
      {own && (
        <Text
          text="you"
          fontSize={9}
          fontStyle="600"
          fill={OWN_ACCENT}
          align="right"
          width={Math.max(0, w - 8)}
          x={0}
          y={0}
          height={headerH}
          verticalAlign="middle"
          listening={false}
        />
      )}
      {/* Resolution: centered in the body, quiet monospace. No pill —
          the solid body is contrast enough. */}
      <Text
        text={res}
        fontSize={10}
        fontFamily="ui-monospace, monospace"
        fill="rgba(255,255,255,0.45)"
        align="center"
        width={w}
        y={headerH + (h - headerH) / 2 - 6}
        listening={false}
      />
    </Group>
  );
}