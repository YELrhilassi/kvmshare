import { memo } from "react";
import { Lock } from "lucide-react";
import type { Screen } from "@/lib/bridge";
import { MODEL_SCALE } from "@/features/layout/geometry";
import { cn } from "@/lib/utils";

interface Props {
  screen: Screen;
  index: number;
  selected: boolean;
  lock: boolean;
  dragging: boolean;
  scale: number;
  onPointerDown: (e: React.PointerEvent<HTMLDivElement>, i: number) => void;
  onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => void;
  onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => void;
}

// One screen on the canvas. The label and border counter-scale so they
// render at native size on screen at any zoom; the fill is a soft
// translucent gradient with backdrop blur so the dot grid behind reads
// through without becoming noise. Memoized: during a drag the element is
// moved by direct DOM writes, so this only re-renders on real changes.
function ScreenNode({
  screen,
  index,
  selected,
  lock,
  dragging,
  scale,
  onPointerDown,
  onPointerMove,
  onPointerUp,
}: Props) {
  // Counter-scale keeps borders/labels at a constant on-screen size,
  // but the values must be CLAMPED: at low zoom the raw 1/scale would
  // turn the backdrop blur and selection shadow into multi-hundred-px
  // rasters that freeze the compositor.
  const inv = Math.min(1 / Math.max(scale, 0.0001), 2);
  const own = index === 0;
  const blur = Math.min(8 * inv, 10);
  const radius = Math.min(10 * inv, 14);
  const border = Math.min(1.5 * inv, 2.5);
  const ring = Math.min(2 * inv, 3);
  const glowBlur = Math.min(24 * inv, 28);
  const glowSpread = Math.min(6 * inv, 8);
  const shadowBlur = Math.min(16 * inv, 20);
  const shadowY = Math.min(4 * inv, 6);

  // Drawn at model scale so screens are compact blocks on the canvas;
  // the label below still shows the real resolution.
  const left = screen.x * MODEL_SCALE;
  const top = screen.y * MODEL_SCALE;
  const width = screen.width * MODEL_SCALE;
  const height = screen.height * MODEL_SCALE;

  return (
    <div
      onPointerDown={(e) => onPointerDown(e, index)}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      className={cn(
        "absolute select-none transition-[box-shadow,border-color,filter,opacity]",
        own ? "border-white/40" : "border-white/25",
        selected && "border-primary",
        lock ? "cursor-default" : "cursor-grab active:cursor-grabbing",
        dragging && "screen-dragging",
      )}
      style={{
        left,
        top,
        width,
        height,
        borderWidth: `${border}px`,
        borderRadius: `${radius}px`,
        background: `linear-gradient(180deg, ${
          own ? "rgba(255,255,255,0.17)" : "rgba(255,255,255,0.1)"
        }, ${own ? "rgba(255,255,255,0.07)" : "rgba(255,255,255,0.04)"})`,
        backdropFilter: `blur(${blur}px)`,
        WebkitBackdropFilter: `blur(${blur}px)`,
        // The selection ring is drawn here — Tailwind's ring utilities
        // use box-shadow, which this inline shadow would override.
        boxShadow: selected
          ? `0 0 0 ${ring}px var(--ring), 0 ${glowSpread}px ${glowBlur}px rgba(0, 0, 0, 0.4)`
          : `0 ${shadowY}px ${shadowBlur}px rgba(0, 0, 0, 0.28)`,
      }}
      title={`${screen.name} — ${screen.width}×${screen.height} at ${screen.x},${screen.y}`}
    >
      {/* Name, centered near the top, always ~12 screen px. */}
      <span
        className="pointer-events-none absolute inset-x-0 top-1.5"
        style={{ transform: `scale(${inv})`, transformOrigin: "center top" }}
      >
        <span className="mx-auto flex w-max items-center gap-1.5">
          <span className="text-[12px] font-semibold tracking-tight text-white/85">
            {screen.name || "screen"}
          </span>
          {own && (
            <span className="rounded-full bg-primary px-1.5 py-px text-[9px] font-bold text-primary-foreground">
              you
            </span>
          )}
          {lock && <Lock className="h-3 w-3 text-white/40" />}
        </span>
      </span>

      {/* Dimensions, centered at the bottom, always ~9.5 screen px. */}
      <span
        className="pointer-events-none absolute inset-x-0 bottom-1.5"
        style={{ transform: `scale(${inv})`, transformOrigin: "center bottom" }}
      >
        <span className="mx-auto block w-max font-mono text-[9.5px] text-white/45">
          {screen.width}×{screen.height}
        </span>
      </span>
    </div>
  );
}

export default memo(ScreenNode);