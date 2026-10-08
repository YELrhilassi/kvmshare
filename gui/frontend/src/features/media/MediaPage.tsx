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
// pure views over it. Saving is automatic — there is no Save button, so
// the only save UI is the quiet status readout in the header.
//
// Media routing is a server concern (the server is the machine being
// typed on), so a client's page is only its own sound sharing — the
// settings that describe *this* machine, which the wire cannot carry.

import { useEffect, useMemo } from "react";
import { Button } from "@/components/ui/button";
import { PageSkeleton } from "@/components/PageSkeleton";
import { useApp } from "@/app/AppProvider";
import { api } from "@/lib/bridge";
import { useStore } from "@/lib/useStore";
import { createMediaStore } from "./mediaStore";
import { MediaKeysSection } from "./MediaKeysSection";
import { AudioSectionEditor } from "./AudioSectionEditor";
import { AudioActivity } from "./AudioActivity";
import { PeerSection } from "./PeerSection";
import { AutoSaveStatus } from "./AutoSaveStatus";
import { targetLabel } from "./targets";

export default function MediaPage() {
  const { mode, audio: audioLive, clients } = useApp();
  const isServer = mode === "server";

  // One store per role. Recreated when the role flips (the client and
  // server read different files), disposed with the page so a pending
  // autosave or the saved-flash timer cannot outlive the view.
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
        <h1 className="text-lg font-semibold tracking-tight">{isServer ? "Media & audio" : "Audio"}</h1>
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
        <header className="mb-10 flex items-start justify-between gap-6">
          <div className="space-y-2">
            <h1 className="text-2xl font-semibold tracking-tight">{isServer ? "Media & audio" : "Audio"}</h1>
            <p className="text-sm text-muted-foreground">
              {isServer
                ? "Where playback control goes — independent of the cursor — and whether this machine shares its sound."
                : "Whether this machine shares its sound with the server you are connected to."}
            </p>
          </div>
          <AutoSaveStatus
            dirty={dirty.mediaDirty || dirty.audioDirty}
            saving={state.saving}
            justSaved={state.justSaved}
            error={state.saveError}
          />
        </header>

        {isServer && media && (
          <>
            <MediaKeysSection media={media} clients={clients} onEdit={store.editMedia} />
            {media.routeMediaKeys && media.target !== "local" && (
              <p className="mt-4 text-xs text-muted-foreground/70">
                While routing is on, kvmshare intercepts the media keys on this machine and places
                each one — {targetLabel(media.target) ?? "the configured target"} receives
                playback and volume, and the local OS does not act on the same press twice.
              </p>
            )}
          </>
        )}

        <div className={isServer ? "mt-12" : undefined}>
          <AudioSectionEditor
            isServer={isServer}
            audio={audio}
            devices={state.devices}
            devicesError={state.devicesError}
            onEdit={store.editAudio}
          />
        </div>

        {isServer && (
          <PeerSection audio={audio} clients={clients} onEdit={store.editAudio} />
        )}

        {/* What the role process is actually doing, and a tone to hear it
            with — the page's own answer to "is this really working?". */}
        <AudioActivity
          audio={audio}
          live={audioLive}
          isServer={isServer}
          onTest={(device) => api().TestAudio(device)}
        />

        {!isServer && (
          <p className="mt-6 text-xs text-muted-foreground/70">
            A saved change applies at the next connection to the server.
          </p>
        )}
      </div>
    </div>
  );
}
