import { useEffect, useRef, useState } from "react";

// Chord recording for the shortcut registry: one hook that captures the
// next keystroke as a chord (modifier set + physical key), plus the
// small display pieces the registry renders around it.
//
// Keys are identified as canonical HID usages (the same identity the
// backend's action engine matches on), so a binding means the same
// physical key on every platform pair. The DOM-to-HID map here covers
// the keys a shortcut can plausibly use; unmapped keys keep listening
// rather than recording something wrong.

export interface Chord {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  key: number; // HID usage of the non-modifier key
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
  Semicolon: 0x33,
  Quote: 0x34,
  Comma: 0x36,
  Period: 0x37,
  Slash: 0x38,
  Tab: 0x2b,
  Enter: 0x28,
  Space: 0x2c,
};

function codeToHid(code: string): number {
  if (CODE_TO_HID[code]) return CODE_TO_HID[code];
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
    if (f >= 1 && f <= 12) return HID_F1 + f - 1;
  }
  // Numpad: code "Numpad1"…"Numpad0" (HID keeps the keypad usage range).
  if (code.startsWith("Numpad") && code.length === 7) {
    const last = code.charCodeAt(6);
    if (last >= "1".charCodeAt(0) && last <= "9".charCodeAt(0)) {
      return 0x59 + (last - "1".charCodeAt(0));
    }
    if (last === "0".charCodeAt(0)) return 0x62;
  }
  return 0;
}

// Friendly name for a recorded key (the registry renders these).
const KEY_NAMES: Record<number, string> = {
  0x28: "Enter", 0x29: "Esc", 0x2a: "Backspace", 0x2b: "Tab", 0x2c: "Space",
  0x2d: "-", 0x2e: "=", 0x2f: "[", 0x30: "]", 0x31: "\\", 0x33: ";", 0x34: "'",
  0x35: "`", 0x36: ",", 0x37: ".", 0x38: "/", 0x39: "Caps Lock",
  0x47: "Scroll Lock", 0x48: "Pause", 0x49: "Insert", 0x4a: "Home",
  0x4b: "Page Up", 0x4c: "Delete", 0x4d: "End", 0x4e: "Page Down",
  0x4f: "→", 0x50: "←", 0x51: "↓", 0x52: "↑", 0x53: "Num Lock",
  0x59: "Num 1", 0x5a: "Num 2", 0x5b: "Num 3", 0x5c: "Num 4", 0x5d: "Num 5",
  0x5e: "Num 6", 0x5f: "Num 7", 0x60: "Num 8", 0x61: "Num 9", 0x62: "Num 0",
};

export function keyName(key: number): string {
  if (key >= 0x04 && key <= 0x1d) return String.fromCharCode("A".charCodeAt(0) + key - 0x04);
  if (key >= 0x1e && key <= 0x27) return String.fromCharCode("1".charCodeAt(0) + key - 0x1e);
  if (key >= 0x3a && key <= 0x45) return `F${key - 0x3a + 1}`;
  return KEY_NAMES[key] ?? `Key ${key}`;
}

export function modLabels(c: { ctrl: boolean; alt: boolean; shift: boolean; meta: boolean }): string[] {
  const out: string[] = [];
  if (c.ctrl) out.push("Ctrl");
  if (c.alt) out.push("Alt");
  if (c.shift) out.push("Shift");
  if (c.meta) out.push("Super");
  return out;
}

/** A keyboard-key chip (the registry's visual unit). */
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

interface LiveMods {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

const NO_MODS: LiveMods = { ctrl: false, alt: false, shift: false, meta: false };

/**
 * Capture the next chord while `active`: modifier chips update **live**
 * as keys are held (the "live key detection" the registry shows), and
 * the first non-modifier key press completes the chord. Escape cancels.
 */
export function useChordRecorder(
  active: boolean,
  onDone: (chord: Chord) => void,
  onCancel: () => void,
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
    const sync = (e: KeyboardEvent) =>
      setLive({ ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey });
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      sync(e);
      if (e.key === "Escape") {
        cancelRef.current();
        return;
      }
      // Modifier presses only light the live chips; the chord completes
      // on the first non-modifier key.
      if (["Control", "Alt", "Shift", "Meta", "AltGraph", "CapsLock", "NumLock", "ScrollLock"].includes(e.key) && !isBindableLock(e)) {
        return;
      }
      const hid = codeToHid(e.code);
      if (hid === 0) return; // unmapped — keep listening, never guess
      doneRef.current({
        ctrl: e.ctrlKey,
        alt: e.altKey,
        shift: e.shiftKey,
        meta: e.metaKey,
        key: hid,
      });
    };
    const onKeyUp = (e: KeyboardEvent) => sync(e);
    const onBlur = () => setLive(NO_MODS);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("blur", onBlur, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("blur", onBlur, true);
    };
  }, [active, doneRef, cancelRef]);

  return { live, listening: active };
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
