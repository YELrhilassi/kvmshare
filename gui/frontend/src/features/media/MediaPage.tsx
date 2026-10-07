import { useEffect, useMemo, useState } from "react";
import { api, type AudioSection, type MediaSection, type Screen } from "@/lib/bridge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";
import { PageSkeleton } from "@/components/PageSkeleton";
import { useApp } from "@/app/AppProvider";
import { cn } from "@/lib/utils";

// Media & audio: where playback and volume keys go, and whether this
// machine streams its output to the other one. Two independent systems —
// routing decides who receives a media key, audio decides where sound
// plays — which is why this is its own page and not a Server/Client
// subsection: the choices survive role switches, and both roles' pages
// stay about connection.

/** One routing target choice for a category. */
const TARGETS: { value: string; label: string; hint: string }[] = [
  {
    value: "focus_or_last_active",
    label: "Follow the cursor",
    hint: "…and keep controlling the last machine that made sound once the cursor is home",
  },
  {
    value: "last_active_source",
    label: "Last active source",
    hint: "Whichever machine most recently played audio, wherever the cursor is",
  },
  {
    value: "follow_focus",
    label: "Cursor only",
    hint: "Exactly the classic behaviour: media keys go where the mouse is",
  },
  {
    value: "local",
    label: "This machine",
    hint: "The keys never leave this machine — kvmshare does not intercept them",
  },
];

const TRANSPORT_COMMANDS = "play/pause · next · previous · stop · seek";
const VOLUME_COMMANDS = "volume up/down · mute";

/** One machine-pinned option row (built from the layout screens). */
function MachineTarget({
  screens,
  onPick,
  onClear,
  active,
}: {
  screens: Screen[];
  onPick: (name: string) => void;
  onClear: () => void;
  active: string | null;
}) {
  return (
    <div className="grid gap-2 sm:grid-cols-2">
      {screens.map((s) => (
        <button
          key={s.name}
          onClick={() => (active === `machine:${s.name}` ? onClear() : onPick(s.name))}
          className={cn(
            "rounded-lg border px-3 py-2.5 text-left transition-colors",
            active === `machine:${s.name}`
              ? "border-primary bg-primary/5"
              : "border-border/70 hover:border-border hover:bg-muted/30",
          )}
        >
          <div className={cn("text-sm font-medium", active === `machine:${s.name}` && "text-primary")}>
            {s.name}
          </div>
          <div className="mt-0.5 text-[11px] leading-snug text-muted-foreground">
            Always control this machine
          </div>
        </button>
      ))}
    </div>
  );
}

function CategoryChoice({
  title,
  commands,
  value,
  onChange,
}: {
  title: string;
  commands: string;
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div className="border-t border-border/50 py-4 first:border-t-0 first:pt-0">
      <div className="text-sm">{title}</div>
      <p className="text-xs text-muted-foreground">{commands}</p>
      <div className="mt-3 grid gap-2 sm:grid-cols-2">
        {TARGETS.map((t) => (
          <button
            key={t.value}
            onClick={() => onChange(t.value)}
            className={cn(
              "rounded-lg border px-3 py-2.5 text-left transition-colors",
              value === t.value
                ? "border-primary bg-primary/5"
                : "border-border/70 hover:border-border hover:bg-muted/30",
            )}
          >
            <div className={cn("text-sm font-medium", value === t.value && "text-primary")}>{t.label}</div>
            <div className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{t.hint}</div>
          </button>
        ))}
      </div>
    </div>
  );
}

function AudioToggle({
  checked,
  onChange,
  title,
  description,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  title: string;
  description: string;
}) {
  return (
    <div className="flex items-center justify-between gap-6 border-t border-border/50 py-3 first:border-t-0 first:pt-0">
      <div>
        <div className="text-sm">{title}</div>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} />
    </div>
  );
}

