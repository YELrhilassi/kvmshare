import type { Binding } from "@/lib/bridge";

// The shortcut model shared by the keyboard page's split panes: what
// can be bound (the actions), how bindings map to those actions, and
// the conflict rules. The Rust schema owns the wire shape
// (crates/core/src/actions.rs); this module is its frontend mirror.

export interface ActionSpec {
  /** Config action id: "cycle" | "lock" | "home" | "switch:<screen>". */
  id: string;
  title: string;
  hint: string;
  /** Base actions exist in every layout; switch actions come from screens. */
  base: boolean;
}

export const BASE_ACTIONS: ActionSpec[] = [
  { id: "cycle", title: "Cycle screens", hint: "Move control to the next machine in the layout", base: true },
  { id: "lock", title: "Toggle wall lock", hint: "Freeze or release the screen edges", base: true },
  { id: "home", title: "Go home", hint: "Return control to this machine immediately", base: true },
];

/** Every configured screen gets a "switch to it" action, after the base ones. */
export function allActions(screens: { name: string }[]): ActionSpec[] {
  const switchers = screens
    .filter((s) => s.name)
    .map((s) => ({
      id: `switch:${s.name}`,
      title: `Switch to ${s.name}`,
      hint: "Jump control straight to this machine",
      base: false,
    }));
  return [...BASE_ACTIONS, ...switchers];
}

/** The config action id of a binding, including its screen target ("switch:hp"). */
export function actionIdOf(b: Binding): string {
  return b.action === "switch" && b.screen ? `switch:${b.screen}` : b.action;
}

export function chordSig(b: { mods: Binding["mods"]; key: number }): string {
  return `${b.mods.ctrl}|${b.mods.alt}|${b.mods.shift}|${b.mods.meta}|${b.key}`;
}

export function titleOf(b: Binding, actions: ActionSpec[]): string {
  const id = actionIdOf(b);
  return actions.find((a) => a.id === id)?.title ?? id;
}
