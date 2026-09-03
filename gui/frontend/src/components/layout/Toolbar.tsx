import { ZoomIn, ZoomOut, Maximize, Lock, Unlock, Magnet, Plus, Save, Move } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { Separator } from "@/components/ui/separator";

export const MIN_ZOOM = 0.05;
export const MAX_ZOOM = 4;

interface ToolbarProps {
  zoom: number;
  onZoomChange: (zoom: number) => void;
  onZoomBy: (factor: number) => void;
  onFit: () => void;
  onReset: () => void;
  snap: boolean;
  onSnapChange: (v: boolean) => void;
  lock: boolean;
  onLockChange: (v: boolean) => void;
  onAdd: () => void;
  onSave: () => void;
  dirty: boolean;
  savedMsg: string;
}

export default function Toolbar({
  zoom,
  onZoomChange,
  onZoomBy,
  onFit,
  onReset,
  snap,
  onSnapChange,
  lock,
  onLockChange,
  onAdd,
  onSave,
  dirty,
  savedMsg,
}: ToolbarProps) {
  return (
    <div className="flex flex-wrap items-center gap-2 border-b px-3 py-2">
      <Button variant="ghost" size="icon" title="Zoom out" onClick={() => onZoomBy(1 / 1.25)}>
        <ZoomOut className="h-4 w-4" />
      </Button>
      <Slider
        className="w-28"
        min={MIN_ZOOM * 100}
        max={MAX_ZOOM * 100}
        step={5}
        value={[Math.round(zoom * 100)]}
        onValueChange={([v]) => onZoomChange(v / 100)}
      />
      <Button variant="ghost" size="icon" title="Zoom in" onClick={() => onZoomBy(1.25)}>
        <ZoomIn className="h-4 w-4" />
      </Button>
      <span className="w-12 text-center font-mono text-xs text-muted-foreground">
        {Math.round(zoom * 100)}%
      </span>
      <Button variant="ghost" size="icon" title="Fit layout" onClick={onFit}>
        <Maximize className="h-4 w-4" />
      </Button>
      <Button variant="ghost" size="sm" onClick={onReset} className="text-xs">
        100%
      </Button>

      <Separator orientation="vertical" className="h-5" />

      <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Magnet className="h-4 w-4" />
        <Switch checked={snap} onCheckedChange={onSnapChange} />
        snap
      </label>
      <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
        {lock ? <Lock className="h-4 w-4" /> : <Unlock className="h-4 w-4" />}
        <Switch checked={lock} onCheckedChange={onLockChange} />
        lock
      </label>

      <div className="flex-1" />

      <span className="text-xs text-muted-foreground">
        <Move className="mr-1 inline h-3.5 w-3.5" />
        drag to move · middle-drag to pan · wheel to zoom · arrows to nudge
      </span>

      <Button onClick={onAdd} variant="outline" size="sm" disabled={lock}>
        <Plus className="h-4 w-4" /> Add
      </Button>
      <Button onClick={onSave} size="sm" disabled={lock} variant={dirty ? "default" : "outline"}>
        <Save className="h-4 w-4" /> {dirty ? "Save*" : "Save"}
      </Button>
      {savedMsg && <span className="text-xs text-emerald-600">{savedMsg}</span>}
    </div>
  );
}