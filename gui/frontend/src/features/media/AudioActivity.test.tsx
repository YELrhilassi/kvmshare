// AudioActivity.test.tsx — the pure part of the live audio panel. The three
// helpers here are the ones where a wrong answer is invisible in the UI but
// wrong to the user: which device a test tone goes to, where a level sits on
// the meter, and what a test result is actually telling them to do.

import { describe, expect, it } from "vitest";
import type { AudioSection, AudioTestResult } from "@/lib/bridge";
import { deviceLabel, meterPercent, testDevice, testVerdict } from "./AudioActivity";

const section: AudioSection = {
  send: true,
  receive: false,
  captureDevice: "sink.monitor",
  playbackDevice: "speakers",
  activityFloorDb: -50,
};

function result(over: Partial<AudioTestResult> = {}): AudioTestResult {
  return {
    played: true,
    device: "",
    error: "",
    sending: false,
    receiving: false,
    captureHeard: false,
    peakDb: -100,
    hasPeak: false,
    ...over,
  };
}

describe("testDevice", () => {
  // Playing means the peer's audio goes to the playback device, so that is
  // what the tone tests. Sending (or off) plays the tone through the system
  // default output — where the shared sound is produced — never through the
  // capture device, which is a loopback monitor (an input).
  it("tests the device under test for the chosen direction", () => {
    expect(testDevice("send", section)).toBe("");
    expect(testDevice("receive", section)).toBe("speakers");
    expect(testDevice("off", section)).toBe("");
  });
});

describe("meterPercent", () => {
  it("maps the meter's range onto the bar", () => {
    expect(meterPercent(0)).toBe(100);
    expect(meterPercent(-60)).toBe(0);
    expect(meterPercent(-30)).toBe(50);
  });

  it("has no position for a missing or impossible reading", () => {
    expect(meterPercent(null)).toBe(0);
    expect(meterPercent(Number.NaN)).toBe(0);
    expect(meterPercent(Number.NEGATIVE_INFINITY)).toBe(0);
  });

  it("clamps a reading beyond either end", () => {
    expect(meterPercent(12)).toBe(100);
    expect(meterPercent(-120)).toBe(0);
  });
});

describe("testVerdict", () => {
  it("a device that refused the tone is an error naming the reason", () => {
    const verdict = testVerdict(result({ played: false, error: "no such device" }));
    expect(verdict.tone).toBe("error");
    expect(verdict.text).toContain("no such device");
  });

  it("a tone that was captured is the good outcome, with the peak", () => {
    const verdict = testVerdict(
      result({ device: "sink.monitor", captureHeard: true, peakDb: -12.34, hasPeak: true }),
    );
    expect(verdict.tone).toBe("ok");
    expect(verdict.text).toContain("sink.monitor");
    expect(verdict.text).toContain("-12.3 dB");
  });

  // While sending, the whole point is that the tone is heard on the *other*
  // machine — saying "you should have heard it" here would be wrong.
  it("a captured tone on a live send says it is on its way to the other machine", () => {
    const verdict = testVerdict(result({ sending: true, captureHeard: true }));
    expect(verdict.tone).toBe("ok");
    expect(verdict.text).toContain("other machine");
  });

  // The failure that is hardest to diagnose without this text: the output
  // works, the link looks on, and nothing is heard.
  it("a tone the capture did not hear points at the capture device", () => {
    const verdict = testVerdict(result({ sending: true }));
    expect(verdict.tone).toBe("warn");
    expect(verdict.text).toContain("capture device");
  });

  // With nothing shared, the tone is only a speaker check, and saying it
  // passed would overclaim what was tested.
  it("a tone played with no link running says only what it proved", () => {
    const verdict = testVerdict(result());
    expect(verdict.tone).toBe("ok");
    expect(verdict.text).toContain("turn on Send or Play");
  });
});

describe("deviceLabel", () => {
  it("names the empty device rather than printing nothing", () => {
    expect(deviceLabel("")).toBe("the system default output");
    expect(deviceLabel("sink")).toBe("sink");
  });
});
