/** Settings' line for marker-pdf: installed, or how to install it. */
export function markerStatus(
  installed: boolean,
  installHint: string,
): { text: string; className: string } {
  if (installed) return { text: "installed", className: "hint hint--ok" };
  const text = ["not installed · ~2 GB download.", installHint].filter(Boolean).join(" ");
  return { text, className: "hint hint--warn" };
}
