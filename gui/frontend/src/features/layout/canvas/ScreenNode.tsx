import { memo } from "react";
import { Lock } from "lucide-react";
import type { Screen } from "@/lib/bridge";
import { cn } from "@/lib/utils";

interface Props {
  screen: Screen;
  index: number;
  selected: boolean;
  lock: boolean;
  scale: number;
  onPointerDown: (e: React.PointerEvent<HTMLDivElement>, i: number) => void;
  onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => void;
  onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => void;
}

// One screen on the canvas. Memoized: during a drag the element is moved
// by direct DOM writes, so this only re-renders when the screen data
// itself changes. Everything inside is counter-scaled by `scale` so it
// renders at native size on screen at any zoom.
function ScreenNode({ screen, index, selected, lock, scale, onPointerDown, onPointerMove, onPointerUp }: Props) {
  const inv = 1 / Math.max(scale, 0.0001);

  return (
    <div
      onPointerDown={(e) => onPointerDown(e, index)}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      className={cn(
        "absolute rounded-md transition-shadow select-none",
        index === 0 ? "border-dashed" : "border-solid",
        "border-primary/70",
        selected && "ring-2 ring-ring",
        lock ? "cursor-default" : "cursor-grab active:cursor-grabbing",
      )}
      style={{
        left: screen.x,
        top: screen.y,
        width: screen.width,
        height: screen.height,
        borderWidth: `${2 * inv}px`,
        backdropFilter: `blur(${5 * inv}px)`,
        WebkitBackdropFilter: `blur(${5 * inv}px)`,
        background: index === 0 ? "rgba(255, 255, 255, 0.1)" : "rgba(255, 255, 255, 0.07)",
        boxShadow: `0 ${4 * inv}px ${18 * inv}px rgba(0, 0, 0, 0.35)`,
      }}
      title={`${screen.name} — ${screen.width}×${screen.height} at ${screen.x},${screen.y}`}
    >
      <span
        className="pointer-events-none absolute top-1 left-1.5 flex items-center gap-1 text-[11px] font-semibold"
        style={{ transform: `scale(${inv})`, transformOrigin: "top left" }}
      >
        {screen.name || "(unnamed)"}
        {index === 0 && (
          <span className="rounded bg-primary px-1 py-px text-[9px] font-bold text-primary-foreground">
            YOUR MACHINE
          </span>
        )}
        {lock && <Lock className="h-3 w-3 text-muted-foreground" />}
      </span>
      <span
        className="pointer-events-none absolute right-1.5 bottom-1 font-mono text-[10px] text-muted-foreground"
        style={{ transform: `scale(${inv})`, transformOrigin: "bottom right" }}
      >
        {screen.width}×{screen.height}
      </span>
    </div>
  );
}

export default memo(ScreenNode);