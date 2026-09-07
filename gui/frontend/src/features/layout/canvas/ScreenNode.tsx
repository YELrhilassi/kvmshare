import { memo } from "react";
import { Lock } from "lucide-react";
import type { Screen } from "@/lib/bridge";
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
  const inv = 1 / Math.max(scale, 0.0001);
  const own = index === 0;

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
        left: screen.x,
        top: screen.y,
        width: screen.width,
        height: screen.height,
        borderWidth: `${1.5 * inv}px`,
        borderRadius: `${10 * inv}px`,
        background: `linear-gradient(180deg, ${
          own ? "rgba(255,255,255,0.17)" : "rgba(255,255,255,0.1)"
        }, ${own ? "rgba(255,255,255,0.07)" : "rgba(255,255,255,0.04)"})`,
        backdropFilter: `blur(${8 * inv}px)`,
        WebkitBackdropFilter: `blur(${8 * inv}px)`,
        // The selection ring is drawn here — Tailwind's ring utilities
        // use box-shadow, which this inline shadow would override.
        boxShadow: selected
          ? `0 0 0 ${2 * inv}px var(--ring), 0 ${6 * inv}px ${24 * inv}px rgba(0, 0, 0, 0.4)`
          : `0 ${4 * inv}px ${16 * inv}px rgba(0, 0, 0, 0.28)`,
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