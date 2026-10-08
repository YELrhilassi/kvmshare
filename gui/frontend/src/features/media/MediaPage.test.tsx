// MediaPage.test.tsx — the page's own logic: what each role shows, the live
// audio picture that turns a dead stream into something visible, the live
// machine picker, and the test tone that says whether either half works.
//
// The bridge and the app context are faked at their boundaries (api(),
// useApp) — the same seams the real ones enter through — so the page is
// exercised as the user's machine would drive it.

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AudioState, ConnectedClient } from "@/lib/bridge";

const mocks = vi.hoisted(() => {
  const state = {
    mode: "server" as "server" | "client",
    // Widened to AudioState: a `satisfies` would freeze the literal types
    // and reject the per-test assignment of a differently-shaped state.
    audio: {
      send: true,
      receive: false,
      sending: true,
      receiving: false,
      peer: "hp",
      capturePlaying: true,
      captureLevelDb: -18.2,
      receivePlaying: false,
      receiveLevelDb: null,
      captureNote: "",
      error: "",
      active: true,
    } as AudioState,
    clients: [] as ConnectedClient[],
  };
  return {
    state,
    api: {
      LoadMediaAudio: vi.fn(),
      LoadConfig: vi.fn(),
      LoadClientAudio: vi.fn(),
      SaveMediaAudio: vi.fn(),
      SaveClientAudio: vi.fn(),
      ListAudioDevices: vi.fn(),
      TestAudio: vi.fn(),
    },
  };
});

vi.mock("@/app/AppProvider", () => ({
  useApp: () => ({ mode: mocks.state.mode, audio: mocks.state.audio, clients: mocks.state.clients }),
}));

vi.mock("@/lib/bridge", () => ({
  api: () => mocks.api,
}));

import MediaPage from "./MediaPage";

const media = {
  routeMediaKeys: true,
  target: "follow_focus",
  fallbackLocal: true,
};
const audio = {
  send: true,
  receive: false,
  captureDevice: "",
  playbackDevice: "",
  activityFloorDb: -50,
};

function inactiveAudio(over: Partial<AudioState> = {}): AudioState {
  return {
    send: false,
    receive: false,
    sending: false,
    receiving: false,
    peer: "",
    capturePlaying: false,
    captureLevelDb: null,
    receivePlaying: false,
    receiveLevelDb: null,
    captureNote: "",
    error: "",
    active: false,
    ...over,
  };
}

beforeEach(() => {
  mocks.state.mode = "server";
  mocks.state.clients = [];
  mocks.state.audio = inactiveAudio({
    send: true,
    sending: true,
    active: true,
    peer: "hp",
    capturePlaying: true,
    captureLevelDb: -18.2,
  });
  mocks.api.LoadMediaAudio.mockResolvedValue({ media, audio });
  mocks.api.LoadConfig.mockResolvedValue({
    port: 24800,
    screens: [{ name: "pc", width: 1920, height: 1080, x: 0, y: 0 }],
    network: { allowlist: false, localOnly: false, trustedIds: [], revokedIds: [] },
  });
  mocks.api.LoadClientAudio.mockResolvedValue(audio);
  mocks.api.SaveMediaAudio.mockResolvedValue(undefined);
  mocks.api.SaveClientAudio.mockResolvedValue(undefined);
  mocks.api.ListAudioDevices.mockResolvedValue({
    capture: ["sink.monitor"],
    playback: ["sink"],
  });
  mocks.api.TestAudio.mockResolvedValue({
    played: true,
    device: "",
    error: "",
    sending: true,
    receiving: false,
    captureHeard: true,
    peakDb: -14.5,
    hasPeak: true,
  });
});

