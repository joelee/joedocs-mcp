// JoeDocs Board (0.5.5): keeps the Board live. Loaded with `defer` by the
// Board page only. The Content-Security-Policy allows no inline script or
// style: styles that move with the content (`view-transition-name`, popover
// positions) are set through the CSSOM, which `style-src` does not limit.
//
// The stream (`/admin/board/stream`) says only that something changed; the
// grid is then fetched again (`?fragment=1`), at most once per
// `admin.board_refresh_ms` and never two at once, and applied inside a view
// transition. Each card stays the same element, so it floats to its new
// column and keeps focus; only cards that moved, changed, came or went are
// named, because naming every card is too slow on a large board.
"use strict";

(() => {
  const board = document.querySelector("[data-board]");
  if (!board) {
    return;
  }
  const reduced = matchMedia("(prefers-reduced-motion: reduce)");
  const refreshMs = Number(board.dataset.refreshMs) || 1000;
  const previewMs = Number(board.dataset.previewMs);
  let source = null;
  let opens = 0;
  let timer = null;
  let lastRefresh = -Infinity;
  let busy = false;
  // Set while a fetch is in flight: an automatic refresh (`again`) or a
  // filter change (`againInstant`) is waiting to run after it.
  let again = false;
  let againInstant = false;
  let paused = false;
  let waiting = false;
  let eventAt = 0;
  // A failed grid fetch is retried after 2 s, doubling to 30 s; a fetch
  // slower than 10 s counts as failed (JD-49 D-01).
  const RETRY_FIRST_MS = 2000;
  const RETRY_MAX_MS = 30000;
  const FETCH_TIMEOUT_MS = 10000;
  let retryDelay = 0;
  let retryTimer = null;
  // Signed out: nothing more is fetched until the page is reloaded.
  let ended = false;
  // Counters shown with `?debug=1` (developer use).
  const stats = {
    events: 0, refreshes: 0, transitions: 0, skipped: 0, latency: 0, maxLatency: 0,
    frames: 0, longFrames: 0, worstFrame: 0, cards: 0,
  };
  const debug = () => {
    const out = document.querySelector("[data-board-debug]");
    if (out) {
      out.textContent = Object.entries(stats).map(([k, v]) => `${k}: ${Math.round(v)}`).join("  ");
    }
  };

  const live = (state, text) => {
    const el = document.querySelector("[data-board-live]");
    if (el) {
      el.dataset.state = state;
      el.textContent = text;
    }
  };
  const say = (text) => {
    const region = document.getElementById("live-region");
    if (region) {
      region.textContent = text;
    }
  };
  const grid = () => board.querySelector("[data-board-grid]");

  // --- Relative times ------------------------------------------------

  const ago = new Intl.RelativeTimeFormat(undefined, { numeric: "auto", style: "short" });
  const fillAgo = (root) => {
    const now = Date.now();
    for (const el of root.querySelectorAll("time[data-ago]")) {
      const then = Date.parse(el.getAttribute("datetime"));
      if (Number.isNaN(then)) {
        continue;
      }
      const s = Math.round((then - now) / 1000);
      const abs = Math.abs(s);
      el.textContent =
        abs < 60 ? ago.format(s, "second")
        : abs < 3600 ? ago.format(Math.round(s / 60), "minute")
        : abs < 86400 ? ago.format(Math.round(s / 3600), "hour")
        : abs < 86400 * 30 ? ago.format(Math.round(s / 86400), "day")
        : abs < 86400 * 365 ? ago.format(Math.round(s / (86400 * 30)), "month")
        : ago.format(Math.round(s / (86400 * 365)), "year");
    }
  };

  // --- The filter band -----------------------------------------------

  const form = document.querySelector("[data-board-filters]");
  // The attention switch's count: how many documents of the current view
  // need attention, as the grid says (`data-lit`, folded rows included),
  // so it never goes stale on a live refresh.
  const countLit = () => {
    const badge = form?.querySelector("[data-attention-count]");
    if (!badge) {
      return;
    }
    const lit = Number(grid()?.dataset.lit || 0);
    badge.textContent = String(lit);
    badge.hidden = lit === 0;
  };

  // --- The stream ----------------------------------------------------

  const close = () => {
    if (source) {
      source.close();
      source = null;
    }
  };

  const signedOut = () => {
    ended = true;
    close();
    clearTimeout(retryTimer);
    live("offline", "Signed out: reload to sign in");
  };
  const loginPage = (res) => res.redirected && new URL(res.url).pathname === "/admin/login";

  // After the stream ends for good: a lapsed session (the sign-in page), a
  // server with every place taken (503), or one that is away.
  const diagnose = () => {
    const controller = new AbortController();
    fetch(streamUrl(), { credentials: "same-origin", signal: controller.signal }).then(
      (res) => {
        controller.abort();
        if (loginPage(res)) {
          signedOut();
        } else if (res.status === 503) {
          live("offline", "Too many open live pages; close one and reload");
        } else {
          live("offline", "Disconnected, retrying");
          setTimeout(open, 5000);
        }
      },
      () => {
        live("offline", "Disconnected, retrying");
        setTimeout(open, 5000);
      },
    );
  };

  const streamUrl = () => {
    const marker = grid()?.dataset.marker ?? "";
    return `/admin/board/stream?marker=${encodeURIComponent(marker)}`;
  };

  function open() {
    if (source || !board.isConnected || document.hidden) {
      return;
    }
    source = new EventSource(streamUrl());
    live("connecting", "Connecting");
    source.onopen = () => {
      opens += 1;
      live(paused ? "paused" : "live", paused ? "Paused" : "Live");
      // After a reconnect, changes may have been missed.
      if (opens > 1) {
        refreshSoon();
      }
    };
    source.onmessage = () => {
      stats.events += 1;
      eventAt = performance.now();
      refreshSoon();
    };
    source.addEventListener("expired", signedOut);
    source.onerror = () => {
      if (!source) {
        return;
      }
      if (source.readyState === EventSource.CLOSED) {
        close();
        diagnose();
      } else {
        live("connecting", "Reconnecting");
      }
    };
  }

  const refreshSoon = () => {
    if (paused) {
      waiting = true;
      live("paused", "Paused, changes waiting");
      return;
    }
    if (timer) {
      return;
    }
    const wait = Math.max(0, lastRefresh + refreshMs - performance.now());
    timer = setTimeout(() => {
      timer = null;
      refresh(false);
    }, wait);
  };

  // The grid alone, for the address as written: rebuilding it with
  // URLSearchParams would encode the commas of a list (`project=a,b`) as
  // `%2C`, which the server reads as one value.
  const fragmentUrl = () => {
    const query = location.search.replace(/^\?/, "").split("&").filter((p) => p && !p.startsWith("fragment="));
    query.push("fragment=1");
    return `/admin/board?${query.join("&")}`;
  };

  // A failed fetch is tried again, backing off, until one succeeds. While
  // paused or hidden it waits: Resume or showing the page catches up.
  const retryLater = () => {
    if (ended) {
      return;
    }
    retryDelay = retryDelay ? Math.min(retryDelay * 2, RETRY_MAX_MS) : RETRY_FIRST_MS;
    clearTimeout(retryTimer);
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (document.hidden) {
        return;
      }
      if (againInstant) {
        refresh(true);
      } else if (paused) {
        waiting = true;
      } else {
        refresh(false);
      }
    }, retryDelay);
  };

  // `instant`: a filter change, the user's own action, applied at once and
  // without animation even while paused. An automatic refresh that arrives
  // while paused is held: Resume fetches the latest grid.
  //
  // One fetch at a time. A refresh asked for while one is in flight waits
  // for it: a filter change as `againInstant`, run next as a filter change
  // (paused or not, reading the address as it is then, so several changes
  // need one fetch); an automatic one as `again`, through `refreshSoon`. A
  // failed filter fetch keeps `againInstant` for its retry.
  function refresh(instant) {
    if (busy) {
      if (instant) {
        againInstant = true;
      } else {
        again = true;
      }
      return;
    }
    if (instant) {
      // This fetch covers every change asked for until now.
      againInstant = false;
      again = false;
    }
    busy = true;
    lastRefresh = performance.now();
    stats.refreshes += 1;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    let fetched = false;
    fetch(fragmentUrl(), { credentials: "same-origin", signal: controller.signal })
      .then((res) => {
        if (loginPage(res)) {
          signedOut();
          return null;
        }
        if (!res.ok) {
          throw new Error(`HTTP ${res.status}`);
        }
        return res.text();
      })
      .then((html) => {
        if (!html) {
          return null;
        }
        fetched = true;
        if (paused && !instant) {
          waiting = true;
          live("paused", "Paused, changes waiting");
          return null;
        }
        return apply(html, instant);
      })
      .catch(() => {
        fetched = false;
        live("offline", "Disconnected, retrying");
      })
      .finally(() => {
        clearTimeout(timeout);
        busy = false;
        if (fetched) {
          retryDelay = 0;
          clearTimeout(retryTimer);
          if (paused) {
            live("paused", waiting ? "Paused, changes waiting" : "Paused");
          } else if (source?.readyState === EventSource.OPEN) {
            live("live", "Live");
          }
        } else {
          if (instant) {
            againInstant = true;
          }
          retryLater();
        }
        if (againInstant && fetched) {
          refresh(true);
        } else if (again) {
          again = false;
          refreshSoon();
        }
        debug();
      });
  }

  // --- Applying a new grid -------------------------------------------

  const CARD_DATA = ["rev", "status", "row", "attention", "building"];
  const copyCard = (from, to) => {
    to.className = from.className;
    for (const key of CARD_DATA) {
      if (from.dataset[key] === undefined) {
        delete to.dataset[key];
      } else {
        to.dataset[key] = from.dataset[key];
      }
    }
    to.innerHTML = from.innerHTML;
  };

  // Counts frames over 50 ms while a transition runs (`?debug=1`).
  const watchFrames = (done) => {
    let last = performance.now();
    let running = true;
    done.finally(() => {
      running = false;
    });
    const step = (now) => {
      const gap = now - last;
      last = now;
      stats.frames += 1;
      if (gap > 50) {
        stats.longFrames += 1;
      }
      stats.worstFrame = Math.max(stats.worstFrame, gap);
      if (running) {
        requestAnimationFrame(step);
      }
    };
    requestAnimationFrame(step);
  };

  function apply(html, instant) {
    const current = grid();
    const template = document.createElement("template");
    template.innerHTML = html.trim();
    const next = template.content.querySelector("[data-board-grid]");
    if (!current || !next) {
      return Promise.resolve();
    }
    fillAgo(next);
    const old = new Map([...current.querySelectorAll(".bcard")].map((c) => [c.dataset.id, c]));
    const plan = [];
    const moved = [];
    const changed = [];
    const added = [];
    for (const card of next.querySelectorAll(".bcard")) {
      const before = old.get(card.dataset.id);
      if (!before) {
        added.push(card);
        continue;
      }
      old.delete(card.dataset.id);
      const shifted = before.dataset.status !== card.dataset.status || before.dataset.row !== card.dataset.row;
      const differs = ["rev", "attention", "building"].some(
        (key) => (before.dataset[key] ?? "") !== (card.dataset[key] ?? ""),
      );
      if (shifted) {
        moved.push(before);
      } else if (differs) {
        changed.push(before);
      }
      plan.push([card, before, shifted || differs]);
    }
    const removed = [...old.values()];
    const active = document.activeElement instanceof Element ? document.activeElement : null;
    const focusedCard = active?.closest(".bcard");
    const focusedId = focusedCard ? focusedCard.dataset.id : null;
    const onMenu = Boolean(active?.matches("[data-card-menu]"));
    const focusedFold = active?.closest("[data-fold]")?.dataset.fold ?? null;

    const update = () => {
      for (const [card, before, renewed] of plan) {
        if (renewed) {
          copyCard(card, before);
        }
        card.replaceWith(before);
      }
      current.replaceWith(next);
    };
    const finish = () => {
      for (const el of board.querySelectorAll(".bcard")) {
        el.style.viewTransitionName = "";
      }
      if (!instant) {
        for (const el of [...moved, ...changed, ...added]) {
          el.classList.add("bcard-flash");
          el.addEventListener("animationend", () => el.classList.remove("bcard-flash"), { once: true });
        }
      }
      // Focus stays on the same card when it moves, or goes to the board
      // when the card leaves.
      if (focusedId && (!document.activeElement || document.activeElement === document.body)) {
        const card = board.querySelector(`.bcard[data-id="${CSS.escape(focusedId)}"]`);
        const target = card?.querySelector(onMenu ? "[data-card-menu]" : ".bcard-link");
        if (target) {
          target.focus({ preventScroll: true });
        } else {
          board.setAttribute("tabindex", "-1");
          board.focus({ preventScroll: true });
        }
      }
      // A row's fold toggle is replaced with the grid: focus goes to the
      // new one.
      if (focusedFold !== null && (!document.activeElement || document.activeElement === document.body)) {
        board.querySelector(`[data-fold="${CSS.escape(focusedFold)}"]`)?.focus({ preventScroll: true });
      }
      stats.cards = board.querySelectorAll(".bcard").length;
      if (eventAt && !instant) {
        stats.latency = performance.now() - eventAt;
        stats.maxLatency = Math.max(stats.maxLatency, stats.latency);
        eventAt = 0;
      }
      countLit();
      if (instant) {
        // A note from the filter parts ("Statuses: back to open") is said
        // with the count, which would otherwise replace it.
        const note = form?.dataset.filterNote;
        if (form) {
          delete form.dataset.filterNote;
        }
        say(note ? `${note}. Board: ${stats.cards} documents` : `Board: ${stats.cards} documents`);
      } else {
        const parts = [];
        if (added.length) parts.push(`${added.length} new`);
        if (moved.length) parts.push(`${moved.length} moved`);
        if (changed.length) parts.push(`${changed.length} changed`);
        if (removed.length) parts.push(`${removed.length} gone`);
        if (parts.length) {
          say(`Board: ${parts.join(", ")}`);
        }
      }
      debug();
    };

    const animate =
      !instant &&
      typeof document.startViewTransition === "function" &&
      !reduced.matches &&
      (moved.length || changed.length || added.length || removed.length);
    if (!animate) {
      update();
      finish();
      return Promise.resolve();
    }
    for (const el of [...moved, ...changed, ...removed]) {
      el.style.viewTransitionName = `bc-${CSS.escape(el.dataset.id)}`;
    }
    stats.transitions += 1;
    const transition = document.startViewTransition(update);
    transition.ready.catch(() => {
      stats.skipped += 1;
    });
    watchFrames(transition.finished);
    return transition.finished.then(finish, finish);
  }

  // --- Menus and the preview -----------------------------------------

  const cardMenu = () => document.querySelector("[data-board-menu]");
  const boardMenu = () => document.querySelector("[data-board-actions-menu]");
  const previewBox = () => document.querySelector("[data-board-preview]");
  let menuCard = null;
  let menuButton = null;

  // Below `anchor` (above when there is no room), inside the window.
  const below = (box, anchor) => {
    const r = anchor.getBoundingClientRect();
    const left = Math.max(8, Math.min(r.left, innerWidth - box.offsetWidth - 8));
    const down = r.bottom + 6;
    const top = down + box.offsetHeight > innerHeight - 8 ? Math.max(8, r.top - box.offsetHeight - 6) : down;
    box.style.left = `${left}px`;
    box.style.top = `${top}px`;
  };
  // Beside `card` (right, else left), so it covers no other card in its
  // column; below it on a narrow screen.
  const beside = (box, card) => {
    const r = card.getBoundingClientRect();
    const w = box.offsetWidth;
    const top = Math.max(8, Math.min(r.top, innerHeight - box.offsetHeight - 8));
    if (r.right + 8 + w <= innerWidth - 8) {
      box.style.left = `${r.right + 8}px`;
      box.style.top = `${top}px`;
    } else if (r.left - 8 - w >= 8) {
      box.style.left = `${r.left - 8 - w}px`;
      box.style.top = `${top}px`;
    } else {
      below(box, card);
    }
  };

  const copy = (text, what) => {
    if (!navigator.clipboard) {
      say("Copying needs HTTPS or this machine");
      return;
    }
    navigator.clipboard.writeText(text).then(
      () => say(`Copied ${what}`),
      () => say("Could not copy"),
    );
  };

  const openMenu = (box, opener) => {
    hidePreview();
    box.showPopover();
    below(box, opener);
    opener.setAttribute("aria-expanded", "true");
    box.querySelector("[role=menuitem]")?.focus();
  };
  const closeMenu = (box, opener) => {
    if (box?.matches(":popover-open")) {
      box.hidePopover();
    }
    if (opener?.isConnected) {
      opener.focus();
    }
  };

  let hoverTimer = null;
  let leaveTimer = null;
  let hoverCard = null;
  let previewCard = null;

  // Closes the preview box only; a card waiting for its own keeps waiting.
  const closePreview = () => {
    const box = previewBox();
    if (box?.matches(":popover-open")) {
      box.hidePopover();
    }
    previewCard = null;
  };
  // Closes the preview and forgets any card waiting to show one.
  const hidePreview = () => {
    clearTimeout(hoverTimer);
    clearTimeout(leaveTimer);
    hoverCard = null;
    closePreview();
  };

  const showPreview = (card) => {
    const box = previewBox();
    if (!box || !card?.isConnected) {
      return;
    }
    previewCard = card;
    fetch(`/admin/board/preview/${encodeURIComponent(card.dataset.id)}`, { credentials: "same-origin" })
      .then((res) => {
        if (loginPage(res)) {
          signedOut();
          return null;
        }
        return res.ok ? res.text() : null;
      })
      .then((html) => {
        if (!html || previewCard !== card) {
          return;
        }
        box.innerHTML = html;
        fillAgo(box);
        box.showPopover();
        beside(box, card);
      })
      .catch(() => {});
  };

  // Hover anywhere on a card, or focus on its link, for the configured
  // time opens its preview. Leaving one card must not cancel the next
  // card's wait (the cause of "only on the action button" in the spike).
  // `by`: "pointer" or "focus". A preview waiting on focus is cancelled
  // when focus leaves its card; one waiting on the pointer is not.
  let hoverBy = null;
  const startHover = (card, by) => {
    clearTimeout(leaveTimer);
    if (!previewMs || previewCard === card || hoverCard === card) {
      return;
    }
    if (previewCard) {
      closePreview();
    }
    hoverCard = card;
    hoverBy = by;
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => {
      hoverCard = null;
      showPreview(card);
    }, previewMs);
  };
  const endHover = () => {
    clearTimeout(hoverTimer);
    hoverCard = null;
    clearTimeout(leaveTimer);
    leaveTimer = setTimeout(() => {
      const over = (el) => el?.matches(":hover");
      if (previewCard && !over(previewBox()) && !over(previewCard)) {
        closePreview();
      }
    }, 300);
  };

  document.addEventListener("pointerover", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target || event.pointerType !== "mouse") {
      return;
    }
    const card = target.closest(".bcard");
    if (card && board.contains(card)) {
      startHover(card, "pointer");
    } else if (target.closest("[data-board-preview]")) {
      clearTimeout(leaveTimer);
    }
  });
  document.addEventListener("pointerout", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    const from = target?.closest(".bcard, [data-board-preview]");
    const to = event.relatedTarget instanceof Element ? event.relatedTarget.closest(".bcard, [data-board-preview]") : null;
    if (from && from !== to) {
      endHover();
    }
  });
  document.addEventListener("focusin", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (target?.matches(".bcard-link") && board.contains(target)) {
      startHover(target.closest(".bcard"), "focus");
    } else if (!target?.closest("[data-board-preview], [data-board-menu]")) {
      if (previewCard || (hoverCard && hoverBy === "focus")) {
        hidePreview();
      }
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      hidePreview();
    }
    const box = [cardMenu(), boardMenu()].find((m) => m?.matches(":popover-open"));
    if (box && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      event.preventDefault();
      const items = [...box.querySelectorAll("[role=menuitem]")];
      const at = items.indexOf(document.activeElement);
      const step = event.key === "ArrowDown" ? 1 : -1;
      items[(at + step + items.length) % items.length].focus();
    }
  });
  // A menu closed by Escape or a click elsewhere: its button says so.
  document.addEventListener("toggle", (event) => {
    if (event.newState !== "closed") {
      return;
    }
    if (event.target === cardMenu()) {
      menuButton?.setAttribute("aria-expanded", "false");
    } else if (event.target === boardMenu()) {
      document.querySelector("[data-board-actions]")?.setAttribute("aria-expanded", "false");
    }
  }, true);

  // --- Clicks and filters --------------------------------------------

  document.addEventListener("click", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target) {
      return;
    }
    const pause = target.closest("[data-board-pause]");
    if (pause) {
      paused = !paused;
      pause.setAttribute("aria-pressed", String(paused));
      pause.textContent = paused ? "Resume" : "Pause";
      // A refresh already queued waits for Resume; one in flight is held
      // when it arrives (refresh). A filter change's retry still runs.
      if (paused && timer) {
        clearTimeout(timer);
        timer = null;
        waiting = true;
      }
      if (paused && retryTimer && !againInstant) {
        clearTimeout(retryTimer);
        retryTimer = null;
        waiting = true;
      }
      if (paused) {
        live("paused", waiting ? "Paused, changes waiting" : "Paused");
      } else {
        live("live", "Live");
        if (waiting) {
          waiting = false;
          refresh(false);
        }
      }
      return;
    }
    // The Filters toggle opens or shuts the panel: `filters=closed` in the
    // form keeps it in the address, as folds are, with no new grid.
    const filterToggle = target.closest("[data-filter-toggle]");
    if (filterToggle && form) {
      const panel = form.querySelector("[data-filter-panel]");
      const open = filterToggle.getAttribute("aria-expanded") !== "true";
      filterToggle.setAttribute("aria-expanded", String(open));
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
      keepAddress();
      return;
    }
    // A row title folds its row to one line, or shows it again: the fold
    // is a `collapse` key in the form, so the address, Copy link and live
    // refreshes keep it.
    const fold = target.closest("[data-fold]");
    if (fold && form) {
      const key = fold.dataset.fold;
      const held = [...form.querySelectorAll("input[data-collapse]")].find((input) => input.value === key);
      if (held) {
        held.remove();
      } else {
        const input = document.createElement("input");
        input.type = "hidden";
        input.name = "collapse";
        input.value = key;
        input.dataset.collapse = "";
        form.append(input);
      }
      applyFilters();
      return;
    }
    const boardOpener = target.closest("[data-board-actions]");
    if (boardOpener) {
      openMenu(boardMenu(), boardOpener);
      return;
    }
    if (target.closest("[data-board-act=copy-link]")) {
      copy(`${location.origin}/admin/board${location.search}`, "the board link");
      closeMenu(boardMenu(), document.querySelector("[data-board-actions]"));
      return;
    }
    const opener = target.closest("[data-card-menu]");
    if (opener) {
      menuButton = opener;
      menuCard = opener.closest(".bcard");
      const id = encodeURIComponent(menuCard.dataset.id);
      const box = cardMenu();
      box.querySelector('[data-act="open"]').href = `/admin/documents/${id}`;
      box.querySelector('[data-act="history"]').href = `/admin/documents/${id}/history`;
      box.setAttribute("aria-label", `Actions for ${menuCard.dataset.id}`);
      openMenu(box, opener);
      return;
    }
    const item = target.closest("[data-board-menu] [data-act]");
    if (item && menuCard) {
      const act = item.dataset.act;
      if (act === "open" || act === "history") {
        return;
      }
      const id = menuCard.dataset.id;
      closeMenu(cardMenu(), menuButton);
      if (act === "preview") {
        showPreview(menuCard);
      } else if (act === "copy-id") {
        copy(id, "the document ID");
      } else if (act === "copy-doc") {
        copy(`${location.origin}/admin/documents/${encodeURIComponent(id)}`, "the document link");
      }
    }
  });

  // The form is the view: its address, and app.js remembers it from there.
  // The filter parts (filters.js) write it: several values comma-separated,
  // defaults left out, so a Board with nothing filtered is `/admin/board`.
  const keepAddress = () => {
    const query = form.joedocsFilters
      ? form.joedocsFilters.query()
      : new URLSearchParams(new FormData(form)).toString();
    history.replaceState(null, "", query ? `/admin/board?${query}` : "/admin/board");
    document.dispatchEvent(new CustomEvent("joedocs:view-changed", { detail: { view: "board" } }));
  };
  const applyFilters = () => {
    keepAddress();
    hidePreview();
    refresh(true);
  };
  form?.addEventListener("change", (event) => {
    // Folds name the rows of one grouping: another grouping starts unfolded.
    if (event.target instanceof HTMLInputElement && event.target.name === "group") {
      for (const input of form.querySelectorAll("input[data-collapse]")) {
        input.remove();
      }
    }
    applyFilters();
  });
  // A filter part changed the form itself (a chip removed, Only, Clear all).
  form?.addEventListener("filters:changed", applyFilters);
  form?.addEventListener("submit", (event) => {
    event.preventDefault();
    applyFilters();
  });

  // --- Lifecycle -----------------------------------------------------

  // The page is shown again: a filter change still waiting (its fetch
  // failed) runs as one; otherwise the latest grid, as any change would.
  const catchUp = () => {
    if (againInstant) {
      refresh(true);
    } else {
      refreshSoon();
    }
  };

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      close();
    } else {
      open();
      catchUp();
    }
  });
  addEventListener("pagehide", close);
  addEventListener("pageshow", (event) => {
    if (event.persisted) {
      opens = 0;
      open();
      catchUp();
    }
  });
  // Back or Forward to a page htmx kept swaps in a new body: the Board's
  // stream must not outlive it.
  document.addEventListener("htmx:after:swap", () => {
    if (!board.isConnected) {
      close();
    }
  });
  setInterval(() => {
    if (!document.hidden) {
      fillAgo(document);
    }
  }, 60000);

  fillAgo(document);
  countLit();
  open();
})();
