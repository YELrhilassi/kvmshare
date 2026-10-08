// The server-role audio-peer section: which connected machine this server
// shares audio with.
//
// It used to be a machine-id text box, which asked the user to know an id
// the GUI already knows. Now it offers the machines that are actually
// connected — the same list the Clients page shows — plus an explicit
// automatic choice, and it keeps a pinned machine visible when it is not
// connected (a setting must never quietly vanish because a laptop is off).
//
// Why a choice is needed at all: an audio link is pairwise, and with three
// machines the server cannot know which one you meant. Sending refuses to
// guess (sound must never go to a machine nobody chose, so nothing plays
// until one is picked); receiving *can* be observed rather than guessed —
// each machine says whether it is playing something — so with Automatic it
// follows whichever one is.

import { Section } from "@/components/Section";
import type { AudioSection, ConnectedClient } from "@/lib/bridge";
import { TargetCard } from "./TargetCard";
import { audioDirection } from "./audioDirection";

/** Machine ids are long and users paste prefixes; the Rust side matches a
 *  pinned id by prefix in either direction, so the section must too, or the
 *  card that looks selected and the config would disagree. */
function sameMachine(pinned: string, id: string): boolean {
  return id === pinned || id.startsWith(pinned) || pinned.startsWith(id);
}

export function PeerSection({
  audio,
  clients,
  onEdit,
}: {
  audio: AudioSection;
  clients: ConnectedClient[];
  onEdit: (next: AudioSection) => void;
}) {
  const pinned = (audio.peer ?? "").trim();
  const direction = audioDirection(audio);
  const pinnedOffline = pinned !== "" && !clients.some((c) => sameMachine(pinned, c.id));

  const automaticHint =
    direction === "send"
      ? clients.length > 1
        ? "Not used while more than one machine is connected — sending always needs one chosen."
        : "Follows the only connected machine."
      : direction === "receive"
        ? clients.length > 1
          ? "Follows whichever connected machine is playing something."
          : "Follows the only connected machine."
        : "Audio sharing is off, so nothing is paired yet.";

  return (
    <Section title="Which machine shares audio" className="mt-12">
      <p className="text-sm text-muted-foreground">
        An audio link is between exactly two machines. Pick the one to pair with, or let it follow
        the machine you are actually at.
      </p>

      <div className="mt-3 grid gap-2 sm:grid-cols-2">
        <TargetCard
          label="Automatic"
          hint={automaticHint}
          active={pinned === ""}
          onClick={() => onEdit({ ...audio, peer: "" })}
        />
        {clients.map((c) => (
          <TargetCard
            key={c.id}
            label={c.name}
            hint={`Connected from ${c.addr}`}
            active={pinned !== "" && sameMachine(pinned, c.id)}
            onClick={() => onEdit({ ...audio, peer: c.id })}
          />
        ))}
        {pinnedOffline && (
          <TargetCard
            label={pinned}
            hint="Pinned, but not connected right now"
            active
            onClick={() => onEdit({ ...audio, peer: "" })}
          />
        )}
      </div>

      {clients.length === 0 && (
        <p className="mt-3 text-xs text-muted-foreground/70">
          No machines are connected right now. Automatic pairs with the only machine when one
          connects.
        </p>
      )}

      {/* The one state that looks like a bug and is not: sending is on, several
          machines are connected, and nothing is streaming because choosing
          for you would send your sound somewhere you never picked. */}
      {direction === "send" && pinned === "" && clients.length > 1 && (
        <p className="mt-3 text-xs text-amber-500">
          Nothing is being sent: {clients.length} machines are connected and the server will not
          choose one for you. Pick the machine above.
        </p>
      )}
      {direction === "receive" && pinned === "" && clients.length > 1 && (
        <p className="mt-3 text-xs text-muted-foreground">
          Listening follows whichever machine is playing something, and hands over when that one
          goes quiet. Pin one above to stop it moving.
        </p>
      )}
    </Section>
  );
}
