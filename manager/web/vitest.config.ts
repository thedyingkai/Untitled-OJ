import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

export default defineConfig({
  // Vitest 3 carries its own compatible Vite type while the application uses
  // Vite 8. The plugin is runtime-compatible; the cast only bridges duplicate
  // type packages in this standalone test configuration.
  plugins: [vue() as never],
  test: {
    environment: "happy-dom",
    setupFiles: ["./src/test/setup.ts"],
    exclude: ["e2e/**", "node_modules/**", "dist/**"],
  },
});
