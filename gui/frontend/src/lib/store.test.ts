// store.test.ts — the observable store every feature store is built on.
// The contract that matters: state is replaced (never mutated), and
// subscribers hear about it exactly once per change.

import { describe, expect, it, vi } from "vitest";
import { createStore } from "./store";

describe("createStore", () => {
  it("starts at the initial state", () => {
    const store = createStore({ n: 1 });
    expect(store.get()).toEqual({ n: 1 });
  });

  it("set replaces the state and notifies once", () => {
    const store = createStore({ n: 1 });
    const seen = vi.fn();
    store.subscribe(seen);
    store.set({ n: 2 });
    expect(seen).toHaveBeenCalledTimes(1);
    expect(store.get()).toEqual({ n: 2 });
  });

  it("patch merges and notifies", () => {
    const store = createStore({ a: 1, b: 2 });
    store.patch({ b: 3 });
    expect(store.get()).toEqual({ a: 1, b: 3 });
  });

  it("replaces the state object identity on every change", () => {
    const store = createStore({ a: 1 });
    const before = store.get();
    store.patch({ a: 1 });
    expect(store.get()).not.toBe(before);
  });

  it("unsubscribes cleanly", () => {
    const store = createStore({ n: 1 });
    const seen = vi.fn();
    const off = store.subscribe(seen);
    off();
    store.set({ n: 2 });
    expect(seen).not.toHaveBeenCalled();
  });
});
