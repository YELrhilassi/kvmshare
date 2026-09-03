import { Copy, Trash2 } from "lucide-react";
import type { Screen } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";

interface SidePanelProps {
  screen: Screen | null;
  index: number; // selected screen index, -1 when none
  lock: boolean;
  port: string;
  error: string;
  serverRunning: boolean;
  onPatch: (i: number, patch: Partial<Screen>) => void;
  onDuplicate: () => void;
  onDelete: () => void;
  onPortChange: (v: string) => void;
}

export default function SidePanel({
  screen,
  index,
  lock,
  port,
  error,
  serverRunning,
  onPatch,
  onDuplicate,
  onDelete,
  onPortChange,
}: SidePanelProps) {
  const none = !screen;
  const fieldsDisabled = none || lock;

  return (
    <aside className="w-72 shrink-0 space-y-4 overflow-y-auto border-l p-4">
      <div>
        <h2 className="text-sm font-semibold">
          {screen ? (
            <>
              {screen.name || "(unnamed)"}
              {index === 0 && (
                <span className="ml-2 text-xs font-normal text-muted-foreground">(server)</span>
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
          onClick={onDelete}
        >
          <Trash2 className="h-4 w-4" /> Delete
        </Button>
      </div>
      {index === 0 && screen && (
        <p className="text-xs text-muted-foreground">The server's own screen can't be deleted.</p>
      )}

      <Separator />

      <div className="space-y-1">
        <Label htmlFor="l-port" className="text-xs">
          Server port
        </Label>
        <Input
          id="l-port"
          type="number"
          min={1024}
          max={65535}
          value={port}
          disabled={lock}
          onChange={(e) => onPortChange(e.target.value)}
        />
      </div>

      {error && <p className="text-xs text-destructive">{error}</p>}
      {serverRunning && (
        <p className="text-xs text-muted-foreground">Saving restarts the running server.</p>
      )}
    </aside>
  );
}