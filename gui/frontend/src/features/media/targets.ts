// The routing-target choices, shared by every category control on the
// media page. The `value` strings are the server config's wire forms
// (kvmshare_core::media::MediaTarget) — this file passes them through,
// it does not own them, and the Go side validates against the same list
// (gui/media.go validMediaTarget).

/** One routing target choice for a category. */
export const TARGETS: { value: string; label: string; hint: string }[] = [
  {
    value: "focus_or_last_active",
    label: "Follow the cursor",
    hint: "…and keep controlling the last machine that made sound once the cursor is home",
  },
  {
    value: "last_active_source",
    label: "Last active source",
    hint: "Whichever machine most recently played audio, wherever the cursor is",
  },
  {
    value: "follow_focus",
    label: "Cursor only",
    hint: "Exactly the classic behaviour: media keys go where the mouse is",
  },
  {
    value: "local",
    label: "This machine",
    hint: "The keys never leave this machine — kvmshare does not intercept them",
  },
];

/** What each category controls, shown under its heading. */
export const TRANSPORT_COMMANDS = "play/pause · next · previous · stop · seek";
export const VOLUME_COMMANDS = "volume up/down · mute";

/** The human label for a config-form target (machine pins included). */
export function targetLabel(value: string | undefined): string | undefined {
  if (!value) return undefined;
  if (value.startsWith("machine:")) {
    const name = value.slice("machine:".length);
    return name ? `Always ${name}` : undefined;
  }
  return TARGETS.find((t) => t.value === value)?.label ?? value;
}

/**
 * The machine id a `machine:<id>` target pins, or `""` for any other
 * policy. The id is what the Rust router matches; the page only ever
 * renders the connected client it belongs to.
 */
export function pinnedMachine(value: string | undefined): string {
  if (!value) return "";
  const id = value.startsWith("machine:") ? value.slice("machine:".length) : "";
  return id.trim();
}

/**
 * Do a pinned machine and a connected client refer to the same machine?
 * The Rust side matches a machine id by prefix in either direction (users
 * paste what they copied), and accepts a display name too; the card that
 * looks selected must agree with what the router will actually do.
 */
export function sameMachine(pinned: string, id: string): boolean {
  const p = pinned.trim();
  const v = id.trim();
  if (p === "" || v === "") return false;
  return v === p || v.startsWith(p) || p.startsWith(v);
}
