import { Copy, Plus, Trash2 } from "lucide-react";
import type { Screen } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

interface Props {
  screen: Screen | null;
  index: number; // selected screen index, -1 when none
  lock: boolean;
  onPatch: (i: number, patch: Partial<Screen>) => void;
  onAdd: () => void;
  onDuplicate: () => void;
  onDelete: () => void;
}

export default function ScreenInspector({
  screen,
  index,
  lock,
  onPatch,
  onAdd,
  onDuplicate,
  onDelete,
}: Props) {
  if (!screen) {
    return (
      <aside className="flex w-64 shrink-0 flex-col border-l">
        <div className="flex flex-1 flex-col items-center justify-center gap-4 p-6 text-center">
          <p className="text-xs text-muted-foreground">Select a screen on the canvas.</p>
          <Button variant="outline" size="sm" onClick={onAdd} disabled={lock}>
            <Plus className="h-4 w-4" /> Add a screen
          </Button>
        </div>
      </aside>
    );
  }

  const own = index === 0;
  const disabled = lock;

  return (
    <aside className="flex w-64 shrink-0 flex-col border-l">
      <div className="flex items-center justify-between border-b px-4 py-2.5">
        <h2 className="text-xs font-semibold tracking-widest text-muted-foreground uppercase">Screen</h2>
        {own && (
          <span className="rounded-full bg-primary px-1.5 py-px text-[9px] font-bold text-primary-foreground">
            you
          </span>
        )}
      </div>

      <div className="space-y-4 overflow-y-auto p-4">
        <div className="space-y-1">
          <Label htmlFor="s-name" className="text-xs">
            Name
          </Label>
          <Input
            id="s-name"
            value={screen.name}
            disabled={disabled}
            placeholder="e.g. hp"
            onChange={(e) => onPatch(index, { name: e.target.value })}
          />
        </div>

        <div className="grid grid-cols-2 gap-2">
          <div className="space-y-1">
            <Label htmlFor="s-w" className="text-xs">
              Width
            </Label>
            <Input
              id="s-w"
              type="number"
              min={320}
              step={10}
              value={screen.width}
              disabled={disabled}
              onChange={(e) => onPatch(index, { width: parseInt(e.target.value, 10) || 0 })}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="s-h" className="text-xs">
              Height
            </Label>
            <Input
              id="s-h"
              type="number"
              min={240}
              step={10}
              value={screen.height}
              disabled={disabled}
              onChange={(e) => onPatch(index, { height: parseInt(e.target.value, 10) || 0 })}
            />
          </div>
        </div>

        <div className="flex items-center justify-between text-xs">
          <span className="text-muted-foreground">Position</span>
          <span className="font-mono">
            {screen.x}, {screen.y}
          </span>
        </div>

        <div className="flex gap-2">
          <Button variant="outline" size="sm" className="flex-1" disabled={disabled} onClick={onDuplicate}>
            <Copy className="h-4 w-4" /> Duplicate
          </Button>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="outline"
                size="sm"
                className="flex-1 text-destructive hover:text-destructive"
                disabled={disabled || own}
                onClick={onDelete}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{own ? "This screen is your machine — it can't be removed" : "Delete screen"}</TooltipContent>
          </Tooltip>
        </div>
      </div>
    </aside>
  );
}