describe("MediaPage", () => {
  it("server role: media routing plus the audio pickers", async () => {
    render(<MediaPage />);
    expect(await screen.findByText("Media & audio")).toBeInTheDocument();
    // The server-only media routing section, and the capture picker that
    // appears with the `send` direction already on in the fixture.
    expect(screen.getByText("Route media keys through kvmshare")).toBeInTheDocument();
    expect(await screen.findByLabelText(/Capture from/)).toBeInTheDocument();
  });

  it("shows what is actually being captured, not just that a switch is on", async () => {
    render(<MediaPage />);
    expect(await screen.findByText("Being captured here")).toBeInTheDocument();
    expect(screen.getByText(/Streaming the system default output to hp\./)).toBeInTheDocument();
    expect(screen.getByText(/playing · -18\.2 dB/)).toBeInTheDocument();
  });

  // A muted output and a working one both produce silence; only the note
  // tells them apart, and it is the only thing that says what to change.
  it("says why a healthy-looking stream is silent", async () => {
    mocks.state.audio = inactiveAudio({
      send: true,
      sending: true,
      active: true,
      peer: "hp",
      captureLevelDb: -100,
      captureNote: "alsa_output.hdmi is muted, so nothing it plays can be captured",
    });
    render(<MediaPage />);
    expect(await screen.findByText(/is muted, so nothing it plays can be captured/)).toBeInTheDocument();
  });

  it("says a live link is quiet rather than showing a stream with no sound", async () => {
    mocks.state.audio = inactiveAudio({
      send: true,
      sending: true,
      active: true,
      peer: "hp",
      capturePlaying: false,
      captureLevelDb: -100,
    });
    render(<MediaPage />);
    expect(await screen.findByText("Being captured here")).toBeInTheDocument();
    expect(screen.getByText("silent")).toBeInTheDocument();
    expect(screen.queryByText(/playing · /)).toBeNull();
  });

  it("shows the failure reason instead of a silent stream", async () => {
    mocks.state.audio = inactiveAudio({
      send: true,
      sending: true,
      active: true,
      error: "capture stopped: device gone",
    });
    render(<MediaPage />);
    expect(
      await screen.findByText(/Audio problem: capture stopped: device gone/),
    ).toBeInTheDocument();
  });

  it("says nothing is running when audio is not set up", async () => {
    mocks.state.audio = inactiveAudio();
    render(<MediaPage />);
    await screen.findByText("Media & audio");
    expect(screen.getByText(/No audio is running\./)).toBeInTheDocument();
    expect(screen.queryByText("Being captured here")).toBeNull();
    expect(screen.queryByText(/Audio problem/)).toBeNull();
  });

  // A client has no media routing to configure — that is a server
  // concern — so its page is only its own sound sharing.
  it("client role: sound sharing only, no media routing", async () => {
    mocks.state.mode = "client";
    render(<MediaPage />);
    expect(await screen.findByText("Audio")).toBeInTheDocument();
    expect(screen.getByText("Audio sharing")).toBeInTheDocument();
    expect(screen.queryByText("Route media keys through kvmshare")).toBeNull();
    expect(screen.queryByText("Which machine shares audio")).toBeNull();
  });

  // There is no Save button: an edit is written by the store's debounce.
  it("saves an edit without any Save button", async () => {
    mocks.state.mode = "client";
    render(<MediaPage />);
    await screen.findByText("Audio sharing");

    await userEvent.click(screen.getByRole("button", { name: /^Off/ }));
    await waitFor(() => expect(mocks.api.SaveClientAudio).toHaveBeenCalled());
    expect(mocks.api.SaveClientAudio).toHaveBeenCalledWith({ ...audio, send: false });
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
  });

  // The peer picker offers the machines that are actually connected, and
  // picking one writes its machine id — the value the Rust rule matches.
  it("picks the audio peer from the connected machines", async () => {
    mocks.state.clients = [
      { name: "hp", id: "98980a4d9afac273", addr: "192.168.1.72:5000", sinceMs: 0 },
      { name: "laptop", id: "4b1c0f77deadbeef", addr: "192.168.1.80:5000", sinceMs: 0 },
    ];
    render(<MediaPage />);
    await screen.findByText("Which machine shares audio");
    // Automatic is the resting choice for a config with no pin.
    expect(screen.getByRole("button", { name: /Automatic/ })).toBeInTheDocument();

    // The audio peer card is the one that names the address it is
    // connected from; the media-routing section has its own card for the
    // same machine ("Always use laptop"), so the query must be specific.
    await userEvent.click(screen.getByRole("button", { name: /laptop.*Connected from 192\.168\.1\.80/ }));
    await waitFor(() => expect(mocks.api.SaveMediaAudio).toHaveBeenCalled());
    const calls = mocks.api.SaveMediaAudio.mock.calls;
    const saved = calls[calls.length - 1]?.[1];
    expect(saved.peer).toBe("4b1c0f77deadbeef");
  });

  // A media-key machine pin must be the machine *id*, not the display
  // name: the Rust router resolves `machine:<id>` against the connected
  // client's id. The page used to pin the screen name, so the key silently
  // stayed on this machine.
  it("pins a media-key machine by its machine id, not its name", async () => {
    mocks.state.clients = [
      { name: "hp", id: "98980a4d9afac273", addr: "192.168.1.72:5000", sinceMs: 0 },
    ];
    render(<MediaPage />);
    await screen.findByText("Media keys");
    // The Playback category's machine card is the first one.
    const cards = await screen.findAllByRole("button", { name: /Always use hp/ });
    await userEvent.click(cards[0]);
    await waitFor(() => expect(mocks.api.SaveMediaAudio).toHaveBeenCalled());
    const calls = mocks.api.SaveMediaAudio.mock.calls;
    const saved = calls[calls.length - 1]?.[0];
    expect(saved.target).toBe("machine:98980a4d9afac273");
  });

  // With several machines connected and nothing pinned, sending is refused
  // by the Rust side — the page must say so rather than look switched on.
  it("warns when sending has more than one machine to choose between", async () => {
    mocks.state.clients = [
      { name: "hp", id: "98980a4d9afac273", addr: "192.168.1.72:5000", sinceMs: 0 },
      { name: "laptop", id: "4b1c0f77deadbeef", addr: "192.168.1.80:5000", sinceMs: 0 },
    ];
    render(<MediaPage />);
    expect(await screen.findByText(/Nothing is being sent: 2 machines/)).toBeInTheDocument();
  });

  // The test button plays a tone through the device under test and reports
  // what the capture side saw — the page's own "does this work?" answer.
  it("runs the test tone and reports what capture heard", async () => {
    render(<MediaPage />);
    await userEvent.click(await screen.findByRole("button", { name: /Play a test tone/ }));
    await waitFor(() => expect(mocks.api.TestAudio).toHaveBeenCalledWith(""));
    expect(
      await screen.findByText(/Tone played through the system default output, and capture heard it/),
    ).toBeInTheDocument();
  });

  it("tells the user when the tone played but capture heard nothing", async () => {
    mocks.api.TestAudio.mockResolvedValue({
      played: true,
      device: "sink.monitor",
      error: "",
      sending: true,
      receiving: false,
      captureHeard: false,
      peakDb: -100,
      hasPeak: true,
    });
    render(<MediaPage />);
    await userEvent.click(await screen.findByRole("button", { name: /Play a test tone/ }));
    expect(await screen.findByText(/but capture heard nothing/)).toBeInTheDocument();
  });
});
