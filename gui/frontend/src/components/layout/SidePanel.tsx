import { Copy, Trash2 } from "lucide-react";
import type { Screen } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

interface SidePanelProps {
  screen: Screen | null;
  index: number; // selected screen index, -1 when none
  lock: boolean;
  onPatch: (i: number, patch: Partial<Screen>) => void;
  onDuplicate: () => void;
  onDelete: () => void;
}

export default function SidePanel({
  screen,
  index,
  lock,
  onPatch,
  onDuplicate,
  onDelete,
}: SidePanelProps) {
  const none = !screen;
  const fieldsDisabled = none || lock;

  return (
    <aside className="flex w-72 shrink-0 flex-col border-l">
      <div className="space-y-4 overflow-y-auto p-4">
        <div>
          <h2 className="text-sm font-semibold">
            {screen ? (
              <>
                {screen.name || "(unnamed)"}
                {index === 0 && (
                  <span className="ml-2 rounded bg-primary px-1 py-px text-[9px] font-bold text-primary-foreground">
                    YOUR MACHINE
                  </span>
                )}
              </>
            ) : (
              "No screen selected"
            )}
          </h2>
          <p className="text-xs text-muted-foreground">
            {screen ? `position ${screen.x}, ${screen.y}` : "Click a screen to edit it."}
          </p>
        </div>

        <div className="space-y-2">
          <div className="space-y-1">
            <Label htmlFor="s-name" className="text-xs">
              Name
            </Label>
            <Input
              id="s-name"
              value={screen?.name ?? ""}
              disabled={fieldsDisabled}
              placeholder="e.g. hp"
              onChange={(e) => index >= 0 && onPatch(index, { name: e.target.value })}
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
                value={screen?.width ?? ""}
                disabled={fieldsDisabled}
                onChange={(e) =>
                  index >= 0 && onPatch(index, { width: parseInt(e.target.value, 10) || 0 })
                }
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
                value={screen?.height ?? ""}
                disabled={fieldsDisabled}
                onChange={(e) =>
                  index >= 0 && onPatch(index, { height: parseInt(e.target.value, 10) || 0 })
                }
              />
            </div>
          </div>
        </div>

        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            className="flex-1"
            disabled={none || lock}
            onClick={onDuplicate}
          >
            <Copy className="h-4 w-4" /> Duplicate
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="flex-1 text-destructive hover:text-destructive"
            disabled={none || lock || index <= 0}
            title={index === 0 && screen ? "This screen is your machine — it can't be removed." : undefined}
            onClick={onDelete}
          >
            <Trash2 className="h-4 w-4" /> Delete
          </Button>
        </div>
        {index === 0 && screen && (
          <p className="text-xs text-muted-foreground">
            This screen is your machine — it can't be removed.
          </p>
        )}
      </div>
    </aside>
  );
}