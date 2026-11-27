import { createRouter } from "@solidjs/router";
import HomePage from "./pages/HomePage";
import NotFoundPage from "./pages/NotFoundPage";
import SessionPage from "./pages/SessionPage";
import { sidePanelIds } from "./panels/registry";

export const Router = createRouter({
  routes: [
    { path: "/", component: HomePage },
    // The optional segment names the open panel; anything else falls through to the 404 page.
    { path: "/s/:id/:panel?", component: SessionPage, matchFilters: { panel: sidePanelIds } },
    { path: "*404", component: NotFoundPage },
  ],
});
