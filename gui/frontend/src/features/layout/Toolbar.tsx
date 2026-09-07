import { ZoomIn, ZoomOut, Lock, Unlock, Magnet, Plus, Save } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Separator } from "@/components/ui/separator";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

function Tip({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

interface ToolbarProps {
  percent: number;
  minPercent: number;
  maxPercent: number;
  onPercentChange: (percent: number) => void;
  onZoomBy: (factor: number) => void;
  onFit: () => void;
  snap: boolean;
  onSnapChange: (v: boolean) => void;
  lock: boolean;
  onLockChange: (v: boolean) => void;
  onAdd: () => void;
  onSave: () => void;
  dirty: boolean;
  savedMsg: string;
  error: string;
}

// Icon-first controls: zoom on the left, snap/lock toggles in the
// middle, add/save on the right. Every icon has a tooltip; the canvas
// gestures (drag, space+pan, wheel, arrows) are discoverable in use.
export default function Toolbar({
  percent,
  minPercent,
  maxPercent,
  onPercentChange,
  onZoomBy,
  onFit,
  snap,
  onSnapChange,
  lock,
  onLockChange,
  onAdd,
  onSave,
  dirty,
  savedMsg,
  error,
}: ToolbarProps) {
  return (
    <div className="flex items-center gap-0.5 border-b px-2 py-1.5">
      <Tip label="Zoom out">
        <Button variant="ghost" size="icon" onClick={() => onZoomBy(1 / 1.5)}>
          <ZoomOut className="h-4 w-4" />
        </Button>
      </Tip>
      <Slider
        className="w-24"
        min={minPercent}
        max={maxPercent}
        step={5}
        value={[Math.round(percent)]}
        onValueChange={([v]) => onPercentChange(v)}
      />
      <Tip label="Zoom in">
        <Button variant="ghost" size="icon" onClick={() => onZoomBy(1.5)}>
          <ZoomIn className="h-4 w-4" />
        </Button>
      </Tip>
      <Tip label="Fit the whole desktop">
        <button
          onClick={onFit}
          className="w-12 cursor-pointer text-center font-mono text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          {Math.round(percent)}%
        </button>
      </Tip>

      <Separator orientation="vertical" className="mx-1.5 h-5" />

      <Tip label={snap ? "Snapping on" : "Snapping off"}>
        <Button
          variant="ghost"
          size="icon"
          onClick={() => onSnapChange(!snap)}
          className={snap ? "bg-muted text-foreground" : "text-muted-foreground"}
        >
          <Magnet className="h-4 w-4" />
        </Button>
      </Tip>
      <Tip label={lock ? "Unlock layout" : "Lock layout"}>
        <Button
          variant="ghost"
          size="icon"
          onClick={() => onLockChange(!lock)}
          className={lock ? "bg-muted text-foreground" : "text-muted-foreground"}
        >
          {lock ? <Lock className="h-4 w-4" /> : <Unlock className="h-4 w-4" />}
        </Button>
      </Tip>

      <div className="flex-1" />

      {error && <span className="text-xs text-destructive">{error}</span>}
      {!error && savedMsg && <span className="text-xs text-emerald-600">{savedMsg}</span>}

      <Tip label="Add a screen">
        <Button variant="outline" size="sm" onClick={onAdd} disabled={lock}>
          <Plus className="h-4 w-4" /> Add
        </Button>
      </Tip>
      <Tip label="Save the layout">
        <Button
          onClick={onSave}
          size="sm"
          disabled={lock}
          variant={dirty ? "default" : "outline"}
          className={cn(!dirty && "text-muted-foreground")}
        >
          <Save className="h-4 w-4" />
        </Button>
      </Tip>
    </div>
  );
}