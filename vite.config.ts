import { resolve } from "path";
import { defineConfig, normalizePath } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
  root: "ui",
  // Prevent Vite from obscuring Rust errors
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // Don't watch the Rust crate. Anchored to this config's directory: the
      // repo folder itself is usually named swift-shifter too, so a bare
      // "**/swift-shifter/**" glob would ignore every file.
      ignored: [normalizePath(resolve(__dirname, "swift-shifter")) + "/**"],
    },
  },
  // to access Tauri environment variables set by the CLI with information about the current target
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  test: {
    include: ["src/**/*.test.ts"],
  },
  build: {
    // Tauri uses Chromium on Windows and WebKit on macOS and Linux
    target:
      process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari16",
    // don't minify for debug builds
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    // produce sourcemaps for debug builds
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    outDir: "dist",
    rollupOptions: {
      input: {
        main: resolve(__dirname, "ui/index.html"),
        settings: resolve(__dirname, "ui/settings.html"),
        quick: resolve(__dirname, "ui/quick.html"),
      },
    },
  },
});
