import "./settings.css";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { markerStatus } from "./marker";
import { folderLabel } from "./paths";
import { recordKeyPress, shortcutKeyNames } from "./shortcut";
import { followAppearance } from "./appearance";

followAppearance();

// Tab switching (ARIA tabs: only the selected tab is in the Tab order;
// arrow keys, Home and End move between tabs)
const tabs = Array.from(document.querySelectorAll<HTMLButtonElement>(".tab"));
const panels = document.querySelectorAll<HTMLElement>(".panel");
function selectTab(tab: HTMLButtonElement): void {
  for (const t of tabs) {
    const selected = t === tab;
    t.classList.toggle("active", selected);
    t.setAttribute("aria-selected", String(selected));
    t.tabIndex = selected ? 0 : -1;
  }
  panels.forEach((p) => p.classList.remove("active"));
  document.getElementById(`panel-${tab.dataset.panel}`)!.classList.add("active");
}
tabs.forEach((tab, i) => {
  tab.addEventListener("click", () => selectTab(tab));
  tab.addEventListener("keydown", (event) => {
    const next =
      event.key === "ArrowRight" ? tabs[(i + 1) % tabs.length]
      : event.key === "ArrowLeft" ? tabs[(i - 1 + tabs.length) % tabs.length]
      : event.key === "Home" ? tabs[0]
      : event.key === "End" ? tabs[tabs.length - 1]
      : null;
    if (!next) return;
    event.preventDefault();
    selectTab(next);
    next.focus();
  });
});

interface Config {
  output_dir: string | null;
  jpeg_quality: number;
  avif_quality: number;
  max_concurrent: number;
  use_marker_pdf: boolean;
  clipboard_output_mode: string;
  quick_bar_shortcut: string;
  recent_folders: string[];
  appearance: string;
}

const win = getCurrentWindow();
const jpegSlider = document.getElementById("jpeg-q") as HTMLInputElement;
const jpegVal = document.getElementById("jpeg-q-val") as HTMLElement;
const avifSlider = document.getElementById("avif-q") as HTMLInputElement;
const avifVal = document.getElementById("avif-q-val") as HTMLElement;
const maxCSlider = document.getElementById("max-c") as HTMLInputElement;
const maxCVal = document.getElementById("max-c-val") as HTMLElement;

const markerToggle = document.getElementById("use-marker") as HTMLInputElement;
const markerHint = document.getElementById("marker-hint") as HTMLElement;
const markerStatusEl = document.getElementById("marker-status") as HTMLElement;

const statusEl = document.getElementById("status") as HTMLElement;
const clipboardModeSelect = document.getElementById("clipboard-mode") as HTMLSelectElement;
const appearanceSelect = document.getElementById("appearance") as HTMLSelectElement;
const saveBtn = document.getElementById("btn-save") as HTMLButtonElement;

const shortcutBtn = document.getElementById("btn-shortcut") as HTMLButtonElement;
const shortcutHint = document.getElementById("shortcut-hint") as HTMLElement;
const folderList = document.getElementById("folder-list") as HTMLUListElement;
const folderEmpty = document.getElementById("folder-empty") as HTMLElement;
const addFolderBtn = document.getElementById("btn-add-folder") as HTMLButtonElement;
const resetFoldersBtn = document.getElementById("btn-reset-folders") as HTMLButtonElement;
const isMac = navigator.userAgent.includes("Macintosh");
const superKeyName = navigator.userAgent.includes("Windows") ? "Win" : "Super";

let cfg: Config = {
  output_dir: null,
  jpeg_quality: 75,
  avif_quality: 65,
  max_concurrent: 4,
  use_marker_pdf: false,
  clipboard_output_mode: "clipboard",
  quick_bar_shortcut: "Control+Shift+Space",
  recent_folders: [],
  appearance: "system",
};

let markerInstalled = false;
/** How to install marker-pdf on this platform, from the backend. */
let markerInstallHint = "";

/** Quick-bar shortcut that Save will apply; recorded but not yet registered. */
let quickBarShortcut = cfg.quick_bar_shortcut;
/** Folders the quick bar scans, as Save will apply them. */
let recentFolders: string[] = [...cfg.recent_folders];
/** Whether the shortcut button is waiting for a key press. */
let recording = false;
/** Counts recorder key presses, to drop validation results that arrive late. */
let latestPressId = 0;

