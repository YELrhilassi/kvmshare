import { useCallback, useEffect, useRef, useState } from "react";
import { Kbd, keyName } from "./ChordRecorder";

// LiveKeys — the visual proof that keyboard capture works: every key
// the user pushes flashes as it passes, and every key still held glows
// sticky until released. Data comes from the chord recorder's raw
// event stream (same capture path the bindings use), so what this
// panel shows is exactly what the recorder sees — no separate capture
// path to drift out of sync.
//
// While a shared session is live, the server kernel-grabs the physical
// devices (input isolation): the webview then sees nothing and this
// panel honestly reports that instead of pretending to listen.

interface Flash {
  id: number;
  name: string;
}

/** Milliseconds a pushed key stays lit in the tape after release. */
const FLASH_MS = 350;
/** Cap on the "recently pushed" tape so a burst never floods the row. */
const TAPE_MAX = 8;

export function useLiveKeys() {
  const [held, setHeld] = useState<string[]>([]);
  const [tape, setTape] = useState<Flash[]>([]);
  const seq = useRef(0);
  const timers = useRef(new Set<number>());

  const push = useCallback((name: string, down: boolean) => {
    if (down) {
      const id = ++seq.current;
      setTape((t) => [...t.slice(-(TAPE_MAX - 1)), { id, name }]);
      setHeld((h) => (h.includes(name) ? h : [...h, name]));
      const timer = window.setTimeout(() => {
        timers.current.delete(timer);
        setTape((cur) => cur.filter((f) => f.id !== id));
      }, FLASH_MS);
      timers.current.add(timer);
    } else {
      setHeld((h) => h.filter((k) => k !== name));
    }
  }, []);

  useEffect(() => {
    const timers_ = timers.current;
    return () => {
      for (const t of timers_) window.clearTimeout(t);
      timers_.clear();
    };
  }, []);

  return { held, tape, push };
}

export function LiveKeys({
  held,
  tape,
  live,
  deafReason,
}: {
  held: string[];
  tape: Flash[];
  live: { ctrl: boolean; alt: boolean; shift: boolean; meta: boolean };
  /** Non-empty: capture cannot see keys right now (why, in plain words). */
  deafReason: string;
}) {
  const mods: [string, boolean][] = [
    ["Ctrl", live.ctrl],
    ["Alt", live.alt],
    ["Shift", live.shift],
    ["Super", live.meta],
  ];

  return (
    <div className="rounded-xl border border-border/60 bg-card/40 p-4">
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">Live keys</span>
        {deafReason && <span className="text-[11px] text-amber-500">{deafReason}</span>}
      </div>

      <div className="mt-3 flex min-h-9 flex-wrap items-center gap-1.5">
        {tape.length === 0 && held.length === 0 && !mods.some(([, on]) => on) && (
          <span className="text-xs text-muted-foreground/50">
            {deafReason || "Push any key — it shows here as you type."}
          </span>
        )}
        {tape.map((f) => (
          <Kbd key={f.id}>{f.name}</Kbd>
        ))}
      </div>

      {held.length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5 border-t border-border/40 pt-2">
          <span className="text-[11px] text-muted-foreground">held:</span>
          {held.map((k) => (
            <span
              key={k}
              className="inline-flex min-w-6 items-center justify-center rounded border border-primary/50 bg-primary/15 px-1.5 py-0.5 font-mono text-[11px] leading-none text-foreground"
            >
              {k}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
