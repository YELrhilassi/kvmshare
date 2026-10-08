// The media & audio settings state, one store for the page.
//
// The page used to hold nine loose useState cells that duplicated each
// other's bookkeeping: which role it was saving to, which half was
// dirty, one error shown in two places, a saved-flash timer nobody
// cancelled. The store owns all of it once:
//
//   - `media` / `audio` are the sections as loaded from the backend
//     (server file or client file, per role);
//   - `mediaDirty` / `audioDirty` are derived from the last-saved
//     copies, not tracked by hand — a save cannot forget to clear them
//     and an edit cannot forget to set them;
//   - errors are one slot per phase, so a stale load error cannot
//     masquerade as a save error;
//   - the saved-flash timer and the autosave debounce are cancelled on
//     dispose, so a leave-page cannot leave a pending write or a
//     blinking "saved" behind.
//
// Saving is automatic. There is no Save button: every edit is debounced
// and written on its own, so the page can never show settings the files
// do not have. Two edits in quick succession collapse into one write;
// an edit that lands while a write is in flight queues the next one
// instead of racing it. The dirty-half rule is unchanged — the server
// file holds both sections, so saving one half sends the last-saved copy
// of the other, never an in-progress edit.
//
// The backend is injected (`api`), so the store can be exercised
// against a fake and the page stays free of data plumbing. The Rust
// schemas these sections mirror live in crates/app/src/config (the
// server's `[media]`/`[audio]` and the client's `[audio]`).

import { createStore, type Store } from "@/lib/store";
import type { AudioDevices, AudioSection, GoApp, MediaSection, Screen } from "@/lib/bridge";
import { normalizeAudioDirection } from "./audioDirection";

export interface MediaPageState {
  /** Server-role media routing section; null on a client. */
  media: MediaSection | null;
  /** Audio section, either role. Null until first load. */
  audio: AudioSection | null;
  /** The server layout's screens, for the pinned-machine target rows. */
  screens: Screen[];
  /** This machine's audio devices, for the pickers. Empty = default only. */
  devices: AudioDevices;
  /** The device list could not be read; the pickers fall back to default. */
  devicesError: string | null;
  /** Load failed: the page cannot render honest settings. */
  loadError: string | null;
  /** Save failed: the last edit is still only on screen. */
  saveError: string | null;
  /** A write is in flight right now. */
  saving: boolean;
  /** Transient "saved" flag, cleared by its own timer. */
  justSaved: boolean;
  /** The role this store is loaded for (the page reloads on a switch). */
  role: "server" | "client";
}

export type MediaPageStore = Store<MediaPageState> & {
  /** Load both sections for the store's role. Safe to re-run. */
  load: () => Promise<void>;
  /** Patch the media section locally; the change auto-saves. */
  editMedia: (next: MediaSection) => void;
  /** Patch the audio section locally; the change auto-saves. */
  editAudio: (next: AudioSection) => void;
  /** Save whichever sections are dirty, immediately. */
  save: () => Promise<void>;
  /** Which sections differ from the last-saved copies. */
  isDirty: () => MediaDirtyFlags;
  /** Stop the autosave debounce and the saved-flash timer (call on unmount). */
  dispose: () => void;
};

const SAVED_FLASH_MS = 1500;
// Long enough that a series of clicks (a device, then a direction) is one
// write, short enough that "Saving…" is barely perceptible.
const AUTOSAVE_DEBOUNCE_MS = 400;

