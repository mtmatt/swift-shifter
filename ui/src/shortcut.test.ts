import { describe, expect, it } from "vitest";
import { formatShortcut, recordKeyPress, type KeyPress } from "./shortcut";

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

describe("formatShortcut", () => {
  it("uses modifier symbols on macOS", () => {
    expect(formatShortcut("Control+Shift+Space", true)).toBe("⌃⇧Space");
    expect(formatShortcut("Alt+Super+KeyK", true)).toBe("⌥⌘K");
  });

  it("spells modifiers out elsewhere, with a platform name for Super", () => {
    expect(formatShortcut("Control+Shift+Space", false)).toBe("Ctrl+Shift+Space");
    expect(formatShortcut("Super+Digit1", false, "Win")).toBe("Win+1");
  });

  it("accepts any case and spacing, like the Rust parser", () => {
    expect(formatShortcut("control + shift + space", true)).toBe("⌃⇧Space");
    expect(formatShortcut("Ctrl+Shift+keyk", false)).toBe("Ctrl+Shift+K");
    expect(formatShortcut("cmd+Option+Digit2", true)).toBe("⌘⌥2");
  });

  it("shows punctuation and arrows as symbols", () => {
    expect(formatShortcut("Control+Slash", false)).toBe("Ctrl+/");
    expect(formatShortcut("Alt+ArrowUp", true)).toBe("⌥↑");
  });
});
