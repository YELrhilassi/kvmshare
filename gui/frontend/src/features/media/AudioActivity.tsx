// The live half of the media & audio page: what this machine is actually
// capturing and playing right now, and a one-press test that says whether
// either half works.
//
// It exists because a switch is not evidence. "Send this machine's sound" is
// a wish; a stream that is running with sound in it is a fact, and telling
// them apart is the difference between a feature a paying user trusts and
// one they cannot debug. So the page shows three separate things:
//
//   * the configured direction (AudioSectionEditor),
//   * what the role process is actually doing — a stream up or down, the
//     device in use, the machine at the other end, and the level in each
//     direction (this file),
//   * and a tone, played on demand, so "is it me or is it the link?" has an
//     answer the user can hear.
//
// The levels are the same RMS the role reports to the media router, so the
// "playing" the page shows is exactly the "playing" that decides where
// playback control goes. The two features are one picture.

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import type { AudioSection, AudioState, AudioTestResult } from "@/lib/bridge";
import { audioDirection } from "./audioDirection";

// The meter's own scale. The silence floor the runtime uses is -50 dBFS, so
// the bottom of the drawing sits just under it: a "silent" reading is still
// somewhere on the bar rather than pinned at zero, which is what makes
// "quieter than usual" visible too.
const METER_FLOOR_DB = -60;
const METER_CEIL_DB = 0;

/** Where a level sits on the meter, as a percentage. A missing reading has
 *  no position — nothing is being captured, which is not the same as
 *  silence. */
export function meterPercent(db: number | null): number {
  if (db === null || !Number.isFinite(db)) return 0;
  const clamped = Math.max(METER_FLOOR_DB, Math.min(METER_CEIL_DB, db));
  return ((clamped - METER_FLOOR_DB) / (METER_CEIL_DB - METER_FLOOR_DB)) * 100;
}

/** "System default" is a choice, not a name: say so rather than showing an
 *  empty string in a sentence. */
export function deviceLabel(device: string): string {
  return device === "" ? "the system default output" : device;
}

/**
 * The output a test tone must play through to test the path that is in use.
 *
 * When this machine *plays*, the tone tests the device the peer's audio
 * actually goes to, so it uses the playback device.
 *
 * When this machine *sends* — or might — the tone has to be produced where
 * the shared sound is produced: the **system default output**. While
 * sending, kvmshare has routed that output through itself, so the tone is
 * captured and sent on to the other machine; with nothing shared it is just
 * a speaker check. It deliberately does not use the capture device: that is
 * a loopback *monitor* (an input), and a tone cannot be played to an input.
 */
export function testDevice(direction: ReturnType<typeof audioDirection>, audio: AudioSection): string {
  return direction === "receive" ? audio.playbackDevice : "";
}

export type Verdict = { tone: "ok" | "warn" | "error"; text: string };

/**
 * What a test result means, in one sentence — and which of the three
 * outcomes it is. Pure, so the mapping from a result to the advice the user
 * acts on is unit-tested rather than eyeballed: this text is the only thing
 * standing between "it does not work" and a fix.
 */
export function testVerdict(result: AudioTestResult): Verdict {
  const device = deviceLabel(result.device);
  if (!result.played) {
    return {
      tone: "error",
      text: `Could not play a tone on ${device}: ${result.error || "the device refused it"}.`,
    };
  }
  if (result.captureHeard) {
    const peak = result.hasPeak ? ` (peak ${result.peakDb.toFixed(1)} dB)` : "";
    const tail = result.sending
      ? " It is on its way to the other machine — that is where you hear it."
      : "";
    return {
      tone: "ok",
      text: `Tone played through ${device}, and capture heard it${peak}.${tail}`,
    };
  }
  if (result.sending) {
    // The tone played, so the output works. The capture is the half under
    // test, and this is the failure that looks like "audio is on and silent".
    return {
      tone: "warn",
      text:
        `Tone played through ${device}, but capture heard nothing. The capture device must be ` +
        `the output you are listening to (its loopback) — check it above, and that it is not muted.`,
    };
  }
  return {
    tone: "ok",
    text:
      `Tone played through ${device} — you should have heard it. Nothing is being shared right ` +
      `now, so turn on Send or Play above to test the link itself.`,
  };
}

