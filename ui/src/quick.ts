import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { parseQuery, rankFiles, relativeTime, type RecentFile } from "./quick-search";

// ─── Backend payloads ─────────────────────────────────────────────────────

interface DetectResult {
  direct: string[];
  chained: { format: string }[];
}

interface ProgressPayload {
  path: string;
  percent: number;
}

// ─── Layout ───────────────────────────────────────────────────────────────

// Each must match the CSS rule (or quick.rs constant) named beside it.
const BAR_WIDTH = 640; // quick.rs WIDTH
const BAR_HEIGHT = 56; // quick.rs HEIGHT; #bar 54px + #panel 1px top/bottom borders
const ROW_HEIGHT = 48; // .row height
const LIST_PADDING = 8; // #results padding
const LIST_BORDER = 1; // #results border-top
const STATUS_HEIGHT = 32; // #status flex-basis, border included
const MAX_ROWS = 7;

// ─── State ────────────────────────────────────────────────────────────────

/** Where a conversion started from the bar stands. */
type Job =
  | { kind: "idle" }
  | { kind: "converting"; path: string; percent: number }
  | { kind: "done"; output: string }
  | { kind: "failed"; message: string };

let recentFiles: RecentFile[] = [];
let shown: RecentFile[] = [];
let selected = 0;
/** Target formats per file path: direct targets first, then chained ones. */
const targetsByPath = new Map<string, string[]>();
/** Index into the selected file's targets, chosen with Tab. */
let formatIndex = 0;
/** The file `formatIndex` was chosen for; re-selecting it keeps the choice. */
let formatPath: string | undefined;
let job: Job = { kind: "idle" };

const appWindow = getCurrentWindow();
const query = document.getElementById("query") as HTMLInputElement;
const hint = document.getElementById("hint") as HTMLElement;
const results = document.getElementById("results") as HTMLUListElement;
const status = document.getElementById("status") as HTMLElement;

// ─── Data ─────────────────────────────────────────────────────────────────

async function refreshRecentFiles(): Promise<void> {
  try {
    recentFiles = await invoke<RecentFile[]>("recent_files");
  } catch (e) {
    console.error("recent_files failed", e);
  }
  // Don't move the highlight out from under the user, e.g. onto a file a
  // conversion just wrote.
  updateResults(selectedFile()?.path);
}

/** Target formats for `path`, fetched once and then cached. */
async function targetsFor(path: string): Promise<string[]> {
  const cached = targetsByPath.get(path);
  if (cached) return cached;
  let targets: string[] = [];
  try {
    const detected = await invoke<DetectResult>("detect_format", { path });
    targets = [...detected.direct, ...detected.chained.map((c) => c.format)];
  } catch {
    // Unsupported after all (e.g. the file changed); offer nothing.
  }
  targetsByPath.set(path, targets);
  return targets;
}

// ─── Selection ────────────────────────────────────────────────────────────

function selectedFile(): RecentFile | undefined {
  return shown[selected];
}

function selectedTargets(): string[] {
  const file = selectedFile();
  return file ? (targetsByPath.get(file.path) ?? []) : [];
}

function selectedFormat(): string | undefined {
  return selectedTargets()[formatIndex];
}

/**
 * Recompute the matches for the current query. The selection stays on
 * `keepPath` if that file is still shown, otherwise it returns to the top.
 */
function updateResults(keepPath?: string): void {
  shown = rankFiles(recentFiles, parseQuery(query.value).text, MAX_ROWS);
  const kept = shown.findIndex((file) => file.path === keepPath);
  void selectRow(Math.max(kept, 0));
}

async function selectRow(index: number): Promise<void> {
  selected = index;
  render();
  const file = selectedFile();
  if (!file) return;
  const targets = await targetsFor(file.path);
  // The selection may have moved on while detect_format ran.
  if (selectedFile() !== file) return;
  // Keep a format already picked for this file, e.g. across the refresh that
  // follows a conversion.
  if (file.path !== formatPath) {
    const wanted = parseQuery(query.value).format;
    formatIndex = wanted && targets.includes(wanted) ? targets.indexOf(wanted) : 0;
    formatPath = file.path;
  }
  render();
}

function moveSelection(delta: number): void {
  if (shown.length === 0) return;
  clearFinishedJob();
  void selectRow((selected + delta + shown.length) % shown.length);
}

function cycleFormat(delta: number): void {
  const count = selectedTargets().length;
  if (count === 0) return;
  clearFinishedJob();
  formatIndex = (formatIndex + delta + count) % count;
  render();
}

// ─── Converting ───────────────────────────────────────────────────────────

/**
 * Forget a finished conversion once the user moves on, so its status line
 * goes away and Enter converts again instead of opening the old output.
 */
function clearFinishedJob(): void {
  if (job.kind === "done" || job.kind === "failed") job = { kind: "idle" };
}

