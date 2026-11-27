import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import solid from "@solidjs/vite-plugin";
import tailwindcss from "@tailwindcss/vite";
import { build, defineConfig, lazyPlugins, type Plugin } from "vite-plus";
import { precacheList, WORKER_FILE, type OutputFile } from "./src/sw/precache.ts";

// Generated from crates/calin-proto by `make gen`; never reformatted or linted here.
const generated = ["src/gen/**"];

/** Every file under `dir`, as `/`-separated paths relative to it. */
function filesUnder(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => relative(dir, join(entry.parentPath, entry.name)).split(sep).join("/"));
}

/**
 * Emits the service worker (`src/sw/sw.ts`) as `/sw.js`: a stable name, so the
 * browser sees each build as an update of the same worker, with the build's
 * pre-cache list and a hash of everything built written into it. The worker is
 * bundled on its own as a classic script, so it imports nothing at run time.
 */
function serviceWorker(): Plugin {
  let root = "";
  let publicDir = "";
  return {
    name: "calin:service-worker",
    apply: "build",
    enforce: "post",
    configResolved(config) {
      root = config.root;
      publicDir = config.publicDir;
    },
    async generateBundle(_options, bundle) {
      const outputs = Object.values(bundle).sort((a, b) => a.fileName.localeCompare(b.fileName));
      const files: OutputFile[] = outputs.map((output) =>
        output.type === "chunk"
          ? {
              type: "chunk",
              fileName: output.fileName,
              // The page's script: its facade is `index.html` itself.
              isEntry: output.isEntry && output.facadeModuleId === join(root, "index.html"),
              imports: output.imports,
              importedCss: [...(output.viteMetadata?.importedCss ?? [])],
              importedAssets: [...(output.viteMetadata?.importedAssets ?? [])],
            }
          : { type: "asset", fileName: output.fileName },
      );
      const publicFiles = publicDir === "" ? [] : filesUnder(publicDir).sort();
      const precache = precacheList(files, publicFiles);

      const hash = createHash("sha256");
      for (const output of outputs) {
        hash.update(output.fileName);
        hash.update(output.type === "chunk" ? output.code : output.source);
      }
      for (const name of publicFiles) {
        hash.update(name);
        hash.update(readFileSync(join(publicDir, name)));
      }
      const version = hash.digest("hex").slice(0, 16);

      const built = await build({
        configFile: false,
        root,
        publicDir: false,
        logLevel: "warn",
        define: {
          __CALIN_VERSION__: JSON.stringify(version),
          __CALIN_PRECACHE__: JSON.stringify(precache),
        },
        build: {
          write: false,
          sourcemap: false,
          minify: true,
          lib: {
            entry: join(root, "src/sw/sw.ts"),
            formats: ["iife"],
            name: "calinServiceWorker",
            fileName: () => WORKER_FILE,
          },
        },
      });
      const results = Array.isArray(built) ? built : [built];
      const chunk = results
        .flatMap((result) => ("output" in result ? result.output : []))
        .find((output) => output.type === "chunk");
      if (!chunk || chunk.type !== "chunk") throw new Error("the service worker was not built");
      this.emitFile({ type: "asset", fileName: WORKER_FILE, source: chunk.code });
    },
  };
}

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
  plugins: lazyPlugins(() => [solid(), tailwindcss(), serviceWorker()]),
  // The graph stress page (stress.html) is a development page: `vp dev` serves it, and a build
  // holds it only when asked, to measure it as built: `CALIN_STRESS=1 vp build --outDir <dir>`.
  build: process.env.CALIN_STRESS
    ? { rollupOptions: { input: ["index.html", "stress.html"] } }
    : undefined,
  server: {
    proxy: {
      // `make dev-server` listens here; `ws: true` carries /api/v1/ws through.
      "/api": { target: "http://127.0.0.1:4680", ws: true },
      // Sign-in in public mode: the browser navigates to these (api.rs).
      "/auth": { target: "http://127.0.0.1:4680" },
    },
  },
  test: {
    include: ["src/**/*.test.{ts,tsx}"],
    environment: "happy-dom",
    setupFiles: ["src/test/setup.ts"],
  },
});
