import { useEffect, useState } from "react";
import { api, type InputSection, type ShortcutSection, type Binding } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Slider } from "@/components/ui/slider";
import { Section } from "@/components/Section";
import { PageSkeleton } from "@/components/PageSkeleton";
import { cn } from "@/lib/utils";

// Input & shortcuts: the sub-page of Layout that configures *how input
// behaves* rather than where screens sit. Two concerns, one page:
//
// * Shortcuts — chords (modifier set + key) bound to actions. Recorded
//   as canonical HID key ids, so a binding means the same physical key
//   on every platform pair. The recorder captures the next non-modifier
//   key press; the chord list shows modifiers + a friendly key name.
// * Input feel — pointer speed (multiplier on forwarded motion), wheel
//   speed, natural scrolling. These are portable transforms applied at
//   the shared-input level, so they affect every client identically.

// HID usages for the modifier keys and a few named keys we render
// nicely. The full tables live in the platform crate.
const HID = {
  CTRL_L: 0xe0, CTRL_R: 0xe4, SHIFT_L: 0xe1, SHIFT_R: 0xe5,
  ALT_L: 0xe2, ALT_R: 0xe6, META_L: 0xe3, META_R: 0xe7,
} as const;

const MOD_ORDER = ["CTRL_L", "CTRL_R", "SHIFT_L", "SHIFT_R", "ALT_L", "ALT_R", "META_L", "META_R"] as const;

const NAMED_KEYS: Record<number, string> = {
  0x04: "A", 0x05: "B", 0x06: "C", 0x07: "D", 0x08: "E", 0x09: "F", 0x0a: "G", 0x0b: "H",
  0x0c: "I", 0x0d: "J", 0x0e: "K", 0x0f: "L", 0x10: "M", 0x11: "N", 0x12: "O", 0x13: "P",
  0x14: "Q", 0x15: "R", 0x16: "S", 0x17: "T", 0x18: "U", 0x19: "V", 0x1a: "W", 0x1b: "X",
  0x1c: "Y", 0x1d: "Z", 0x1e: "1", 0x1f: "2", 0x20: "3", 0x21: "4", 0x22: "5", 0x23: "6",
  0x24: "7", 0x25: "8", 0x26: "9", 0x27: "0", 0x28: "Enter", 0x29: "Esc", 0x2a: "Backspace",
  0x2c: "Space", 0x39: "Caps Lock", 0x47: "Scroll Lock", 0x48: "Pause", 0x49: "Insert",
  0x4a: "Home", 0x4b: "Page Up", 0x4c: "Delete", 0x4d: "End", 0x4e: "Page Down",
  0x4f: "→", 0x50: "←", 0x51: "↓", 0x52: "↑", 0x53: "Num Lock",
  0x3a: "F1", 0x3b: "F2", 0x3c: "F3", 0x3d: "F4", 0x3e: "F5", 0x3f: "F6",
  0x40: "F7", 0x41: "F8", 0x42: "F9", 0x43: "F10", 0x44: "F11", 0x45: "F12",
};

function keyName(key: number): string {
  return NAMED_KEYS[key] ?? `Key ${key}`;
}

function modName(hidKey: number): string {
  switch (hidKey) {
    case HID.CTRL_L: case HID.CTRL_R: return "Ctrl";
    case HID.SHIFT_L: case HID.SHIFT_R: return "Shift";
    case HID.ALT_L: case HID.ALT_R: return "Alt";
    case HID.META_L: case HID.META_R: return "Super";
    default: return "";
  }
}

const ACTIONS: { value: string; label: string; needsScreen: boolean; hint: string }[] = [
  { value: "switch", label: "Switch to screen", needsScreen: true, hint: "Jump the cursor straight to one machine" },
  { value: "cycle", label: "Cycle screens", needsScreen: false, hint: "Move to the next reachable machine" },
  { value: "lock", label: "Toggle wall lock", needsScreen: false, hint: "Freeze or release the screen edges" },
  { value: "home", label: "Go home", needsScreen: false, hint: "Return control to this machine" },
];

interface Chord {
  mods: number[]; // HID usages of held modifiers
  key: number;    // HID usage of the non-modifier key
}

// Live chord recorder: listens for the next non-modifier key press on
// the window and reports it with the modifiers held at that moment.
// Escape cancels. Returns null on cancel.
function useChordRecorder(active: boolean, onDone: (chord: Chord) => void, onCancel: () => void) {
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        onCancel();
        return;
      }
      // Map the DOM event to HID: use code for letters/digits (layout-
      // independent) and keyCode fallback for the rest. The recorder
      // targets the common cases; unmapable keys simply do not record.
      const code = e.code;
      let hid = 0;
      if (code.startsWith("Key")) {
        hid = 0x04 + (code.charCodeAt(3) - "A".charCodeAt(0));
      } else if (code.startsWith("Digit")) {
        hid = 0x1e + (code.charCodeAt(5) - "1".charCodeAt(0));
      } else if (code.startsWith("F") && /^F\d+$/.test(code)) {
        hid = 0x3a + (parseInt(code.slice(1), 10) - 1);
      } else if (code === "ScrollLock") hid = 0x47;
      else if (code === "Pause") hid = 0x48;
      else if (code === "Home") hid = 0x4a;
      else if (code === "End") hid = 0x4d;
      else if (code === "PageUp") hid = 0x4b;
      else if (code === "PageDown") hid = 0x4e;
      else if (code === "Insert") hid = 0x49;
      else if (code === "ArrowRight") hid = 0x4f;
      else if (code === "ArrowLeft") hid = 0x50;
      else if (code === "ArrowDown") hid = 0x51;
      else if (code === "ArrowUp") hid = 0x52;
      if (hid === 0) return; // unmapped — keep listening
      const mods = MOD_ORDER.filter((m) => {
        switch (m) {
          case "CTRL_L": case "CTRL_R": return e.ctrlKey;
          case "SHIFT_L": case "SHIFT_R": return e.shiftKey;
          case "ALT_L": case "ALT_R": return e.altKey;
          case "META_L": case "META_R": return e.metaKey;
        }
      }) as unknown as number[];
      onDone({ mods: mods.map((m) => HID[m as keyof typeof HID]), key: hid });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [active, onDone, onCancel]);
}

