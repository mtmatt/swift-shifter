import "./settings.css";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { folderLabel } from "./paths";
import { formatShortcut, recordKeyPress } from "./shortcut";

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
  use_local_llm: boolean;
  local_llm_model: string;
  local_llm_url: string;
  clipboard_output_mode: string;
  quick_bar_shortcut: string;
  recent_folders: string[];
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
const markerStatus = document.getElementById("marker-status") as HTMLElement;
const installBtn = document.getElementById("btn-install-marker") as HTMLButtonElement;
const markerProgressWrap = document.getElementById("marker-progress-wrap") as HTMLElement;
const markerProgressFill = document.getElementById("marker-progress-fill") as HTMLElement;

const llmToggle = document.getElementById("use-llm") as HTMLInputElement;
const llmHint = document.getElementById("llm-hint") as HTMLElement;
const llmStatus = document.getElementById("llm-status") as HTMLElement;
const llmTestBtn = document.getElementById("btn-test-ollama") as HTMLButtonElement;
const llmInstallBtn = document.getElementById("btn-install-ollama") as HTMLButtonElement;
const llmProgressWrap = document.getElementById("llm-progress-wrap") as HTMLElement;
const llmProgressFill = document.getElementById("llm-progress-fill") as HTMLElement;
const llmModelRow = document.getElementById("llm-model-row") as HTMLElement;
const llmModelInput = document.getElementById("llm-model") as HTMLInputElement;
const llmUrlRow = document.getElementById("llm-url-row") as HTMLElement;
const llmUrlInput = document.getElementById("llm-url") as HTMLInputElement;
const llmModelsList = document.getElementById("llm-models-list") as HTMLDataListElement;

const statusEl = document.getElementById("status") as HTMLElement;
const clipboardModeSelect = document.getElementById("clipboard-mode") as HTMLSelectElement;
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
  use_local_llm: false,
  local_llm_model: "gemma4:e2b",
  local_llm_url: "http://localhost:11434",
  clipboard_output_mode: "clipboard",
  quick_bar_shortcut: "Control+Shift+Space",
  recent_folders: [],
};

let markerInstalled = false;
let ollamaConnected = false;

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
  if (markerInstalled) {
    markerStatus.textContent = "installed";
    markerStatus.className = "hint hint--ok";
    installBtn.hidden = true;
  } else {
    markerStatus.textContent = "not installed · ~2 GB download";
    markerStatus.className = "hint hint--warn";
    installBtn.hidden = false;
  }
}

function updateLlmHint() {
  if (!llmToggle.checked) {
    llmHint.hidden = true;
    llmModelRow.hidden = true;
    llmUrlRow.hidden = true;
    return;
  }
  llmHint.hidden = false;
  llmModelRow.hidden = false;
  llmUrlRow.hidden = false;
  llmTestBtn.hidden = false;
  llmInstallBtn.hidden = false;
  
  if (ollamaConnected) {
    llmStatus.textContent = "ollama connected";
    llmStatus.className = "hint hint--success";
  } else {
    llmStatus.textContent = "ollama not running at this url";
    llmStatus.className = "hint hint--warn";
  }
}

async function refreshOllamaModels() {
  if (!ollamaConnected) return;
  try {
    const models = await invoke<string[]>("list_ollama_models");
    llmModelsList.innerHTML = "";
    for (const m of models) {
      const opt = document.createElement("option");
      opt.value = m;
      llmModelsList.appendChild(opt);
    }
  } catch {}
}

(async () => {
  try {
    cfg = await invoke<Config>("get_config");
  } catch (err) {
    // Saving now would overwrite the real settings with these defaults.
    statusEl.textContent = `couldn't load settings: ${err}`;
    return;
  }
  // Show the quick-bar settings before the slow checks below (Ollama can
  // take a while), and only then allow Save, which sends all of them.
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
  
  markerToggle.checked = cfg.use_marker_pdf;
  markerInstalled = await invoke<boolean>("check_marker").catch(() => false);
  updateMarkerHint();

  llmToggle.checked = cfg.use_local_llm;
  llmModelInput.value = cfg.local_llm_model;
  llmUrlInput.value = cfg.local_llm_url;
  ollamaConnected = await invoke<boolean>("check_ollama").catch(() => false);
  updateLlmHint();
  if (ollamaConnected) refreshOllamaModels();
})();

jpegSlider.addEventListener("input", () => (jpegVal.textContent = jpegSlider.value));
avifSlider.addEventListener("input", () => (avifVal.textContent = avifSlider.value));
maxCSlider.addEventListener("input", () => (maxCVal.textContent = maxCSlider.value));

markerToggle.addEventListener("change", updateMarkerHint);

llmToggle.addEventListener("change", updateLlmHint);
llmTestBtn.addEventListener("click", async () => {
  llmStatus.textContent = "testing…";
  llmStatus.className = "hint";
  ollamaConnected = await invoke<boolean>("check_ollama_url", { url: llmUrlInput.value }).catch(() => false);
  updateLlmHint();
  if (ollamaConnected) refreshOllamaModels();
});

