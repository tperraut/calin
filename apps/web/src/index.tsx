/* @refresh reload */
import { render } from "@solidjs/web";
import "./index.css";
import { App } from "./App";
import { createApi } from "./api/http";
import { createSocket } from "./api/socket";
import { createTerminalOpener } from "./api/terminal";
import { createAppState } from "./state/app";

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root element");

const state = createAppState({
  api: createApi(),
  socket: createSocket({
    onServerError: (message) => console.warn(`calin: the server rejected a frame: ${message}`),
  }),
  openTerminal: createTerminalOpener(),
});

render(() => <App state={state} />, root);
