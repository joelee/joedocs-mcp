# DESIGN.md - the admin web interface

Design rules for the admin web interface (`src/web/`, `templates/`). Plans and changes to it follow them. How to build a page (templates, routes, live filters, the scripts) is in [docs/developer-guide.md](docs/developer-guide.md#web-interface); this file says what a page should be.

## Browsers and JavaScript

- **JavaScript is required.** The web interface needs a browser with JavaScript turned on. Pages may depend on `app.js`, htmx and the bundled scripts (`filters.js`, `board.js`, `document.js`) with no fallback. Do not plan or build a no-JavaScript fallback unless the user asks for one. The few fallbacks that exist (a filter form's button, dialogs opened by invoker commands) may stay; nothing needs to keep working without scripts, and tests need not check it.
- **Supported browsers: current Chrome and Firefox.** Every change to a page is checked in both before it is done: by mouse and keyboard, in the light and dark themes, and at desktop width and 390 px. A difference between the two is a bug to fix or a decision for the user, never left unrecorded.
- **Tested.** `tests/web.rs` checks every page's markup, addresses and accessibility basics; the browser checks above cover what the scripts do.

## Filtering and sorting

- **The filter parts are how a page filters.** `src/web/assets/filters.js` and the "Filter parts" section of `app.css` provide:
  - the pill, for a short group of values (several at once);
  - the searchable picker, for a long group (several values, or one with `data-picker-single`);
  - the active-filter chips, with the count of filters in force and Clear all;
  - the switch, for an on-or-off filter;
  - the View menu, for the sort, the order and other view choices.

  The Board (JD-70, JD-71) and Documents use them. A page that filters reuses them, and the shared server-side parts that build their choices and refuse unknown values, rather than building its own controls.
- **The address holds the view.** Several values are one comma-separated key (`?project=a,b`), each value URL-encoded, and values at their defaults are left out. Repeated keys are also read. An address naming an unknown value is refused with `400` and a way back. Filters apply as they change, and the address follows, so a view can be shared and reloaded.

## Look and structure

- **No build step, no framework:** hand-written HTML templates, `app.css` and plain JavaScript files, served as cached assets.
- **No inline script or style** (the Content-Security-Policy). Behaviour is event listeners in the bundled scripts; positions that move are set through the CSSOM.
- **Native, labelled controls underneath every part** (checkboxes, radio buttons, selects, buttons), each with an `id` and a `<label for>`, so names and states come from the browser.
- **Both themes** through the `light-dark()` design tokens at the top of `app.css`, with at least 4.5:1 contrast in each.
- **Phone width:** every page is usable at 390 px wide with no sideways scroll; popovers become bottom sheets on narrow screens.
- **Announcements:** changes a script makes (results updated, a filter cleared) are announced through the page's live region.
