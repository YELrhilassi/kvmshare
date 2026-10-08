// targets.test.ts — the human labels for routing targets. A wrong label
// is not cosmetic: "Always hp" vs "Cursor only" is the difference between
// a media key that follows the pointer and one that always goes home.

import { describe, expect, it } from "vitest";
import { TARGETS, pinnedMachine, sameMachine, targetLabel } from "./targets";

describe("targetLabel", () => {
  it("names a pinned machine", () => {
    expect(targetLabel("machine:hp")).toBe("Always hp");
    expect(targetLabel("machine:98980a4d")).toBe("Always 98980a4d");
  });

  it("names the built-in targets", () => {
    expect(targetLabel("follow_focus")).toBe("Cursor only");
    expect(targetLabel("local")).toBe("This machine");
    expect(targetLabel("last_active_source")).toBe("Last active source");
  });

  it("passes through an unknown target rather than hiding it", () => {
    expect(targetLabel("something-new")).toBe("something-new");
  });

  it("is undefined for no value and for an empty pin", () => {
    expect(targetLabel(undefined)).toBeUndefined();
    expect(targetLabel("")).toBeUndefined();
    expect(targetLabel("machine:")).toBeUndefined();
  });

  it("offers every built-in target the config accepts", () => {
    const values = TARGETS.map((t) => t.value).sort();
    expect(values).toEqual(
      ["focus_or_last_active", "follow_focus", "last_active_source", "local"].sort(),
    );
  });
});

describe("pinnedMachine", () => {
  it("extracts the id from a machine pin", () => {
    expect(pinnedMachine("machine:98980a4d9afac273")).toBe("98980a4d9afac273");
  });

  it("is empty for the policies that name no machine", () => {
    for (const t of TARGETS) expect(pinnedMachine(t.value)).toBe("");
    expect(pinnedMachine(undefined)).toBe("");
    expect(pinnedMachine("")).toBe("");
    expect(pinnedMachine("machine:")).toBe("");
  });
});

describe("sameMachine", () => {
  // The Rust router matches an id by prefix in either direction; the card
  // that looks selected must agree with what the router will do.
  it("matches an exact id and either-direction prefixes", () => {
    expect(sameMachine("98980a4d9afac273", "98980a4d9afac273")).toBe(true);
    expect(sameMachine("98980a4d", "98980a4d9afac273")).toBe(true);
    expect(sameMachine("98980a4d9afac273", "98980a4d")).toBe(true);
  });

  it("does not match different machines or an empty side", () => {
    expect(sameMachine("98980a4d", "4b1c0f77")).toBe(false);
    expect(sameMachine("", "98980a4d")).toBe(false);
    expect(sameMachine("98980a4d", "")).toBe(false);
  });
});
