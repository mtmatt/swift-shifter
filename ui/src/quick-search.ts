// Pure search logic for the quick bar, kept free of DOM and Tauri calls so it
// can be unit-tested.

/** Mirrors `recent::RecentFile` in the Rust backend. */
export interface RecentFile {
  path: string;
  name: string;
  folder: string;
  modified_ms: number;
}

export interface ParsedQuery {
  /** What to match file names against. */
  text: string;
  /** Format asked for with a trailing "to <format>", lowercased, if any. */
  format: string | null;
}

/** Splits "holiday photo to jpg" into the file part and the target format. */
export function parseQuery(query: string): ParsedQuery {
  const match = /^(.*?)\s+to\s+\.?([a-z0-9]+)\s*$/i.exec(query);
  if (!match) return { text: query.trim(), format: null };
  return { text: match[1].trim(), format: match[2].toLowerCase() };
}

/**
 * How well `name` matches `text`, or null for no match. Every
 * whitespace-separated word must appear in the name; a word at the start of
 * the name or of a word inside it ("photo" in "holiday-photo.png") scores
 * higher than one in the middle of a word. Higher is better.
 */
export function matchScore(name: string, text: string): number | null {
  const haystack = name.toLowerCase();
  const words = text.toLowerCase().split(/\s+/).filter(Boolean);
  let score = 0;
  for (const word of words) {
    const at = haystack.indexOf(word);
    if (at === -1) return null;
    const atWordStart = at === 0 || /[^a-z0-9]/.test(haystack[at - 1]);
    score += atWordStart ? 2 : 1;
  }
  return score;
}

/**
 * The files to show for `text`: the newest ones when it's empty, otherwise
 * the matches, best match first and newest first among equals.
 */
export function rankFiles(files: RecentFile[], text: string, limit: number): RecentFile[] {
  const newestFirst = (a: RecentFile, b: RecentFile) => b.modified_ms - a.modified_ms;
  if (text.trim() === "") return [...files].sort(newestFirst).slice(0, limit);

  return files
    .map((file) => ({ file, score: matchScore(file.name, text) }))
    .filter((scored): scored is { file: RecentFile; score: number } => scored.score !== null)
    .sort((a, b) => b.score - a.score || newestFirst(a.file, b.file))
    .slice(0, limit)
    .map((scored) => scored.file);
}

/** Short age of a file for the result rows: "now", "5m", "3h", "2d", or a date. */
export function relativeTime(modifiedMs: number, nowMs: number): string {
  const minutes = Math.floor((nowMs - modifiedMs) / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  return new Date(modifiedMs).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
