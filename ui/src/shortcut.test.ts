import { describe, expect, it } from "vitest";
import { recordKeyPress, shortcutKeyNames, type KeyPress } from "./shortcut";

function press(code: string, held: Partial<KeyPress> = {}): KeyPress {
  return { code, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...held };
}

describe("recordKeyPress", () => {
  it("builds an accelerator from modifiers and the key's code", () => {
    expect(recordKeyPress(press("Space", { ctrlKey: true, shiftKey: true }))).toEqual({
      kind: "accelerator",
      accelerator: "Control+Shift+Space",
    });
  });

  it("orders modifiers consistently whatever was pressed first", () => {
    const result = recordKeyPress(press("KeyK", { metaKey: true, altKey: true }));
    expect(result).toEqual({ kind: "accelerator", accelerator: "Alt+Super+KeyK" });
  });

  it("keeps listening while only modifiers are held", () => {
    expect(recordKeyPress(press("ShiftLeft", { shiftKey: true }))).toEqual({ kind: "incomplete" });
    expect(recordKeyPress(press("MetaRight", { metaKey: true }))).toEqual({ kind: "incomplete" });
  });

  it("refuses a key without a modifier", () => {
    expect(recordKeyPress(press("KeyA"))).toEqual({ kind: "needs-modifier" });
  });

  it("refuses Shift as the only modifier", () => {
    expect(recordKeyPress(press("KeyA", { shiftKey: true }))).toEqual({ kind: "needs-modifier" });
    expect(recordKeyPress(press("Tab", { shiftKey: true }))).toEqual({ kind: "needs-modifier" });
  });
});

describe("shortcutKeyNames", () => {
  it("spells modifiers out with macOS names on macOS", () => {
    expect(shortcutKeyNames("Control+Shift+Space", true)).toEqual(["Control", "Shift", "Space"]);
    expect(shortcutKeyNames("Alt+Super+KeyK", true)).toEqual(["Option", "Command", "K"]);
  });

  it("uses PC names elsewhere, with a platform name for Super", () => {
    expect(shortcutKeyNames("Control+Shift+Space", false)).toEqual(["Ctrl", "Shift", "Space"]);
    expect(shortcutKeyNames("Super+Digit1", false, "Win")).toEqual(["Win", "1"]);
  });

  it("accepts any case, spacing and aliases, like the Rust parser", () => {
    expect(shortcutKeyNames("control + shift + space", true)).toEqual(["Control", "Shift", "Space"]);
    expect(shortcutKeyNames("Ctrl+Shift+keyk", false)).toEqual(["Ctrl", "Shift", "K"]);
    expect(shortcutKeyNames("cmd+Option+Digit2", true)).toEqual(["Command", "Option", "2"]);
  });

  it("shows punctuation and arrows as symbols", () => {
    expect(shortcutKeyNames("Control+Slash", false)).toEqual(["Ctrl", "/"]);
    expect(shortcutKeyNames("Alt+ArrowUp", true)).toEqual(["Option", "↑"]);
  });
});