export function createMediaStore(api: () => GoApp, role: "server" | "client"): MediaPageStore {
  const store = createStore<MediaPageState>({
    media: null,
    audio: null,
    screens: [],
    devices: { capture: [], playback: [] },
    devicesError: null,
    loadError: null,
    saveError: null,
    saving: false,
    justSaved: false,
    role,
  });

  let lastSaved: { media: MediaSection | null; audio: AudioSection | null } = {
    media: null,
    audio: null,
  };
  let flashTimer: number | undefined;
  let saveTimer: number | undefined;
  let saveInFlight = false;

  const equal = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

  const isDirty = (): MediaDirtyFlags => {
    const { media, audio } = store.get();
    return {
      mediaDirty: media !== null && !equal(media, lastSaved.media),
      audioDirty: audio !== null && !equal(audio, lastSaved.audio),
    };
  };
  const dirtyNow = () => {
    const d = isDirty();
    return d.mediaDirty || d.audioDirty;
  };

  const flashSaved = () => {
    window.clearTimeout(flashTimer);
    store.patch({ justSaved: true });
    flashTimer = window.setTimeout(() => store.patch({ justSaved: false }), SAVED_FLASH_MS);
  };

  // The device list is fetched alongside the settings, but a failure here
  // is never fatal: a machine without an audio server still has a valid
  // "system default", so the pickers fall back to it and the page keeps
  // working. That is why this resolves to a value instead of throwing.
  const listDevices = async (): Promise<{ devices: AudioDevices; error: string | null }> => {
    try {
      return { devices: await api().ListAudioDevices(), error: null };
    } catch (e) {
      return { devices: { capture: [], playback: [] }, error: String(e) };
    }
  };

  const save = async () => {
    window.clearTimeout(saveTimer);
    saveTimer = undefined;
    // One write at a time. Two overlapping writes read their snapshots at
    // different instants, so an older one finishing last could overwrite a
    // newer one; the in-flight save re-checks dirtiness when it finishes
    // and queues whatever changed, so nothing is dropped. An explicit
    // `save()` (the tests) therefore returns immediately when one is
    // already running rather than starting a second.
    if (saveInFlight) return;
    const { media, audio } = store.get();
    if (!audio) return;
    const dirty = isDirty();
    if (!dirty.mediaDirty && !dirty.audioDirty) return;
    saveInFlight = true;
    store.patch({ saveError: null, saving: true });
    let ok = false;
    try {
      if (role === "server") {
        if (!media) return;
        // The server file holds both halves; saving one half sends the
        // last-saved copy of the other, never an in-progress edit.
        await api().SaveMediaAudio(
          dirty.mediaDirty ? media : (lastSaved.media as MediaSection),
          dirty.audioDirty ? audio : (lastSaved.audio as AudioSection),
        );
        lastSaved = { media, audio };
        ok = true;
      } else {
        await api().SaveClientAudio(audio);
        lastSaved = { ...lastSaved, audio };
        ok = true;
      }
    } catch (e) {
      store.patch({ saveError: String(e) });
    } finally {
      saveInFlight = false;
      store.patch({ saving: false });
    }
    if (ok) flashSaved();
    // An edit that landed while the write was in flight left us dirty
    // again; queue the next write rather than dropping it on the floor.
    if (dirtyNow()) scheduleSave();
  };

  function scheduleSave() {
    window.clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => void save(), AUTOSAVE_DEBOUNCE_MS);
  }

  const load = async () => {
    store.patch({ loadError: null, saveError: null });
    try {
      const dev = await listDevices();
      if (role === "server") {
        const [m, cfg] = await Promise.all([api().LoadMediaAudio(), api().LoadConfig()]);
        // A bound Go method's multiple *non-error* returns marshal as a
        // JSON array, not an object (see gui/media.go MediaAudio). If the
        // payload ever arrives in the wrong shape, say so plainly instead
        // of dereferencing `.send` three calls later and crashing the page
        // with "undefined is not an object".
        if (!m || !m.media || !m.audio) {
          throw new Error("the backend returned an unexpected media/audio payload");
        }
        // A legacy config with both directions on is collapsed to one;
        // `lastSaved` keeps the raw copy so the correction is dirty and
        // gets written by the autosave rather than only living on screen.
        const audio = normalizeAudioDirection(m.audio);
        lastSaved = { media: m.media, audio: m.audio };
        store.set({
          media: m.media,
          audio,
          screens: cfg.screens ?? [],
          devices: dev.devices,
          devicesError: dev.error,
          loadError: null,
          saveError: null,
          saving: false,
          justSaved: false,
          role,
        });
      } else {
        const raw = await api().LoadClientAudio();
        if (!raw) throw new Error("the backend returned an unexpected audio payload");
        const audio = normalizeAudioDirection(raw);
        lastSaved = { media: null, audio: raw };
        store.set({
          media: null,
          audio,
          screens: [],
          devices: dev.devices,
          devicesError: dev.error,
          loadError: null,
          saveError: null,
          saving: false,
          justSaved: false,
          role,
        });
      }
      if (dirtyNow()) scheduleSave();
    } catch (e) {
      store.patch({ loadError: String(e) });
    }
  };

  return {
    ...store,
    load,
    editMedia: (next) => {
      store.patch({ media: next });
      scheduleSave();
    },
    editAudio: (next) => {
      store.patch({ audio: next });
      scheduleSave();
    },
    save,
    isDirty,
    dispose: () => {
      window.clearTimeout(flashTimer);
      // A pending autosave is the user's last edit. Flush it as the page
      // goes away instead of dropping it — leaving the page (or switching
      // role, which recreates the store) must never lose a change.
      if (saveTimer !== undefined) {
        window.clearTimeout(saveTimer);
        saveTimer = undefined;
        void save();
      }
    },
  };
}

// Derived flags the page renders from; they read the store's own
// last-saved bookkeeping through the closure above, so the page never
// re-implements the comparison.
export type MediaDirtyFlags = { mediaDirty: boolean; audioDirty: boolean };