// Show/hide the hint row based on toggle state and install status.
// The hint is invisible when the toggle is off — no ambient noise.
function updateMarkerHint() {
  if (!markerToggle.checked) {
    markerHint.hidden = true;
    return;
  }
  markerHint.hidden = false;
  const { text, className } = markerStatus(markerInstalled, markerInstallHint);
  markerStatusEl.textContent = text;
  markerStatusEl.className = className;
}

(async () => {
  try {
    cfg = await invoke<Config>("get_config");
  } catch (err) {
    // Saving now would overwrite the real settings with these defaults.
    statusEl.textContent = `couldn't load settings: ${err}`;
    return;
  }
  // Show the quick-bar settings before the slow check below, and only
  // then allow Save, which sends all of them.
  quickBarShortcut = cfg.quick_bar_shortcut;
  recentFolders = [...cfg.recent_folders];
  renderShortcut();
  renderFolders();
  // Disabled in the HTML so nothing edited before now gets overwritten here.
  for (const btn of [shortcutBtn, addFolderBtn, resetFoldersBtn, saveBtn]) btn.disabled = false;

  jpegSlider.value = String(cfg.jpeg_quality);
  jpegVal.textContent = String(cfg.jpeg_quality);
  avifSlider.value = String(cfg.avif_quality);
  avifVal.textContent = String(cfg.avif_quality);
  maxCSlider.value = String(cfg.max_concurrent);
  maxCVal.textContent = String(cfg.max_concurrent);
  clipboardModeSelect.value = cfg.clipboard_output_mode;
  appearanceSelect.value = cfg.appearance;
  
  markerToggle.checked = cfg.use_marker_pdf;
  markerInstalled = await invoke<boolean>("check_marker").catch(() => false);
  markerInstallHint = await invoke<string>("marker_install_hint").catch(() => "");
  updateMarkerHint();
})();

jpegSlider.addEventListener("input", () => (jpegVal.textContent = jpegSlider.value));
avifSlider.addEventListener("input", () => (avifVal.textContent = avifSlider.value));
maxCSlider.addEventListener("input", () => (maxCVal.textContent = maxCSlider.value));

markerToggle.addEventListener("change", updateMarkerHint);

// ─── Quick bar: shortcut ──────────────────────────────────────────────────

function renderShortcut(): void {
  if (recording) {
    shortcutBtn.textContent = "press keys…";
  } else {
    // One keycap per key; the "+" text between them also gives screen readers
    // "Control + Shift + Space" instead of one run-together word.
    const caps = shortcutKeyNames(quickBarShortcut, isMac, superKeyName).map((name) => {
      const cap = document.createElement("kbd");
      cap.className = "keycap";
      cap.textContent = name;
      return cap;
    });
    shortcutBtn.replaceChildren(...caps.flatMap((cap, i) => (i === 0 ? [cap] : [" + ", cap])));
  }
  shortcutBtn.classList.toggle("recording", recording);
  shortcutBtn.setAttribute("aria-pressed", String(recording));
}

function setShortcutHint(text: string, tone: "warn" | "error" = "warn"): void {
  shortcutHint.textContent = text;
  shortcutHint.className = `hint hint--${tone}`;
}

function stopRecording(): void {
  recording = false;
  renderShortcut();
}

shortcutBtn.addEventListener("click", () => {
  recording = !recording;
  // WebKit (macOS) doesn't focus a button when it is clicked. Focus it
  // explicitly so clicking elsewhere blurs it and cancels recording.
  if (recording) shortcutBtn.focus();
  renderShortcut();
  setShortcutHint(recording ? "press the new keys · esc to cancel" : "click to change");
});

// WebKit clears focus on mousedown over an element it won't focus by mouse,
// buttons included. Clicking the button to cancel recording would then blur
// it (stopping recording) and the click would start recording again.
shortcutBtn.addEventListener("mousedown", (event) => {
  if (recording) event.preventDefault();
});

shortcutBtn.addEventListener("blur", () => {
  if (!recording) return;
  stopRecording();
  setShortcutHint("click to change");
});

