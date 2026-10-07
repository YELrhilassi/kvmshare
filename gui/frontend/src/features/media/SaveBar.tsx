// The save row: one button plus one status slot, used by the media
// section and the audio sections alike. The dirty/saved/error logic
// lives in the store; this renders it. `label` names what is being
// saved so two visible rows never say the same thing.

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export function SaveBar({
  dirty,
  justSaved,
  error,
  label,
  onSave,
}: {
  dirty: boolean;
  justSaved: boolean;
  error: string | null;
  label: string;
  onSave: () => void;
}) {
  return (
    <div className="flex items-center gap-3">
      <Button onClick={onSave} disabled={!dirty}>
        {dirty ? label : "Saved"}
      </Button>
      {justSaved && !dirty && <span className="text-xs text-emerald-600">saved</span>}
      {error && <span className="text-xs text-destructive">{error}</span>}
    </div>
  );
}

/** The page-wide status strip under the last section. */
export function StatusLine({ error, justSaved, dirty }: { error: string | null; justSaved: boolean; dirty: boolean }) {
  if (!error && !(justSaved && dirty)) return null;
  return (
    <p className={cn("mt-4 text-xs", error ? "text-destructive" : "text-emerald-600")}>
      {error ?? "saved"}
    </p>
  );
}
