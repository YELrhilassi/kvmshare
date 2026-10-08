// The server-role "Media keys" section: routing on/off, one target per
// category, and the machines a category can be pinned to. Pure view over
// the store — every mutation goes through editMedia, every value from
// `media`; no local state and no bridge calls, so it cannot drift from
// what the store has.
//
// A machine pin is stored as `machine:<machine id>`, never as the display
// name: the id is what survives a rename and what the Rust router matches
// (see gui/frontend/src/features/media/targets.ts and
// kvmshare_core::media::MediaTarget). The list of machines comes from the
// live clients, exactly like the audio peer picker, so the card the user
// clicks is a machine that is actually connected right now.

import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";
import type { ConnectedClient, MediaSection } from "@/lib/bridge";
import { TargetCard } from "./TargetCard";
import { TARGETS, TRANSPORT_COMMANDS, VOLUME_COMMANDS, pinnedMachine, sameMachine } from "./targets";

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
          <TargetCard key={t.value} label={t.label} hint={t.hint} active={value === t.value} onClick={() => onChange(t.value)} />
        ))}
      </div>
    </div>
  );
}

/**
 * The row of machines a category can be pinned to. Picking a card writes
 * `machine:<id>`; picking the selected one again clears back to the
 * follow-the-cursor policy. A pinned machine that is not connected still
 * shows, so a setting never quietly vanishes because a laptop is off.
 */
function MachineTarget({
  clients,
  pinned,
  onPick,
  onClear,
}: {
  clients: ConnectedClient[];
  pinned: string;
  onPick: (id: string) => void;
  onClear: () => void;
}) {
  const offline = pinned !== "" && !clients.some((c) => sameMachine(pinned, c.id));
  return (
    <div className="mt-2">
      <p className="text-xs text-muted-foreground">
        …or always use one machine, wherever the cursor is:
      </p>
      <div className="mt-2 grid gap-2 sm:grid-cols-2">
        {clients.map((c) => (
          <TargetCard
            key={c.id}
            label={c.name}
            hint={`Always use ${c.name}`}
            active={sameMachine(pinned, c.id)}
            onClick={() => (sameMachine(pinned, c.id) ? onClear() : onPick(c.id))}
          />
        ))}
        {offline && (
          <TargetCard
            label={pinned}
            hint="Pinned, but not connected right now"
            active
            onClick={onClear}
          />
        )}
        {clients.length === 0 && !offline && (
          <p className="text-xs text-muted-foreground/70">
            No machines are connected right now.
          </p>
        )}
      </div>
    </div>
  );
}

export function MediaKeysSection({
  media,
  clients,
  onEdit,
}: {
  media: MediaSection;
  clients: ConnectedClient[];
  onEdit: (next: MediaSection) => void;
}) {
  return (
    <Section title="Media keys">
      <p className="text-sm text-muted-foreground">
        Playback control does not have to follow the mouse: press play, next or volume on this
        keyboard while you are typing here and it goes to the machine below — no need to move the
        cursor over first. One choice covers every media key, playback and volume alike; an
        unresolved target always falls back to this machine, so keys are never swallowed.
      </p>
      <div className="flex items-center justify-between gap-6">
        <div>
          <div className="text-sm">Route media keys through kvmshare</div>
          <p className="text-xs text-muted-foreground">
            Off means the OS handles them exactly as if kvmshare were not running.
          </p>
        </div>
        <Switch checked={media.routeMediaKeys} onCheckedChange={(v) => onEdit({ ...media, routeMediaKeys: v })} />
      </div>
      {media.routeMediaKeys && (
        <>
          <CategoryChoice
            title="Where media keys go"
            commands={`${TRANSPORT_COMMANDS} · ${VOLUME_COMMANDS}`}
            value={media.target}
            onChange={(v) => onEdit({ ...media, target: v })}
          />
          <MachineTarget
            clients={clients}
            pinned={pinnedMachine(media.target)}
            onPick={(id) => onEdit({ ...media, target: `machine:${id}` })}
            onClear={() => onEdit({ ...media, target: "focus_or_last_active" })}
          />
        </>
      )}
    </Section>
  );
}
