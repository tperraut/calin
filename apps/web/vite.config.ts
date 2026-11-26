import solid from "@solidjs/vite-plugin";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, lazyPlugins } from "vite-plus";

// Generated from crates/calin-proto by `make gen`; never reformatted or linted here.
const generated = ["src/gen/**"];

export default defineConfig({
  fmt: { ignorePatterns: generated },
  lint: {
    ignorePatterns: generated,
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: {
      "vite-plus/prefer-vite-plus-imports": "error",
      "typescript/no-explicit-any": "error",
    },
    options: { typeAware: true, typeCheck: true },
  },
  plugins: lazyPlugins(() => [solid(), tailwindcss()]),
  server: {
    proxy: {
      // `make dev-server` listens here; `ws: true` carries /api/v1/ws through.
      "/api": { target: "http://127.0.0.1:4680", ws: true },
    },
  },
  test: {
    include: ["src/**/*.test.{ts,tsx}"],
    environment: "happy-dom",
    setupFiles: ["src/test/setup.ts"],
  },
});
