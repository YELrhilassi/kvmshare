import { useCallback, useEffect, useState } from "react";
import { api, type LayoutConfig, type Binding } from "@/lib/bridge";
import { useApp } from "@/app/AppProvider";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { PageSkeleton } from "@/components/PageSkeleton";
import { useChordRecorder, keyName, modLabels, Kbd, type LiveMods } from "./ChordRecorder";
import { LiveKeys, useLiveKeys } from "./LiveKeys";
import { allActions, actionIdOf, bindingIsBindable, bareSafeKey, chordSig, titleOf, type ActionSpec } from "./shortcuts";
import { cn } from "@/lib/utils";

// Keyboard — the keyboard half of the old Input & shortcuts page, now
// its own page with a split-pane editor:
//
//   left  · binder   every action as a card; record/clear its chord in
//                    place, modifier chips lighting as keys are held,
//                    the live-keys panel flashing pushes and holds
//                    *while recording* — capture is only ever armed for
//                    a recording, never in the background
//   right · behavior the shortcut system as a whole: enabled switch,
//                    what a binding means (physical-key identity, OS
//                    override), and why capture goes quiet while a
//                    shared session is live (kernel isolation)
//
// The recorder suppresses every key it sees while active, so recording
// never operates the UI underneath it, and OS-stolen chords (Win+Tab)
// complete on blur instead of vanishing.

const NO_MODS: LiveMods = { ctrl: false, alt: false, shift: false, meta: false };

interface ActionCardProps {
  spec: ActionSpec;
  binding?: Binding;
  recording: boolean;
  live: LiveMods;
  disabled: boolean;
  onRecord: () => void;
  onCancelRecord: () => void;
  onClear: () => void;
}

