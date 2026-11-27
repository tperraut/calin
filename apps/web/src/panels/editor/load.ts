import type { EditorModule } from "./types";

/** CodeMirror arrives the first time an editor or a diff is shown, and once. */
export const loadEditorModule = (): Promise<EditorModule> =>
  import("./codemirror").then((module) => module.default);
