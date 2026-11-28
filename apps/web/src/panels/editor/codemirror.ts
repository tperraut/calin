// The one module that touches CodeMirror. It is reached only through a dynamic
// import (see load.ts), so it and everything below it stay out of the first load.
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import {
  bracketMatching,
  HighlightStyle,
  indentOnInput,
  syntaxHighlighting,
} from "@codemirror/language";
import {
  getChunks,
  goToNextChunk,
  goToPreviousChunk,
  MergeView,
  unifiedMergeView,
} from "@codemirror/merge";
import {
  highlightSelectionMatches,
  openSearchPanel,
  search,
  searchKeymap,
} from "@codemirror/search";
import {
  Annotation,
  Compartment,
  EditorState,
  Transaction,
  type Extension,
} from "@codemirror/state";
import {
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { pageTheme, watchTheme } from "../../lib/theme";
import "./editor.css";
import { languageFor } from "./languages";
import { minimalChange } from "./textChange";
import type { DiffHandle, DiffOptions, EditorHandle, EditorModule, EditorOptions } from "./types";

// Colors are the app's own custom properties (index.css, editor.css), so both
// themes come from the stylesheet and follow the page's theme on their own.
const theme = EditorView.theme({
  "&": { height: "100%", color: "var(--fg)", backgroundColor: "var(--surface)", fontSize: "13px" },
  "&.cm-focused": { outline: "2px solid var(--accent)", outlineOffset: "-2px" },
  ".cm-scroller": {
    fontFamily: 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace',
    lineHeight: "1.5",
  },
  ".cm-content": { caretColor: "var(--fg)" },
  ".cm-content:focus-visible": { outline: "none" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--fg)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "var(--editor-selection)" },
  ".cm-gutters": {
    backgroundColor: "var(--bg)",
    color: "var(--muted)",
    borderRight: "1px solid var(--line)",
  },
  ".cm-activeLine": { backgroundColor: "var(--editor-active-line)" },
  ".cm-activeLineGutter": { backgroundColor: "var(--sunken)", color: "var(--fg)" },
  ".cm-selectionMatch": { backgroundColor: "var(--editor-match)" },
  ".cm-searchMatch": {
    backgroundColor: "var(--attention-soft)",
    outline: "1px solid var(--tone-attention)",
  },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--editor-selection)" },
  "&.cm-focused .cm-matchingBracket": {
    backgroundColor: "var(--editor-match)",
    outline: "1px solid var(--line)",
  },
  ".cm-panels": { backgroundColor: "var(--bg)", color: "var(--fg)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--line)" },
  // The merge view, in the app's own diff colors.
  "&.cm-merge-a .cm-changedLine, .cm-deletedChunk": { backgroundColor: "var(--diff-removed)" },
  "&.cm-merge-b .cm-changedLine, .cm-inlineChangedLine": { backgroundColor: "var(--diff-added)" },
  "&.cm-merge-a .cm-changedText, .cm-deletedChunk .cm-deletedText": {
    background: "var(--diff-removed-strong)",
  },
  "&.cm-merge-b .cm-changedText": { background: "var(--diff-added-strong)" },
  "&.cm-merge-a .cm-changedLineGutter, .cm-deletedLineGutter": {
    background: "var(--tone-danger)",
  },
  "&.cm-merge-b .cm-changedLineGutter": { background: "var(--tone-success)" },
  ".cm-collapsedLines": {
    minHeight: "32px",
    borderBlock: "1px solid var(--line)",
    background: "var(--sunken)",
    padding: "6px 10px",
    color: "var(--muted)",
    fontFamily: "system-ui, sans-serif",
    fontSize: "12px",
  },
});

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.operatorKeyword, tags.modifier], color: "var(--syntax-keyword)" },
  { tag: [tags.string, tags.special(tags.string), tags.regexp], color: "var(--syntax-string)" },
  { tag: [tags.number, tags.bool, tags.null, tags.atom], color: "var(--syntax-number)" },
  { tag: [tags.comment, tags.meta], color: "var(--syntax-comment)", fontStyle: "italic" },
  {
    tag: [tags.typeName, tags.className, tags.namespace, tags.tagName],
    color: "var(--syntax-type)",
  },
  {
    tag: [tags.function(tags.variableName), tags.function(tags.propertyName), tags.macroName],
    color: "var(--syntax-function)",
  },
  {
    tag: [tags.propertyName, tags.attributeName, tags.labelName],
    color: "var(--syntax-property)",
  },
  { tag: [tags.heading, tags.strong], fontWeight: "600" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: [tags.link, tags.url], color: "var(--syntax-string)", textDecoration: "underline" },
  { tag: tags.invalid, color: "var(--tone-danger)" },
]);

const external = Annotation.define<boolean>();

/** CodeMirror picks some of its own defaults by this flag; it follows the page's theme. */
function followScheme(views: () => readonly EditorView[], compartment: Compartment): () => void {
  return watchTheme((theme) => {
    for (const view of views()) {
      view.dispatch({
        effects: compartment.reconfigure(EditorView.darkTheme.of(theme === "dark")),
      });
    }
  });
}

const isDark = (): boolean => pageTheme() === "dark";

/** Fetches the grammar for `path`, if there is one, and hands it over. */
function loadLanguage(path: string, apply: (language: Extension) => void): void {
  languageFor(path)?.()
    .then(apply)
    .catch(() => {
      // Without its grammar a file is still readable as plain text.
    });
}