function ActionCard({ spec, binding, recording, live, disabled, onRecord, onCancelRecord, onClear }: ActionCardProps) {
  const lit = (m: string) =>
    (m === "Ctrl" && live.ctrl) ||
    (m === "Alt" && live.alt) ||
    (m === "Shift" && live.shift) ||
    (m === "Super" && live.meta);
  return (
    <div
      className={cn(
        "rounded-xl border bg-card/40 p-4 transition-colors",
        recording ? "border-primary ring-1 ring-primary/40" : "border-border/60",
        disabled && !recording && "opacity-50",
      )}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-sm font-medium">{spec.title}</div>
          <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{spec.hint}</p>
        </div>
        {binding && !recording && (
          <button
            className="text-muted-foreground/40 transition-colors hover:text-destructive"
            onClick={onClear}
            aria-label={`clear ${spec.title} shortcut`}
          >
            ×
          </button>
        )}
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
              className="ml-auto text-xs text-muted-foreground transition-colors hover:text-foreground"
              onClick={onCancelRecord}
            >
              cancel
            </button>
          </div>
        ) : (
          <div className="flex items-center gap-2">
            {binding ? (
              <span className="flex flex-wrap items-center gap-1">
                {modLabels(binding.mods).map((m) => (
                  <Kbd key={m}>{m}</Kbd>
                ))}
                <Kbd>{keyName(binding.key)}</Kbd>
              </span>
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

export default function KeyboardPage() {
  const { mode, running, clientState, clients } = useApp();
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [loadErr, setLoadErr] = useState("");
  const [err, setErr] = useState("");
  const [saved, setSaved] = useState(false);
  // Which action card is recording (its ActionSpec.id), or null.
  const [recordingFor, setRecordingFor] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoadErr("");
    try {
      setConfig(await api().LoadConfig());
    } catch (e) {
      setLoadErr(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = useCallback(
    async (patch: Partial<Pick<LayoutConfig, "shortcuts">>) => {
      if (!config) return;
      setErr("");
      try {
        const next = { ...config, ...patch };
        await api().SaveConfig(next);
        setConfig(next);
        setSaved(true);
        window.setTimeout(() => setSaved(false), 1500);
      } catch (e) {
        setErr(String(e));
      }
    },
    [config],
  );

  const screens = config?.screens ?? [];
  const actions = allActions(screens);
  const shortcuts = config?.shortcuts ?? { enabled: true, bindings: [] };
  // Only bindings the engine actually honors are shown or matched; a
  // stale unsafe one (from an older release) disappears from the page
  // and is dropped from the file on the next save (self-healing).
  const bindings = (shortcuts.bindings ?? []).filter(bindingIsBindable);

  // Live keys: the recorder reports every push and release **while a
  // card is recording** — the panel exists only inside the record UI,
  // and the capture hook is only mounted then (outside recording the
  // page never touches key events, so normal keyboard use of the UI is
  // untouched).
  const { held, tape, push } = useLiveKeys();

  const recording = recordingFor !== null;
  const { live } = useChordRecorder(
    recording,
    // onDone: a completed chord while a card records. Fires on any
    // bindable key; validation decides whether it becomes a binding.
    (chord: Chord) => {
      const action = recordingFor;
      if (!action || !config) return;
      // A binding must carry a modifier unless the key is app-meaningless
      // bare (Scroll Lock, Pause, F13–F24) — the same rule the engine's
      // BindSection sanitization enforces, mirrored here so the user gets
      // an immediate, actionable error instead of a silently dropped
      // binding in the file. Recording stays armed so a retry is one
      // keystroke away.
      const hasMods = chord.ctrl || chord.alt || chord.shift || chord.meta;
      if (!hasMods && !bareSafeKey(chord.key)) {
        setErr("Add a modifier (Ctrl/Alt/Shift/Super) — a bare key like that is used by every app and would be swallowed.");
        return;
      }
      setRecordingFor(null);
      // Esc is reserved for returning home (the recorder also treats it
      // as cancel — this is the belt-and-braces guard).
      if (chord.key === 0x29) {
        setErr("Esc is reserved for returning home — pick another key.");
        return;
      }
      // Refuse a chord another action already holds: two actions on one
      // key can both fire, and which one wins would be a mystery.
      const clash = bindings.find(
        (b) =>
          chordSig(b) === chordSig({ mods: { ctrl: chord.ctrl, alt: chord.alt, shift: chord.shift, meta: chord.meta }, key: chord.key }) &&
          actionIdOf(b) !== action,
      );
      if (clash) {
        setErr(`${titleOf(clash, actions)} already uses that chord — clear it first.`);
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
      // The save writes only bindable bindings — the file heals.
      const rest = bindings.filter((b) => actionIdOf(b) !== action);
      void save({ shortcuts: { ...shortcuts, bindings: [...rest, binding] } });
    },
    // onCancel: Esc while a card records.
    () => setRecordingFor(null),
    // onKey: the live panel mirrors every key the capture sees.
    (e: KeyboardEvent, down: boolean) => {
      const name = modifierName(e) ?? keyNameOfEvent(e);
      if (name) push(name, down);
    },
    // Recording is the only capture mode: always suppress while armed.
    true,
  );

  const bindingFor = (id: string) => bindings.find((b) => actionIdOf(b) === id);

  const clearBinding = (id: string) => {
    const sep = id.indexOf(":");
    void save({
      shortcuts: {
        ...shortcuts,
        bindings: bindings.filter(
          (b) =>
            !(b.action === (sep === -1 ? id : id.slice(0, sep)) && (sep === -1 || (b.screen ?? "") === id.slice(sep + 1))),
        ),
      },
    });
  };

  if (loadErr) {
    return (
      <div className="h-full overflow-y-auto">
        <div className="mx-auto w-full max-w-5xl px-8 py-8">
          <h1 className="text-lg font-semibold tracking-tight">Keyboard</h1>
          <p className="mt-4 text-sm text-destructive">Could not load: {loadErr}</p>
          <Button className="mt-3" variant="outline" onClick={() => void load()}>
            Retry
          </Button>
        </div>
      </div>
    );
  }
  if (!config) return <PageSkeleton rows={3} />;

  // While control is away on another machine the physical devices are
  // kernel-isolated: the webview sees no keys at all, so a recording
  // started now would never complete — tell the user instead of arming
  // a recorder that cannot hear them.
  const sessionLive = mode === "server" ? clients.length > 0 : clientState.status === "connected" && running.client;

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto w-full max-w-6xl px-8 py-8">
        <header className="flex items-start justify-between gap-6">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Keyboard</h1>
            <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
              Bind chords to actions and watch every key land live. Bindings are physical keys — they mean the same
              thing on every machine, and a bound chord overrides whatever the OS would do with it (Win+Tab, media
              keys).
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2 pt-1">
            <span className="text-xs text-muted-foreground">shortcuts</span>
            <Switch
              checked={shortcuts.enabled}
              onCheckedChange={(v) => void save({ shortcuts: { ...shortcuts, enabled: v } })}
            />
          </div>
        </header>

        <div className={cn("mt-6 grid items-start gap-6 lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)]", !shortcuts.enabled && "opacity-60")}>
          {/* Left pane — the binder */}
          <div className="min-w-0">
            {recording && <LiveKeys held={held} tape={tape} live={live} />}
            <div className="mt-4 grid grid-cols-1 gap-3 xl:grid-cols-2">
              {actions.map((spec) => (
                <ActionCard
                  key={spec.id}
                  spec={spec}
                  binding={bindingFor(spec.id)}
                  recording={recordingFor === spec.id}
                  live={recordingFor === spec.id ? live : NO_MODS}
                  disabled={!shortcuts.enabled}
                  onRecord={() => setRecordingFor(spec.id)}
                  onCancelRecord={() => setRecordingFor(null)}
                  onClear={() => clearBinding(spec.id)}
                />
              ))}
            </div>
            {recording && !sessionLive && (
              <p className="mt-3 text-xs text-primary/80">
                Hold modifiers, then tap the key — every key you push shows in the live panel. Esc cancels.
              </p>
            )}
            {recording && sessionLive && (
              <p className="mt-3 text-xs text-amber-500">
                Recording needs local control — stop the session or bring the cursor home first.
              </p>
            )}
          </div>

          {/* Right pane — behavior & meaning */}
          <div className="min-w-0 space-y-4">
            <section className="rounded-xl border border-border/60 bg-card/40 p-4">
              <h2 className="text-sm font-medium">What a binding means</h2>
              <ul className="mt-2 space-y-1.5 text-xs leading-relaxed text-muted-foreground">
                <li>• Bindings are recorded as physical keys, so they work the same on every machine pair.</li>
                <li>• A bound chord is swallowed before the desktop sees it — Win+Tab, Alt+Tab, media keys included.</li>
                <li>• One chord per action; recording replaces the card's chord, and a chord held elsewhere is refused.</li>
              </ul>
            </section>

            <section className="rounded-xl border border-border/60 bg-card/40 p-4">
              <h2 className="text-sm font-medium">While control is on another machine</h2>
              <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
                When a client is using this machine's keyboard and mouse, the physical devices are isolated at the
                kernel — the desktop (and this window) see nothing, by design. Key recording and the live panel resume
                the moment control comes home.
              </p>
            </section>

            {bindings.length > 0 && (
              <section className="rounded-xl border border-border/60 bg-card/40 p-4">
                <h2 className="text-sm font-medium">Registry</h2>
                <ul className="mt-2 space-y-1.5">
                  {bindings.map((b) => (
                    <li key={`${b.action}:${b.screen ?? ""}`} className="flex items-center justify-between gap-3 text-xs">
                      <span className="flex flex-wrap items-center gap-1">
                        {modLabels(b.mods).map((m) => (
                          <Kbd key={m}>{m}</Kbd>
                        ))}
                        <Kbd>{keyName(b.key)}</Kbd>
                      </span>
                      <span className="text-muted-foreground">{titleOf(b, actions)}</span>
                    </li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        </div>

        {(err || saved) && (
          <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>{err || "Saved"}</p>
        )}
      </div>
    </div>
  );
}

/** Human name of a modifier keydown, or null when the key is not one. */
function modifierName(e: KeyboardEvent): string | null {
  switch (e.key) {
    case "Control":
      return "Ctrl";
    case "Alt":
    case "AltGraph":
      return "Alt";
    case "Shift":
      return "Shift";
    case "Meta":
      return "Super";
    default:
      return null;
  }
}

/** Human name of any key event, via the shared HID mapping when possible. */
function keyNameOfEvent(e: KeyboardEvent): string {
  // The recorder's own keyName() is HID-based; for live display we can
  // afford the DOM label when the HID id is unknown (never recorded).
  const hidKey = e.key;
  if (hidKey.length === 1) return hidKey.toUpperCase();
  switch (hidKey) {
    case "Escape":
      return "Esc";
    case " ":
      return "Space";
    case "ArrowUp":
      return "↑";
    case "ArrowDown":
      return "↓";
    case "ArrowLeft":
      return "←";
    case "ArrowRight":
      return "→";
    case "Backspace":
    case "Tab":
    case "Enter":
    case "CapsLock":
    case "NumLock":
    case "ScrollLock":
    case "Insert":
    case "Delete":
    case "Home":
    case "End":
    case "PageUp":
    case "PageDown":
    case "Pause":
    case "PrintScreen":
      return hidKey;
    default:
      if (/^F\d+$/.test(hidKey)) return hidKey;
      return hidKey;
  }
}
