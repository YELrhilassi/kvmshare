// BackendCapture — the recording path that sees keys **before the OS
// does** (Windows: a WH_KEYBOARD_LL hook in the GUI process; other
// platforms fall back to DOM events, which cannot see OS-bound chords
// like Win+Tab but cover every other key).
//
// Why a hook: the webview's preventDefault stops the page from acting
// on a key — it cannot stop the OS. Super+Tab's Tab never reaches the
// page at all. The hook sees the physical chord first, swallows it so
// the shell never fires the native binding, and reports it as canonical
// HID ids (the same identity the bindings are stored as). The mapping
// below is the read-only UI half of the set-1 scancode table from
// crates/platform/src/keys.rs.

import { api } from "@/lib/bridge";

/** One raw key transition from the backend hook. */
export interface RawKeyEvent {
  token: string;
  down: boolean;
  repeat: boolean;
  vk: number;
  scan: number;
  extended: boolean;
  control: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

/** Which capture implementation this platform offers. */
export type CaptureKind = "hook" | "dom";

/** Windows runs the hook; elsewhere the DOM recorder is the only ear. */
export const captureKind: CaptureKind =
  typeof navigator !== "undefined" && /Windows/.test(navigator.userAgent) ? "hook" : "dom";

export interface BackendMods {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

// (set-1 make code, E0-extended) → HID. Mirrors HID_TO_SCAN in
// crates/platform/src/keys.rs. Special cases (scan shared between a
// bare and an extended key) are resolved in hidOf below.
const SCAN_TO_HID: Record<string, number> = {
  // Letters
  "30:0": 0x04, "48:0": 0x05, "46:0": 0x06, "32:0": 0x07, "18:0": 0x08,
  "33:0": 0x09, "34:0": 0x0a, "35:0": 0x0b, "23:0": 0x0c, "36:0": 0x0d,
  "37:0": 0x0e, "38:0": 0x0f, "50:0": 0x10, "49:0": 0x11, "24:0": 0x12,
  "25:0": 0x13, "16:0": 0x14, "19:0": 0x15, "31:0": 0x16, "20:0": 0x17,
  "22:0": 0x18, "47:0": 0x19, "45:0": 0x1a, "21:0": 0x1b, "44:0": 0x1c,
  // Digits
  "2:0": 0x1e, "3:0": 0x1f, "4:0": 0x20, "5:0": 0x21, "6:0": 0x22,
  "7:0": 0x23, "8:0": 0x24, "9:0": 0x25, "10:0": 0x26, "11:0": 0x27,
  // Editing / punctuation
  "28:0": 0x28, "1:0": 0x29, "14:0": 0x2a, "15:0": 0x2b, "57:0": 0x2c,
  "12:0": 0x2d, "13:0": 0x2e, "26:0": 0x2f, "27:0": 0x30, "43:0": 0x31,
  "39:0": 0x33, "40:0": 0x34, "41:0": 0x35, "51:0": 0x36, "52:0": 0x37,
  "53:0": 0x38, "58:0": 0x39,
  // Function row
  "59:0": 0x3a, "60:0": 0x3b, "61:0": 0x3c, "62:0": 0x3d, "63:0": 0x3e,
  "64:0": 0x3f, "65:0": 0x40, "66:0": 0x41, "67:0": 0x42, "68:0": 0x43,
  "87:0": 0x44, "88:0": 0x45,
  // Navigation cluster (E0-extended; resolved via the extended flag)
  "70:0": 0x47, // Scroll Lock
  "82:1": 0x49, "71:1": 0x4a, "73:1": 0x4b, "83:1": 0x4c, "79:1": 0x4d,
  "81:1": 0x4e, "77:1": 0x4f, "75:1": 0x50, "80:1": 0x51, "72:1": 0x52,
  "69:0": 0x53,
  // Keypad
  "55:0": 0x55, "74:0": 0x56, "78:0": 0x57, "53:1": 0x54, "83:0": 0x63,
  "79:0": 0x59, "80:0": 0x5a, "81:0": 0x5b, "75:0": 0x5c, "76:0": 0x5d,
  "77:0": 0x5e, "71:0": 0x5f, "72:0": 0x60, "73:0": 0x61, "82:0": 0x62,
  "89:0": 0x67, // KP =
  "86:0": 0x32, // # ~ (ISO)
  // Modifiers — recorded as HID modifier usages (0xe0–0xe7) so the
  // recorder can light the live chips and NEVER treat a modifier press
  // as the chord-completing key. L/R pairs share one usage; the set of
  // held modifiers travels separately in the event's mod snapshot.
  // Shift is the only pair without an E0 ambiguity (RShift = 0x36 E0).
  "29:0": 0xe0, "29:1": 0xe4, // Ctrl left / right (also resolved in hidOf)
  "42:0": 0xe1, "54:1": 0xe5, // Shift left / right
  "56:0": 0xe2, "56:1": 0xe6, // Alt left / AltGr (also resolved in hidOf)
  "91:1": 0xe3, // Left Win  (scan 0x5b E0 — NOT a media key; see below)
  "92:1": 0xe7, // Right Win (scan 0x5c E0)
  // Media transport (E0-extended). The scans are the standard set-1
  // extended codes (0x19/0x10/0x24/0x22) — they deliberately alias the
  // non-extended letter scans (p/q/j/4), which is why the extended flag
  // is part of every key here.
  "25:1": 0xb5, // Next Track
  "16:1": 0xb6, // Previous Track
  "36:1": 0xb7, // Stop
  "34:1": 0xcd, // Play/Pause
  // International (JIS)
  "115:0": 0x87, // Ro (0x73)
  "93:0": 0x88, // Katakana (0x70)
  "124:0": 0x89, // Yen (0x7d)
  "94:0": 0x8a, // Henkan / Convert (0x79)
  "95:0": 0x8b, // Muhenkan / Non-convert (0x7b)
  // Volume (E0-extended)
  "160:1": 0xe8, // Mute (0x20 E0)
  "174:1": 0xe9, // Volume Up (0x30 E0)
  // Browser / AC (E0-extended)
  "50:1": 0x194, // WWW Home (0x32 E0)
  "101:1": 0x221, // Search (0x65 E0)
  "106:1": 0x224, // Back (0x6a E0)
  "105:1": 0x225, // Forward (0x69 E0)
};

/**
 * The HID usage id for a (set-1 make code, E0-extended) pair, 0 when
 * unknown (unknown keys keep listening rather than recording wrong).
 *
 * Scan codes shared between a bare and an extended key are resolved
 * here, matching keys.rs: 28 (Enter / KP Enter), 55 (KP * / Print
 * Screen), 29 (Ctrl / R-Ctrl), 56 (Alt / AltGr).
 */
function hidOf(scan: number, extended: boolean): number {
  if (scan === 28) return extended ? 0x58 : 0x28; // Enter / KP Enter
  if (scan === 55) return extended ? 0x46 : 0x55; // PrtSc / KP *
  if (scan === 56) return extended ? 0xe6 : 0xe2; // Alt / AltGr
  if (scan === 29) return extended ? 0xe4 : 0xe0; // Ctrl / R-Ctrl
  if (scan === 42) return 0xe1; // Left Shift (never E0)
  if (scan === 54) return 0xe5; // Right Shift (E0)
  if (scan === 91) return 0xe3; // Left Win (E0) — collides with nothing
  if (scan === 92) return 0xe7; // Right Win (E0)
  return SCAN_TO_HID[`${scan}:${extended ? 1 : 0}`] ?? 0;
}

/** Is this HID usage one of the eight modifier keys (0xe0–0xe7)? */
export function isModifierHid(hid: number): boolean {
  return hid >= 0xe0 && hid <= 0xe7;
}

/** The token-tagged event name the backend emits on. */
const eventNameFor = (token: string) => `keycapture:${token}`;

/** The backend's TTL-lapse broadcast: the session died, keys pass through. */
const EXPIRED_EVENT = "keycapture:expired";

/**
 * Open a backend capture session. Returns a disposer — null when the
 * hook is unavailable (non-Windows, old backend, standard user without
 * the tasks), which the caller treats as "fall back to DOM capture".
 *
 * - `onKey` fires for every mapped, non-repeat key transition.
 * - `onMods` fires whenever the live modifier snapshot changes.
 * - `onExpired` fires when the hook TTL ended the session (the page
 *   must stop recording — capture is over whether it likes it or not).
 */
export async function startBackendCapture(
  onKey: (hid: number, down: boolean, mods: BackendMods) => void,
  onMods: (mods: BackendMods) => void,
  onExpired: () => void,
): Promise<(() => void) | null> {
  const token = `rec-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  try {
    await api().StartKeyCapture(token);
  } catch {
    return null;
  }

  let expired = false;
  let lastMods: BackendMods = { ctrl: false, alt: false, shift: false, meta: false };
  const sameMods = (a: BackendMods, b: BackendMods) =>
    a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift && a.meta === b.meta;

  const ev = window.wails?.Events;
  if (!ev?.On) {
    void api().StopKeyCapture(token).catch(() => {});
    return null;
  }
  const handler = (e: { data: RawKeyEvent }) => {
    const raw = e?.data;
    if (!raw || raw.token !== token) return;
    const mods: BackendMods = { ctrl: raw.control, alt: raw.alt, shift: raw.shift, meta: raw.meta };
    if (!sameMods(mods, lastMods)) {
      lastMods = mods;
      onMods(mods);
    }
    if (raw.repeat) return;
    void api().RenewKeyCapture(token).catch(() => {});
    const hid = hidOf(raw.scan, raw.extended);
    if (hid !== 0) onKey(hid, raw.down, mods);
  };
  ev.On(eventNameFor(token), handler);
  // The TTL-lapse broadcast: the backend killed the session (the page
  // stalled past captureTTL). One notification, then keys pass through
  // system-wide — the caller must re-arm or accept the loss.
  const expiredHandler = (e: { data: RawKeyEvent }) => {
    const raw = e?.data;
    if (!raw || raw.token !== token || expired) return;
    expired = true;
    onExpired();
  };
  ev.On(EXPIRED_EVENT, expiredHandler);

  return () => {
    ev.Off(eventNameFor(token));
    ev.Off(EXPIRED_EVENT);
    if (!expired) void api().StopKeyCapture(token).catch(() => {});
  };
}
