// JoeDocs document page: keeps the latest revision of a document live
// (JD-65 D-07). Loaded with `defer`, only on a document's latest revision,
// whose page wrapper carries `data-live` (the stream) and `data-revision`.
//
// `GET /admin/documents/{id}/stream?revision=<n>` sends `event: revision`
// with the document's new revision when it changes, and `event: gone` when
// it is archived. The page then fetches its own address again and swaps the
// wrapper's content in place, keeping the scroll position. A swap waits
// while a dialog or a menu is open, or text in the page is selected; the
// check is made again when the reply arrives (JD-66 MED-02). A fetch that
// fails is tried again, with growing pauses (MED-01). The stream is closed
// while the tab is hidden and opened again when it is shown, from the
// revision the page holds, so a change in between is sent at once; a stream
// refused on opening is followed by one fetch of the page, which says
// whether the document is gone or the session ended (MED-05).
"use strict";

(() => {
  const page = () => document.querySelector("[data-doc-page][data-live]");
  if (!page()) {
    return;
  }
  const streamPath = page().dataset.live;

  // As the Board's (board.js): the first retry, the longest pause, and how
  // long a fetch may take before it counts as failed.
  const RETRY_FIRST_MS = 2000;
  const RETRY_MAX_MS = 30000;
  const FETCH_TIMEOUT_MS = 10000;

  let source = null;
  let ended = false;
  // A fetch of the page is on its way.
  let busy = false;
  // An update is owed: a change was announced and not yet shown.
  let pending = false;
  let retryDelay = 0;
  let retryTimer = null;
  let reopenDelay = 0;
  let reopenTimer = null;

  const announce = (text) => {
    const region = document.getElementById("live-region");
    if (region) {
      region.textContent = text;
    }
  };

  // Shown once, at the top of the page, when the page stops updating.
  const stop = (text) => {
    ended = true;
    pending = false;
    close();
    clearTimeout(retryTimer);
    clearTimeout(reopenTimer);
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

  // Swaps the fetched page in when it is newer than the one shown, and says
  // so; an equal or older one changes nothing.
  const swap = (html) => {
    const next = new DOMParser().parseFromString(html, "text/html");
    const fresh = next.querySelector("[data-doc-page]");
    const wrapper = page();
    if (!fresh || !wrapper) {
      return;
    }
    if (Number(fresh.dataset.revision || 0) <= Number(wrapper.dataset.revision || 0)) {
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

  // Fetches the page and swaps it in. Resolves `false` when the fetch
  // failed (an error reply, no reply in time, the network), `true`
  // otherwise; a sign-in redirect, `404` or `409` stops the page.
  const fetchPage = () => {
    busy = true;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    return fetch(location.href, { credentials: "same-origin", signal: controller.signal })
      .then((res) => {
        if (loginPage(res)) {
          stop("Signed out: reload to sign in.");
          return null;
        }
        if (res.status === 404 || res.status === 409) {
          stop("This document was archived or this file moved: it no longer updates.");
          return null;
        }
        if (!res.ok) {
          throw new Error(`HTTP ${res.status}`);
        }
        return res.text();
      })
      .then((html) => {
        if (html === null || ended) {
          return true;
        }
        // The reader may have opened something, or left the tab, while the
        // reply was on its way: nothing changes, and the update stays owed.
        if (document.hidden || inTheWay()) {
          pending = true;
          return true;
        }
        swap(html);
        return true;
      })
      .catch(() => false)
      .finally(() => {
        clearTimeout(timeout);
        busy = false;
      });
  };

  const retryLater = () => {
    pending = true;
    retryDelay = retryDelay ? Math.min(retryDelay * 2, RETRY_MAX_MS) : RETRY_FIRST_MS;
    clearTimeout(retryTimer);
    retryTimer = setTimeout(() => {
      retryTimer = null;
      update();
    }, retryDelay);
  };

  // Brings the page up to date, or leaves the update owed while a fetch is
  // on its way, a retry waits, the tab is hidden or the reader is busy.
  function update() {
    if (ended) {
      return;
    }
    if (busy || retryTimer || document.hidden || inTheWay()) {
      pending = true;
      return;
    }
    pending = false;
    fetchPage().then((fetched) => {
      if (ended) {
        return;
      }
      if (fetched) {
        retryDelay = 0;
      } else {
        retryLater();
      }
    });
  }

  // An owed update goes ahead once the way is clear.
  setInterval(() => {
    if (pending && !busy && !retryTimer && !document.hidden && !inTheWay()) {
      update();
    }
  }, 1000);

  function close() {
    if (source) {
      source.close();
      source = null;
    }
  }

  const reopenLater = () => {
    reopenDelay = reopenDelay ? Math.min(reopenDelay * 2, RETRY_MAX_MS) : RETRY_FIRST_MS;
    clearTimeout(reopenTimer);
    reopenTimer = setTimeout(() => {
      reopenTimer = null;
      open();
    }, reopenDelay);
  };

  // The stream was refused on opening (archived, signed out, too many open
  // pages, the server away): the page itself says which (D-04). A page that
  // still loads catches up, and the stream is opened again after a pause.
  const diagnose = () => {
    if (ended) {
      return;
    }
    if (busy) {
      reopenLater();
      return;
    }
    fetchPage().then(() => {
      if (!ended) {
        reopenLater();
      }
    });
  };

  function open() {
    if (ended || source || document.hidden) {
      return;
    }
    const revision = page()?.dataset.revision || "0";
    source = new EventSource(`${streamPath}?revision=${encodeURIComponent(revision)}`);
    source.addEventListener("open", () => {
      reopenDelay = 0;
    });
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
    // A dropped connection the browser retries by itself; a refusal closes
    // the stream, and the page is asked why.
    source.addEventListener("error", () => {
      if (source?.readyState === EventSource.CLOSED) {
        close();
        diagnose();
      }
    });
  }

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      close();
    } else {
      open();
      // An update owed while hidden is fetched at once, a waiting retry
      // included (D-02).
      if (pending) {
        clearTimeout(retryTimer);
        retryTimer = null;
        update();
      }
    }
  });
  open();
})();
