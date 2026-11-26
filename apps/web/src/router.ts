import { createRouter } from "@solidjs/router";
import HomePage from "./pages/HomePage";
import NotFoundPage from "./pages/NotFoundPage";
import SessionPage from "./pages/SessionPage";

export const Router = createRouter({
  routes: [
    { path: "/", component: HomePage },
    { path: "/s/:id", component: SessionPage },
    { path: "*404", component: NotFoundPage },
  ],
});
