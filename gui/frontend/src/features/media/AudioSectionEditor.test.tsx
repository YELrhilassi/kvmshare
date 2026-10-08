// AudioSectionEditor.test.tsx — the single-direction chooser and the
// device pickers. Two things are pinned here because getting them wrong is
// silent: the chooser must never leave both directions on (the whole point
// of replacing the two toggles), and a configured device that disappears
// from the list must stay selectable (otherwise opening the page would
// reset it to the default).

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { AudioSection } from "@/lib/bridge";
import { AudioSectionEditor } from "./AudioSectionEditor";

const devices = { capture: ["sinkA.monitor", "sinkB.monitor"], playback: ["sinkA", "sinkB"] };

function base(over: Partial<AudioSection> = {}): AudioSection {
  return {
    send: false,
    receive: false,
    captureDevice: "",
    playbackDevice: "",
    activityFloorDb: -50,
    ...over,
  };
}

function renderEditor(audio: AudioSection, devicesError: string | null = null) {
  const onEdit = vi.fn();
  render(
    <AudioSectionEditor
      isServer
      audio={audio}
      devices={devices}
      devicesError={devicesError}
      onEdit={onEdit}
    />,
  );
  return { onEdit };
}

describe("AudioSectionEditor", () => {
  it("shows no device picker when sharing is off", () => {
    renderEditor(base());
    expect(screen.queryByLabelText(/Capture from/)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/Play to/)).not.toBeInTheDocument();
  });

  it("choosing Send turns send on and receive off", async () => {
    const { onEdit } = renderEditor(base());
    await userEvent.click(screen.getByRole("button", { name: /Send this machine's sound/ }));
    expect(onEdit).toHaveBeenCalledWith(base({ send: true, receive: false }));
  });

  // The regression the chooser exists for: picking the other direction
  // must turn the first one OFF, not add to it.
  it("choosing Receive turns receive on and send off", async () => {
    const { onEdit } = renderEditor(base({ send: true }));
    await userEvent.click(screen.getByRole("button", { name: /Play the other machine's sound here/ }));
    expect(onEdit).toHaveBeenCalledWith(base({ send: false, receive: true }));
  });

  it("choosing Off turns both directions off", async () => {
    const { onEdit } = renderEditor(base({ receive: true }));
    await userEvent.click(screen.getByRole("button", { name: /^Off/ }));
    expect(onEdit).toHaveBeenCalledWith(base({ send: false, receive: false }));
  });

  it("offers this machine's output monitors for capture, plus the default", () => {
    renderEditor(base({ send: true }));
    expect(screen.getByLabelText(/Capture from/)).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "System default" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "sinkA.monitor" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "sinkB.monitor" })).toBeInTheDocument();
  });

  it("reports a capture-device choice through onEdit", async () => {
    const { onEdit } = renderEditor(base({ send: true }));
    await userEvent.selectOptions(screen.getByLabelText(/Capture from/), "sinkB.monitor");
    expect(onEdit).toHaveBeenCalledWith(base({ send: true, captureDevice: "sinkB.monitor" }));
  });

  it("offers the outputs for playback", async () => {
    const { onEdit } = renderEditor(base({ receive: true }));
    const select = screen.getByLabelText(/Play to/);
    expect(screen.getByRole("option", { name: "sinkA" })).toBeInTheDocument();
    await userEvent.selectOptions(select, "sinkA");
    expect(onEdit).toHaveBeenCalledWith(base({ receive: true, playbackDevice: "sinkA" }));
  });

  // A configured device that the audio server is not reporting right now
  // must stay selectable — otherwise opening the page and saving would
  // silently reset it to the default.
  it("keeps a configured device that is not in the current list", () => {
    renderEditor(base({ send: true, captureDevice: "legacy.monitor" }));
    expect(screen.getByRole("option", { name: "legacy.monitor" })).toBeInTheDocument();
  });

  it("explains an empty device list without blocking the default", () => {
    renderEditor(base({ send: true }), "no pactl");
    expect(screen.getByText(/Could not list audio devices/)).toBeInTheDocument();
    expect(screen.getByText(/System default.*still works/)).toBeInTheDocument();
  });
});
