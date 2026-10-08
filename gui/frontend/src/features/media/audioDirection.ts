// Audio sharing has exactly one direction at a time: this machine's
// output goes to the peer, the peer's output comes here, or audio is
// off. Two independent toggles made "both" expressible, and "both" is
// never what a user means — the two streams are two different pieces of
// sound arriving at one mixer with no way to tell them apart.
//
// The config file still stores the two booleans the Rust schema defines
// (`send` / `receive`), because that is the wire/on-disk contract the
// role processes already read — this module is the one place that keeps
// them mutually exclusive, so no view can leave both on by accident.

import type { AudioSection } from "@/lib/bridge";

export type AudioDirection = "off" | "send" | "receive";

/** The single direction a section expresses. `send` wins if a legacy
 *  config somehow has both on; `normalizeAudioDirection` is what makes
 *  that state unreachable. */
export function audioDirection(
  audio: Pick<AudioSection, "send" | "receive"> | null | undefined,
): AudioDirection {
  if (!audio) return "off";
  if (audio.send) return "send";
  if (audio.receive) return "receive";
  return "off";
}

/** The section with exactly `dir` on and the other direction off. */
export function withAudioDirection(audio: AudioSection, dir: AudioDirection): AudioSection {
  return { ...audio, send: dir === "send", receive: dir === "receive" };
}

/** Collapse a config that has both directions on into a single choice.
 *  `send` survives, deterministically, so the correction is stable. */
export function normalizeAudioDirection(audio: AudioSection): AudioSection {
  if (audio.send && audio.receive) return withAudioDirection(audio, "send");
  return audio;
}
