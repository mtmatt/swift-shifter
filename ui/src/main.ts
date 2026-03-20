import "./style.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

interface ProgressPayload {
  path: string;
  percent: number;
}

interface FileEntry {
  path: string;
  name: string;
  formats: string[];
  el: HTMLElement;
}

interface BatchItem {
  path: string;
  targetFormat: string;
}

interface BatchResult {
  path: string;
  output_path: string | null;
  error: string | null;
}

interface Config {
  output_dir: string | null;
  jpeg_quality: number;
  avif_quality: number;
  max_concurrent: number;
}

const files = new Map<string, FileEntry>();

const dropZone = document.getElementById("drop-zone") as HTMLDivElement;
const fileList = document.getElementById("file-list") as HTMLDivElement;
const settingsPanel = document.getElementById("settings-panel") as HTMLDivElement;
const ffmpegStatus = document.getElementById("ffmpeg-status") as HTMLSpanElement;
const btnClose = document.getElementById("btn-close") as HTMLButtonElement;
const btnSettings = document.getElementById("btn-settings") as HTMLButtonElement;

const appWindow = getCurrentWindow();

btnClose.addEventListener("click", () => {
  invoke("quit");
});

// ===== Settings panel =====

btnSettings.addEventListener("click", () => {
  if (settingsPanel.hidden) {
    openSettings();
  } else {
    closeSettings();
  }
});

document.getElementById("btn-save-config")!.addEventListener("click", saveSettings);

document.getElementById("btn-browse-output")!.addEventListener("click", async () => {
  const dir = await openDialog({ multiple: false, directory: true });
  if (!dir) return;
  const path = Array.isArray(dir) ? dir[0] : dir;
  document.getElementById("cfg-output-dir-display")!.textContent = path;
});

document.getElementById("btn-clear-output")!.addEventListener("click", () => {
  document.getElementById("cfg-output-dir-display")!.textContent = "Same as source";
});

// Live-update value displays for sliders
for (const [id, valId] of [
  ["cfg-jpeg-quality", "cfg-jpeg-quality-val"],
  ["cfg-avif-quality", "cfg-avif-quality-val"],
  ["cfg-max-concurrent", "cfg-max-concurrent-val"],
] as [string, string][]) {
  document.getElementById(id)!.addEventListener("input", (e) => {
    document.getElementById(valId)!.textContent = (e.target as HTMLInputElement).value;
  });
}

async function openSettings(): Promise<void> {
  let cfg: Config;
  try {
    cfg = await invoke<Config>("get_config");
  } catch {
    cfg = { output_dir: null, jpeg_quality: 85, avif_quality: 80, max_concurrent: 4 };
  }

  const displayEl = document.getElementById("cfg-output-dir-display")!;
  displayEl.textContent = cfg.output_dir ?? "Same as source";

  const setSlider = (id: string, valId: string, value: number) => {
    (document.getElementById(id) as HTMLInputElement).value = String(value);
    document.getElementById(valId)!.textContent = String(value);
  };
  setSlider("cfg-jpeg-quality",   "cfg-jpeg-quality-val",   cfg.jpeg_quality);
  setSlider("cfg-avif-quality",   "cfg-avif-quality-val",   cfg.avif_quality);
  setSlider("cfg-max-concurrent", "cfg-max-concurrent-val", cfg.max_concurrent);

  const statusEl = document.getElementById("settings-status")!;
  statusEl.textContent = "";
  statusEl.className = "settings-status";

  // Show settings panel, hide main content
  dropZone.hidden = true;
  fileList.hidden = true;
  settingsPanel.hidden = false;
  btnSettings.classList.add("active");
}

function closeSettings(): void {
  settingsPanel.hidden = true;
  btnSettings.classList.remove("active");
  // Restore correct content area
  if (files.size > 0) {
    fileList.hidden = false;
  } else {
    dropZone.hidden = false;
  }
}

