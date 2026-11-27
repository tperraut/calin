// Puts the stored theme on the page before its first paint, so a dark page never
// flashes light. A classic file, not inline: the CSP allows scripts from the origin only.
// The key and values are src/lib/prefs.ts's; the rule is src/lib/theme.ts's resolveTheme.
(function () {
  var choice;
  try {
    choice = JSON.parse(localStorage.getItem("calin.theme.v1"));
  } catch {
    choice = null;
  }
  var dark =
    choice === "dark" || (choice !== "light" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
})();
