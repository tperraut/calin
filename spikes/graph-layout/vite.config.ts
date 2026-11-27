import solid from "@solidjs/vite-plugin";
import { defineConfig, lazyPlugins } from "vite-plus";

export default defineConfig({
  plugins: lazyPlugins(() => [solid()]),
  // flow.html is the library trial (@dschz/solid-flow), kept apart from the hand-written renderers.
  build: { rolldownOptions: { input: { main: "index.html", flow: "flow.html" } } },
});