function Meter({
  label,
  status,
  detail,
  db,
  tone,
}: {
  label: string;
  status: string;
  detail: string;
  db: number | null;
  tone: "live" | "idle";
}) {
  return (
    <div>
      <div className="flex items-baseline justify-between gap-4">
        <span className="text-sm">{label}</span>
        <span className="text-xs text-muted-foreground">{status}</span>
      </div>
      <div
        className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-muted"
        role="meter"
        aria-label={label}
        aria-valuenow={db === null ? 0 : Math.round(db)}
        aria-valuemin={METER_FLOOR_DB}
        aria-valuemax={METER_CEIL_DB}
      >
        <div
          className={tone === "live" ? "h-full bg-primary" : "h-full bg-muted-foreground/40"}
          style={{ width: `${meterPercent(db)}%` }}
        />
      </div>
      <p className="mt-1 text-xs text-muted-foreground">{detail}</p>
    </div>
  );
}

/** A level, said the way a person would say it. */
function levelText(db: number | null): string {
  if (db === null) return "no reading";
  if (db <= METER_FLOOR_DB) return "silent";
  return `${db.toFixed(1)} dB`;
}

export function AudioActivity({
  audio,
  live,
  isServer,
  onTest,
}: {
  audio: AudioSection;
  live: AudioState;
  isServer: boolean;
  onTest: (device: string) => Promise<AudioTestResult>;
}) {
  const [result, setResult] = useState<AudioTestResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const direction = audioDirection(audio);

  const run = async () => {
    setTesting(true);
    setError(null);
    try {
      setResult(await onTest(testDevice(direction, audio)));
    } catch (e) {
      setError(String(e));
    } finally {
      setTesting(false);
    }
  };

  const verdict = result ? testVerdict(result) : null;
  const peer = live.peer || (isServer ? "the connected machine" : "the server");

  return (
    <Section title="What is happening now" className="mt-12">
      <p className="text-sm text-muted-foreground">
        The settings above say what should happen. This is what is happening: whether a stream is
        running, which device it uses, and how much sound is in it. “Playing” here is the same
        answer that decides where playback control goes, so a machine playing here is the machine
        the media keys follow.
      </p>

      {!live.active ? (
        <p className="mt-4 text-xs text-muted-foreground/70">
          No audio is running. That needs a direction chosen above <em>and</em> this machine’s role
          process running.
        </p>
      ) : (
        <div className="mt-4 space-y-5">
          {live.error && (
            <p className="text-xs text-destructive">Audio problem: {live.error}</p>
          )}
          {/* Not a failure — the link is exactly as configured — but the
              reason it is silent, and the setting to change. Without this,
              a muted output and a working one look identical: both are
              simply quiet. */}
          {live.captureNote && (
            <p className="text-xs text-amber-500">{live.captureNote}</p>
          )}
          {live.sending && (
            <p className="text-xs text-muted-foreground">
              This machine is quiet while it shares: the sound comes out of {peer}, not here.
            </p>
          )}
          {live.sending && (
            <Meter
              label="Being captured here"
              tone={live.capturePlaying ? "live" : "idle"}
              status={live.capturePlaying ? `playing · ${levelText(live.captureLevelDb)}` : "silent"}
              detail={
                `Streaming ${deviceLabel(audio.captureDevice)} to ${peer}.` +
                (live.capturePlaying ? "" : " Nothing is playing on this machine right now.")
              }
              db={live.captureLevelDb}
            />
          )}
          {live.receiving && (
            <Meter
              label={`Arriving from ${peer}`}
              tone={live.receivePlaying ? "live" : "idle"}
              status={live.receivePlaying ? `playing · ${levelText(live.receiveLevelDb)}` : "nothing arriving"}
              detail={`Played on ${deviceLabel(audio.playbackDevice)}.`}
              db={live.receiveLevelDb}
            />
          )}
          {!live.sending && !live.receiving && (
            <p className="text-xs text-muted-foreground">
              Linked with {peer}, but no stream is running in either direction yet.
            </p>
          )}
        </div>
      )}

      <div className="mt-6 flex items-center gap-3">
        <Button variant="outline" size="sm" onClick={() => void run()} disabled={testing}>
          {testing ? "Playing a tone…" : "Play a test tone"}
        </Button>
        <span className="text-xs text-muted-foreground">
          Plays a short tone through {deviceLabel(testDevice(direction, audio))} and listens for it.
        </span>
      </div>

      {verdict && (
        <p
          className={
            verdict.tone === "error"
              ? "mt-3 text-xs text-destructive"
              : verdict.tone === "warn"
                ? "mt-3 text-xs text-amber-500"
                : "mt-3 text-xs text-muted-foreground"
          }
        >
          {verdict.text}
        </p>
      )}
      {error && <p className="mt-3 text-xs text-destructive">The test could not run: {error}</p>}
    </Section>
  );
}
