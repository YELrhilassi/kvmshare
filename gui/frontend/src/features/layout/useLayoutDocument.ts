import { useReducer } from "react";
import type { Screen } from "@/lib/bridge";
import { placementFor } from "@/features/layout/geometry";

// The layout document, as a pure reducer: every edit is an action, so
// the page never sprinkles setState calls through gesture handlers.

export interface DocState {
  screens: Screen[];
  selected: number; // screen index, -1 when none
  lock: boolean;
  snap: boolean;
  dirty: boolean;
  savedMsg: string;
  error: string;
}

export type DocAction =
  | { type: "select"; index: number }
  | { type: "move"; index: number; x: number; y: number }
  | { type: "patch"; index: number; patch: Partial<Screen> }
  | { type: "add" }
  | { type: "duplicate" }
  | { type: "remove" }
  | { type: "lock"; on: boolean }
  | { type: "snap"; on: boolean }
  | { type: "markSaved"; message: string }
  | { type: "clearMessage" }
  | { type: "fail"; message: string };

export function initialDoc(screens: Screen[]): DocState {
  return { screens, selected: -1, lock: false, snap: true, dirty: false, savedMsg: "", error: "" };
}

export function docReducer(state: DocState, action: DocAction): DocState {
  switch (action.type) {
    case "select":
      return { ...state, selected: action.index };
    case "move":
      return {
        ...state,
        screens: state.screens.map((s, i) => (i === action.index ? { ...s, x: action.x, y: action.y } : s)),
        dirty: true,
      };
    case "patch":
      return {
        ...state,
        screens: state.screens.map((s, i) => (i === action.index ? { ...s, ...action.patch } : s)),
        dirty: true,
      };
    case "add":
      return {
        ...state,
        screens: [...state.screens, placementFor(state.screens)],
        selected: state.screens.length,
        dirty: true,
      };
    case "duplicate": {
      const s = state.screens[state.selected];
      if (!s) return state;
      return {
        ...state,
        screens: [...state.screens, { ...s, name: `${s.name} copy`, x: s.x + 40, y: s.y + 40 }],
        selected: state.screens.length,
        dirty: true,
      };
    }
    case "remove": {
      // Index 0 is this machine's own screen — it cannot be removed.
      if (state.selected <= 0) return state;
      return {
        ...state,
        screens: state.screens.filter((_, i) => i !== state.selected),
        selected: -1,
        dirty: true,
      };
    }
    case "lock":
      return { ...state, lock: action.on };
    case "snap":
      return { ...state, snap: action.on };
    case "markSaved":
      return { ...state, dirty: false, savedMsg: action.message, error: "" };
    case "clearMessage":
      return { ...state, savedMsg: "" };
    case "fail":
      return { ...state, error: action.message };
  }
}

export function useLayoutDocument(initial: Screen[] | undefined) {
  const [state, dispatch] = useReducer(docReducer, initial ?? [], initialDoc);
  return { state, dispatch };
}