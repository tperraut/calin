import { describe, expect, it } from "vite-plus/test";
import { precacheList, type OutputFile } from "./precache";

const chunk = (
  fileName: string,
  patch: Partial<Extract<OutputFile, { type: "chunk" }>> = {},
): OutputFile => ({
  type: "chunk",
  fileName,
  isEntry: false,
  imports: [],
  importedCss: [],
  importedAssets: [],
  ...patch,
});
const asset = (fileName: string): OutputFile => ({ type: "asset", fileName });

const bundle: OutputFile[] = [
  asset("index.html"),
  chunk("assets/index-a1.js", {
    isEntry: true,
    imports: ["assets/errors-b2.js", "assets/preload-helper-c3.js"],
    importedCss: ["assets/index-d4.css"],
  }),
  chunk("assets/errors-b2.js", { imports: ["assets/format-e5.js"] }),
  chunk("assets/format-e5.js"),
  chunk("assets/preload-helper-c3.js"),
  asset("assets/index-d4.css"),
  asset("assets/index-a1.js.map"),
  // Loaded on demand: a panel and a grammar.
  chunk("assets/SettingsPage-f6.js", { imports: ["assets/errors-b2.js"] }),
  chunk("assets/sql-g7.js"),
  asset("assets/xterm-h8.css"),
  // A development page built alongside, with its own entry.
  asset("stress.html"),
  chunk("assets/stress-i9.js", { isEntry: false, imports: ["assets/sql-g7.js"] }),
  asset("sw.js"),
];
const publicFiles = [
  "favicon.svg",
  "manifest.webmanifest",
  "apple-touch-icon.png",
  "icons/icon-192.png",
  "icons/icon-512.png",
  "icons/icon-maskable-512.png",
  "icons/badge-96.png",
  "robots.txt",
  "theme.js",
];

describe("the pre-cache list", () => {
  it("holds the page, the entry and what it imports statically, and the icons, sorted", () => {
    expect(precacheList(bundle, publicFiles)).toEqual([
      "/apple-touch-icon.png",
      "/assets/errors-b2.js",
      "/assets/format-e5.js",
      "/assets/index-a1.js",
      "/assets/index-d4.css",
      "/assets/preload-helper-c3.js",
      "/favicon.svg",
      "/icons/badge-96.png",
      "/icons/icon-192.png",
      "/icons/icon-512.png",
      "/icons/icon-maskable-512.png",
      "/index.html",
      "/manifest.webmanifest",
      "/theme.js",
    ]);
  });

  it("leaves out source maps, the worker itself, lazy chunks and other pages", () => {
    const list = precacheList(
      [...bundle, asset("assets/errors-b2.js.map"), asset("sw.js.map")],
      [...publicFiles, "sw.js"],
    );
    expect(list.filter((url) => url.endsWith(".map"))).toEqual([]);
    expect(list).not.toContain("/sw.js");
    expect(list).not.toContain("/assets/SettingsPage-f6.js");
    expect(list).not.toContain("/assets/sql-g7.js");
    expect(list).not.toContain("/assets/xterm-h8.css");
    expect(list).not.toContain("/stress.html");
    expect(list).not.toContain("/robots.txt");
  });

  it("never lists a source map or the worker, even when the entry says it loads them", () => {
    const odd = [
      chunk("assets/a.js", {
        isEntry: true,
        imports: ["sw.js"],
        importedAssets: ["assets/a.js.map"],
      }),
      asset("sw.js"),
      asset("assets/a.js.map"),
    ];
    expect(precacheList(odd, [])).toEqual(["/assets/a.js"]);
  });

  it("lists only files the build made, the page only when there is one", () => {
    const partial = [
      chunk("assets/a.js", {
        isEntry: true,
        imports: ["assets/gone.js", "assets/data.bin"],
        importedAssets: ["assets/font.woff2"],
      }),
      asset("assets/data.bin"),
    ];
    expect(precacheList(partial, ["notes.md"])).toEqual([
      "/assets/a.js",
      "/assets/data.bin",
      "/assets/font.woff2",
    ]);
  });

  it("follows import cycles once", () => {
    const cyclic = [
      asset("index.html"),
      chunk("assets/a.js", { isEntry: true, imports: ["assets/b.js"] }),
      chunk("assets/b.js", { imports: ["assets/a.js"] }),
    ];
    expect(precacheList(cyclic, [])).toEqual(["/assets/a.js", "/assets/b.js", "/index.html"]);
  });
});
