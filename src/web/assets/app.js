// JoeDocs admin web interface. Loaded with `defer` from the same origin;
// the Content-Security-Policy allows no inline script and no eval, so all
// behaviour is wired here with event listeners.
"use strict";

(() => {
  // Scripts run: features that only work with them (filters that apply as
  // they change) can hide what is there for browsers without them.
  document.documentElement.classList.add("js");

  const THEME_KEY = "joedocs-theme";
  // Whether buttons open dialogs by themselves (invoker commands).
  const NATIVE_COMMANDS = "command" in HTMLButtonElement.prototype;

  // localStorage holds the theme choice and the remembered Board and
  // Documents views; sessionStorage holds the Files panel's folds (below).
  // Either can be unavailable (private windows, blocked storage); the page
  // works anyway, and nothing is remembered.
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

  // The navigation is open in the page, so it shows without this script;
  // on a narrow screen it folds behind its Menu control, and opens again
  // when the screen is wide enough to show it beside the page. It is looked
  // up each time: Back and Forward between pages htmx added to the history
  // swap in a new body, with a new navigation.
  const narrow = matchMedia("(max-width: 720px)");
  const fitNav = () => {
    const navMenu = document.querySelector(".nav-menu");
    if (navMenu instanceof HTMLDetailsElement) {
      navMenu.open = !narrow.matches;
    }
  };
  fitNav();
  narrow.addEventListener("change", fitNav);

  // The back-to-top button (layout.html) shows once the page has scrolled
  // more than one window height. It is looked up each time, as the
  // navigation is: Back and Forward swap in a new body.
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
  let toTopQueued = false;
  const fitToTop = () => {
    toTopQueued = false;
    const button = document.querySelector("[data-to-top]");
    if (button instanceof HTMLElement) {
      button.hidden = window.scrollY <= window.innerHeight;
    }
  };
  const queueToTop = () => {
    if (!toTopQueued) {
      toTopQueued = true;
      requestAnimationFrame(fitToTop);
    }
  };
  addEventListener("scroll", queueToTop, { passive: true });
  addEventListener("resize", queueToTop, { passive: true });
  fitToTop();

  // Back to the top, with focus on the content as the skip link gives it.
  const toTop = () => {
    scrollTo({ top: 0, behavior: reduceMotion.matches ? "auto" : "smooth" });
    document.getElementById("main")?.focus({ preventScroll: true });
  };

  // The Files panel of a multi-file document: each directory folds. What
  // was opened or closed is kept for the tab (sessionStorage), per
  // document, as `{ "<dir path>": true | false }`; a directory holding the
  // file shown is always open. Unavailable storage changes nothing.
  const filesKey = (panel) => `joedocs-files-${panel.dataset.files}`;
  const readFolds = (panel) => {
    try {
      const folds = JSON.parse(sessionStorage.getItem(filesKey(panel)) || "{}");
      return folds && typeof folds === "object" ? folds : {};
    } catch {
      return {};
    }
  };
  const setUpFiles = (root) => {
    for (const panel of root.querySelectorAll("[data-files]")) {
      const folds = readFolds(panel);
      for (const dir of panel.querySelectorAll("details[data-dir]")) {
        const open = folds[dir.dataset.dir];
        if (typeof open === "boolean" && !dir.hasAttribute("data-holds-current")) {
          dir.open = open;
        }
      }
    }
  };
  // `toggle` does not bubble, so it is caught on the way down.
  document.addEventListener(
    "toggle",
    (event) => {
      const dir = event.target instanceof HTMLDetailsElement ? event.target : null;
      const panel = dir?.dataset.dir !== undefined ? dir.closest("[data-files]") : null;
      if (!panel) {
        return;
      }
      const folds = readFolds(panel);
      folds[dir.dataset.dir] = dir.open;
      try {
        sessionStorage.setItem(filesKey(panel), JSON.stringify(folds));
      } catch {
        // Kept for this page only.
      }
    },
    true,
  );

  // Remembered views: the Board and Documents keep their last filters and
  // order as a query string in localStorage (`joedocs-<view>-view`), and
  // the menu links marked `data-remember` lead back to it. An address with
  // a query shows what it says; a bare one shows the defaults. "Reset
  // filters" forgets the view.
  const VIEWS = {
    board: {
      path: "/admin/board",
      keys: ["project", "type", "status", "attention", "group", "sort", "dir", "collapse", "filters"],
    },
    documents: { path: "/admin/documents", keys: ["project", "type", "status", "sort", "dir", "limit"] },
  };
  const viewKey = (view) => `joedocs-${view}-view`;
  const storedView = (view) => {
    try {
      return localStorage.getItem(viewKey(view)) || "";
    } catch {
      return "";
    }
  };
  // The view's own keys of `params`, without empty values.
  const viewQuery = (view, params) => {
    const out = new URLSearchParams();
    for (const [key, value] of params) {
      if (VIEWS[view].keys.includes(key) && value !== "") {
        out.append(key, value);
      }
    }
    return out.toString();
  };
  const rememberLinks = () => {
    for (const link of document.querySelectorAll("a[data-remember]")) {
      const view = VIEWS[link.dataset.remember];
      if (view) {
        const query = storedView(link.dataset.remember);
        link.href = query ? `${view.path}?${query}` : view.path;
      }
    }
  };
  const saveView = (view, query) => {
    try {
      if (query) {
        localStorage.setItem(viewKey(view), query);
      } else {
        localStorage.removeItem(viewKey(view));
      }
    } catch {
      // Not remembered.
    }
    rememberLinks();
  };
  // A page opened with a view in its address remembers it.
  for (const [view, { path }] of Object.entries(VIEWS)) {
    const query = location.pathname === path ? viewQuery(view, new URLSearchParams(location.search)) : "";
    if (query) {
      saveView(view, query);
    }
  }
  rememberLinks();
  // The Board's script says when its view changed (it has put it in the
  // address by then).
  document.addEventListener("joedocs:view-changed", (event) => {
    const view = event.detail?.view;
    if (VIEWS[view]) {
      saveView(view, viewQuery(view, new URLSearchParams(location.search)));
    }
  });

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
    if (target.closest("[data-to-top]")) {
      toTop();
      return;
    }
    const reset = target.closest("[data-reset-view]");
    if (reset && VIEWS[reset.dataset.resetView]) {
      saveView(reset.dataset.resetView, "");
      // A link goes to the bare page by itself.
      if (!(reset instanceof HTMLAnchorElement)) {
        location.assign(VIEWS[reset.dataset.resetView].path);
      }
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
    // Dialogs open with the browser's own invoker commands
    // (`commandfor`, `command="show-modal"`) and close with
    // `formmethod="dialog"`, so they work without this script. It only
    // closes a menu the opener sits in, and opens the dialog itself in a
    // browser without invoker commands.
    const opener = target.closest("[commandfor][command=show-modal]");
    if (opener) {
      opener.closest("details")?.removeAttribute("open");
      const dialog = document.getElementById(opener.getAttribute("commandfor"));
      if (!NATIVE_COMMANDS && dialog instanceof HTMLDialogElement && !dialog.open) {
        dialog.showModal();
      }
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

  // A form marked `data-busy` shows it is working: the button that sent it
  // is disabled and says so until the next page arrives. A submission that
  // only closes a dialog (Cancel, `formmethod="dialog"`) is not work.
  document.addEventListener("submit", (event) => {
    const form = event.target instanceof HTMLFormElement ? event.target : null;
    if (!form || !form.dataset.busy) {
      return;
    }
    const button = event.submitter instanceof HTMLButtonElement ? event.submitter : null;
    const closesDialog =
      form.getAttribute("method") === "dialog" ||
      (button && button.getAttribute("formmethod") === "dialog");
    if (closesDialog) {
      return;
    }
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

  // Tables marked `data-sortable` show every row, so they sort here: each
  // header marked `data-sort` becomes a button that cycles ascending,
  // descending and the order the server sent. A cell sorts by its
  // `data-sort-value` when it has one (times), a `.num` column as numbers,
  // and any other as text with numbers in it compared by value (JD-9
  // before JD-10). Without this script the headers are plain text.
  const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
  const cellValue = (row, index) => {
    const cell = row.cells[index];
    if (!cell) {
      return "";
    }
    return cell.dataset.sortValue !== undefined ? cell.dataset.sortValue : cell.textContent.trim();
  };
  const setUpSorting = (root) => {
    for (const table of root.querySelectorAll("table[data-sortable]:not([data-sort-ready])")) {
      table.dataset.sortReady = "true";
      const body = table.tBodies[0];
      const head = table.tHead ? table.tHead.rows[0] : null;
      if (!body || !head) {
        continue;
      }
      const original = [...body.rows];
      const headers = [...head.cells];
      headers.forEach((th, index) => {
        if (!th.hasAttribute("data-sort")) {
          return;
        }
        const label = th.textContent.trim();
        const button = document.createElement("button");
        button.type = "button";
        button.className = "sort-button";
        button.textContent = label;
        th.replaceChildren(button);
        const numeric = th.classList.contains("num");
        button.addEventListener("click", () => {
          const state = th.getAttribute("aria-sort");
          const next =
            state === "ascending" ? "descending" : state === "descending" ? null : "ascending";
          for (const other of headers) {
            other.removeAttribute("aria-sort");
          }
          const rows = original.slice();
          if (next) {
            th.setAttribute("aria-sort", next);
            const sign = next === "ascending" ? 1 : -1;
            rows.sort((a, b) => {
              const x = cellValue(a, index);
              const y = cellValue(b, index);
              const order = numeric ? (Number(x) || 0) - (Number(y) || 0) : collator.compare(x, y);
              return sign * order;
            });
          }
          body.append(...rows);
          announce(next ? `Sorted by ${label}, ${next}` : "Original order");
        });
      });
    }
  };

  drawDiagrams(document);
  setUpSorting(document);
  setUpFiles(document);

  // A live document page (document.js) swapped in its latest revision.
  document.addEventListener("joedocs:swapped", (event) => {
    const region = event.target instanceof Element ? event.target : null;
    if (region) {
      drawDiagrams(region);
      setUpSorting(region);
      setUpFiles(region);
    }
  });

  // After htmx swaps a region in, move focus there so keyboard and
  // screen-reader users land on the new content, and announce it. htmx 4
  // fires the event on the element that made the request; the swapped
  // region is `detail.ctx.target`. A region marked `data-focus="keep"`
  // (search results, which reload while the user types) leaves focus where
  // it is.
  // A control marked `data-focus-key` that htmx replaces (a sort header
  // link inside the results it reloads) gets focus back on its new copy.
  let focusKey = null;
  document.addEventListener("focusin", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    const keyed = target ? target.closest("[data-focus-key]") : null;
    focusKey = keyed ? keyed.dataset.focusKey : null;
  });

  document.addEventListener("htmx:after:swap", (event) => {
    const target = event.detail && event.detail.ctx ? event.detail.ctx.target : null;
    const region = target instanceof HTMLElement ? target : null;
    if (!region) {
      return;
    }
    const lost = !document.activeElement || document.activeElement === document.body;
    const again = focusKey
      ? [...region.querySelectorAll("[data-focus-key]")].find((el) => el.dataset.focusKey === focusKey)
      : null;
    if (lost && again instanceof HTMLElement) {
      again.focus({ preventScroll: true });
    } else if (region.dataset.focus !== "keep") {
      if (!region.hasAttribute("tabindex")) {
        region.setAttribute("tabindex", "-1");
      }
      region.focus({ preventScroll: false });
    }
    // A whole new body (Back or Forward) brings a navigation that has not
    // been fitted to the screen. Results alone leave the Menu as it is.
    if (region === document.body) {
      fitNav();
      fitToTop();
      rememberLinks();
    }
    // New Documents results: the form (with the sort its results carry)
    // holds the view; `limit` only comes from the address.
    if (region.id === "results" && location.pathname === VIEWS.documents.path) {
      const form = document.getElementById("documents-filters");
      if (form instanceof HTMLFormElement) {
        const params = new URLSearchParams(new FormData(form));
        const limit = new URLSearchParams(location.search).get("limit");
        if (limit) {
          params.set("limit", limit);
        }
        saveView("documents", viewQuery("documents", params));
      }
    }
    drawDiagrams(region);
    setUpSorting(region);
    setUpFiles(region);
    // Results can say what changed (the Documents order); otherwise the
    // region's own label is announced.
    const said = region.querySelector(":scope > [data-announcement]");
    const label = said ? said.textContent : region.dataset.announce;
    if (label) {
      announce(label);
    }
  });
})();
