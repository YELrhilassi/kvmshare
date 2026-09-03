import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";

// The built app is embedded into the Go binary by Wails, so assets must
// load from relative paths (no absolute /assets/ URLs).
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  base: "./",
  build: {
    target: "es2020",
    outDir: "dist",
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});