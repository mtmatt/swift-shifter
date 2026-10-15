// Converting key presses to global-shortcut accelerators and back for display.
// Accelerators use the names the Rust side parses (`hotkey::parse_shortcut`):
// modifiers "Control", "Alt", "Shift", "Super", then a KeyboardEvent.code
// such as "KeyK", "Digit1", "Space" or "F5".

/** The parts of a KeyboardEvent the recorder needs. */
export interface KeyPress {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

export type RecordResult =
  | { kind: "accelerator"; accelerator: string }
  /** Only modifiers held so far: keep listening. */
  | { kind: "incomplete" }
  /**
   * No Control, Alt or Super held. Shift alone doesn't count: a global
   * Shift+A would swallow every capital A (mirrors `hotkey::parse_shortcut`).
   */
  | { kind: "needs-modifier" };

const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "ShiftLeft",
  "ShiftRight",
  "MetaLeft",
  "MetaRight",
  "OSLeft",
  "OSRight",
]);

/** Accelerator for a key press while recording a new shortcut. */
export function recordKeyPress(press: KeyPress): RecordResult {
  if (MODIFIER_CODES.has(press.code)) return { kind: "incomplete" };
  const modifiers = [
    press.ctrlKey && "Control",
    press.altKey && "Alt",
    press.shiftKey && "Shift",
    press.metaKey && "Super",
  ].filter((m): m is string => Boolean(m));
  if (!press.ctrlKey && !press.altKey && !press.metaKey) return { kind: "needs-modifier" };
  return { kind: "accelerator", accelerator: [...modifiers, press.code].join("+") };
}

// Keyed by lowercase name: like the Rust parser, display accepts any case and
// the aliases a hand-edited config.toml might use.
const MODIFIER_KINDS: Record<string, "ctrl" | "alt" | "shift" | "super"> = {
  control: "ctrl",
  ctrl: "ctrl",
  alt: "alt",
  option: "alt",
  shift: "shift",
  super: "super",
  command: "super",
  cmd: "super",
};

const MAC_SYMBOLS = { ctrl: "⌃", alt: "⌥", shift: "⇧", super: "⌘" };
const OTHER_NAMES = { ctrl: "Ctrl", alt: "Alt", shift: "Shift", super: "Super" };

const KEY_NAMES: Record<string, string> = {
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

function keyLabel(code: string): string {
  if (/^Key[A-Z]$/i.test(code)) return code.slice(3).toUpperCase();
  if (/^Digit\d$/i.test(code)) return code.slice(5);
  return KEY_NAMES[code] ?? code.charAt(0).toUpperCase() + code.slice(1);
}

/**
 * Human-readable form: "⌃⇧Space" on macOS, "Ctrl+Shift+Space" elsewhere.
 * `superName` is what the Super key is called off macOS ("Win" on Windows).
 */
export function formatShortcut(accelerator: string, isMac: boolean, superName = "Super"): string {
  const parts = accelerator.split("+").map((part) => part.trim());
  const key = keyLabel(parts.pop() ?? "");
  const kinds = parts.map((m) => MODIFIER_KINDS[m.toLowerCase()]);
  if (isMac) return kinds.map((kind, i) => (kind ? MAC_SYMBOLS[kind] : parts[i])).join("") + key;
  const modifiers = kinds.map((kind, i) => {
    if (!kind) return parts[i];
    return kind === "super" ? superName : OTHER_NAMES[kind];
  });
  return [...modifiers, key].join("+");
}
