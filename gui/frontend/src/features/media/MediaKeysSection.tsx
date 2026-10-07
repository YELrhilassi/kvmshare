// The server-role "Media keys" section: routing on/off, one target per
// category, pinned machines. Pure view over the store — every mutation
// goes through editMedia, every value from `media`; no local state and
// no bridge calls, so it cannot drift from what the store has.

import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";
import type { MediaSection, Screen } from "@/lib/bridge";
import { TargetCard } from "./TargetCard";
import { TARGETS, TRANSPORT_COMMANDS, VOLUME_COMMANDS } from "./targets";

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

function MachineTarget({
  screens,
  active,
  onPick,
  onClear,
}: {
  screens: Screen[];
  active: string | null;
  onPick: (name: string) => void;
  onClear: () => void;
}) {
  return (
    <div className="grid gap-2 sm:grid-cols-2">
      {screens.map((s) => (
        <TargetCard
          key={s.name}
          label={s.name}
          hint="Always control this machine"
          active={active === `machine:${s.name}`}
          onClick={() => (active === `machine:${s.name}` ? onClear() : onPick(s.name))}
        />
      ))}
    </div>
  );
}

export function MediaKeysSection({
  media,
  screens,
  onEdit,
}: {
  media: MediaSection;
  screens: Screen[];
  onEdit: (next: MediaSection) => void;
}) {
  return (
    <Section title="Media keys">
      <p className="text-sm text-muted-foreground">
        Playback control does not have to follow the mouse: the music may be on the other machine
        while you type here. An unresolved target always falls back to this machine — keys are
        never swallowed.
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
            title="Playback"
            commands={TRANSPORT_COMMANDS}
            value={media.transport}
            onChange={(v) => onEdit({ ...media, transport: v })}
          />
          <MachineTarget
            screens={screens}
            active={media.transport.startsWith("machine:") ? media.transport : null}
            onPick={(name) => onEdit({ ...media, transport: `machine:${name}` })}
            onClear={() => onEdit({ ...media, transport: "focus_or_last_active" })}
          />
          <CategoryChoice
            title="Volume"
            commands={VOLUME_COMMANDS}
            value={media.volume}
            onChange={(v) => onEdit({ ...media, volume: v })}
          />
        </>
      )}
    </Section>
  );
}
