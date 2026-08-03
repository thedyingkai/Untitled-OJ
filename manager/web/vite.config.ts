import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Business traffic and per-user UI state both use the versioned v1 contract.
const apiPrefixes = ["/api"];

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 5174,
    proxy: Object.fromEntries(
      apiPrefixes.map((prefix) => [
        prefix,
        { target: "http://127.0.0.1:8090", changeOrigin: true },
      ]),
    ),
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 900,
  },
});
