// Media & audio: where playback and volume keys go, and whether this
// machine streams its output to the other one. Two independent systems
// — routing decides who receives a media key, audio decides where sound
// plays — which is why this is its own page and not a Server/Client
// subsection: the choices survive role switches, and both roles' pages
// stay about connection.
//
// The page is deliberately thin: it owns the store's lifecycle (create
// per role, load, dispose) and composes the sections. Every piece of
// state and every bridge call lives in mediaStore; the sections are
// pure views over it. That split is what keeps this file small while
// the feature grows.

import { useEffect, useMemo } from "react";
import { Button } from "@/components/ui/button";
import { PageSkeleton } from "@/components/PageSkeleton";
import { useApp } from "@/app/AppProvider";
import { api } from "@/lib/bridge";
import { useStore } from "@/lib/useStore";
import { createMediaStore } from "./mediaStore";
import { MediaKeysSection } from "./MediaKeysSection";
import { AudioSectionEditor } from "./AudioSectionEditor";
import { PeerSection } from "./PeerSection";
import { SaveBar } from "./SaveBar";
import { targetLabel } from "./targets";

export default function MediaPage() {
  const { mode } = useApp();
  const isServer = mode === "server";

  // One store per role. Recreated when the role flips (the client and
  // server read different files), disposed with the page so the
  // saved-flash timer cannot outlive the view.
  const store = useMemo(() => createMediaStore(api, isServer ? "server" : "client"), [isServer]);
  useEffect(() => {
    void store.load();
    return () => store.dispose();
  }, [store]);

  const state = useStore(store);
  const { media, audio } = state;
  const dirty = store.isDirty();

  if (state.loadError) {
    return (
      <div className="mx-auto w-full max-w-2xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Media & audio</h1>
        <p className="mt-4 text-sm text-destructive">Could not load settings: {state.loadError}</p>
        <Button className="mt-3" variant="outline" onClick={() => void store.load()}>
          Retry
        </Button>
      </div>
    );
  }
  if (!audio || (isServer && !media)) {
    return <PageSkeleton rows={3} />;
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl px-10 py-16">
        <header className="mb-10 space-y-2">
          <h1 className="text-2xl font-semibold tracking-tight">Media & audio</h1>
          <p className="text-sm text-muted-foreground">
            Where playback control goes — independent of the cursor — and whether this machine
            shares its sound.
          </p>
        </header>

        {isServer && media && (
          <>
            <MediaKeysSection media={media} screens={state.screens} onEdit={store.editMedia} />
            <div className="mt-6">
              <SaveBar
                dirty={dirty.mediaDirty}
                justSaved={state.justSaved}
                error={state.saveError}
                label="Save media keys"
                onSave={() => void store.save()}
              />
            </div>
            {media.routeMediaKeys && media.transport !== "local" && (
              <p className="mt-4 text-xs text-muted-foreground/70">
                While routing is on, kvmshare intercepts the media keys on this machine and places
                each one — {targetLabel(media.transport) ?? "the configured target"} receives
                playback, and the local OS does not act on the same press twice.
              </p>
            )}
          </>
        )}

        <AudioSectionEditor isServer={isServer} audio={audio} onEdit={store.editAudio} />
        {isServer && <PeerSection audio={audio} onEdit={store.editAudio} />}

        <div className="mt-6 flex items-center gap-3">
          <SaveBar
            dirty={dirty.audioDirty}
            justSaved={state.justSaved}
            error={state.saveError}
            label="Save audio"
            onSave={() => void store.save()}
          />
          {dirty.audioDirty && (
            <span className="text-xs text-muted-foreground">applies on the next connection</span>
          )}
        </div>
      </div>
    </div>
  );
}
