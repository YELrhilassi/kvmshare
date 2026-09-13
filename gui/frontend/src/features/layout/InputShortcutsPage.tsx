import { useEffect, useState } from "react";
import { api, type LayoutConfig, type Binding } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Slider } from "@/components/ui/slider";
import { Section } from "@/components/Section";
import { PageSkeleton } from "@/components/PageSkeleton";
import { Repeat, Lock, Home, Monitor, Mouse, Keyboard } from "lucide-react";
import { useChordRecorder, keyName, modLabels, Kbd, type Chord } from "./ChordRecorder";
import { cn } from "@/lib/utils";

// Input & shortcuts: the sub-page of Layout that configures *how input
// behaves* rather than where screens sit. Three concerns, clearly
// separated:
//
// * Shortcuts — one card per action. A card shows the chord currently
//   bound to it as key chips; "record" captures a replacement live in
//   the card itself (chips light up as modifiers are held). No mode
//   dropdown, no separate registry: the action *is* the registry entry.
//   Bindings are recorded as canonical HID key ids, so a binding means
//   the same physical key on every platform pair, and a bound chord
//   **overrides** whatever the OS would do with it (Win+Tab, media
//   keys): the capture layer swallows it before the desktop reacts.
// * Mouse — pointer speed, wheel speed, natural scrolling. Portable
//   transforms on the shared stream, applied identically for every
//   client.
// * Keyboard — forwarded by physical identity; only the bound chords
//   above are intercepted.

interface ActionSpec {
  /** Config action id: "cycle" | "lock" | "home" | "switch:<screen>". */
  id: string;
  title: string;
  hint: string;
  icon: typeof Repeat;
}

const BASE_ACTIONS: ActionSpec[] = [
  { id: "cycle", title: "Cycle screens", hint: "Move control to the next machine in the layout", icon: Repeat },
  { id: "lock", title: "Toggle wall lock", hint: "Freeze or release the screen edges", icon: Lock },
  { id: "home", title: "Go home", hint: "Return control to this machine immediately", icon: Home },
];

/** Every configured screen gets a "switch to it" card, after the base actions. */
function switchActions(screens: { name: string }[]): ActionSpec[] {
  return screens
    .filter((s) => s.name)
    .map((s) => ({
      id: `switch:${s.name}`,
      title: `Switch to ${s.name}`,
      hint: "Jump control straight to this machine",
      icon: Monitor,
    }));
}

function chordSig(b: { mods: Binding["mods"]; key: number }): string {
  return `${b.mods.ctrl}|${b.mods.alt}|${b.mods.shift}|${b.mods.meta}|${b.key}`;
}

function chordLabel(b: { mods: Binding["mods"]; key: number }): string {
  return [...modLabels(b.mods), keyName(b.key)].join(" + ");
}

