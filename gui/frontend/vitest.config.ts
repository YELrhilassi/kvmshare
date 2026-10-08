import { defineConfig } from "vitest/config";
import { fileURLToPath, URL } from "node:url";

// Test-only config, deliberately separate from vite.config.ts.
//
// Vitest 3 carries its own Vite build, whose plugin types differ from the
// project's Vite 8 (rolldown) ones; typing the `react()` / `tailwindcss()`
// plugins in a vitest config therefore fails to compile. Tests do not need
// them: esbuild transforms the TSX (tsconfig `jsx: react-jsx`), and no test
// imports CSS. Only the path alias has to be mirrored here.
export default defineConfig({
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  // Component/store tests run under jsdom, in-process. They cover the logic
  // the UI owns (state transitions, rendering) without a display or the
  // Wails runtime — the bridge is faked at the api() boundary, which is
  // exactly where the real one enters.
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
    clearMocks: true,
  },
});
