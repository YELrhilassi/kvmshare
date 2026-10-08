// The page's autosave readout: one slot that says what the files are
// doing right now — a write in flight, a failure worth acting on, or a
// brief "Saved". There is no button: edits are written on their own (see
// mediaStore), so this only ever reports, never asks.
//
// It is deliberately quiet: "Saving…" appears the instant an edit lands
// and usually resolves before it is read; the failure state is the only
// one that asks for attention, because it means the screen and the file
// disagree.

import { cn } from "@/lib/utils";

export function AutoSaveStatus({
  dirty,
  saving,
  justSaved,
  error,
}: {
  dirty: boolean;
  saving: boolean;
  justSaved: boolean;
  error: string | null;
}) {
  if (error) {
    return <p className="text-xs text-destructive">Not saved — {error}</p>;
  }
  if (saving || dirty) {
    return <p className={cn("text-xs text-muted-foreground")}>Saving…</p>;
  }
  if (justSaved) {
    return <p className="text-xs text-emerald-600">Saved</p>;
  }
  return null;
}
