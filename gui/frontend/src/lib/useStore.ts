// Bind any Store to React: one subscription per mounted component via
// useSyncExternalStore. The store's get() is reference-stable between
// updates, which is exactly the snapshot contract this hook requires —
// components re-render only when a store actually replaced its state.

import { useSyncExternalStore } from "react";
import type { Store } from "./store";

export function useStore<S>(store: Store<S>): S {
  return useSyncExternalStore(store.subscribe, store.get, store.get);
}
