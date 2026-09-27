import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri 期望固定端口；构建产物完全本地，不依赖任何 CDN。
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: ["es2021", "safari15"],
    minify: "esbuild",
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
});