// Listen on the window, in the capture phase, rather than on the button: key
// presses must be caught while recording even if the button doesn't have
// focus, and before any other handler acts on them.
window.addEventListener("keydown", async (event) => {
  if (!recording) return;
  const result = recordKeyPress(event);
  // Plain Esc cancels; plain (Shift+)Tab cancels and moves focus as usual,
  // so keyboard users aren't trapped in the recorder.
  if (result.kind === "needs-modifier" && (event.key === "Escape" || event.key === "Tab")) {
    if (event.key === "Escape") event.preventDefault();
    stopRecording();
    setShortcutHint("click to change");
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  if (result.kind === "incomplete") return;
  if (result.kind === "needs-modifier") {
    const superKey = isMac ? "cmd" : superKeyName.toLowerCase();
    setShortcutHint(`add ctrl, alt or ${superKey} (shift alone isn't enough)`, "error");
    return;
  }
  // Rust decides which keys a global shortcut can use; ask before accepting.
  // Only the latest key press counts, and only while still recording: the
  // user may cancel or press another combination while this is in flight.
  const pressId = ++latestPressId;
  let error: string | null = null;
  try {
    await invoke("validate_shortcut", { accelerator: result.accelerator });
  } catch (err) {
    error = String(err);
  }
  if (!recording || pressId !== latestPressId) return;
  if (error !== null) {
    setShortcutHint(error, "error");
    return;
  }
  quickBarShortcut = result.accelerator;
  stopRecording();
  setShortcutHint("applies when you save");
}, true);

// ─── Quick bar: folders ───────────────────────────────────────────────────

function renderFolders(): void {
  folderList.replaceChildren(
    ...recentFolders.map((folder) => {
      const row = document.createElement("li");
      row.className = "row";
      const path = document.createElement("span");
      path.className = "folder-path";
      path.title = folder;
      // Name first: when space runs out, the parent path is what gets cut.
      const { name, parent } = folderLabel(folder);
      const nameEl = document.createElement("span");
      nameEl.className = "folder-name";
      nameEl.textContent = name;
      const parentEl = document.createElement("span");
      parentEl.className = "folder-parent";
      parentEl.textContent = parent;
      path.append(nameEl, parentEl);
      const remove = document.createElement("button");
      remove.className = "btn-inline";
      remove.textContent = "remove";
      remove.setAttribute("aria-label", `Remove ${folder}`);
      remove.addEventListener("click", () => {
        const index = recentFolders.indexOf(folder);
        recentFolders = recentFolders.filter((f) => f !== folder);
        renderFolders();
        // The focused button was just removed; move focus to the row that took
        // its place, or to "add folder…" once the list is empty.
        const buttons = folderList.querySelectorAll<HTMLButtonElement>("button");
        (buttons[Math.min(index, buttons.length - 1)] ?? addFolderBtn).focus();
      });
      row.append(path, remove);
      return row;
    }),
  );
  folderEmpty.hidden = recentFolders.length > 0;
}

addFolderBtn.addEventListener("click", async () => {
  let picked: string | string[] | null;
  try {
    picked = await openDialog({ directory: true, multiple: false });
  } catch (err) {
    statusEl.textContent = `couldn't open folder picker: ${err}`;
    return;
  }
  statusEl.textContent = "";
  if (typeof picked === "string" && !recentFolders.includes(picked)) {
    recentFolders = [...recentFolders, picked];
    renderFolders();
  }
});

resetFoldersBtn.addEventListener("click", async () => {
  try {
    recentFolders = await invoke<string[]>("default_recent_folders");
  } catch (err) {
    statusEl.textContent = `couldn't restore defaults: ${err}`;
    return;
  }
  statusEl.textContent = "";
  renderFolders();
});

document.getElementById("btn-cancel")!.addEventListener("click", () => win.close());

saveBtn.addEventListener("click", async () => {
  const newConfig: Config = {
    ...cfg,
    jpeg_quality: Number(jpegSlider.value),
    avif_quality: Number(avifSlider.value),
    max_concurrent: Number(maxCSlider.value),
    use_marker_pdf: markerToggle.checked,
    clipboard_output_mode: clipboardModeSelect.value,
    appearance: appearanceSelect.value,
    quick_bar_shortcut: quickBarShortcut,
    recent_folders: recentFolders,
  };
  try {
    await invoke("set_config", { newConfig });
    win.close();
  } catch (err) {
    statusEl.textContent = String(err);
  }
});