async function saveSettings(): Promise<void> {
  const statusEl = document.getElementById("settings-status")!;
  const rawOutputDir = document.getElementById("cfg-output-dir-display")!.textContent ?? "";
  const newConfig: Config = {
    output_dir: rawOutputDir === "Same as source" ? null : rawOutputDir,
    jpeg_quality: Number((document.getElementById("cfg-jpeg-quality") as HTMLInputElement).value),
    avif_quality: Number((document.getElementById("cfg-avif-quality") as HTMLInputElement).value),
    max_concurrent: Number((document.getElementById("cfg-max-concurrent") as HTMLInputElement).value),
  };

  try {
    await invoke("set_config", { newConfig });
    statusEl.textContent = "Saved";
    statusEl.className = "settings-status ok";
    setTimeout(() => closeSettings(), 800);
  } catch (err) {
    statusEl.textContent = String(err);
    statusEl.className = "settings-status error";
  }
}

// startDragging() is the reliable way to move a decoration-less window on macOS
const titlebar = document.getElementById("titlebar") as HTMLDivElement;
titlebar.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  if ((e.target as HTMLElement).closest(".titlebar-actions")) return;
  appWindow.startDragging();
});

// Use Tauri's native drag-drop event — HTML5 drag events don't receive OS file paths
appWindow.onDragDropEvent((event) => {
  const { type } = event.payload;
  if (type === "over" || type === "enter") {
    dropZone.classList.add("drag-over");
  } else if (type === "drop") {
    dropZone.classList.remove("drag-over");
    const paths = (event.payload as { type: string; paths: string[] }).paths ?? [];
    for (const path of paths) {
      addFile(path);
    }
  } else {
    dropZone.classList.remove("drag-over");
  }
}).catch(console.error);

// Click-to-browse (Tauri dialog)
dropZone.addEventListener("click", async () => {
  const selected = await openDialog({ multiple: true, directory: false });
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  for (const path of paths) {
    await addFile(path);
  }
});

async function addFile(path: string): Promise<void> {
  if (files.has(path)) return; // deduplicate

  let formats: string[];
  try {
    formats = await invoke<string[]>("detect_format", { path });
  } catch (err) {
    showInlineError(path, String(err));
    return;
  }

  const name = path.split("/").pop() ?? path;
  const el = buildFileItem(path, name, formats);
  files.set(path, { path, name, formats, el });

  fileList.appendChild(el);
  setFileListVisible(true);
  updateBatchToolbar();
}

function removeFile(path: string): void {
  const entry = files.get(path);
  if (!entry) return;
  entry.el.remove();
  files.delete(path);
  if (files.size === 0) setFileListVisible(false);
  updateBatchToolbar();
}

function setFileListVisible(visible: boolean): void {
  if (visible) {
    dropZone.hidden = true;
    fileList.hidden = false;
  } else {
    dropZone.hidden = false;
    fileList.hidden = true;
  }
}

function buildFileItem(
  path: string,
  name: string,
  formats: string[]
): HTMLElement {
  const item = document.createElement("div");
  item.className = "file-item";

  // Top row: filename + remove button
  const row = document.createElement("div");
  row.className = "file-row";

  const nameEl = document.createElement("span");
  nameEl.className = "file-name";
  nameEl.textContent = name;
  nameEl.title = path;

  const removeBtn = document.createElement("button");
  removeBtn.className = "file-remove";
  removeBtn.textContent = "×";
  removeBtn.title = "Remove";
  removeBtn.addEventListener("click", () => removeFile(path));

  row.appendChild(nameEl);
  row.appendChild(removeBtn);
  item.appendChild(row);

  // Format buttons
  const btnRow = document.createElement("div");
  btnRow.className = "format-buttons";

  for (const fmt of formats) {
    const btn = document.createElement("button");
    btn.className = "fmt-btn";
    btn.textContent = fmt;
    btn.addEventListener("click", () => startConversion(path, fmt, item));
    btnRow.appendChild(btn);
  }

  item.appendChild(btnRow);
  return item;
}

