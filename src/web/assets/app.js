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
        // A menu the opener sits in closes, so it is not left open behind.
        opener.closest("details")?.removeAttribute("open");
        dialog.showModal();
      }
      return;
    }
    const closer = target.closest("[data-dialog-close]");
    if (closer) {
      closer.closest("dialog")?.close();
    }
  });

  // Mermaid diagrams: the server sends each as its source in a
  // `.diagram.mermaid` block; Pele draws it beside the source, with links
  // and images off. If it cannot draw, the source stays shown with the
  // message (as text: it may quote the diagram).
  const drawDiagrams = (root) => {
    if (typeof pele === "undefined") {
      return;
    }
    for (const block of root.querySelectorAll(".diagram.mermaid:not([data-drawn])")) {
      block.dataset.drawn = "true";
      const source = block.querySelector(".diagram-source");
      const text = source ? source.textContent : "";
      const canvas = document.createElement("div");
      canvas.className = "diagram-canvas";
      block.prepend(canvas);
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = "secondary diagram-toggle";
      toggle.textContent = "Show source";
      toggle.setAttribute("aria-expanded", "false");
      toggle.addEventListener("click", () => {
        const shown = block.classList.toggle("show-source");
        toggle.textContent = shown ? "Hide source" : "Show source";
        toggle.setAttribute("aria-expanded", String(shown));
      });
      // Pele also writes its own error line into the canvas; the failed
      // block hides the canvas so the message is shown once.
      const failed = (error) => {
        block.classList.remove("drawn");
        block.classList.add("diagram-failed");
        toggle.remove();
        let note = block.querySelector(".diagram-error");
        if (!note) {
          note = document.createElement("p");
          note.className = "diagram-error";
          block.append(note);
        }
        note.textContent = `This diagram could not be drawn: ${error && error.message ? error.message : error}`;
      };
      try {
        const mounted = pele.mount(canvas, text, {
          links: false,
          images: false,
          onError: failed,
        });
        if (mounted && mounted.error) {
          failed(mounted.error);
        } else {
          block.classList.add("drawn");
          block.append(toggle);
        }
      } catch (error) {
        failed(error);
      }
    }
  };

  // A form marked `data-busy` shows it is working: its button is disabled
  // and says so until the next page arrives.
  document.addEventListener("submit", (event) => {
    const form = event.target instanceof HTMLFormElement ? event.target : null;
    if (!form || !form.dataset.busy) {
      return;
    }
    const button = form.querySelector("button[type=submit]");
    if (button) {
      button.setAttribute("aria-busy", "true");
      button.textContent = form.dataset.busy;
      // Disabled after this tick, so the submission itself goes ahead.
      setTimeout(() => {
        button.disabled = true;
      });
    }
    announce(form.dataset.busy);
  });

  drawDiagrams(document);

  // After htmx swaps a region in, move focus there so keyboard and
  // screen-reader users land on the new content, and announce it. htmx 4
  // fires the event on the element that made the request; the swapped
  // region is `detail.ctx.target`. A region marked `data-focus="keep"`
  // (search results, which reload while the user types) leaves focus where
  // it is.
  document.addEventListener("htmx:after:swap", (event) => {
    const target = event.detail && event.detail.ctx ? event.detail.ctx.target : null;
    const region = target instanceof HTMLElement ? target : null;
    if (!region) {
      return;
    }
    if (region.dataset.focus !== "keep") {
      if (!region.hasAttribute("tabindex")) {
        region.setAttribute("tabindex", "-1");
      }
      region.focus({ preventScroll: false });
    }
    drawDiagrams(region);
    const label = region.dataset.announce;
    if (label) {
      announce(label);
    }
  });
})();
