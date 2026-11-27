import { describe, expect, it } from "vite-plus/test";
import { languageFor } from "./languages";

describe("languageFor", () => {
  it.each([
    "src/App.tsx",
    "src/index.ts",
    "lib/util.js",
    "components/Button.jsx",
    "package.json",
    "README.md",
    "src/main.rs",
    "index.html",
    "src/index.css",
    "tools/gen.py",
    "config/app.yaml",
    ".github/workflows/ci.yml",
    "Cargo.toml",
    "Cargo.lock",
    "scripts/deploy.sh",
    "Dockerfile",
    "SRC/UPPER.RS",
  ])("has a grammar for %s", (path) => {
    expect(languageFor(path)).toBeTypeOf("function");
  });

  it.each([
    "LICENSE",
    "notes.txt",
    "Makefile",
    ".gitignore",
    "archive.tar.gz",
    "weird.constructor",
  ])("leaves %s as plain text", (path) => {
    expect(languageFor(path)).toBeUndefined();
  });

  it("loads a grammar on demand, a parser package and a classic mode alike", async () => {
    await expect(languageFor("a.json")?.()).resolves.toBeDefined();
    await expect(languageFor("a.toml")?.()).resolves.toBeDefined();
    await expect(languageFor("a.sh")?.()).resolves.toBeDefined();
  });
});
