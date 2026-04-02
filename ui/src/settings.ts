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
const statusEl = document.getElementById("status") as HTMLElement;

let cfg: Config = {
  output_dir: null,
  jpeg_quality: 75,
  avif_quality: 65,
  max_concurrent: 4,
  use_marker_pdf: false,
};

let markerInstalled = false;

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
  markerToggle.checked = cfg.use_marker_pdf;

  markerInstalled = await invoke<boolean>("check_marker").catch(() => false);
  updateMarkerHint();
})();

jpegSlider.addEventListener("input", () => (jpegVal.textContent = jpegSlider.value));
avifSlider.addEventListener("input", () => (avifVal.textContent = avifSlider.value));
maxCSlider.addEventListener("input", () => (maxCVal.textContent = maxCSlider.value));
markerToggle.addEventListener("change", updateMarkerHint);

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
  };
  try {
    await invoke("set_config", { newConfig });
    win.close();
  } catch (err) {
    statusEl.textContent = String(err);
  }
});
