import "./settings.css";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";

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
};

let markerInstalled = false;
let ollamaConnected = false;

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
  } catch {}

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

document.getElementById("btn-cancel")!.addEventListener("click", () => win.close());

document.getElementById("btn-save")!.addEventListener("click", async () => {
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
  };
  try {
    await invoke("set_config", { newConfig });
    win.close();
  } catch (err) {
    statusEl.textContent = String(err);
  }
});
