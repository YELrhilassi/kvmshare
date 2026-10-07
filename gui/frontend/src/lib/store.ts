// A minimal observable store: one mutable state cell, subscribe to
// changes, replace the state object on every update so React's
// useSyncExternalStore can bind to it.
//
// The app deliberately has no state library: the one piece of global
// state (the backend-pushed live snapshot) is hand-rolled in
// AppProvider on exactly this subscribe/getSnapshot idea. Feature
// state follows the same pattern instead of introducing a dependency
// for what is sixty lines: pages render from a store, actions mutate
// it, React re-renders through the one subscription hook below.
//
// State is always replaced (never mutated in place), so a snapshot
// taken by React stays valid across renders and re-renders are driven
// only by real changes.

export interface Store<S> {
  /** The current state object. Reference-stable between updates. */
  get(): S;
  /** Replace the state with `next` and notify subscribers. */
  set(next: S): void;
  /** Patch part of the state (shallow merge) and notify. */
  patch(part: Partial<S>): void;
  /**
   * Subscribe to state changes. Returns the unsubscribe function.
   * The listener sees no argument — read `get()` inside the callback
   * (this is what useSyncExternalStore expects).
   */
  subscribe(listener: () => void): () => void;
}

export function createStore<S>(initial: S): Store<S> {
  let state = initial;
  const listeners = new Set<() => void>();
  const notify = () => listeners.forEach((l) => l());
  return {
    get: () => state,
    set: (next) => {
      state = next;
      notify();
    },
    patch: (part) => {
      state = { ...state, ...part };
      notify();
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
