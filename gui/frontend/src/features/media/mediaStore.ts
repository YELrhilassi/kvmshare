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
//   - the saved-flash timer is cancelled on every new save and on
//     dispose, so two quick saves cannot leave a blinking "saved"
//     forever.
//
// The backend is injected (`api`), so the store can be exercised
// against a fake and the page stays free of data plumbing. The Rust
// schemas these sections mirror live in crates/app/src/config (the
// server's `[media]`/`[audio]` and the client's `[audio]`).

import { createStore, type Store } from "@/lib/store";
import type { AudioSection, GoApp, MediaSection, Screen } from "@/lib/bridge";

export interface MediaPageState {
  /** Server-role media routing section; null on a client. */
  media: MediaSection | null;
  /** Audio section, either role. Null until first load. */
  audio: AudioSection | null;
  /** The server layout's screens, for the pinned-machine target rows. */
  screens: Screen[];
  /** Load failed: the page cannot render honest settings. */
  loadError: string | null;
  /** Save failed: the last edit is still only on screen. */
  saveError: string | null;
  /** Transient "saved" flag, cleared by its own timer. */
  justSaved: boolean;
  /** The role this store is loaded for (the page reloads on a switch). */
  role: "server" | "client";
}

export type MediaPageStore = Store<MediaPageState> & {
  /** Load both sections for the store's role. Safe to re-run. */
  load: () => Promise<void>;
  /** Patch the media section locally (marks it dirty). */
  editMedia: (next: MediaSection) => void;
  /** Patch the audio section locally (marks it dirty). */
  editAudio: (next: AudioSection) => void;
  /** Save whichever sections are dirty. */
  save: () => Promise<void>;
  /** Which sections differ from the last-saved copies. */
  isDirty: () => MediaDirtyFlags;
  /** Stop the saved-flash timer (call on unmount). */
  dispose: () => void;
};

const SAVED_FLASH_MS = 1500;

export function createMediaStore(api: () => GoApp, role: "server" | "client"): MediaPageStore {
  const store = createStore<MediaPageState>({
    media: null,
    audio: null,
    screens: [],
    loadError: null,
    saveError: null,
    justSaved: false,
    role,
  });

  let lastSaved: { media: MediaSection | null; audio: AudioSection | null } = {
    media: null,
    audio: null,
  };
  let flashTimer: number | undefined;

  const equal = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

  const mediaDirty = () => {
    const { media } = store.get();
    return media !== null && !equal(media, lastSaved.media);
  };

  const audioDirty = () => {
    const { audio } = store.get();
    return audio !== null && !equal(audio, lastSaved.audio);
  };

  const flashSaved = () => {
    window.clearTimeout(flashTimer);
    store.patch({ justSaved: true });
    flashTimer = window.setTimeout(() => store.patch({ justSaved: false }), SAVED_FLASH_MS);
  };

  const load = async () => {
    store.patch({ loadError: null, saveError: null });
    try {
      if (role === "server") {
        const [m, cfg] = await Promise.all([api().LoadMediaAudio(), api().LoadConfig()]);
        lastSaved = { media: m.media, audio: m.audio };
        store.set({
          media: m.media,
          audio: m.audio,
          screens: cfg.screens ?? [],
          loadError: null,
          saveError: null,
          justSaved: false,
          role,
        });
      } else {
        const audio = await api().LoadClientAudio();
        lastSaved = { media: null, audio };
        store.set({
          media: null,
          audio,
          screens: [],
          loadError: null,
          saveError: null,
          justSaved: false,
          role,
        });
      }
    } catch (e) {
      store.patch({ loadError: String(e) });
    }
  };

  const save = async () => {
    const { media, audio } = store.get();
    if (!audio) return;
    store.patch({ saveError: null });
    try {
      if (role === "server") {
        if (!media) return;
        // The server file holds both halves; saving one half sends the
        // last-saved copy of the other, never an in-progress edit.
        await api().SaveMediaAudio(
          mediaDirty() ? media : (lastSaved.media as MediaSection),
          audioDirty() ? audio : (lastSaved.audio as AudioSection),
        );
        lastSaved = { media, audio };
      } else {
        await api().SaveClientAudio(audio);
        lastSaved = { ...lastSaved, audio };
      }
      flashSaved();
    } catch (e) {
      store.patch({ saveError: String(e) });
    }
  };

  return {
    ...store,
    load,
    editMedia: (next) => store.patch({ media: next }),
    editAudio: (next) => store.patch({ audio: next }),
    save,
    isDirty: () => ({ mediaDirty: mediaDirty(), audioDirty: audioDirty() }),
    dispose: () => window.clearTimeout(flashTimer),
  };
}

// Derived flags the page renders from; they read the store's own
// last-saved bookkeeping through the closure above, so the page never
// re-implements the comparison.
export type MediaDirtyFlags = { mediaDirty: boolean; audioDirty: boolean };
