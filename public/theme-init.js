// Applies the saved theme before the first paint, so a light-theme user never sees a dark flash.
// A file rather than an inline script because the app CSP allows scripts from 'self' only.
// Kept in step with src/lib/theme.ts (same storage key and values).
(function () {
  var pref = "system";
  try {
    pref = JSON.parse(localStorage.getItem("studio-theme") || '"system"');
  } catch (e) {
    pref = "system";
  }
  var light =
    pref === "light" ||
    (pref !== "dark" && window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
})();
