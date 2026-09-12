import { useEffect, useState } from "react";
import { api, type InputSection, type ShortcutSection, type Binding } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Slider } from "@/components/ui/slider";
import { Section } from "@/components/Section";
import { PageSkeleton } from "@/components/PageSkeleton";
import {
  useChordRecorder,
  keyName,
  modLabels,
  Kbd,
  type Chord,
} from "./ChordRecorder";
import { cn } from "@/lib/utils";

// Input & shortcuts: the sub-page of Layout that configures *how input
// behaves* rather than where screens sit. Three concerns, clearly
// separated:
//
// * Shortcuts — chords (modifier set + key) bound to actions. Recorded
//   as canonical HID key ids, so a binding means the same physical key
//   on every platform pair, and a bound chord **overrides** whatever
//   the OS would do with it (Win+Tab, media keys): the capture layer
//   swallows it before the desktop reacts.
// * Mouse — pointer speed, wheel speed, natural scrolling. Portable
//   transforms on the shared stream, applied identically for every
//   client.
// * Keyboard — forwarded by physical identity; only the bound chords
//   above are intercepted.

const ACTIONS: { value: string; label: string; hint: string }[] = [
  { value: "cycle", label: "Cycle screens", hint: "Move to the next reachable machine" },
  { value: "lock", label: "Toggle wall lock", hint: "Freeze or release the screen edges" },
  { value: "home", label: "Go home", hint: "Return control to this machine" },
];

function bindingToChord(b: Binding): Chord {
  return { ctrl: b.mods.ctrl, alt: b.mods.alt, shift: b.mods.shift, meta: b.mods.meta, key: b.key };
}

function chordMatches(a: Chord, b: Chord): boolean {
  return a.key === b.key && a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift && a.meta === b.meta;
}

function chordEqualsMods(c: Chord, mods: Binding["mods"], key: number): boolean {
  return chordMatches(c, { ctrl: mods.ctrl, alt: mods.alt, shift: mods.shift, meta: mods.meta, key });
}

/** The chord registry row: kbd chips for the chord, action, remove. */
function BindingRow({
  binding,
  duplicate,
  onRemove,
}: {
  binding: Binding;
  duplicate: boolean;
  onRemove: () => void;
}) {
  const mods = modLabels(binding.mods);
  return (
    <li className="flex items-center gap-3 rounded-lg border border-border/50 bg-card/40 px-3 py-2">
      <span className="flex items-center gap-1">
        {mods.map((m) => (
          <Kbd key={m}>{m}</Kbd>
        ))}
        <Kbd>{keyName(binding.key)}</Kbd>
      </span>
      <span className="flex-1 truncate text-xs text-muted-foreground">
        {binding.action === "switch" ? `Switch to ${binding.screen || "?"}` : binding.action}
      </span>
      {duplicate && (
        <span className="rounded bg-destructive/10 px-1.5 py-0.5 text-[10px] font-medium text-destructive">
          duplicate
        </span>
      )}
      <button
        className="text-muted-foreground/50 hover:text-destructive"
        onClick={onRemove}
        aria-label="remove shortcut"
      >
        ×
      </button>
    </li>
  );
}

/** The live recorder panel: chips light up as modifiers are held. */
function RecorderPanel({ live }: { live: Chord }) {
  const mods = modLabels(live);
  return (
    <div className="mt-3 rounded-lg border border-dashed border-primary/50 bg-primary/5 px-3 py-3">
      <div className="flex items-center gap-1.5">
        {["Ctrl", "Alt", "Shift", "Super"].map((m) => {
          const on = mods.includes(m);
          return (
            <Kbd key={m} dim={!on}>
              {m}
            </Kbd>
          );
        })}
        <span className="mx-1 text-muted-foreground/40">+</span>
        <Kbd dim>key…</Kbd>
      </div>
      <p className="mt-2 text-[11px] text-primary/80">
        Hold modifiers, then tap the key. The chord overrides OS shortcuts (Win+Tab, media keys). Esc cancels.
      </p>
    </div>
  );
}