async function startConversion(
  path: string,
  targetFormat: string,
  item: HTMLElement
): Promise<void> {
  // Disable all format buttons
  item.querySelectorAll<HTMLButtonElement>(".fmt-btn").forEach((b) => {
    b.disabled = true;
  });

  // Show / reset progress
  let progressRow = item.querySelector<HTMLElement>(".progress-row");
  let progressFill: HTMLElement;
  let progressLabel: HTMLElement;
  let statusEl: HTMLElement | null = item.querySelector(".file-status");

  if (statusEl) statusEl.remove();

  if (!progressRow) {
    progressRow = document.createElement("div");
    progressRow.className = "progress-row";

    const bar = document.createElement("div");
    bar.className = "progress-bar";
    progressFill = document.createElement("div");
    progressFill.className = "progress-fill";
    bar.appendChild(progressFill);

    progressLabel = document.createElement("span");
    progressLabel.className = "progress-label";
    progressLabel.textContent = "0%";

    progressRow.appendChild(bar);
    progressRow.appendChild(progressLabel);
    item.appendChild(progressRow);
  } else {
    progressFill = progressRow.querySelector(".progress-fill") as HTMLElement;
    progressLabel = progressRow.querySelector(
      ".progress-label"
    ) as HTMLElement;
  }

  progressFill.style.width = "0%";
  progressLabel.textContent = "0%";

  // Listen for progress events
  const unlisten = await listen<ProgressPayload>("convert:progress", (ev) => {
    if (ev.payload.path !== path) return;
    const pct = Math.round(ev.payload.percent);
    progressFill.style.width = `${pct}%`;
    progressLabel.textContent = `${pct}%`;
  });

  try {
    const outputPath = await invoke<string>("convert", {
      path,
      targetFormat,
    });

    // 100% done
    progressFill.style.width = "100%";
    progressLabel.textContent = "100%";

    // Replace progress with success link
    setTimeout(() => {
      progressRow!.remove();
      const status = document.createElement("span");
      status.className = "file-status success";
      status.textContent = `✓ Saved as ${outputPath.split("/").pop()}`;
      status.title = "Click to reveal in Finder";
      status.addEventListener("click", () => {
        invoke("open_output_folder", { path: outputPath }).catch(console.error);
      });
      item.appendChild(status);

      // Re-enable format buttons
      item.querySelectorAll<HTMLButtonElement>(".fmt-btn").forEach((b) => {
        b.disabled = false;
      });
    }, 600);
  } catch (err) {
    progressRow.remove();
    const status = document.createElement("span");
    status.className = "file-status error";
    status.textContent = `✗ ${String(err)}`;
    item.appendChild(status);

    // Re-enable format buttons
    item.querySelectorAll<HTMLButtonElement>(".fmt-btn").forEach((b) => {
      b.disabled = false;
    });
  } finally {
    unlisten();
  }
}

function updateBatchToolbar(): void {
  const existing = fileList.querySelector<HTMLElement>(".batch-toolbar");
  if (existing) existing.remove();

  if (files.size < 2) return;

  // Compute the intersection of all files' supported formats
  const allFormats = Array.from(files.values()).map((e) => new Set(e.formats));
  const intersection = allFormats.reduce(
    (acc, set) => new Set([...acc].filter((f) => set.has(f)))
  );
  if (intersection.size === 0) return;

  const toolbar = document.createElement("div");
  toolbar.className = "batch-toolbar";

  const label = document.createElement("span");
  label.className = "batch-label";
  label.textContent = "All →";
  toolbar.appendChild(label);

  for (const fmt of intersection) {
    const btn = document.createElement("button");
    btn.className = "fmt-btn";
    btn.textContent = fmt;
    btn.addEventListener("click", () => startBatchConversion(fmt));
    toolbar.appendChild(btn);
  }

  fileList.insertBefore(toolbar, fileList.firstChild);
}

