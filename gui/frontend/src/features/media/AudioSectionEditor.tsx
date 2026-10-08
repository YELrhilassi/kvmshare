// The audio-sharing section, both roles: one direction at a time — send
// this machine's output, play the peer's, or off. Consent semantics are
// identical across roles, so there is exactly one implementation; only
// the wording of who "the other machine" is changes. The server-only
// peer picker is a separate section (PeerSection), because it answers a
// different question (which machine) than this chooser (whether at all).
//
// A pair of independent toggles used to live here, and both could be on.
// That state has no meaning to a listener: two streams land on the same
// mixer, and there is no way to tell which machine you are hearing. The
// chooser makes the single valid choice explicit (see
// audioDirection.ts for the enforcement), and the direction's device
// picker appears beneath it.
//
// The picker's list comes from the platform (see the media store); an
// empty selection means "the system default", which follows the user when
// they switch outputs — so it is the recommended choice, not a fallback.

import { useMemo } from "react";
import { Section } from "@/components/Section";
import type { AudioSection } from "@/lib/bridge";
import { audioDirection, withAudioDirection, type AudioDirection } from "./audioDirection";
import { TargetCard } from "./TargetCard";

// One device row: a native select over the platform's list. A configured
// device that is not in the current list is still shown (pinned to the
// top) so a setting can never be silently dropped just because the audio
// server is not reporting it right now.
function DevicePicker({
  label,
  hint,
  value,
  options,
  onChange,
}: {
  label: string;
  hint: string;
  value: string;
  options: string[];
  onChange: (v: string) => void;
}) {
  const list = useMemo(() => {
    const set = [...options];
    if (value && !set.includes(value)) set.unshift(value);
    return set;
  }, [options, value]);

  return (
    <label className="mt-4 block">
      <span className="text-sm">{label}</span>
      <p className="mb-1.5 text-xs text-muted-foreground">{hint}</p>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="h-9 w-full rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <option value="">System default</option>
        {list.map((d) => (
          <option key={d} value={d}>
            {d}
          </option>
        ))}
      </select>
    </label>
  );
}

export function AudioSectionEditor({
  isServer,
  audio,
  devices,
  devicesError,
  onEdit,
}: {
  isServer: boolean;
  audio: AudioSection;
  devices: { capture: string[]; playback: string[] };
  devicesError: string | null;
  onEdit: (next: AudioSection) => void;
}) {
  const direction = audioDirection(audio);
  const choose = (dir: AudioDirection) => onEdit(withAudioDirection(audio, dir));

  const choices: { value: AudioDirection; label: string; hint: string }[] = [
    {
      value: "off",
      label: "Off",
      hint: "Nothing is captured here and nothing is played from the other machine.",
    },
    {
      value: "send",
      label: "Send this machine's sound",
      hint: isServer
        ? "The connected machine becomes the output: its speakers play this machine's sound, and this machine goes quiet. Never a microphone — only what is playing."
        : "The server becomes your output: its speakers play this machine's sound, and this machine goes quiet. Never a microphone — only what is playing.",
    },
    {
      value: "receive",
      label: "Play the other machine's sound here",
      hint: "Plays the other machine's audio through the output below.",
    },
  ];

  return (
    <Section title="Audio sharing">
      <p className="text-sm text-muted-foreground">
        One direction at a time: share this machine's sound with the other one, or play the other
        machine's sound here.
      </p>
      <div className="grid gap-2 sm:grid-cols-3">
        {choices.map((c) => (
          <TargetCard
            key={c.value}
            label={c.label}
            hint={c.hint}
            active={direction === c.value}
            onClick={() => choose(c.value)}
          />
        ))}
      </div>

      {direction === "send" && (
        <>
          <p className="mt-4 text-xs text-muted-foreground">
            While sending, kvmshare routes this machine's whole sound through itself: it is
            captured and streamed to the other machine, and nothing comes out of this machine's
            speakers. That is automatic — there is nothing to choose here.
          </p>
          <DevicePicker
            label="Capture from"
            hint="Usually leave this on “System default”. A specific output only applies if kvmshare cannot route this machine's sound through itself (for example, a platform without a virtual output)."
            value={audio.captureDevice}
            options={devices.capture}
            onChange={(v) => onEdit({ ...audio, captureDevice: v })}
          />
        </>
      )}
      {direction === "receive" && (
        <DevicePicker
          label="Play to"
          hint="Where the other machine's sound comes out."
          value={audio.playbackDevice}
          options={devices.playback}
          onChange={(v) => onEdit({ ...audio, playbackDevice: v })}
        />
      )}

      {devicesError && (
        <p className="mt-4 text-xs text-muted-foreground/70">
          Could not list audio devices ({devicesError}). “System default” still works.
        </p>
      )}
    </Section>
  );
}