export default function InputShortcutsPage({ onSaved }: { onSaved?: () => void }) {
  const [config, setConfig] = useState<{ shortcuts?: ShortcutSection; input?: InputSection } | null>(null);
  const [loadErr, setLoadErr] = useState("");
  const [err, setErr] = useState("");
  const [saved, setSaved] = useState(false);
  const [recording, setRecording] = useState(false);
  const [pendingAction, setPendingAction] = useState<{ action: string; screen: string } | null>(null);
  const [live, setLive] = useState<Chord>({ ctrl: false, alt: false, shift: false, meta: false, key: 0 });

  const load = async () => {
    setLoadErr("");
    try {
      setConfig(await api().LoadConfig());
    } catch (e) {
      setLoadErr(String(e));
    }
  };
  useEffect(() => {
    void load();
  }, []);

  const save = async (patch: { shortcuts?: ShortcutSection; input?: InputSection }) => {
    if (!config) return;
    setErr("");
    try {
      const next = { ...config, ...patch };
      await api().SaveConfig(next);
      setConfig(next);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1500);
      onSaved?.();
    } catch (e) {
      setErr(String(e));
    }
  };

  useChordRecorder(
    recording,
    (chord) => {
      setLive({ ...chord, key: 0 });
      setRecording(false);
      if (pendingAction && config) {
        const binding: Binding = {
          mods: { ctrl: chord.ctrl, alt: chord.alt, shift: chord.shift, meta: chord.meta },
          key: chord.key,
          action: pendingAction.action,
          screen: pendingAction.screen,
        };
        const shortcuts = config.shortcuts ?? { enabled: true, bindings: [] };
        // A chord already bound (or the bare escape key, always
        // reserved) is refused with a message instead of silently
        // creating a dead or conflicting entry.
        if (chord.key === 0x29) {
          setErr("Esc is reserved for returning home — pick another key.");
          setPendingAction(null);
          return;
        }
        if (shortcuts.bindings.some((b) => chordEqualsMods(chord, b.mods, b.key))) {
          setErr(`${[...modLabels(binding.mods), keyName(binding.key)].join(" + ")} is already bound.`);
          setPendingAction(null);
          return;
        }
        void save({ shortcuts: { ...shortcuts, bindings: [...shortcuts.bindings, binding] } });
      }
      setPendingAction(null);
    },
    () => {
      setRecording(false);
      setPendingAction(null);
    },
  );

  if (loadErr) {
    return (
      <div className="mx-auto w-full max-w-2xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Input &amp; shortcuts</h1>
        <p className="mt-4 text-sm text-destructive">Could not load: {loadErr}</p>
        <Button className="mt-3" variant="outline" onClick={() => void load()}>Retry</Button>
      </div>
    );
  }
  if (!config) return <PageSkeleton rows={3} />;

  const bindings = config.shortcuts?.bindings ?? [];
  const input = config.input ?? { pointerSpeed: 1, wheelSpeed: 1, swapScroll: false };
  const screens = config.screens ?? [];

  const duplicates = new Set(
    bindings
      .map((b, i) => ({ sig: `${b.mods.ctrl}|${b.mods.alt}|${b.mods.shift}|${b.mods.meta}|${b.key}`, i }))
      .filter((x, _, all) => all.filter((y) => y.sig === x.sig).length > 1)
      .map((x) => x.i),
  );

  const removeBinding = (i: number) => {
    save({ shortcuts: { ...(config.shortcuts ?? { enabled: true }), bindings: bindings.filter((_, j) => j !== i) } });
  };

  return (
    <div className="mx-auto w-full max-w-2xl px-8 py-8">
      <h1 className="text-lg font-semibold tracking-tight">Input &amp; shortcuts</h1>
      <p className="mt-1 text-sm text-muted-foreground">
        How the shared keyboard and mouse behave, and the chords that switch screens without touching the mouse.
        Bindings are recorded as physical keys, so they work the same on every machine.
      </p>

      <Section title="Shortcuts" className="mt-8">
        <div className="flex items-center justify-between gap-6 py-3">
          <div>
            <div className="text-sm">Shortcuts enabled</div>
            <p className="text-xs text-muted-foreground">
              When off, every key is forwarded to the controlled machine and no chord is intercepted.
            </p>
          </div>
          <Switch
            checked={config.shortcuts?.enabled ?? true}
            onCheckedChange={(v) => save({ shortcuts: { ...(config.shortcuts ?? { bindings: [] }), enabled: v } })}
          />
        </div>

        <div className="border-t border-border/50 pt-3">
          {bindings.length === 0 && (
            <p className="text-xs text-muted-foreground/60">No custom shortcuts yet.</p>
          )}
          <ul className="space-y-1.5">
            {bindings.map((b, i) => (
              <BindingRow
                key={i}
                binding={b}
                duplicate={duplicates.has(i)}
                onRemove={() => removeBinding(i)}
              />
            ))}
          </ul>
          {recording ? (
            <RecorderPanel live={live} />
          ) : (
            <div className="mt-3 flex items-center gap-2">
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  setPendingAction((p) => p ?? { action: "cycle", screen: "" });
                  setRecording(true);
                }}
              >
                Add shortcut
              </Button>
              <select
                className="h-8 rounded-md border border-border/70 bg-muted/40 px-2 text-xs text-foreground outline-none focus:border-primary"
                value={
                  pendingAction
                    ? `${pendingAction.action}${pendingAction.screen ? `:${pendingAction.screen}` : ""}`
                    : "cycle"
                }
                onChange={(e) => {
                  const v = e.target.value;
                  // "switch:<screen>" encodes a screen-targeted action.
                  const sep = v.indexOf(":");
                  setPendingAction(
                    sep === -1
                      ? { action: v, screen: "" }
                      : { action: v.slice(0, sep), screen: v.slice(sep + 1) },
                  );
                }}
                aria-label="Action for the next recorded shortcut"
              >
                {ACTIONS.map((a) => (
                  <option key={a.value} value={a.value}>{a.label}</option>
                ))}
                <optgroup label="Switch to screen">
                  {screens.filter((s) => s.name).map((s) => (
                    <option key={s.name} value={`switch:${s.name}`}>{s.name}</option>
                  ))}
                </optgroup>
              </select>
            </div>
          )}
        </div>
      </Section>

      <Section title="Mouse" className="mt-6">
        <div className="py-3">
          <div className="flex items-baseline justify-between">
            <div className="text-sm">Pointer speed</div>
            <span className="font-mono text-xs text-muted-foreground">{input.pointerSpeed.toFixed(2)}×</span>
          </div>
          <p className="text-xs text-muted-foreground">
            How far the cursor travels on the controlled machine for the same hand movement. 1.00× mirrors this machine.
          </p>
          <Slider
            className="mt-3"
            min={0.25}
            max={3}
            step={0.05}
            value={[input.pointerSpeed]}
            onValueChange={([v]) => save({ input: { ...input, pointerSpeed: v } })}
          />
        </div>
        <div className="border-t border-border/50 py-3">
          <div className="flex items-baseline justify-between">
            <div className="text-sm">Wheel speed</div>
            <span className="font-mono text-xs text-muted-foreground">{input.wheelSpeed.toFixed(2)}×</span>
          </div>
          <p className="text-xs text-muted-foreground">Scroll distance per wheel notch on the controlled machine.</p>
          <Slider
            className="mt-3"
            min={0.25}
            max={8}
            step={0.25}
            value={[input.wheelSpeed]}
            onValueChange={([v]) => save({ input: { ...input, wheelSpeed: v } })}
          />
        </div>
        <div className="border-t border-border/50 py-3">
          <div className="flex items-center justify-between gap-6">
            <div>
              <div className="text-sm">Natural scrolling</div>
              <p className="text-xs text-muted-foreground">
                Invert the vertical wheel direction, like a touchscreen (content follows the fingers).
              </p>
            </div>
            <Switch checked={input.swapScroll} onCheckedChange={(v) => save({ input: { ...input, swapScroll: v } })} />
          </div>
        </div>
      </Section>

      <Section title="Keyboard" className="mt-6">
        <p className="py-3 text-xs text-muted-foreground">
          Keys are forwarded by their physical identity, so layout follows whichever machine is being controlled —
          type as usual. Only the chords recorded above are intercepted (before the controlled machine — and this
          machine's own shortcuts — see them); everything else passes through untouched.
        </p>
      </Section>

      {(err || saved) && (
        <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>{err || "Saved"}</p>
      )}
    </div>
  );
}
