// Every window's colors follow the appearance setting (see `tokens.css`).
// The backend sets the native window chrome; each page sets its own
// `data-theme`, as the setting and the system say.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { effectiveTheme } from "./theme";

/** Makes this window show the appearance the config holds, and the
 *  system's while that's "system", the system switching included: now,
 *  and again each time Settings saves it. */
export function followAppearance(): void {
  const systemLight = window.matchMedia("(prefers-color-scheme: light)");
  let appearance: unknown = "system";
  const apply = () => {
    document.documentElement.dataset.theme = effectiveTheme(appearance, systemLight.matches);
  };
  const load = async () => {
    try {
      appearance = (await invoke<{ appearance?: unknown }>("get_config")).appearance;
    } catch (err) {
      console.error("get_config failed", err);
    }
    apply();
  };
  apply();
  systemLight.addEventListener("change", apply);
  void load();
  listen("config:updated", () => void load()).catch(console.error);
}
