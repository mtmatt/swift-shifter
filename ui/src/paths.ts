// Path helpers for display. Paths come from the OS (folder picker, config),
// so both "/" and Windows "\" separators occur.

export interface FolderLabel {
  /** The folder's own name, e.g. "acme". */
  name: string;
  /** Everything before it, e.g. "/Users/me/Clients"; empty for a root. */
  parent: string;
}

/** Splits "/Users/me/Clients/acme/" into name "acme" and parent "/Users/me/Clients". */
export function folderLabel(path: string): FolderLabel {
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  // A root ("/", "C:\") has no name of its own: show it whole.
  if (trimmed === "" || /^[A-Za-z]:$/.test(trimmed)) return { name: path, parent: "" };
  if (cut === -1) return { name: trimmed, parent: "" };
  const parent = trimmed.slice(0, cut);
  // Keep the root's separator: "/" for "/data", "C:\" for "C:\Users".
  const rootParent = parent === "" || /^[A-Za-z]:$/.test(parent);
  return { name: trimmed.slice(cut + 1), parent: rootParent ? trimmed.slice(0, cut + 1) : parent };
}
