import { useEffect, useRef, useState } from "react";
import { startBackendCapture, captureKind, isModifierHid, type BackendMods } from "./BackendCapture";

// Chord recording for the shortcut system: one hook that captures the
// next keystroke as a chord (modifier set + physical key), plus the
// small display pieces the pages render around it.
//
// Keys are identified as canonical HID usages (the same identity the
// backend's action engine matches on), so a binding means the same
// physical key on every platform pair.

export interface Chord {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  key: number; // HID usage of the non-modifier key
}

export interface LiveMods {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

const HID_LETTER = 0x04;
const HID_DIGIT_ONE = 0x1e;
const HID_F1 = 0x3a;

// DOM `KeyboardEvent.code` → HID usage for the bindable keys.
const CODE_TO_HID: Record<string, number> = {
  ScrollLock: 0x47,
  Pause: 0x48,
  Insert: 0x49,
  Home: 0x4a,
  PageUp: 0x4b,
  Delete: 0x4c,
  End: 0x4d,
  PageDown: 0x4e,
  ArrowRight: 0x4f,
  ArrowLeft: 0x50,
  ArrowDown: 0x51,
  ArrowUp: 0x52,
  NumLock: 0x53,
  Backquote: 0x35,
  Minus: 0x2d,
  Equal: 0x2e,
  BracketLeft: 0x2f,
  BracketRight: 0x30,
  Backslash: 0x31,
  IntlBackslash: 0x31,
  Semicolon: 0x33,
  Quote: 0x34,
  Comma: 0x36,
  Period: 0x37,
  Slash: 0x38,
  IntlRo: 0x38,
  Tab: 0x2b,
  Enter: 0x28,
  NumpadEnter: 0x28,
  Space: 0x2c,
  CapsLock: 0x39,
  PrintScreen: 0x46,
  ContextMenu: 0x65,
  MediaPlayPause: 0xb0,
  MediaNext: 0xb5,
  MediaPrev: 0xb6,
  MediaStop: 0xb7,
  LaunchMail: 0x9c,
  LaunchApp1: 0x9d,
  LaunchApp2: 0x9e,
  BrowserSearch: 0xa1,
  BrowserHome: 0xa3,
  BrowserBack: 0xa4,
  BrowserForward: 0xa5,
  NumpadAdd: 0x57,
  NumpadSubtract: 0x56,
  NumpadMultiply: 0x55,
  NumpadDivide: 0x54,
  NumpadDecimal: 0x63,
};

function codeToHid(code: string): number {
  const direct = CODE_TO_HID[code];
  if (direct !== undefined) return direct;
  if (code.startsWith("Key")) {
    const letter = code.charCodeAt(3) - "A".charCodeAt(0);
    if (letter >= 0 && letter < 26) return HID_LETTER + letter;
  }
  if (code.startsWith("Digit")) {
    const digit = code.charCodeAt(5) - "1".charCodeAt(0);
    if (digit >= 0 && digit < 10) return HID_DIGIT_ONE + digit;
  }
  if (/^F\d+$/.test(code)) {
    const f = parseInt(code.slice(1), 10);
    if (f >= 1 && f <= 24) return HID_F1 + f - 1;
  }
  // Numpad 1–9 (0x59–0x61) and 0 (0x62): code "Numpad1"…"Numpad0".
  if (code.startsWith("Numpad") && code.length === 7) {
    const last = code.charCodeAt(6);
    if (last >= "1".charCodeAt(0) && last <= "9".charCodeAt(0)) {
      return 0x59 + (last - "1".charCodeAt(0));
    }
    if (last === "0".charCodeAt(0)) return 0x62;
  }
  return 0;
}

// DOM legacy `keyCode` → HID, used only when `code` is empty. Linux
// WebKitGTK reports no `code` at all for some keys — Pause and Scroll
// Lock among them — so without this fallback those keys could never be
// recorded there. Only keys with a stable, unambiguous legacy code are
// mapped; anything else keeps listening rather than guessing.
const KEYCODE_TO_HID: Record<number, number> = {
  19: 0x48, // Pause
  145: 0x47, // Scroll Lock
  45: 0x49, // Insert
  36: 0x4a, // Home
  33: 0x4b, // Page Up
  46: 0x4c, // Delete
  35: 0x4d, // End
  34: 0x4e, // Page Down
  39: 0x4f, // ArrowRight
  37: 0x50, // ArrowLeft
  40: 0x51, // ArrowDown
  38: 0x52, // ArrowUp
  144: 0x53, // Num Lock
  179: 0xb0, // Media Play/Pause
  176: 0xb5, // Media Next
  177: 0xb6, // Media Prev
};

function eventToHid(e: KeyboardEvent): number {
  if (e.code) return codeToHid(e.code);
  const kc = e.keyCode;
  if (kc >= 65 && kc <= 90) return HID_LETTER + (kc - 65);
  if (kc >= 48 && kc <= 57) return HID_DIGIT_ONE + (kc - 49);
  if (kc >= 112 && kc <= 123) return HID_F1 + (kc - 112);
  const mapped = KEYCODE_TO_HID[kc];
  return mapped !== undefined ? mapped : 0;
}

// Friendly name for a recorded key (the registry renders these).
const KEY_NAMES: Record<number, string> = {
  0x28: "Enter", 0x29: "Esc", 0x2a: "Backspace", 0x2b: "Tab", 0x2c: "Space",
  0x2d: "-", 0x2e: "=", 0x2f: "[", 0x30: "]", 0x31: "\\", 0x33: ";", 0x34: "'",
  0x35: "`", 0x36: ",", 0x37: ".", 0x38: "/", 0x39: "Caps Lock",
  0x46: "PrtSc", 0x47: "Scroll Lock", 0x48: "Pause",
  0x49: "Insert", 0x4a: "Home", 0x4b: "Page Up", 0x4c: "Delete", 0x4d: "End",
  0x4e: "Page Down", 0x4f: "→", 0x50: "←", 0x51: "↓", 0x52: "↑", 0x53: "Num Lock",
  0x54: "Num /", 0x55: "Num *", 0x56: "Num -", 0x57: "Num +", 0x63: "Num .",
  0x59: "Num 1", 0x5a: "Num 2", 0x5b: "Num 3", 0x5c: "Num 4", 0x5d: "Num 5",
  0x5e: "Num 6", 0x5f: "Num 7", 0x60: "Num 8", 0x61: "Num 9", 0x62: "Num 0",
  0x65: "Menu", 0xb0: "Play/Pause", 0xb5: "Next Track", 0xb6: "Prev Track",
  0xb7: "Stop", 0x9c: "Mail", 0x9d: "My Computer", 0x9e: "Calculator",
  0xa1: "Browser Search", 0xa3: "Browser Home", 0xa4: "Browser Back", 0xa5: "Browser Fwd",
};

export function keyName(key: number): string {
  if (key >= 0x04 && key <= 0x1d) return String.fromCharCode("A".charCodeAt(0) + key - 0x04);
  if (key >= 0x1e && key <= 0x27) return String.fromCharCode("1".charCodeAt(0) + key - 0x1e);
  if (key >= 0x3a && key <= 0x45) return `F${key - 0x3a + 1}`;
  return KEY_NAMES[key] ?? `Key ${key.toString(16)}`;
}

export function modLabels(c: { ctrl: boolean; alt: boolean; shift: boolean; meta: boolean }): string[] {
  const out: string[] = [];
  if (c.ctrl) out.push("Ctrl");
  if (c.alt) out.push("Alt");
  if (c.shift) out.push("Shift");
  if (c.meta) out.push("Super");
  return out;
}

/** A keyboard-key chip (the shortcut UI's visual unit). */
export function Kbd({ children, dim = false }: { children: React.ReactNode; dim?: boolean }) {
  return (
    <kbd
      className={
        "inline-flex min-w-6 items-center justify-center rounded border px-1.5 py-0.5 font-mono text-[11px] leading-none " +
        (dim
          ? "border-border/40 text-muted-foreground/40"
          : "border-border bg-muted text-foreground")
      }
    >
      {children}
    </kbd>
  );
}

const NO_MODS: LiveMods = { ctrl: false, alt: false, shift: false, meta: false };

/** Where the chord bytes come from: the backend hook or the DOM. */
type Ear = "hook" | "dom";

/**
 * Decide the ear once, before any listener is attached — the decision
 * is awaited, not timed. Windows resolves "hook" when the backend
 * session opens and "dom" when it cannot (non-Windows, old backend,
 * standard user without the tasks). Everywhere else: "dom".
 */
async function openEar(
  onHookKey: (hid: number, down: boolean, mods: BackendMods) => void,
  onHookMods: (mods: BackendMods) => void,
  onHookDead: () => void,
): Promise<{ ear: Ear; dispose: () => void }> {
  if (captureKind !== "hook") {
    return { ear: "dom", dispose: () => {} };
  }
  const dispose = await startBackendCapture(onHookKey, onHookMods, onHookDead);
  return dispose ? { ear: "hook", dispose } : { ear: "dom", dispose: () => {} };
}

/**
 * Capture the next chord while `active`: modifier chips update **live**
 * as keys are held, the first non-modifier key press completes the
 * chord, Esc cancels.
 *
 * `onDone` returns whether the chord was **accepted**. Returning false
 * (a declined chord — validation error, clash, held-only "tap a key
 * next") keeps the session armed: the next keystroke is still heard.
 * Returning true (or an absent onDone) ends the capture; the parent
 * then clears `active`.
 *
 * One ear is attached — never both, decided before attaching:
 *
 *  - **The backend hook** (Windows): a system-wide WH_KEYBOARD_LL
 *    session, active only while recording. It sees the physical chord
 *    *before the OS* — Super+Tab and friends are swallowed there, so
 *    the native binding never fires. Keys arrive as canonical HID ids.
 *    If the session dies mid-recording (lease lapse — the page
 *    stalled), `onExpired` fires and the recorder re-opens a DOM ear
 *    so a long-running recording degrades instead of going deaf.
 *  - **DOM events** (fallback / other platforms): keydown/keyup with
 *    preventDefault while recording — enough for every key the OS has
 *    not bound; an OS-bound chord completes on blur (the key held when
 *    focus was stolen is what the user meant).
 *
 * Chord completion follows the way users actually type shortcuts: a
 * modifier **held** counts, and the chord completes when the first
 * non-modifier key arrives — or, when only modifiers were pressed and
 * everything is released ("I held Super and let go"), on the release
 * of the last modifier. A held-only recording therefore always yields
 * a chord instead of hanging the session (and with it the keyboard).
 */
export function useChordRecorder(
  active: boolean,
  onDone?: (chord: Chord) => boolean,
  onCancel?: () => void,
): { live: LiveMods; listening: boolean } {
  const [live, setLive] = useState<LiveMods>(NO_MODS);

  // Stable identity for the callbacks across renders, so the effect
  // does not re-attach (and lose the live state) on every parent render.
  const doneRef = useRefLatest(onDone);
  const cancelRef = useRefLatest(onCancel);

  useEffect(() => {
    if (!active) {
      setLive(NO_MODS);
      return;
    }
    let cancelled = false; // the effect was torn down mid-await
    let disposeHook: (() => void) | null = null;
    let disposeDom: (() => void) | null = null;

    // The live modifier state, tracked synchronously (React state lags
    // one render — the blur handler needs it *now*).
    let modsNow: LiveMods = { ...NO_MODS };
    // Whether the session is still listening. A completed chord ends
    // the *capture*, but the page decides what happens next: onDone
    // returning false means the chord was declined (validation, clash,
    // held-only "now tap a key") and the session stays armed — the
    // next keystroke must still be heard. Without this, one declined
    // chord bricked the recorder until the page was remounted ("it
    // thinks I released and never reads the next key").
    let listening = true;
    const complete = (mods: LiveMods, hid: number): boolean => {
      if (!listening) return false;
      const accepted = doneRef.current?.({ ...mods, key: hid }) ?? true;
      listening = accepted;
      return accepted;
    };
    const setMods = (mods: LiveMods) => {
      modsNow = mods;
      if (anyMod(mods)) heldMods = { ...mods };
      setLive(mods);
    };
    // anyMod: does the current live set carry at least one modifier?
    const anyMod = (m: LiveMods) => m.ctrl || m.alt || m.shift || m.meta;
    // heldMods: the most recent non-empty modifier snapshot — the set
    // the user actually held. Release events report a post-release
    // snapshot (modifiers already cleared), so held-only completion
    // must come from here, not from the live state.
    let heldMods: LiveMods = { ...NO_MODS };
    const asLiveMods = (m: BackendMods): LiveMods => ({
      ctrl: m.ctrl, alt: m.alt, shift: m.shift, meta: m.meta,
    });

    // ---- The DOM ear (attached only when it is the chosen one, or
    // ---- after a hook death mid-recording).
    const attachDom = () => {
      // Keys seen down since the chord started. Scroll Lock / Pause are
      // reported on keyup (not keydown) by some browsers.
      let seenDown = new Set<number>();
      let lastKey = 0;
      const sync = (e: KeyboardEvent) => {
        setMods({ ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey });
      };
      const onKey = (e: KeyboardEvent) => {
        if (!listening) return;
        // While recording, suppress everything: no default behavior,
        // no bubbling — a recorded chord is captured, never acted on.
        e.preventDefault();
        e.stopPropagation();
        sync(e);
        if (e.key === "Escape") {
          listening = false;
          cancelRef.current?.();
          return;
        }
        // Modifier presses only light the live chips; the chord
        // completes on the first non-modifier key.
        if (isModifierKey(e) && !isBindableLock(e)) return;
        const hid = eventToHid(e);
        if (hid === 0) return; // unmapped — keep listening, never guess
        if (!e.repeat) seenDown.add(e.keyCode);
        lastKey = hid;
        complete(modsNow, hid);
      };
      const onKeyUp = (e: KeyboardEvent) => {
        if (!listening) return;
        e.preventDefault();
        e.stopPropagation();
        sync(e);
        if (!seenDown.has(e.keyCode) && isBindableLock(e)) {
          // The keydown never reached us (browser reports these on
          // keyup only) — the release *is* the press.
          const hid = eventToHid(e);
          if (hid !== 0) complete(modsNow, hid);
        }
        seenDown.delete(e.keyCode);
        // Held-only recording: every key is back up and the user
        // pressed only modifiers — the held set is the chord (key 0;
        // validation asks for the plain key). Completing here keeps
        // the recording from hanging on "held Super, released". The
        // snapshot comes from heldMods: this very release already
        // cleared the live state.
        if (seenDown.size === 0 && lastKey === 0 && anyMod(heldMods)) {
          complete(heldMods, 0);
        }
      };
      const onBlur = () => {
        if (!listening) return;
        if (lastKey !== 0) {
          // The OS stole focus mid-chord (Win+Tab's switcher): the
          // chord the user was holding is what they meant.
          complete(modsNow, lastKey);
          return;
        }
        setMods(NO_MODS);
      };
      window.addEventListener("keydown", onKey, true);
      window.addEventListener("keyup", onKeyUp, true);
      window.addEventListener("blur", onBlur, true);
      disposeDom = () => {
        window.removeEventListener("keydown", onKey, true);
        window.removeEventListener("keyup", onKeyUp, true);
        window.removeEventListener("blur", onBlur, true);
      };
    };

    // lastModOnly: the hook ear has seen a modifier press and nothing
    // else yet — the held-only completion above keys off it.
    let lastModOnly = false;

    // ---- Ear selection, awaited before anything listens.
    const open = async () => {
      const { ear, dispose } = await openEar(
        // hook key: arrives as a canonical HID id already
        (hid, down, mods) => {
          if (cancelled || !listening) return;
          const liveMods = asLiveMods(mods);
          setMods(liveMods);
          if (!down) {
            // Held-only recording on the hook ear: the last modifier
            // came up with no plain key ever pressed — the held set is
            // the chord (key 0; validation asks for a plain key).
            if (!anyMod(liveMods) && lastModOnly) {
              complete(heldMods, 0);
            }
            return;
          }
          // A modifier key press updates the live chips (done above via
          // the snapshot) and nothing else: a modifier is never the
          // chord-completing key. Before this guard, Super's HID usage
          // arrived here as a "key" and the recorder completed a bare
          // chord instantly — the "Add a modifier" error.
          if (isModifierHid(hid)) {
            lastModOnly = true;
            return;
          }
          lastModOnly = false;
          if (hid === 0x29) {
            // Esc cancels (reserved for returning home).
            listening = false;
            cancelRef.current?.();
            return;
          }
          complete(liveMods, hid);
        },
        // hook mods
        (mods) => {
          if (!cancelled) setMods(asLiveMods(mods));
        },
        // hook dead: the lease expired the session mid-recording.
        // Re-arm on the DOM ear — degraded (OS-bound chords now
        // unreachable) but never deaf. A dead session cannot fire more
        // events, so there is no double-ear window.
        () => {
          if (cancelled || !listening) return;
          disposeHook = null;
          setMods(NO_MODS);
          attachDom();
        },
      );
      if (cancelled) {
        dispose();
        return;
      }
      if (ear === "hook") {
        disposeHook = dispose;
      } else {
        dispose();
        attachDom();
      }
    };
    void open();

    return () => {
      cancelled = true;
      disposeHook?.();
      disposeDom?.();
    };
  }, [active, doneRef, cancelRef]);

  return { live, listening: active };
}

function isModifierKey(e: KeyboardEvent): boolean {
  return ["Control", "Alt", "Shift", "Meta", "AltGraph", "CapsLock", "NumLock", "ScrollLock"].includes(e.key);
}

// ScrollLock/CapsLock/NumLock are modifier-shaped in DOM `key` but are
// perfectly bindable non-modifier keys — do not swallow them.
function isBindableLock(e: KeyboardEvent): boolean {
  return e.key === "ScrollLock" || e.key === "CapsLock" || e.key === "NumLock";
}

/** Keep a ref mirrored to the latest value without retriggering effects. */
function useRefLatest<T>(value: T) {
  const ref = useRef(value);
  ref.current = value;
  return ref;
}
