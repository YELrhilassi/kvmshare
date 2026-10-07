// The server-role audio-peer section: which connected machine this
// server streams its output to. Empty means automatic (the only
// connected client); with several connected and no pin, the server
// refuses to choose and stops the link — which is why the pin exists.

import { Input } from "@/components/ui/input";
import { Section } from "@/components/Section";
import type { AudioSection } from "@/lib/bridge";

export function PeerSection({
  audio,
  onEdit,
}: {
  audio: AudioSection;
  onEdit: (next: AudioSection) => void;
}) {
  return (
    <Section title="Which machine" className="mt-12">
      <p className="text-sm text-muted-foreground">
        An audio link is between exactly two machines. With one machine connected it is chosen
        automatically; pin one here when several are connected, so sound always goes to the
        machine you meant.
      </p>
      <Input
        className="w-72 font-mono"
        placeholder="machine id (short form works — empty = automatic)"
        value={audio.peer ?? ""}
        onChange={(e) => onEdit({ ...audio, peer: e.target.value })}
        aria-label="Audio peer machine id"
      />
    </Section>
  );
}
