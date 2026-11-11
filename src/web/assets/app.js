// JoeDocs admin web interface. Loaded with `defer` from the same origin;
// the Content-Security-Policy allows no inline script and no eval, so all
// behaviour is wired here with event listeners.
"use strict";

(() => {
  const THEME_KEY = "joedocs-theme";

  // localStorage holds the theme choice and nothing else. It can be
  // unavailable (private windows, blocked storage); the page works anyway.
  const storedTheme = () => {
    try {
      return localStorage.getItem(THEME_KEY);
    } catch {
      return null;
    }
  };

  const applyTheme = (theme) => {
    if (theme === "light" || theme === "dark") {
      document.documentElement.dataset.theme = theme;
    } else {
      delete document.documentElement.dataset.theme;
    }
  };

  applyTheme(storedTheme());

  const toggleTheme = () => {
    const current =
      document.documentElement.dataset.theme ||
      (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
    const next = current === "dark" ? "light" : "dark";
    applyTheme(next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {
      // The choice lasts for this page only.
    }
  };

  const announce = (text) => {
    const region = document.getElementById("live-region");
    if (region) {
      region.textContent = text;
    }
  };

  document.addEventListener("click", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target) {
      return;
    }
    if (target.closest("[data-theme-toggle]")) {
      toggleTheme();
      return;
    }
    const copy = target.closest("[data-copy]");
    if (copy) {
      const source = document.getElementById(copy.dataset.copy);
      if (source && navigator.clipboard) {
        navigator.clipboard.writeText(source.textContent.trim()).then(
          () => announce("Copied"),
          () => announce("Could not copy; select the text and copy it"),
        );
      }
      return;
    }
    const opener = target.closest("[data-dialog-open]");
    if (opener) {
      const dialog = document.getElementById(opener.dataset.dialogOpen);
      if (dialog instanceof HTMLDialogElement) {
        dialog.showModal();
      }
      return;
    }
    const closer = target.closest("[data-dialog-close]");
    if (closer) {
      closer.closest("dialog")?.close();
    }
  });

  // After htmx swaps a region in, move focus there so keyboard and
  // screen-reader users land on the new content, and announce it.
  document.addEventListener("htmx:after:swap", (event) => {
    const region = event.target instanceof HTMLElement ? event.target : null;
    if (!region) {
      return;
    }
    if (!region.hasAttribute("tabindex")) {
      region.setAttribute("tabindex", "-1");
    }
    region.focus({ preventScroll: false });
    const label = region.dataset.announce;
    if (label) {
      announce(label);
    }
  });
})();