async function convertSelected(): Promise<void> {
  const file = selectedFile();
  const format = selectedFormat();
  if (!file || !format || job.kind === "converting") return;

  job = { kind: "converting", path: file.path, percent: 0 };
  render();
  try {
    const output = await invoke<string>("convert", { path: file.path, targetFormat: format });
    job = { kind: "done", output };
    void refreshRecentFiles(); // the output is now a recent file too
  } catch (e) {
    job = { kind: "failed", message: String(e) };
  }
  render();
}

void listen<ProgressPayload>("convert:progress", ({ payload }) => {
  if (job.kind === "converting" && job.path === payload.path) {
    job = { ...job, percent: Math.round(payload.percent) };
    render();
  }
});

// ─── Rendering ────────────────────────────────────────────────────────────

function render(): void {
  renderRows();
  renderStatus();
  void resizeToContent();
}

function renderRows(): void {
  const now = Date.now();
  results.replaceChildren(
    ...shown.map((file, index) => {
      const row = document.createElement("li");
      row.className = "row";
      row.id = rowId(index);
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(index === selected));

      const ext = document.createElement("span");
      ext.className = "ext";
      ext.textContent = file.name.includes(".") ? file.name.split(".").pop()! : "";

      const text = document.createElement("div");
      text.className = "text";
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = file.name;
      const meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = `${file.folder} · ${relativeTime(file.modified_ms, now)}`;
      text.append(name, meta);

      row.append(ext, text);
      if (index === selected) row.append(formatPicker());

      row.addEventListener("mousedown", (event) => {
        event.preventDefault(); // keep focus in the search field
        if (index === selected) return;
        clearFinishedJob();
        void selectRow(index);
      });
      return row;
    }),
  );
  // Focus stays in the search field; this tells assistive tech which row it drives.
  query.setAttribute("aria-expanded", String(shown.length > 0));
  if (shown.length > 0) query.setAttribute("aria-activedescendant", rowId(selected));
  else query.removeAttribute("aria-activedescendant");
}

function rowId(index: number): string {
  return `quick-row-${index}`;
}

/** "→ jpg png webp" chips for the selected row; clicking one converts. */
function formatPicker(): HTMLElement {
  const picker = document.createElement("div");
  picker.className = "formats";
  selectedTargets().forEach((format, index) => {
    const chip = document.createElement("button");
    chip.className = "format";
    chip.tabIndex = -1;
    chip.textContent = format;
    chip.classList.toggle("active", index === formatIndex);
    chip.setAttribute("aria-pressed", String(index === formatIndex));
    chip.addEventListener("mousedown", (event) => {
      event.preventDefault();
      event.stopPropagation();
      formatIndex = index;
      void convertSelected();
    });
    picker.append(chip);
  });
  return picker;
}

function renderStatus(): void {
  status.className = job.kind;
  switch (job.kind) {
    case "idle":
      status.textContent = "";
      hint.textContent = shown.length > 0 ? "↵ convert · tab format" : "esc";
      break;
    case "converting":
      status.textContent = `converting… ${job.percent}%`;
      hint.textContent = "esc";
      break;
    case "done":
      status.textContent = `✓ saved ${job.output.split(/[\\/]/).pop()}`;
      hint.textContent = "↵ show in folder";
      break;
    case "failed":
      status.textContent = job.message;
      hint.textContent = "esc";
      break;
  }
}

/** Grow or shrink the window to fit the rows and the status line. */
async function resizeToContent(): Promise<void> {
  const listHeight =
    shown.length > 0 ? shown.length * ROW_HEIGHT + 2 * LIST_PADDING + LIST_BORDER : 0;
  const statusHeight = job.kind === "idle" ? 0 : STATUS_HEIGHT;
  await appWindow.setSize(new LogicalSize(BAR_WIDTH, BAR_HEIGHT + listHeight + statusHeight));
}

// ─── Input ────────────────────────────────────────────────────────────────

query.addEventListener("input", () => {
  clearFinishedJob();
  formatPath = undefined; // the query may now ask for a format ("… to png")
  updateResults();
});

document.addEventListener("keydown", (event) => {
  switch (event.key) {
    case "Escape":
      event.preventDefault();
      void invoke("hide_quick_bar");
      break;
    case "ArrowDown":
      event.preventDefault();
      moveSelection(1);
      break;
    case "ArrowUp":
      event.preventDefault();
      moveSelection(-1);
      break;
    case "Tab":
      event.preventDefault();
      cycleFormat(event.shiftKey ? -1 : 1);
      break;
    case "Enter":
      event.preventDefault();
      if (job.kind === "done") void invoke("open_output_folder", { path: job.output });
      else void convertSelected();
      break;
  }
});

// The window is hidden, not destroyed, between uses: start every summon with
// the field focused, last time's text selected, and a fresh file list. The
// list is only loaded here, on first use, not at app launch: on macOS reading
// Desktop/Documents/Downloads asks the user for permission.
void appWindow.onFocusChanged(({ payload: focused }) => {
  if (!focused) return;
  clearFinishedJob();
  query.focus();
  query.select();
  void refreshRecentFiles();
});
