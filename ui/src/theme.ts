// Which theme a window shows for the appearance setting (`appearance` in
// the Rust `Config`), kept free of Tauri so it can be tested.

export type Theme = "light" | "dark";

/** The theme for `appearance`: light or dark as chosen, else the system's
 *  (`systemLight`), for "system" and for anything this version doesn't
 *  know. */
export function effectiveTheme(appearance: unknown, systemLight: boolean): Theme {
  if (appearance === "light" || appearance === "dark") return appearance;
  return systemLight ? "light" : "dark";
}
