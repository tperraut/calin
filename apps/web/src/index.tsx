/* @refresh reload */
import { render } from "@solidjs/web";
import "./index.css";
import { App } from "./App";
import { createApi } from "./api/http";
import { createSocket } from "./api/socket";
import { createTerminalOpener } from "./api/terminal";
import { browserDevice } from "./lib/device";
import { registerWorker } from "./lib/serviceWorker";
import { createAppState } from "./state/app";

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root element");

// The development server serves modules, not a build: no worker there (remote-access spec, section 6).
const workerEnabled = import.meta.env.PROD && "serviceWorker" in navigator;
const worker = workerEnabled
  ? registerWorker(navigator.serviceWorker, {
      reload: () => location.reload(),
      onVisible: (check) =>
        document.addEventListener("visibilitychange", () => {
          if (document.visibilityState === "visible") check();
        }),
      warn: (message) => console.warn(message),
    })
  : undefined;

const state = createAppState({
  api: createApi(),
  socket: createSocket({
    onServerError: (message) => console.warn(`calin: the server rejected a frame: ${message}`),
  }),
  openTerminal: createTerminalOpener(),
  device: browserDevice(workerEnabled),
  worker,
});

render(() => <App state={state} />, root);
