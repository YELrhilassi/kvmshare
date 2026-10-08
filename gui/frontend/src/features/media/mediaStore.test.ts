// mediaStore.test.ts — the Media & audio page's state machine.
//
// These are the transitions that decide what actually gets written: a
// store that gets dirty-tracking or the last-saved copies wrong will
// silently erase a setting the user did not touch, and one that gets the
// autosave debounce wrong will either lose an edit or write twice. The
// backend is injected (the api() thunk), which is exactly why this store
// was written that way.

import { afterEach, describe, expect, it, vi } from "vitest";
import type { AudioSection, GoApp, LayoutConfig, MediaSection } from "@/lib/bridge";
import { createMediaStore } from "./mediaStore";

const AUTOSAVE_MS = 400;

const media: MediaSection = {
  routeMediaKeys: true,
  target: "follow_focus",
  fallbackLocal: true,
};
const audio: AudioSection = {
  send: false,
  receive: false,
  captureDevice: "",
  playbackDevice: "",
  activityFloorDb: -50,
};
const config: LayoutConfig = {
  port: 24800,
  screens: [{ name: "pc", width: 1920, height: 1080, x: 0, y: 0 }],
  network: { allowlist: false, localOnly: false, trustedIds: [], revokedIds: [] },
};

/** A fake GoApp carrying only what the store calls, with per-test overrides. */
function fakeApi(overrides: Partial<Record<keyof GoApp, unknown>> = {}) {
  const fns = {
    LoadMediaAudio: vi.fn(async () => ({ media, audio })),
    LoadConfig: vi.fn(async () => config),
    LoadClientAudio: vi.fn(async () => audio),
    SaveMediaAudio: vi.fn(async () => {}),
    SaveClientAudio: vi.fn(async () => {}),
    ListAudioDevices: vi.fn(async () => ({ capture: ["sink.monitor"], playback: ["sink"] })),
    ...overrides,
  };
  return { api: (() => fns) as unknown as () => GoApp, fns };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("mediaStore (server role)", () => {
  it("loads both sections, screens, and devices; starts clean", async () => {
    const { api } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    const s = store.get();
    expect(s.media).toEqual(media);
    expect(s.audio).toEqual(audio);
    expect(s.screens).toHaveLength(1);
    expect(s.devices.capture).toEqual(["sink.monitor"]);
    expect(s.loadError).toBeNull();
    expect(store.isDirty()).toEqual({ mediaDirty: false, audioDirty: false });
  });

  it("marks only the edited half dirty", async () => {
    const { api } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    store.editMedia({ ...media, target: "machine:hp" });
    expect(store.isDirty()).toEqual({ mediaDirty: true, audioDirty: false });

    store.editAudio({ ...audio, send: true });
    expect(store.isDirty()).toEqual({ mediaDirty: true, audioDirty: true });
  });

  it("saves only the dirty half and sends the last-saved copy of the other", async () => {
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    const edited = { ...media, target: "machine:hp" as const };
    store.editMedia(edited);
    await store.save();

    // The media half is the edit; the audio half is the loaded copy, not
    // anything an in-progress edit might have left behind.
    expect(fns.SaveMediaAudio).toHaveBeenCalledWith(edited, audio);
    expect(store.isDirty()).toEqual({ mediaDirty: false, audioDirty: false });
    expect(store.get().justSaved).toBe(true);
  });

  it("saves the audio half alone when only it changed", async () => {
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    const editedAudio = { ...audio, receive: true };
    store.editAudio(editedAudio);
    await store.save();

    expect(fns.SaveMediaAudio).toHaveBeenCalledWith(media, editedAudio);
  });

  it("surfaces a load failure without inventing settings", async () => {
    const { api } = fakeApi({
      LoadMediaAudio: vi.fn(async () => {
        throw new Error("config is unreadable");
      }),
    });
    const store = createMediaStore(api, "server");
    await store.load();

    expect(store.get().media).toBeNull();
    expect(store.get().loadError).toContain("config is unreadable");
  });

  it("surfaces a save failure and stays dirty", async () => {
    const { api } = fakeApi({
      SaveMediaAudio: vi.fn(async () => {
        throw new Error("locked");
      }),
    });
    const store = createMediaStore(api, "server");
    await store.load();
    store.editMedia({ ...media, target: "machine:hp" });
    await store.save();

    expect(store.get().saveError).toContain("locked");
    expect(store.isDirty().mediaDirty).toBe(true);
  });

  // A machine with no audio server (or a missing binary) still has a valid
  // "system default": the page must keep working, with empty pickers.
  it("treats a device-list failure as non-fatal", async () => {
    const { api } = fakeApi({
      ListAudioDevices: vi.fn(async () => {
        throw new Error("no pactl");
      }),
    });
    const store = createMediaStore(api, "server");
    await store.load();

    expect(store.get().media).toEqual(media);
    expect(store.get().devices).toEqual({ capture: [], playback: [] });
    expect(store.get().devicesError).toContain("no pactl");
  });
});

describe("mediaStore (autosave)", () => {
  it("writes an edit on its own, with no explicit save", async () => {
    vi.useFakeTimers();
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    store.editMedia({ ...media, target: "machine:hp" });
    expect(fns.SaveMediaAudio).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(AUTOSAVE_MS);
    expect(fns.SaveMediaAudio).toHaveBeenCalledTimes(1);
    expect(fns.SaveMediaAudio).toHaveBeenCalledWith({ ...media, target: "machine:hp" }, audio);
  });

  it("collapses a burst of edits into a single write", async () => {
    vi.useFakeTimers();
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    store.editAudio({ ...audio, send: true, receive: false });
    store.editAudio({ ...audio, send: true, receive: false, captureDevice: "sink.monitor" });
    store.editMedia({ ...media, target: "machine:hp" });

    await vi.advanceTimersByTimeAsync(AUTOSAVE_MS);
    expect(fns.SaveMediaAudio).toHaveBeenCalledTimes(1);
  });

  it("writes nothing when nothing changed", async () => {
    vi.useFakeTimers();
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    await vi.advanceTimersByTimeAsync(AUTOSAVE_MS);
    expect(fns.SaveMediaAudio).not.toHaveBeenCalled();
  });

  // A legacy config with both directions on is collapsed to one and the
  // correction is written — the file ends up as honest as the screen.
  it("normalizes a both-directions config and saves the correction", async () => {
    vi.useFakeTimers();
    const both = { ...audio, send: true, receive: true };
    const { api, fns } = fakeApi({
      LoadMediaAudio: vi.fn(async () => ({ media, audio: both })),
    });
    const store = createMediaStore(api, "server");
    await store.load();

    expect(store.get().audio).toEqual({ ...both, receive: false });
    expect(store.isDirty().audioDirty).toBe(true);

    await vi.advanceTimersByTimeAsync(AUTOSAVE_MS);
    expect(fns.SaveMediaAudio).toHaveBeenCalledWith(media, { ...both, receive: false });
  });

  // Leaving the page (or switching role, which recreates the store) must
  // never drop the edit that was still inside the debounce window.
  it("flushes a pending edit when the page is disposed", async () => {
    vi.useFakeTimers();
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "server");
    await store.load();

    store.editAudio({ ...audio, send: true });
    store.dispose();
    await vi.advanceTimersByTimeAsync(0);
    expect(fns.SaveMediaAudio).toHaveBeenCalledWith(media, { ...audio, send: true });
  });

  // Two writes in flight at once could land out of order and erase the
  // newer one; the second waits, and the first queues the newer edit.
  it("never runs two writes at once", async () => {
    vi.useFakeTimers();
    let release!: () => void;
    const { api, fns } = fakeApi({
      SaveMediaAudio: vi.fn(() => new Promise<void>((r) => { release = r; })),
    });
    const store = createMediaStore(api, "server");
    await store.load();

    store.editMedia({ ...media, target: "machine:a" });
    const first = store.save(); // holds the write open
    store.editMedia({ ...media, target: "machine:b" });
    await store.save(); // no-op while the first is in flight
    expect(fns.SaveMediaAudio).toHaveBeenCalledTimes(1);

    release();
    await first;
    await vi.advanceTimersByTimeAsync(AUTOSAVE_MS);
    expect(fns.SaveMediaAudio).toHaveBeenCalledTimes(2);
    expect(fns.SaveMediaAudio).toHaveBeenLastCalledWith({ ...media, target: "machine:b" }, audio);
  });
});

describe("mediaStore (client role)", () => {
  it("loads only this machine's audio consent and saves it", async () => {
    const { api, fns } = fakeApi();
    const store = createMediaStore(api, "client");
    await store.load();

    expect(store.get().media).toBeNull();
    expect(store.get().audio).toEqual(audio);

    store.editAudio({ ...audio, send: true });
    await store.save();
    expect(fns.SaveClientAudio).toHaveBeenCalledWith({ ...audio, send: true });
    expect(fns.SaveMediaAudio).not.toHaveBeenCalled();
  });

  it("normalizes a both-directions client config too", async () => {
    vi.useFakeTimers();
    const both = { ...audio, send: true, receive: true };
    const { api, fns } = fakeApi({ LoadClientAudio: vi.fn(async () => both) });
    const store = createMediaStore(api, "client");
    await store.load();

    expect(store.get().audio).toEqual({ ...both, receive: false });
    await vi.advanceTimersByTimeAsync(AUTOSAVE_MS);
    expect(fns.SaveClientAudio).toHaveBeenCalledWith({ ...both, receive: false });
  });
});
