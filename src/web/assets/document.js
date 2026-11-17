// JoeDocs document page: keeps the latest revision of a document live
// (JD-65 D-07). Loaded with `defer`, only on a document's latest revision,
// whose page wrapper carries `data-live` (the stream) and `data-revision`.
//
// `GET /admin/documents/{id}/stream?revision=<n>` sends `event: revision`
// with the document's new revision when it changes, and `event: gone` when
// it is archived. The page then fetches its own address again and swaps the
// wrapper's content in place, keeping the scroll position. A swap waits
// while a dialog or a menu is open, or text in the page is selected. The
// stream is closed while the tab is hidden and opened again when it is
// shown, from the revision the page holds, so a change in between is sent
// at once.
"use strict";

(() => {
  const page = () => document.querySelector("[data-doc-page][data-live]");
  if (!page()) {
    return;
  }
  const streamPath = page().dataset.live;

  let source = null;
  let ended = false;
  let busy = false;
  let pending = false;
  let retryTimer = null;

  const announce = (text) => {
    const region = document.getElementById("live-region");
    if (region) {
      region.textContent = text;
    }
  };

  // Shown once, at the top of the page, when the page stops updating.
  const stop = (text) => {
    ended = true;
    close();
    clearTimeout(retryTimer);
    const wrapper = page();
    if (wrapper && !wrapper.querySelector("[data-live-notice]")) {
      const note = document.createElement("p");
      note.className = "notice";
      note.setAttribute("role", "status");
      note.dataset.liveNotice = "";
      note.textContent = text;
      wrapper.prepend(note);
    }
    announce(text);
  };

  // Something the reader is doing that a swap would undo.
  const inTheWay = () => {
    const wrapper = page();
    if (!wrapper) {
      return false;
    }
    if (wrapper.querySelector("dialog[open], details.actions-menu[open]")) {
      return true;
    }
    const selection = window.getSelection();
    return Boolean(
      selection && !selection.isCollapsed && selection.anchorNode && wrapper.contains(selection.anchorNode),
    );
  };

  const swap = (html) => {
    const next = new DOMParser().parseFromString(html, "text/html");
    const fresh = next.querySelector("[data-doc-page]");
    const wrapper = page();
    if (!fresh || !wrapper) {
      return;
    }
    const y = window.scrollY;
    const active = document.activeElement;
    const activeHref = active instanceof HTMLAnchorElement ? active.getAttribute("href") : null;
    wrapper.replaceChildren(...fresh.childNodes);
    for (const name of ["revision", "live"]) {
      if (fresh.dataset[name] === undefined) {
        delete wrapper.dataset[name];
      } else {
        wrapper.dataset[name] = fresh.dataset[name];
      }
    }
    document.title = next.title || document.title;
    window.scrollTo(0, y);
    // app.js draws diagrams, sets up sorting and the Files panel's folds.
    wrapper.dispatchEvent(new CustomEvent("joedocs:swapped", { bubbles: true }));
    if (activeHref) {
      const again = [...wrapper.querySelectorAll("a[href]")].find((a) => a.getAttribute("href") === activeHref);
      again?.focus({ preventScroll: true });
    }
    announce(`Updated to revision ${wrapper.dataset.revision}`);
  };

  const loginPage = (res) => res.redirected && new URL(res.url).pathname === "/admin/login";

  // Fetches the page again and swaps it in, or waits until the reader is
  // done with a dialog, a menu or a selection.
  const update = () => {
    if (ended) {
      return;
    }
    if (busy || inTheWay()) {
      pending = true;
      return;
    }
    pending = false;
    busy = true;
    fetch(location.href, { credentials: "same-origin" })
      .then((res) => {
        if (loginPage(res)) {
          stop("Signed out: reload to sign in.");
          return null;
        }
        if (res.status === 404 || res.status === 409) {
          stop("This document was archived or this file moved: it no longer updates.");
          return null;
        }
        return res.ok ? res.text() : null;
      })
      .then((html) => {
        if (html) {
          swap(html);
        }
      })
      .catch(() => {
        pending = true;
      })
      .finally(() => {
        busy = false;
      });
  };
  // A swap that waited is tried again once the way is clear.
  setInterval(() => {
    if (pending && !document.hidden) {
      update();
    }
  }, 1000);

  function close() {
    if (source) {
      source.close();
      source = null;
    }
  }

  function open() {
    if (ended || source || document.hidden) {
      return;
    }
    const revision = page()?.dataset.revision || "0";
    source = new EventSource(`${streamPath}?revision=${encodeURIComponent(revision)}`);
    source.addEventListener("revision", (event) => {
      if (Number(event.data) > Number(page()?.dataset.revision || 0)) {
        update();
      }
    });
    source.addEventListener("gone", () => {
      stop("This document was archived: it no longer updates.");
    });
    source.addEventListener("expired", () => {
      stop("Signed out: reload to sign in.");
    });
    // The browser retries by itself after a dropped connection; a refusal
    // (busy, signed out) closes the stream, so it is opened again later.
    source.addEventListener("error", () => {
      if (source?.readyState === EventSource.CLOSED) {
        close();
        clearTimeout(retryTimer);
        retryTimer = setTimeout(open, 15000);
      }
    });
  }

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      close();
    } else {
      open();
    }
  });
  open();
})();