async function startBatchConversion(targetFormat: string): Promise<void> {
  const entries = Array.from(files.values()).filter((e) =>
    e.formats.includes(targetFormat)
  );
  if (entries.length === 0) return;

  // Disable the batch toolbar during the operation
  const toolbar = fileList.querySelector<HTMLElement>(".batch-toolbar");
  toolbar
    ?.querySelectorAll<HTMLButtonElement>("button")
    .forEach((b) => (b.disabled = true));

  // Set up progress UI and listeners for each file
  const unlisteners: Array<() => void> = [];

  for (const entry of entries) {
    const item = entry.el;
    item
      .querySelectorAll<HTMLButtonElement>(".fmt-btn")
      .forEach((b) => (b.disabled = true));

    item.querySelector(".file-status")?.remove();

    let progressRow = item.querySelector<HTMLElement>(".progress-row");
    let progressFill: HTMLElement;
    let progressLabel: HTMLElement;

    if (!progressRow) {
      progressRow = document.createElement("div");
      progressRow.className = "progress-row";
      const bar = document.createElement("div");
      bar.className = "progress-bar";
      progressFill = document.createElement("div");
      progressFill.className = "progress-fill";
      bar.appendChild(progressFill);
      progressLabel = document.createElement("span");
      progressLabel.className = "progress-label";
      progressLabel.textContent = "0%";
      progressRow.appendChild(bar);
      progressRow.appendChild(progressLabel);
      item.appendChild(progressRow);
    } else {
      progressFill = progressRow.querySelector(".progress-fill") as HTMLElement;
      progressLabel = progressRow.querySelector(
        ".progress-label"
      ) as HTMLElement;
      progressFill.style.width = "0%";
      progressLabel.textContent = "0%";
    }

    const filePath = entry.path;
    const fill = progressFill;
    const lbl = progressLabel;
    const unlisten = await listen<ProgressPayload>("convert:progress", (ev) => {
      if (ev.payload.path !== filePath) return;
      const pct = Math.round(ev.payload.percent);
      fill.style.width = `${pct}%`;
      lbl.textContent = `${pct}%`;
    });
    unlisteners.push(unlisten);
  }

  try {
    const batchItems: BatchItem[] = entries.map((e) => ({
      path: e.path,
      targetFormat,
    }));
    const results = await invoke<BatchResult[]>("convert_batch", { items: batchItems });

    for (const result of results) {
      const entry = files.get(result.path);
      if (!entry) continue;
      const item = entry.el;
      const progressRow = item.querySelector<HTMLElement>(".progress-row");

      if (result.output_path) {
        const fill = progressRow?.querySelector<HTMLElement>(".progress-fill");
        const lbl = progressRow?.querySelector<HTMLElement>(".progress-label");
        if (fill) fill.style.width = "100%";
        if (lbl) lbl.textContent = "100%";

        setTimeout(() => {
          progressRow?.remove();
          const status = document.createElement("span");
          status.className = "file-status success";
          status.textContent = `✓ Saved as ${result.output_path!.split("/").pop()}`;
          status.title = "Click to reveal in Finder";
          status.addEventListener("click", () => {
            invoke("open_output_folder", { path: result.output_path }).catch(
              console.error
            );
          });
          item.appendChild(status);
          item
            .querySelectorAll<HTMLButtonElement>(".fmt-btn")
            .forEach((b) => (b.disabled = false));
        }, 600);
      } else {
        progressRow?.remove();
        const status = document.createElement("span");
        status.className = "file-status error";
        status.textContent = `✗ ${result.error ?? "Unknown error"}`;
        item.appendChild(status);
        item
          .querySelectorAll<HTMLButtonElement>(".fmt-btn")
          .forEach((b) => (b.disabled = false));
      }
    }
  } finally {
    unlisteners.forEach((u) => u());
    toolbar
      ?.querySelectorAll<HTMLButtonElement>("button")
      .forEach((b) => (b.disabled = false));
  }
}

function showInlineError(path: string, message: string): void {
  // Add a transient error item to the list
  const name = path.split("/").pop() ?? path;
  const item = document.createElement("div");
  item.className = "file-item";

  const row = document.createElement("div");
  row.className = "file-row";

  const nameEl = document.createElement("span");
  nameEl.className = "file-name";
  nameEl.textContent = name;
  nameEl.title = path;

  const removeBtn = document.createElement("button");
  removeBtn.className = "file-remove";
  removeBtn.textContent = "×";
  removeBtn.addEventListener("click", () => {
    item.remove();
    if (fileList.children.length === 0) setFileListVisible(false);
  });

  row.appendChild(nameEl);
  row.appendChild(removeBtn);

  const status = document.createElement("span");
  status.className = "file-status error";
  status.textContent = `✗ ${message}`;

  item.appendChild(row);
  item.appendChild(status);

  fileList.appendChild(item);
  setFileListVisible(true);
}

listen("ffmpeg:missing", () => {
  ffmpegStatus.className = "warning";
  ffmpegStatus.textContent = "Installing ffmpeg…";
}).catch(console.error);

listen("ffmpeg:installed", () => {
  ffmpegStatus.className = "";
  ffmpegStatus.textContent = "ffmpeg installed ✓";
  setTimeout(() => {
    ffmpegStatus.textContent = "";
  }, 4000);
}).catch(console.error);
