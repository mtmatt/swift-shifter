import "./settings.css";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface Config {
  output_dir: string | null;
  jpeg_quality: number;
  avif_quality: number;
  max_concurrent: number;
}

const win = getCurrentWindow();
const jpegSlider = document.getElementById("jpeg-q") as HTMLInputElement;
const jpegVal = document.getElementById("jpeg-q-val") as HTMLElement;
const avifSlider = document.getElementById("avif-q") as HTMLInputElement;
const avifVal = document.getElementById("avif-q-val") as HTMLElement;
const maxCSlider = document.getElementById("max-c") as HTMLInputElement;
const maxCVal = document.getElementById("max-c-val") as HTMLElement;
const statusEl = document.getElementById("status") as HTMLElement;

let cfg: Config = {
  output_dir: null,
  jpeg_quality: 75,
  avif_quality: 65,
  max_concurrent: 4,
};

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
})();

jpegSlider.addEventListener("input", () => {
  jpegVal.textContent = jpegSlider.value;
});
avifSlider.addEventListener("input", () => {
  avifVal.textContent = avifSlider.value;
});
maxCSlider.addEventListener("input", () => {
  maxCVal.textContent = maxCSlider.value;
});

document
  .getElementById("btn-cancel")!
  .addEventListener("click", () => win.close());

document.getElementById("btn-save")!.addEventListener("click", async () => {
  const newConfig: Config = {
    ...cfg,
    jpeg_quality: Number(jpegSlider.value),
    avif_quality: Number(avifSlider.value),
    max_concurrent: Number(maxCSlider.value),
  };
  try {
    await invoke("set_config", { newConfig });
    win.close();
  } catch (err) {
    statusEl.textContent = String(err);
  }
});