const locked = (value: boolean): Extension => [
  EditorState.readOnly.of(value),
  // Still focusable and selectable when locked, so it can be read and searched.
  EditorView.contentAttributes.of({ "aria-readonly": value ? "true" : "false" }),
];

const wrapping = (value: boolean): Extension => (value ? EditorView.lineWrapping : []);

/** Calls `onChange` for the reader's edits only: a text set from outside is not one. */
const reportEdits = (onChange: ((text: string) => void) | undefined): Extension =>
  EditorView.updateListener.of((update) => {
    if (!update.docChanged) return;
    if (update.transactions.some((transaction) => transaction.annotation(external))) return;
    onChange?.(update.state.doc.toString());
  });

function createEditor(parent: HTMLElement, options: EditorOptions): EditorHandle {
  const language = new Compartment();
  const readOnly = new Compartment();
  const wrap = new Compartment();
  const scheme = new Compartment();
  let destroyed = false;

  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: options.text,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightActiveLine(),
        history(),
        drawSelection(),
        indentOnInput(),
        bracketMatching(),
        highlightSelectionMatches(),
        // On top: an on-screen keyboard must not cover the search field.
        search({ top: true }),
        keymap.of([
          {
            key: "Mod-s",
            preventDefault: true,
            run: () => {
              options.onSave?.();
              return true;
            },
          },
          ...defaultKeymap,
          ...historyKeymap,
          ...searchKeymap,
          indentWithTab,
        ]),
        EditorView.contentAttributes.of({
          "aria-label": options.label,
          // Tab indents here, so the way out has to be said.
          "aria-description": "Press Escape, then Tab, to leave the editor.",
        }),
        reportEdits(options.onChange),
        theme,
        syntaxHighlighting(highlight),
        scheme.of(EditorView.darkTheme.of(isDark())),
        language.of([]),
        readOnly.of(locked(options.readOnly)),
        wrap.of(wrapping(options.wrap)),
      ],
    }),
  });

  loadLanguage(options.path, (extension) => {
    if (!destroyed) view.dispatch({ effects: language.reconfigure(extension) });
  });
  const unfollow = followScheme(() => [view], scheme);

  return {
    text: () => view.state.doc.toString(),
    setText(text) {
      const change = minimalChange(view.state.doc.toString(), text);
      if (!change) return;
      view.dispatch({
        changes: change,
        // Not the reader's edit: it neither marks the file edited nor joins the undo history.
        annotations: [external.of(true), Transaction.addToHistory.of(false)],
      });
    },
    setReadOnly(value) {
      view.dispatch({ effects: readOnly.reconfigure(locked(value)) });
    },
    setWrap(value) {
      view.dispatch({ effects: wrap.reconfigure(wrapping(value)) });
    },
    openSearch() {
      openSearchPanel(view);
    },
    focus() {
      view.focus();
    },
    destroy() {
      destroyed = true;
      unfollow();
      view.destroy();
    },
  };
}

function createDiff(parent: HTMLElement, options: DiffOptions): DiffHandle {
  const language = new Compartment();
  const scheme = new Compartment();
  let destroyed = false;

  const shared = (label: string): Extension => [
    lineNumbers(),
    EditorState.readOnly.of(true),
    EditorView.editable.of(false),
    // Not editable, yet reachable: the keyboard scrolls it and moves between changes.
    EditorView.contentAttributes.of({ tabindex: "0", "aria-label": label }),
    // The panes are narrow, two of them in a side pane more so: lines wrap rather than scroll sideways.
    EditorView.lineWrapping,
    theme,
    syntaxHighlighting(highlight),
    scheme.of(EditorView.darkTheme.of(isDark())),
    language.of([]),
  ];
  const collapseUnchanged = { margin: 3, minSize: 4 };

  let views: readonly EditorView[];
  let main: EditorView;
  let dispose: () => void;

  if (options.layout === "split") {
    const merge = new MergeView({
      parent,
      a: { doc: options.original, extensions: shared(`${options.label}, before`) },
      b: { doc: options.modified, extensions: shared(`${options.label}, after`) },
      collapseUnchanged,
      gutter: true,
    });
    views = [merge.a, merge.b];
    main = merge.b;
    dispose = () => merge.destroy();
  } else {
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: options.modified,
        extensions: [
          shared(options.label),
          unifiedMergeView({
            original: options.original,
            mergeControls: false,
            gutter: true,
            collapseUnchanged,
          }),
        ],
      }),
    });
    views = [view];
    main = view;
    dispose = () => view.destroy();
  }

  loadLanguage(options.path, (extension) => {
    if (destroyed) return;
    for (const view of views) view.dispatch({ effects: language.reconfigure(extension) });
  });
  const unfollow = followScheme(() => views, scheme);

  const move = (command: typeof goToNextChunk): void => {
    command({ state: main.state, dispatch: (transaction) => main.dispatch(transaction) });
    // The command moves the selection; a view that is not editable does not follow it by itself.
    main.dispatch({
      effects: EditorView.scrollIntoView(main.state.selection.main.head, { y: "center" }),
    });
  };

  return {
    changes: () => getChunks(main.state)?.chunks.length ?? 0,
    nextChange: () => move(goToNextChunk),
    previousChange: () => move(goToPreviousChunk),
    destroy() {
      destroyed = true;
      unfollow();
      dispose();
    },
  };
}

const editorModule: EditorModule = { createEditor, createDiff };
export default editorModule;
