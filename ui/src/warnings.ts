// What a conversion left out, as the backend words it: each message says
// how many and, where the reader knows it, at which line.

/** Something of the input a conversion left out of its output. */
export interface ConversionWarning {
  message: string;
  count: number;
  line: number | null;
}

/**
 * "⚠ 2 raw HTML elements kept only their text · 1 Typst loop wasn't run at
 * line 4", or null when nothing was left out.
 */
export function warningLine(warnings: ConversionWarning[]): string | null {
  if (warnings.length === 0) return null;
  return `⚠ ${warnings.map((w) => w.message).join(" · ")}`;
}

/** Every message, one a line: a tooltip showing what a short line cuts off. */
export function warningTooltip(warnings: ConversionWarning[]): string {
  return warnings.map((w) => w.message).join("\n");
}