/** One action card: identity, its chord (or unbound), record/clear. */
function ActionCard({
  spec,
  binding,
  recording,
  live,
  disabled,
  onRecord,
  onCancelRecord,
  onClear,
}: {
  spec: ActionSpec;
  binding?: Binding;
  recording: boolean;
  live: { ctrl: boolean; alt: boolean; shift: boolean; meta: boolean };
  disabled: boolean;
  onRecord: () => void;
  onCancelRecord: () => void;
  onClear: () => void;
}) {
  const Icon = spec.icon;
  const lit = (m: string) =>
    (m === "Ctrl" && live.ctrl) || (m === "Alt" && live.alt) || (m === "Shift" && live.shift) || (m === "Super" && live.meta);
  return (
    <div
      className={cn(
        "rounded-xl border bg-card/40 p-4 transition-colors",
        recording ? "border-primary" : "border-border/60",
        disabled && "opacity-50",
      )}
    >
      <div className="flex items-start gap-3">
        <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-border/60 bg-muted/40 text-muted-foreground">
          <Icon className="h-4 w-4" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">{spec.title}</div>
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{spec.hint}</p>
        </div>
      </div>

      <div className="mt-3 border-t border-border/40 pt-3">
        {recording ? (
          <div className="flex flex-wrap items-center gap-1.5">
            {["Ctrl", "Alt", "Shift", "Super"].map((m) => (
              <Kbd key={m} dim={!lit(m)}>
                {m}
              </Kbd>
            ))}
            <span className="mx-0.5 text-muted-foreground/40">+</span>
            <Kbd dim>key…</Kbd>
            <button
              className="ml-auto text-xs text-muted-foreground hover:text-foreground"
              onClick={onCancelRecord}
            >
              cancel
            </button>
          </div>
        ) : (
          <div className="flex items-center gap-2">
            {binding ? (
              <>
                <span className="flex items-center gap-1">
                  {modLabels(binding.mods).map((m) => (
                    <Kbd key={m}>{m}</Kbd>
                  ))}
                  <Kbd>{keyName(binding.key)}</Kbd>
                </span>
                <button
                  className="ml-1 text-muted-foreground/40 hover:text-destructive"
                  onClick={onClear}
                  aria-label={`clear ${spec.title} shortcut`}
                >
                  ×
                </button>
              </>
            ) : (
              <span className="text-xs text-muted-foreground/50">Unbound</span>
            )}
            <Button
              size="sm"
              variant="outline"
              className="ml-auto h-7 px-2.5 text-xs"
              disabled={disabled}
              onClick={onRecord}
            >
              {binding ? "Replace" : "Record"}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

export default function InputShortcutsPage({ onSaved }: { onSaved?: () => void }) {
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [loadErr, setLoadErr] = useState("");
  const [err, setErr] = useState("");
  const [saved, setSaved] = useState(false);
  // Which action card is recording (its ActionSpec.id), or null.
  const [recordingFor, setRecordingFor] = useState<string | null>(null);

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

  const save = async (patch: Partial<Pick<LayoutConfig, "shortcuts" | "input">>) => {
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

  const screens = config?.screens ?? [];
  const actions = [...BASE_ACTIONS, ...switchActions(screens)];
  const shortcuts = config?.shortcuts ?? { enabled: true, bindings: [] };
  const bindings = shortcuts.bindings ?? [];

  // The recorder is active while any card records. Its callbacks close
  // over the latest render's state, so `recordingFor` is current here.
  const { live } = useChordRecorder(
    recordingFor !== null,
    (chord: Chord) => {
      const action = recordingFor;
      setRecordingFor(null);
      if (!action || !config) return;
      // Esc is reserved for returning home (and the recorder also
      // treats it as cancel — this is the belt-and-braces guard).
      if (chord.key === 0x29) {
        setErr("Esc is reserved for returning home — pick another key.");
        return;
      }
      // Refuse a chord another action already holds: two actions on one
      // key can both fire, and which one wins would be a mystery.
      const clash = bindings.find(
        (b) => chordSig(b) === chordSig({ mods: { ctrl: chord.ctrl, alt: chord.alt, shift: chord.shift, meta: chord.meta }, key: chord.key }) &&
          actionIdOf(b) !== action,
      );
      if (clash) {
        setErr(`${chordLabel(clash)} is already bound to "${titleOf(clash, actions)}" — clear it first.`);
        return;
      }
      const sep = action.indexOf(":");
      const binding: Binding = {
        mods: { ctrl: chord.ctrl, alt: chord.alt, shift: chord.shift, meta: chord.meta },
        key: chord.key,
        action: sep === -1 ? action : action.slice(0, sep),
        screen: sep === -1 ? "" : action.slice(sep + 1),
      };
      // One chord per action: recording replaces whatever the card held.
      const rest = bindings.filter((b) => actionIdOf(b) !== action);
      void save({ shortcuts: { ...shortcuts, bindings: [...rest, binding] } });
    },
    () => setRecordingFor(null),
  );

  if (loadErr) {
    return (
      <div className="mx-auto w-full max-w-3xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Input &amp; shortcuts</h1>
        <p className="mt-4 text-sm text-destructive">Could not load: {loadErr}</p>
        <Button className="mt-3" variant="outline" onClick={() => void load()}>Retry</Button>
      </div>
    );
  }
  if (!config) return <PageSkeleton rows={3} />;

  const input = config.input ?? { pointerSpeed: 1, wheelSpeed: 1, swapScroll: false };

  const bindingFor = (id: string) =>
    bindings.find((b) => actionIdOf(b) === id);

  const clearBinding = (id: string) => {
    const sep = id.indexOf(":");
    void save({
      shortcuts: {
        ...shortcuts,
        bindings: bindings.filter(
          (b) => !(b.action === (sep === -1 ? id : id.slice(0, sep)) && (sep === -1 || (b.screen ?? "") === id.slice(sep + 1))),
        ),
      },
    });
  };

  return (
    <div className="mx-auto w-full max-w-3xl px-8 py-8">
      <h1 className="text-lg font-semibold tracking-tight">Input &amp; shortcuts</h1>
      <p className="mt-1 text-sm text-muted-foreground">
        How the shared keyboard and mouse behave — and the chords that switch screens without touching the mouse.
        Bindings are recorded as physical keys, so they work the same on every machine, and a bound chord overrides
        whatever the OS would do with it (Win+Tab, media keys).
      </p>

      <Section
        title="Shortcuts"
        className="mt-8"
        action={
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">enabled</span>
            <Switch
              checked={shortcuts.enabled}
              onCheckedChange={(v) => save({ shortcuts: { ...shortcuts, enabled: v } })}
            />
          </div>
        }
      >
        <div className={cn("grid grid-cols-1 gap-3 sm:grid-cols-2", !shortcuts.enabled && "opacity-60")}>
          {actions.map((spec) => (
            <ActionCard
              key={spec.id}
              spec={spec}
              binding={bindingFor(spec.id)}
              recording={recordingFor === spec.id}
              live={live}
              disabled={!shortcuts.enabled}
              onRecord={() => setRecordingFor(spec.id)}
              onCancelRecord={() => setRecordingFor(null)}
              onClear={() => clearBinding(spec.id)}
            />
          ))}
        </div>
        {recordingFor !== null && (
          <p className="mt-3 text-xs text-primary/80">
            Hold modifiers, then tap the key. Esc cancels.
          </p>
        )}
      </Section>

      <Section title="Mouse" className="mt-6">
        <div className="py-3">
          <div className="flex items-center gap-2">
            <Mouse className="h-3.5 w-3.5 text-muted-foreground" />
            <div className="flex-1">
              <div className="flex items-baseline justify-between">
                <div className="text-sm">Pointer speed</div>
                <span className="font-mono text-xs text-muted-foreground">{input.pointerSpeed.toFixed(2)}×</span>
              </div>
              <p className="mt-0.5 text-xs text-muted-foreground">
                How far the cursor travels on the controlled machine for the same hand movement. 1.00× mirrors this machine.
              </p>
            </div>
          </div>
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
        <div className="flex items-start gap-2 py-3">
          <Keyboard className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <p className="text-xs leading-relaxed text-muted-foreground">
            Keys are forwarded by their physical identity, so layout follows whichever machine is being controlled —
            type as usual. Only the chords recorded above are intercepted (before the controlled machine — and this
            machine's own shortcuts — see them); everything else passes through untouched.
          </p>
        </div>
      </Section>

      {(err || saved) && (
        <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>{err || "Saved"}</p>
      )}
    </div>
  );
}

/** The config action id of a binding, including its screen target ("switch:hp"). */
function actionIdOf(b: Binding): string {
  return b.action === "switch" && b.screen ? `switch:${b.screen}` : b.action;
}

/** Human title of a binding (used in the duplicate error). */
function titleOf(b: Binding, actions: ActionSpec[]): string {
  const id = actionIdOf(b);
  return actions.find((a) => a.id === id)?.title ?? id;
}
