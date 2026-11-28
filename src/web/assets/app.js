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
  // view" (Board) and "Reset filters" (Documents) forget the view. A view
  // is remembered only from a page with its filter form (`form`), so an
  // error page at the same address, such as a 400 refusal, leaves the
  // remembered view alone (JD-74).
  const VIEWS = {
    board: {
      path: "/admin/board",
      form: "form[data-board-filters]",
      keys: ["project", "type", "status", "attention", "group", "sort", "dir", "collapse", "filters"],
    },
    documents: {
      path: "/admin/documents",
      form: "#documents-filters",
      keys: ["project", "type", "status", "filters", "sort", "dir", "limit"],
    },
  };
  const viewKey = (view) => `joedocs-${view}-view`;
  const storedView = (view) => {
    try {
      return localStorage.getItem(viewKey(view)) || "";
    } catch {
      return "";
    }
  };
  // The view's own keys of a query string, without empty values, copied
  // as written: a comma-separated list (`status=draft,done`, JD-71) keeps
  // its commas, which the server splits before decoding.
  const viewQuery = (view, search) => {
    const out = [];
    for (const pair of search.replace(/^\?/, "").split("&")) {
      const at = pair.indexOf("=");
      if (at < 1 || at === pair.length - 1) {
        continue;
      }
      let key;
      try {
        key = decodeURIComponent(pair.slice(0, at).replace(/\+/g, " "));
      } catch {
        continue;
      }
      if (VIEWS[view].keys.includes(key)) {
        out.push(pair);
      }
    }
    return out.join("&");
  };
  // The address of a filter form marked `data-filter-address` (Documents,
  // JD-81): the filter parts' query (comma-separated lists, defaults left
  // out), with the order written as the page's column links write it:
  // nothing for the default order (the attribute, e.g. "updated desc"),
  // else `sort`, and `dir=desc` only when descending. `limit` comes from
  // the address, as the page size has no control.
  const filterAddress = (form) => {
    const [defaultSort, defaultDir] = (form.dataset.filterAddress || "").split(" ");
    const parts = [];
    let sort = defaultSort;
    let dir = defaultDir;
    for (const pair of form.joedocsFilters.query().split("&")) {
      const [key, value] = pair.split("=");
      if (key === "sort") {
        sort = decodeURIComponent(value);
      } else if (key === "dir") {
        dir = decodeURIComponent(value);
      } else if (pair) {
        parts.push(pair);
      }
    }
    if (sort !== defaultSort || dir !== defaultDir) {
      parts.push(`sort=${encodeURIComponent(sort)}`);
      if (dir === "desc") {
        parts.push("dir=desc");
      }
    }
    const limit = new URLSearchParams(location.search).get("limit");
    if (limit) {
      parts.push(`limit=${encodeURIComponent(limit)}`);
    }
    return parts.join("&");
  };
  // htmx sends such a form's address instead of its fields (repeated keys,
  // every default), so the address it pushes is the comma form.
  document.addEventListener("htmx:config:request", (event) => {
    const ctx = event.detail?.ctx;
    const form = ctx?.sourceElement;
    if (!(form instanceof HTMLFormElement) || !form.hasAttribute("data-filter-address") || !form.joedocsFilters) {
      return;
    }
    const query = filterAddress(form);
    const path = form.getAttribute("hx-get") || location.pathname;
    ctx.request.action = query ? `${path}?${query}` : path;
    ctx.request.body = new FormData();
  });
  // The filter parts change the form without a `change` event (a chip's x,
  // Clear all, a pill's All): such a form reloads its results then too.
  document.addEventListener("filters:changed", (event) => {
    const form = event.target;
    if (form instanceof HTMLFormElement && form.hasAttribute("data-filter-address")) {
      form.requestSubmit();
    }
  });
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
  // A view's own page opened with a view in its address remembers it.
  for (const [view, { path, form }] of Object.entries(VIEWS)) {
    const own = location.pathname === path && document.querySelector(form);
    const query = own ? viewQuery(view, location.search) : "";
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
      saveView(view, viewQuery(view, location.search));
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
    // The Filters toggle of a `data-filter-address` form (Documents) opens
    // or shuts the panel, as the Board's does: `filters=closed` in the form
    // and the address, with no new results.
    const panelToggle = target.closest("form[data-filter-address] [data-filter-toggle]");
    if (panelToggle) {
      const form = panelToggle.closest("form");
      const panel = form.querySelector("[data-filter-panel]");
      const open = panelToggle.getAttribute("aria-expanded") !== "true";
      panelToggle.setAttribute("aria-expanded", String(open));
      if (panel) {
        panel.hidden = !open;
      }
      const held = form.querySelector("input[data-filters-closed]");
      if (open) {
        held?.remove();
      } else if (!held) {
        const input = document.createElement("input");
        input.type = "hidden";
        input.name = "filters";
        input.value = "closed";
        input.dataset.filtersClosed = "";
        form.append(input);
      }
      if (form.joedocsFilters) {
        const query = filterAddress(form);
        history.replaceState(history.state, "", query ? `${location.pathname}?${query}` : location.pathname);
        document.dispatchEvent(new CustomEvent("joedocs:view-changed", { detail: { view: "documents" } }));
      }
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
    // New Documents results: the View menu follows the order they are in
    // (a column header's link may have changed it), and the form then
    // holds the view, written as its address.
    if (region.id === "results" && location.pathname === VIEWS.documents.path) {
      const form = document.getElementById("documents-filters");
      const order = region.querySelector("[data-order]");
      if (form instanceof HTMLFormElement && order instanceof HTMLElement) {
        const sort = form.querySelector('select[name="sort"]');
        if (sort instanceof HTMLSelectElement) {
          sort.value = order.dataset.order;
        }
        const dir = form.querySelector(`input[name="dir"][value="${order.dataset.orderDir}"]`);
        if (dir instanceof HTMLInputElement) {
          dir.checked = true;
        }
        form.joedocsFilters?.refresh();
      }
      if (form instanceof HTMLFormElement && form.joedocsFilters) {
        saveView("documents", viewQuery("documents", filterAddress(form)));
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