export default function MediaPage() {
  const { mode } = useApp();
  const isServer = mode === "server";
  const [media, setMedia] = useState<MediaSection | null>(null);
  const [audio, setAudio] = useState<AudioSection | null>(null);
  const [screens, setScreens] = useState<Screen[]>([]);
  const [err, setErr] = useState("");
  const [loadErr, setLoadErr] = useState("");
  const [saved, setSaved] = useState(false);
  const [mediaDirty, setMediaDirty] = useState(false);
  const [audioDirty, setAudioDirty] = useState(false);

  const load = async () => {
    setLoadErr("");
    try {
      if (isServer) {
        const [m, a, c] = await Promise.all([api().LoadMediaAudio(), api().LoadConfig()]);
        setMedia(m.media);
        setAudio(m.audio);
        setAudio(a);
        setScreens(c.screens);
      } else {
        const a = await api().LoadClientAudio();
        setAudio(a);
      }
    } catch (e) {
      setLoadErr(String(e));
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isServer]);

  const flashSaved = () => {
    setSaved(true);
    window.setTimeout(() => setSaved(false), 1500);
  };

  const saveMedia = async (next: MediaSection) => {
    setErr("");
    try {
      await api().SaveMediaAudio(next, audio as AudioSection);
      setMedia(next);
      setMediaDirty(false);
      flashSaved();
    } catch (e) {
      setErr(String(e));
    }
  };

  const saveAudio = async (next: AudioSection) => {
    setErr("");
    try {
      if (isServer) {
        await api().SaveMediaAudio(media as MediaSection, next);
      } else {
        await api().SaveClientAudio(next);
      }
      setAudio(next);
      setAudioDirty(false);
      flashSaved();
    } catch (e) {
      setErr(String(e));
    }
  };

  const transportLabel = useMemo(
    () => TARGETS.find((t) => t.value === media?.transport)?.label ?? media?.transport,
    [media],
  );

  if (loadErr) {
    return (
      <div className="mx-auto w-full max-w-2xl px-8 py-8">
        <h1 className="text-lg font-semibold tracking-tight">Media & audio</h1>
        <p className="mt-4 text-sm text-destructive">Could not load settings: {loadErr}</p>
        <Button className="mt-3" variant="outline" onClick={() => void load()}>
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
          <Section title="Media keys">
            <p className="text-sm text-muted-foreground">
              Playback control does not have to follow the mouse: the music may be on the other
              machine while you type here. An unresolved target always falls back to this machine —
              keys are never swallowed.
            </p>
            <div className="flex items-center justify-between gap-6">
              <div>
                <div className="text-sm">Route media keys through kvmshare</div>
                <p className="text-xs text-muted-foreground">
                  Off means the OS handles them exactly as if kvmshare were not running.
                </p>
              </div>
              <Switch
                checked={media.routeMediaKeys}
                onCheckedChange={(v) => {
                  setMedia({ ...media, routeMediaKeys: v });
                  setMediaDirty(true);
                }}
              />
            </div>
            {media.routeMediaKeys && (
              <>
                <CategoryChoice
                  title="Playback"
                  commands={TRANSPORT_COMMANDS}
                  value={media.transport}
                  onChange={(v) => {
                    setMedia({ ...media, transport: v });
                    setMediaDirty(true);
                  }}
                />
                <MachineTarget
                  screens={screens}
                  active={media.transport.startsWith("machine:") ? media.transport : null}
                  onPick={(name) => {
                    setMedia({ ...media, transport: `machine:${name}` });
                    setMediaDirty(true);
                  }}
                  onClear={() => {
                    setMedia({ ...media, transport: "focus_or_last_active" });
                    setMediaDirty(true);
                  }}
                />
                <CategoryChoice
                  title="Volume"
                  commands={VOLUME_COMMANDS}
                  value={media.volume}
                  onChange={(v) => {
                    setMedia({ ...media, volume: v });
                    setMediaDirty(true);
                  }}
                />
              </>
            )}
            <div className="mt-6 flex items-center gap-3">
              <Button onClick={() => void saveMedia(media)} disabled={!mediaDirty}>
                {mediaDirty ? "Save media keys" : "Saved"}
              </Button>
              {saved && !mediaDirty && <span className="text-xs text-emerald-600">saved</span>}
              {err && <span className="text-xs text-destructive">{err}</span>}
            </div>
            {media.routeMediaKeys && media.transport !== "local" && (
              <p className="text-xs text-muted-foreground/70">
                While routing is on, kvmshare intercepts the media keys on this machine and places
                each one — {transportLabel ?? "the configured target"} receives playback, and the
                local OS does not act on the same press twice.
              </p>
            )}
          </Section>
        )}

        <Section title={isServer ? "Audio sharing" : "Audio sharing (as a client)"} className="mt-12">
          <AudioToggle
            checked={audio.send}
            onChange={(v) => {
              setAudio({ ...audio, send: v });
              setAudioDirty(true);
            }}
            title="Send this machine's sound to the other machine"
            description={
              isServer
                ? "Streams this machine's output to the connected machine, over its own network channel. Never a microphone — only what is playing."
                : "Streams this machine's output to the server you are connected to. Never a microphone — only what is playing."
            }
          />
          <AudioToggle
            checked={audio.receive}
            onChange={(v) => {
              setAudio({ ...audio, receive: v });
              setAudioDirty(true);
            }}
            title="Play the other machine's sound here"
            description="Both machines can be audible at once — your OS mixer combines them, so nothing has to be muted."
          />
          {audioDirty && (
            <div className="mt-4 flex items-center gap-3">
              <Button onClick={() => void saveAudio(audio)}>Save audio</Button>
              {err && <span className="text-xs text-destructive">{err}</span>}
              <span className="text-xs text-muted-foreground">applies on the next connection</span>
            </div>
          )}
        </Section>

        {isServer && (
          <Section title="Which machine" className="mt-12">
            <p className="text-sm text-muted-foreground">
              An audio link is between exactly two machines. With one machine connected it is
              chosen automatically; pin one here when several are connected, so sound always goes
              to the machine you meant.
            </p>
            <Input
              className="w-72 font-mono"
              placeholder="machine id (short form works — empty = automatic)"
              value={audio.peer ?? ""}
              onChange={(e) => {
                setAudio({ ...audio, peer: e.target.value });
                setAudioDirty(true);
              }}
              aria-label="Audio peer machine id"
            />
            {audioDirty && (
              <div className="flex items-center gap-3">
                <Button onClick={() => void saveAudio(audio)}>Save audio</Button>
                <span className="text-xs text-muted-foreground">applies on the next connection</span>
              </div>
            )}
          </Section>
        )}

        {(err || (saved && mediaDirty)) && (
          <p className={cn("mt-4 text-xs", err ? "text-destructive" : "text-emerald-600")}>
            {err || "saved"}
          </p>
        )}
      </div>
    </div>
  );
}
