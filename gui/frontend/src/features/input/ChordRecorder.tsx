import { useEffect, useRef, useState } from "react";

// Chord recording for the shortcut system: one hook that captures the
// next keystroke as a chord (modifier set + physical key), plus the
// small display pieces the pages render around it.
//
// Keys are identified as canonical HID usages (the same identity the
// backend's action engine matches on), so a binding means the same
// physical key on every platform pair. The DOM-to-HID coverage below
// spans everything a shortcut can plausibly use — letters, digits,
// function row, navigation cluster, punctuation, numpad, and the
// media keys (Play/Pause, track controls) keyboards ship. Unmapped
// keys keep listening rather than recording something wrong.

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

/**
 * Capture the next chord while `active`: modifier chips update **live**
 * as keys are held, the first non-modifier key press completes the
 * chord, and — while `suppress` — every key event is swallowed so
 * nothing the user pushes acts on the UI (Enter must not press a
 * focused button mid-recording; Tab must not move focus). With
 * `onKey`, the caller also observes every push and release — that is
 * the live-keys panel's data source. Listen-only mode (`active`
 * without `suppress`) reports keys without eating them.
 */
export function useChordRecorder(
  active: boolean,
  onDone?: (chord: Chord) => void,
  onCancel?: () => void,
  onKey?: (e: KeyboardEvent, down: boolean) => void,
  suppress = true,
): { live: LiveMods; listening: boolean } {
  const [live, setLive] = useState<LiveMods>(NO_MODS);

  // Stable identity for the callbacks across renders, so the effect
  // does not re-attach (and lose the live state) on every parent render.
  const doneRef = useRefLatest(onDone);
  const cancelRef = useRefLatest(onCancel);
  const keyRef = useRefLatest(onKey);
  const suppressRef = useRefLatest(suppress);

  useEffect(() => {
    if (!active) {
      setLive(NO_MODS);
      return;
    }
    // Keys seen down since the chord started. Scroll Lock / Pause are
    // reported on keyup (not keydown) by some browsers, and an OS may
    // swallow the whole chord after keydown — both handled below.
    let seenDown = new Set<number>();
    // The live modifier state, tracked synchronously (React state lags
    // one render — the blur handler below needs it *now*).
    let modsNow: LiveMods = { ...NO_MODS };
    // The last bindable key held: if the OS steals focus mid-chord
    // (Win+Tab opens the task switcher), the window blurs and the
    // keyup never arrives. The chord collected so far is still what
    // the user meant — complete it on blur instead of dropping it.
    let lastKey = 0;
    const sync = (e: KeyboardEvent) => {
      modsNow = { ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey };
      setLive(modsNow);
    };
    const complete = (mods: LiveMods, hid: number) => {
      lastKey = 0;
      seenDown = new Set();
      doneRef.current?.({ ...mods, key: hid });
    };
    const onKey = (e: KeyboardEvent) => {
      // While recording, suppress everything: no default behavior, no
      // bubbling — a recorded chord is captured, never acted on.
      if (suppressRef.current) {
        e.preventDefault();
        e.stopPropagation();
      }
      sync(e);
      keyRef.current?.(e, true);
      if (e.key === "Escape") {
        cancelRef.current?.();
        return;
      }
      // Modifier presses only light the live chips; the chord completes
      // on the first non-modifier key.
      if (isModifierKey(e) && !isBindableLock(e)) {
        return;
      }
      const hid = eventToHid(e);
      if (hid === 0) return; // unmapped — keep listening, never guess
      if (!e.repeat) seenDown.add(e.keyCode);
      lastKey = hid;
      complete(modsNow, hid);
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (suppressRef.current) {
        e.preventDefault();
        e.stopPropagation();
      }
      sync(e);
      keyRef.current?.(e, false);
      if (!seenDown.has(e.keyCode) && isBindableLock(e)) {
        // The keydown never reached us (browser reports these on keyup
        // only) — the release *is* the press: record the chord now.
        const hid = eventToHid(e);
        if (hid !== 0) complete(modsNow, hid);
      }
      seenDown.delete(e.keyCode);
    };
    const onBlur = () => {
      if (lastKey !== 0) {
        complete(modsNow, lastKey);
        return;
      }
      setLive(NO_MODS);
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("blur", onBlur, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("blur", onBlur, true);
    };
  }, [active, doneRef, cancelRef, keyRef, suppressRef]);

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
