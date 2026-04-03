import "./style.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

// ─── Interfaces ──────────────────────────────────────────────────────────

interface ProgressPayload {
  path: string;
  percent: number;
}
interface InstallLogPayload {
  line: string;
  phase: string;
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

// ─── Element refs ─────────────────────────────────────────────────────────

const files = new Map<string, FileEntry>();
const progressUnlisteners = new Map<string, () => void>();
const dropZone = document.getElementById("drop-zone") as HTMLDivElement;
const fileList = document.getElementById("file-list") as HTMLDivElement;
const installPanel = document.getElementById("install-panel") as HTMLDivElement;
const installTitle = document.getElementById(
  "install-title",
) as HTMLParagraphElement;
const installSubtitle = document.getElementById(
  "install-subtitle",
) as HTMLParagraphElement;
const installLogWrap = document.getElementById(
  "install-log-wrap",
) as HTMLDivElement;
const installLog = document.getElementById("install-log") as HTMLDivElement;
const bottomStatus = document.getElementById(
  "bottom-status",
) as HTMLSpanElement;
const btnClearAll = document.getElementById(
  "btn-clear-all",
) as HTMLButtonElement;

const appWindow = getCurrentWindow();

// ─── Dep install tracking ─────────────────────────────────────────────────

/** Set of dep keys currently being installed in the background. */
const depsInstalling = new Set<string>();

const DEP_DISPLAY: Record<string, string> = {
  ffmpeg: "ffmpeg",
  pandoc: "pandoc",
  pdftohtml: "poppler",
  "ebook-convert": "Calibre",
};

/** Map error substrings → dep key, for friendly "installing" messages. */
const DEP_ERROR_FRAGMENTS: [string, string][] = [
  ["pandoc not found", "pandoc"],
  ["pdftohtml not found", "pdftohtml"],
  ["ebook-convert not found", "ebook-convert"],
  ["ffmpeg not found", "ffmpeg"],
];

function installingDepForError(msg: string): string | null {
  const lower = msg.toLowerCase();
  for (const [fragment, dep] of DEP_ERROR_FRAGMENTS) {
    if (lower.includes(fragment) && depsInstalling.has(dep)) return dep;
  }
  return null;
}

// ─── Clear all ────────────────────────────────────────────────────────────

btnClearAll.addEventListener("click", () => {
  files.forEach((entry) => entry.el.remove());
  files.clear();
  setFileListVisible(false);
  updateBatchToolbar();
});

// ─── Drag-drop ────────────────────────────────────────────────────────────

appWindow
  .onDragDropEvent((event) => {
    const { type } = event.payload;
    if (type === "over" || type === "enter") {
      dropZone.classList.add("drag-over");
    } else if (type === "drop") {
      dropZone.classList.remove("drag-over");
      const paths =
        (event.payload as { type: string; paths: string[] }).paths ?? [];
      for (const path of paths) addFile(path);
    } else {
      dropZone.classList.remove("drag-over");
    }
  })
  .catch(console.error);

// Click-to-browse
dropZone.addEventListener("click", async () => {
  const selected = await openDialog({ multiple: true, directory: false });
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  for (const path of paths) await addFile(path);
});

// ─── File management ─────────────────────────────────────────────────────

async function addFile(path: string): Promise<void> {
  if (files.has(path)) return;

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
  installPanel.hidden = true;
  if (visible) {
    dropZone.hidden = true;
    fileList.hidden = false;
    btnClearAll.hidden = false;
  } else {
    dropZone.hidden = false;
    fileList.hidden = true;
    btnClearAll.hidden = true;
  }
}

function showInstallPanel(title: string, subtitle = ""): void {
  dropZone.hidden = true;
  fileList.hidden = true;
  btnClearAll.hidden = true;
  installLog.innerHTML = "";
  installTitle.textContent = title;
  installSubtitle.textContent = subtitle;
  installPanel.hidden = false;
}

function buildFileItem(
  path: string,
  name: string,
  formats: string[],
): HTMLElement {
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
  removeBtn.title = "Remove";
  removeBtn.addEventListener("click", () => removeFile(path));

  row.appendChild(nameEl);
  row.appendChild(removeBtn);
  item.appendChild(row);

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

// ─── Single-file conversion ───────────────────────────────────────────────

async function startConversion(
  path: string,
  targetFormat: string,
  item: HTMLElement,
): Promise<void> {
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
    progressLabel = progressRow.querySelector(".progress-label") as HTMLElement;
  }

  progressFill.style.width = "0%";
  progressLabel.textContent = "0%";

  progressUnlisteners.get(path)?.();
  const unlisten = await listen<ProgressPayload>("convert:progress", (ev) => {
    if (ev.payload.path !== path) return;
    const pct = Math.round(ev.payload.percent);
    progressFill.style.width = `${pct}%`;
    progressLabel.textContent = `${pct}%`;
  });
  progressUnlisteners.set(path, unlisten);

  try {
    const outputPath = await invoke<string>("convert", { path, targetFormat });
    progressFill.style.width = "100%";
    progressLabel.textContent = "100%";

    setTimeout(() => {
      progressRow!.remove();
      const status = document.createElement("span");
      status.className = "file-status success";
      status.textContent = `↗ ${outputPath.split("/").pop()}`;
      status.title = "Click to reveal in Finder";
      status.addEventListener("click", () =>
        invoke("open_output_folder", { path: outputPath }).catch(console.error),
      );
      item.appendChild(status);
      item
        .querySelectorAll<HTMLButtonElement>(".fmt-btn")
        .forEach((b) => (b.disabled = false));
    }, 600);
  } catch (err) {
    progressRow.remove();
    const status = document.createElement("span");
    const errStr = String(err);
    const installingDep = installingDepForError(errStr);
    if (installingDep) {
      status.className = "file-status warning";
      status.textContent = `⏳ Installing ${DEP_DISPLAY[installingDep]}, please try again…`;
    } else {
      status.className = "file-status error";
      status.textContent = `✗ ${errStr}`;
    }
    item.appendChild(status);
    item
      .querySelectorAll<HTMLButtonElement>(".fmt-btn")
      .forEach((b) => (b.disabled = false));
  } finally {
    unlisten();
    progressUnlisteners.delete(path);
  }
}

// ─── Batch toolbar ────────────────────────────────────────────────────────

function updateBatchToolbar(): void {
  fileList.querySelector(".batch-toolbar")?.remove();
  if (files.size < 2) return;

  const allFormats = Array.from(files.values()).map((e) => new Set(e.formats));
  const intersection = allFormats.reduce(
    (acc, set) => new Set([...acc].filter((f) => set.has(f))),
  );
  if (intersection.size === 0) return;

  const toolbar = document.createElement("div");
  toolbar.className = "batch-toolbar";

  const label = document.createElement("span");
  label.className = "batch-label";
  label.textContent = "all →";
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

// ─── Batch conversion ─────────────────────────────────────────────────────

async function startBatchConversion(targetFormat: string): Promise<void> {
  const entries = Array.from(files.values()).filter((e) =>
    e.formats.includes(targetFormat),
  );
  if (entries.length === 0) return;

  const toolbar = fileList.querySelector<HTMLElement>(".batch-toolbar");
  toolbar
    ?.querySelectorAll<HTMLButtonElement>("button")
    .forEach((b) => (b.disabled = true));

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
        ".progress-label",
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
    const results = await invoke<BatchResult[]>("convert_batch", {
      items: batchItems,
    });

    for (const result of results) {
      const entry = files.get(result.path);
      if (!entry) continue;
      const item = entry.el;
      const pr = item.querySelector<HTMLElement>(".progress-row");

      if (result.output_path) {
        const fill = pr?.querySelector<HTMLElement>(".progress-fill");
        const lbl = pr?.querySelector<HTMLElement>(".progress-label");
        if (fill) fill.style.width = "100%";
        if (lbl) lbl.textContent = "100%";
        setTimeout(() => {
          pr?.remove();
          const status = document.createElement("span");
          status.className = "file-status success";
          status.textContent = `↗ ${result.output_path!.split("/").pop()}`;
          status.title = "Click to reveal in Finder";
          status.addEventListener("click", () =>
            invoke("open_output_folder", { path: result.output_path }).catch(
              console.error,
            ),
          );
          item.appendChild(status);
          item
            .querySelectorAll<HTMLButtonElement>(".fmt-btn")
            .forEach((b) => (b.disabled = false));
        }, 600);
      } else {
        pr?.remove();
        const status = document.createElement("span");
        const errStr = result.error ?? "Unknown error";
        const installingDep = installingDepForError(errStr);
        if (installingDep) {
          status.className = "file-status warning";
          status.textContent = `⏳ Installing ${DEP_DISPLAY[installingDep]}, please try again…`;
        } else {
          status.className = "file-status error";
          status.textContent = `✗ ${errStr}`;
        }
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

// ─── Inline error helper ──────────────────────────────────────────────────

function showInlineError(path: string, message: string): void {
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

// ─── Install progress ─────────────────────────────────────────────────────

listen("brew:installing", () => {
  showInstallPanel("Installing Homebrew", "needed to install ffmpeg");
  bottomStatus.className = "warning";
  bottomStatus.textContent = "installing Homebrew…";
}).catch(console.error);

listen("brew:installed", () => {
  installTitle.textContent = "Homebrew installed ✓";
  installSubtitle.textContent = "installing ffmpeg…";
  bottomStatus.className = "ok";
  bottomStatus.textContent = "Homebrew installed ✓";
}).catch(console.error);

listen("ffmpeg:missing", () => {
  depsInstalling.add("ffmpeg");
  bottomStatus.className = "warning";
  bottomStatus.textContent = "ffmpeg not found — installing…";
}).catch(console.error);

listen("ffmpeg:installing", () => {
  depsInstalling.add("ffmpeg");
  showInstallPanel(
    "Installing ffmpeg",
    "required for video & audio conversion",
  );
  bottomStatus.className = "warning";
  bottomStatus.textContent = "installing ffmpeg…";
}).catch(console.error);

listen("ffmpeg:installed", () => {
  depsInstalling.delete("ffmpeg");
  installPanel.hidden = true;
  bottomStatus.className = "ok";
  bottomStatus.textContent = "ffmpeg installed ✓";
  // Return to drop zone if no files are loaded
  if (files.size === 0) setFileListVisible(false);
  setTimeout(() => {
    bottomStatus.textContent = "";
    bottomStatus.className = "";
  }, 4000);
}).catch(console.error);

listen("ffmpeg:failed", (ev) => {
  depsInstalling.delete("ffmpeg");
  installTitle.textContent = "Installation failed";
  installSubtitle.textContent = String(ev.payload);
  installLogWrap.classList.add("visible");
  installLog.scrollTop = installLog.scrollHeight;
  bottomStatus.className = "warning";
  bottomStatus.textContent = "ffmpeg unavailable";
}).catch(console.error);

// ─── Background dep install status (pandoc, poppler, Calibre) ─────────────

const BACKGROUND_DEPS: [string, string][] = [
  ["pandoc", "pandoc"],
  ["pdftohtml", "poppler"],
  ["ebook-convert", "Calibre"],
];

for (const [dep, label] of BACKGROUND_DEPS) {
  listen(`${dep}:missing`, () => {
    depsInstalling.add(dep);
    bottomStatus.className = "warning";
    bottomStatus.textContent = `installing ${label}…`;
  }).catch(console.error);

  listen(`${dep}:installing`, () => {
    depsInstalling.add(dep);
    bottomStatus.className = "warning";
    bottomStatus.textContent = `installing ${label}…`;
  }).catch(console.error);

  listen(`${dep}:installed`, () => {
    depsInstalling.delete(dep);
    bottomStatus.className = "ok";
    bottomStatus.textContent = `${label} installed ✓`;
    setTimeout(() => {
      if (bottomStatus.textContent === `${label} installed ✓`) {
        bottomStatus.textContent = "";
        bottomStatus.className = "";
      }
    }, 4000);
  }).catch(console.error);

  listen(`${dep}:failed`, () => {
    depsInstalling.delete(dep);
    bottomStatus.className = "warning";
    bottomStatus.textContent = `${label} unavailable`;
  }).catch(console.error);
}

// Buffer log lines silently — only visible if an error occurs
listen<InstallLogPayload>("install:log", (ev) => {
  const line = ev.payload.line.trim();
  if (!line) return;
  const el = document.createElement("div");
  el.textContent = line;
  installLog.appendChild(el);
  // Cap buffer at 300 lines
  while (installLog.children.length > 300) {
    installLog.removeChild(installLog.firstChild!);
  }
}).catch(console.error);
