import { StreamLanguage, type StreamParser } from "@codemirror/language";
import type { Extension } from "@codemirror/state";

type Loader = () => Promise<Extension>;

// A classic CodeMirror 5 mode, wrapped for CodeMirror 6.
const legacy =
  (load: () => Promise<StreamParser<unknown>>): Loader =>
  () =>
    load().then((parser) => StreamLanguage.define(parser));

const javascript =
  (config: { jsx?: boolean; typescript?: boolean }): Loader =>
  () =>
    import("@codemirror/lang-javascript").then((m) => m.javascript(config));

const shell = legacy(() => import("@codemirror/legacy-modes/mode/shell").then((m) => m.shell));
const toml = legacy(() => import("@codemirror/legacy-modes/mode/toml").then((m) => m.toml));
const clike = () => import("@codemirror/legacy-modes/mode/clike");
const xml = legacy(() => import("@codemirror/legacy-modes/mode/xml").then((m) => m.xml));
const diff = legacy(() => import("@codemirror/legacy-modes/mode/diff").then((m) => m.diff));
const yaml: Loader = () => import("@codemirror/lang-yaml").then((m) => m.yaml());
const json: Loader = () => import("@codemirror/lang-json").then((m) => m.json());
const markdown: Loader = () => import("@codemirror/lang-markdown").then((m) => m.markdown());
const html: Loader = () => import("@codemirror/lang-html").then((m) => m.html());

// Each grammar is its own chunk, fetched the first time a file of that type is shown.
const byExtension: Record<string, Loader> = {
  ts: javascript({ typescript: true }),
  mts: javascript({ typescript: true }),
  cts: javascript({ typescript: true }),
  tsx: javascript({ typescript: true, jsx: true }),
  js: javascript({}),
  mjs: javascript({}),
  cjs: javascript({}),
  jsx: javascript({ jsx: true }),
  json,
  jsonc: json,
  md: markdown,
  markdown,
  rs: () => import("@codemirror/lang-rust").then((m) => m.rust()),
  html,
  htm: html,
  css: () => import("@codemirror/lang-css").then((m) => m.css()),
  py: () => import("@codemirror/lang-python").then((m) => m.python()),
  pyi: () => import("@codemirror/lang-python").then((m) => m.python()),
  yaml,
  yml: yaml,
  toml,
  sh: shell,
  bash: shell,
  zsh: shell,
  go: legacy(() => import("@codemirror/legacy-modes/mode/go").then((m) => m.go)),
  c: legacy(() => clike().then((m) => m.c)),
  h: legacy(() => clike().then((m) => m.c)),
  cpp: legacy(() => clike().then((m) => m.cpp)),
  hpp: legacy(() => clike().then((m) => m.cpp)),
  cc: legacy(() => clike().then((m) => m.cpp)),
  java: legacy(() => clike().then((m) => m.java)),
  kt: legacy(() => clike().then((m) => m.kotlin)),
  sql: legacy(() => import("@codemirror/legacy-modes/mode/sql").then((m) => m.standardSQL)),
  rb: legacy(() => import("@codemirror/legacy-modes/mode/ruby").then((m) => m.ruby)),
  xml,
  svg: xml,
  diff,
  patch: diff,
};

// Files known by their whole name rather than an extension.
const byName: Record<string, Loader> = {
  dockerfile: legacy(() =>
    import("@codemirror/legacy-modes/mode/dockerfile").then((m) => m.dockerFile),
  ),
  "cargo.lock": toml,
  ".bashrc": shell,
  ".zshrc": shell,
  ".profile": shell,
  ".envrc": shell,
};

/** The grammar for a file, by its name; `undefined` means plain text. */
export function languageFor(path: string): Loader | undefined {
  const name = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
  const named = Object.hasOwn(byName, name) ? byName[name] : undefined;
  if (named) return named;
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return undefined;
  const extension = name.slice(dot + 1);
  return Object.hasOwn(byExtension, extension) ? byExtension[extension] : undefined;
}
