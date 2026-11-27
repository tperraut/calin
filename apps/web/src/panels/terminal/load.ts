import type { TerminalModule } from "./types";

/** xterm.js arrives with the first terminal, and once. */
export const loadTerminalModule = (): Promise<TerminalModule> =>
  import("./xterm").then((module) => module.default);
