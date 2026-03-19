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

const files = new Map<string, FileEntry>();

const dropZone = document.getElementById("drop-zone") as HTMLDivElement;
const fileList = document.getElementById("file-list") as HTMLDivElement;
const ffmpegStatus = document.getElementById("ffmpeg-status") as HTMLSpanElement;
const btnClose = document.getElementById("btn-close") as HTMLButtonElement;

const appWindow = getCurrentWindow();

btnClose.addEventListener("click", () => {
  invoke("quit");
});

// startDragging() is the reliable way to move a decoration-less window on macOS
const titlebar = document.getElementById("titlebar") as HTMLDivElement;
titlebar.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  if ((e.target as HTMLElement).closest("#btn-close")) return;
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
}

function removeFile(path: string): void {
  const entry = files.get(path);
  if (!entry) return;
  entry.el.remove();
  files.delete(path);
  if (files.size === 0) setFileListVisible(false);
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