llmInstallBtn.addEventListener("click", async () => {
  let crawlTimer: ReturnType<typeof setInterval> | null = null;
  let fillPct = 0;

  const setFill = (pct: number) => {
    fillPct = Math.min(pct, 99);
    llmProgressFill.style.width = `${fillPct}%`;
  };
  const crawlTo = (start: number, ceiling: number) => {
    if (crawlTimer) clearInterval(crawlTimer);
    setFill(start);
    crawlTimer = setInterval(() => {
      if (fillPct < ceiling - 0.5) setFill(fillPct + 0.4);
    }, 600);
  };
  const stopCrawl = () => {
    if (crawlTimer) { clearInterval(crawlTimer); crawlTimer = null; }
  };

  llmInstallBtn.disabled = true;
  llmInstallBtn.textContent = "installing / pulling…";
  llmStatus.textContent = "Starting…";
  llmStatus.className = "hint hint--installing";
  llmProgressWrap.hidden = false;
  setFill(0);

  let unlistenStep: (() => void) | undefined;
  let unlistenProgress: (() => void) | undefined;
  try {
    unlistenStep = await listen<string>("ollama:step", (e) => {
      llmStatus.textContent = e.payload;
      stopCrawl();
      if (e.payload.startsWith("Installing")) crawlTo(5, 25);
      else if (e.payload.startsWith("Starting")) crawlTo(30, 48);
      else if (e.payload.startsWith("Pulling")) crawlTo(52, 90);
    });
    unlistenProgress = await listen<number>("ollama:progress", (e) => {
      stopCrawl();
      setFill(e.payload);
    });
  } catch (err) {
    stopCrawl();
    llmProgressWrap.hidden = true;
    llmStatus.textContent = `Failed to set up listeners: ${err}`;
    llmStatus.className = "hint hint--error";
    llmInstallBtn.disabled = false;
    llmInstallBtn.textContent = "install / pull";
    return;
  }

  try {
    await invoke("install_ollama_and_model", { baseUrl: llmUrlInput.value, model: llmModelInput.value });
    stopCrawl();
    setFill(100);
    await new Promise((r) => setTimeout(r, 500));
    ollamaConnected = await invoke<boolean>("check_ollama_url", { url: llmUrlInput.value }).catch(() => false);
    updateLlmHint();
    if (ollamaConnected) refreshOllamaModels();
    llmProgressWrap.hidden = true;
  } catch (err) {
    stopCrawl();
    llmProgressWrap.hidden = true;
    llmStatus.textContent = String(err);
    llmStatus.className = "hint hint--error";
  } finally {
    unlistenProgress?.();
    unlistenStep?.();
    llmInstallBtn.disabled = false;
    llmInstallBtn.textContent = "install / pull";
  }
});

installBtn.addEventListener("click", async () => {
  // ── 1. synchronous UI update — happens instantly, confirms handler fired ──
  let crawlTimer: ReturnType<typeof setInterval> | null = null;
  let fillPct = 0;

  const setFill = (pct: number) => {
    fillPct = Math.min(pct, 99);
    markerProgressFill.style.width = `${fillPct}%`;
  };
  const crawlTo = (start: number, ceiling: number) => {
    if (crawlTimer) clearInterval(crawlTimer);
    setFill(start);
    crawlTimer = setInterval(() => {
      if (fillPct < ceiling - 0.5) setFill(fillPct + 0.4);
    }, 600);
  };
  const stopCrawl = () => {
    if (crawlTimer) { clearInterval(crawlTimer); crawlTimer = null; }
  };

  installBtn.disabled = true;
  installBtn.textContent = "installing…";
  markerStatus.textContent = "Starting…";
  markerStatus.className = "hint hint--installing";
  markerProgressWrap.hidden = false;
  setFill(0);

  // ── 2. set up async listeners — wrapped so errors surface in the UI ───────
  let unlisten: (() => void) | undefined;
  let unlistenClose: (() => void) | undefined;
  try {
    unlisten = await listen<string>("marker:step", (e) => {
      const msg = e.payload;
      markerStatus.textContent = msg;
      stopCrawl();
      if (msg.startsWith("Setting up package installer")) crawlTo(5, 25);
      else if (msg.startsWith("Setting up Python")) crawlTo(30, 48);
      else if (msg.startsWith("Downloading")) crawlTo(52, 90);
    });
    unlistenClose = await win.onCloseRequested(() => {
      markerStatus.textContent = "installing in background…";
    });
  } catch (err) {
    stopCrawl();
    markerProgressWrap.hidden = true;
    markerStatus.textContent = `Failed to set up listeners: ${err}`;
    markerStatus.className = "hint hint--error";
    installBtn.disabled = false;
    installBtn.textContent = "install";
    return;
  }

  // ── 3. invoke the backend installer ──────────────────────────────────────
  try {
    await invoke("install_marker");
    stopCrawl();
    setFill(100);
    await new Promise((r) => setTimeout(r, 500));
    markerInstalled = true;
    updateMarkerHint();
    markerProgressWrap.hidden = true;
  } catch (err) {
    stopCrawl();
    markerProgressWrap.hidden = true;
    markerStatus.textContent = String(err);
    markerStatus.className = "hint hint--error";
  } finally {
    unlistenClose?.();
    unlisten?.();
    installBtn.disabled = false;
    installBtn.textContent = "install";
  }
});

// ─── Quick bar: shortcut ──────────────────────────────────────────────────

function renderShortcut(): void {
  shortcutBtn.textContent = recording
    ? "press keys…"
    : formatShortcut(quickBarShortcut, isMac, superKeyName);
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
  renderShortcut();
  setShortcutHint(recording ? "press the new keys · esc to cancel" : "click to change");
});

shortcutBtn.addEventListener("blur", () => {
  if (!recording) return;
  stopRecording();
  setShortcutHint("click to change");
});

shortcutBtn.addEventListener("keydown", async (event) => {
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
});

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
    use_local_llm: llmToggle.checked,
    local_llm_model: llmModelInput.value.trim() || "gemma4:e2b",
    local_llm_url: llmUrlInput.value.trim() || "http://localhost:11434",
    clipboard_output_mode: clipboardModeSelect.value,
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