export default function InputShortcutsPage({ onSaved }: { onSaved?: () => void }) {
  const [config, setConfig] = useState<{ shortcuts?: ShortcutSection; input?: InputSection } | null>(null);
  const [loadErr, setLoadErr] = useState("");
  const [err, setErr] = useState("");
  const [saved, setSaved] = useState(false);
  const [recording, setRecording] = useState(false);
  const [pendingAction, setPendingAction] = useState<{ action: string; screen: string } | null>(null);

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

  useChordRecorder(
    recording,
    (chord) => {
      setRecording(false);
      if (pendingAction && config) {
        const binding: Binding = {
          mods: {
            ctrl: chord.mods.includes(HID.CTRL_L) || chord.mods.includes(HID.CTRL_R),
            alt: chord.mods.includes(HID.ALT_L) || chord.mods.includes(HID.ALT_R),
            shift: chord.mods.includes(HID.SHIFT_L) || chord.mods.includes(HID.SHIFT_R),
            meta: chord.mods.includes(HID.META_L) || chord.mods.includes(HID.META_R),
          },
          key: chord.key,
          action: pendingAction.action,
          screen: pendingAction.screen,
        };
        save({ shortcuts: { ...(config.shortcuts ?? { enabled: true, bindings: [] }), bindings: [...(config.shortcuts?.bindings ?? []), binding] } });
      }
      setPendingAction(null);
    },
    () => {
      setRecording(false);
      setPendingAction(null);
    },
  );

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

  if (loadErr) {
    return (
      <div className="mx-auto w-full max-w-2xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Input &amp; shortcuts</h1>
        <p className="mt-4 text-sm text-destructive">Could not load: {loadErr}</p>
        <Button className="mt-3" variant="outline" onClick={() => void load()}>Retry</Button>
      </div>
    );
  }
  if (!config) return <PageSkeleton rows={2} />;

  const bindings = config.shortcuts?.bindings ?? [];
  const input = config.input ?? { pointerSpeed: 1, wheelSpeed: 1, swapScroll: false };
  const screens = config.screens ?? [];

  const removeBinding = (i: number) => {
    save({ shortcuts: { ...(config.shortcuts ?? { enabled: true }), bindings: bindings.filter((_, j) => j !== i) } });
  };

  const chordLabel = (b: Binding) => {
    const mods: string[] = [];
    if (b.mods.ctrl) mods.push("Ctrl");
    if (b.mods.alt) mods.push("Alt");
    if (b.mods.shift) mods.push("Shift");
    if (b.mods.meta) mods.push("Super");
    return [...mods, keyName(b.key)].join(" + ");
  };

  const actionLabel = (b: Binding) => {
    if (b.action === "switch") return `Switch to ${b.screen || "?"}`;
    return ACTIONS.find((a) => a.value === b.action)?.label ?? b.action;
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
              <li key={i} className="flex items-center justify-between gap-3 rounded-lg border border-border/50 px-3 py-2">
                <span className="font-mono text-xs">{chordLabel(b)}</span>
                <span className="flex-1 truncate text-xs text-muted-foreground">{actionLabel(b)}</span>
                <button
                  className="text-muted-foreground/50 hover:text-destructive"
                  onClick={() => removeBinding(i)}
                  aria-label="remove shortcut"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
          {recording ? (
            <p className="mt-3 rounded-lg border border-dashed border-primary/50 bg-primary/5 px-3 py-2 text-xs text-primary">
              Press a key (with any modifiers). Esc cancels.
            </p>
          ) : (
            <div className="mt-3 flex items-center gap-2">
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  setPendingAction({ action: "cycle", screen: "" });
                  setRecording(true);
                }}
              >
                Add shortcut
              </Button>
              <select
                className="h-8 rounded-md border border-border/70 bg-muted/40 px-2 text-xs text-foreground outline-none focus:border-primary"
                value={pendingAction ? `${pendingAction.action}${pendingAction.screen ? `:${pendingAction.screen}` : ""}` : "cycle"}
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
                {ACTIONS.filter((a) => !a.needsScreen).map((a) => (
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
          <p className="mt-2 text-[11px] text-muted-foreground/70">
            Pick an action, press Add shortcut, then tap the chord. Scroll Lock alone cycles; the escape key is always reserved.
          </p>
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
          type as usual. Modifier chords you record above are intercepted before forwarding; everything else reaches
          the controlled machine untouched.
        </p>
      </Section>

      {(err || saved) && (
        <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>{err || "Saved"}</p>
      )}
    </div>
  );
}
