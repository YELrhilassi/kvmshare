// The audio-sharing section, both roles: what this machine sends and
// whether it plays the peer's sound. One component, one role-dependent
// string — the consent semantics are identical, so there is exactly one
// implementation of them. The server-only peer picker is a separate
// section (PeerSection), because it answers a different question
// (which machine) than these switches (whether at all).

import { Switch } from "@/components/ui/switch";
import { Section } from "@/components/Section";
import type { AudioSection } from "@/lib/bridge";

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

export function AudioSectionEditor({
  isServer,
  audio,
  onEdit,
}: {
  isServer: boolean;
  audio: AudioSection;
  onEdit: (next: AudioSection) => void;
}) {
  return (
    <Section title={isServer ? "Audio sharing" : "Audio sharing (as a client)"} className="mt-12">
      <AudioToggle
        checked={audio.send}
        onChange={(v) => onEdit({ ...audio, send: v })}
        title="Send this machine's sound to the other machine"
        description={
          isServer
            ? "Streams this machine's output to the connected machine, over its own network channel. Never a microphone — only what is playing."
            : "Streams this machine's output to the server you are connected to. Never a microphone — only what is playing."
        }
      />
      <AudioToggle
        checked={audio.receive}
        onChange={(v) => onEdit({ ...audio, receive: v })}
        title="Play the other machine's sound here"
        description="Both machines can be audible at once — your OS mixer combines them, so nothing has to be muted."
      />
    </Section>
  );
}
