import { createRouter } from "@solidjs/router";
import { lazy } from "solid-js";
import HomePage from "./pages/HomePage";
import NotFoundPage from "./pages/NotFoundPage";
import SessionPage from "./pages/SessionPage";
import { sidePanelIds } from "./panels/registry";

// Rarely opened, so it is not part of what every visit downloads.
const SettingsPage = lazy(() => import("./pages/SettingsPage"));

export const Router = createRouter({
  routes: [
    { path: "/", component: HomePage },
    { path: "/settings", component: SettingsPage },
    // The optional segment names the open panel; anything else falls through to the 404 page.
    { path: "/s/:id/:panel?", component: SessionPage, matchFilters: { panel: sidePanelIds } },
    { path: "*404", component: NotFoundPage },
  ],
});
