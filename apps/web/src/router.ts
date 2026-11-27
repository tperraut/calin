import { createRouter } from "@solidjs/router";
import { lazy } from "solid-js";
import HomePage from "./pages/HomePage";
import NotFoundPage from "./pages/NotFoundPage";
import SessionPage from "./pages/SessionPage";
import { sidePanelIds } from "./panels/registry";

// Rarely opened, so it is not part of what every visit downloads.
const SettingsPage = lazy(() => import("./pages/SettingsPage"));
const UsagePage = lazy(() => import("./pages/UsagePage"));
const AutomationsPage = lazy(() => import("./pages/AutomationsPage"));
const AutomationPage = lazy(() => import("./pages/AutomationPage"));

export const Router = createRouter({
  routes: [
    { path: "/", component: HomePage },
    { path: "/settings", component: SettingsPage },
    { path: "/usage", component: UsagePage },
    { path: "/automations", component: AutomationsPage },
    // `new` and an automation's id share the editor; the static segment wins (router scoring).
    { path: "/automations/new", component: AutomationPage },
    { path: "/automations/:id", component: AutomationPage },
    // The optional segment names the open panel; anything else falls through to the 404 page.
    { path: "/s/:id/:panel?", component: SessionPage, matchFilters: { panel: sidePanelIds } },
    { path: "*404", component: NotFoundPage },
  ],
});
