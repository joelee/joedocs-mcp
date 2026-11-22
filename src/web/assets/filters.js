// JoeDocs filter parts (JD-71): pills, the searchable picker, and the
// active-filter chips, for any form marked `data-filter-form`. Every part is
// a native control underneath (a labelled checkbox or radio button), so the
// form's own data is the filter state; this script only draws and adjusts
// it. It knows nothing of the page using it: a page reads the form's query
// with `form.joedocsFilters.query()` and listens for the form's `change`
// and `filters:changed` events.
//
// Markup it reads:
// - `[data-filter-group="<name>"]` around a group's checkboxes, with
//   `data-filter-label` (its name in a chip) and `data-filter-default`: the
//   values lit by default, comma-separated, or empty when nothing chosen
//   means all (and so does everything chosen); `data-filter-default-label`
//   names that default in words ("open");
// - any other control inside an element with `data-filter-default` (a
//   radio group, a select) is left out of the query at that value;
// - `[data-filter-picker]`: a picker (its button, popover, search field,
//   list, Select all, Clear, Only, counter, chips);
// - `[data-filter-pills]`: pills, with `[data-pills-all]` (choose none),
//   `[data-pills-default]` and `[data-pills-every]` (in a
//   `[data-pills-tools]`), each hidden while it would change nothing;
// - `[data-filter-chips]`, `[data-filter-count]`, `[data-filter-clear]`:
//   the active filters, their number, and Clear all (shown from two);
// - `input[role="switch"]`: a switch, shown as a chip when on;
// - `[data-view-menu]`: a compact button that names a view and opens a
//   popover of its controls (see View menus below).

(() => {
  "use strict";

  const announce = (text) => {
    const region = document.getElementById("live-region");
    if (region) {
      region.textContent = text;
    }
  };

  // Values of a comma-separated attribute, without empty parts.
  const list = (text) => (text || "").split(",").filter((v) => v !== "");
  const sameSet = (a, b) => a.length === b.length && a.every((v) => b.includes(v));

  // The words a choice shows: its picker name, or its pill's text.
  const labelOf = (input) => {
    const label = input.closest("label");
    const name = label?.querySelector(".picker-name");
    return (name || label)?.textContent.trim() || input.value;
  };

  function setup(form) {
    const groups = [...form.querySelectorAll("[data-filter-group]")];
    const groupOf = (name) => groups.find((g) => g.dataset.filterGroup === name);
    const inputs = (group) => [...group.querySelectorAll(`input[name="${group.dataset.filterGroup}"]`)];
    const lit = (group) => inputs(group).filter((i) => i.checked);
    const defaults = (group) => list(group.dataset.filterDefault);
    const atDefault = (group) => {
      const on = lit(group).map((i) => i.value);
      const base = defaults(group);
      return base.length ? sameSet(on, base) : on.length === 0 || on.length === inputs(group).length;
    };
    const setLit = (group, values) => {
      for (const input of inputs(group)) {
        input.checked = values.includes(input.value);
      }
    };
    const reset = (group) => setLit(group, defaults(group));
    const switches = () => [...form.querySelectorAll('input[role="switch"]')];
    const changed = () => {
      refresh();
      form.dispatchEvent(new CustomEvent("filters:changed", { bubbles: true }));
    };

    // --- The query --------------------------------------------------------

    // The form as a query string: each name once, several values joined by
    // commas (each value encoded, so a comma inside one stays `%2C`), and
    // nothing for a group or control at its default.
    const query = () => {
      const values = new Map();
      const elements = new Map();
      for (const [name, value] of new FormData(form)) {
        if (!values.has(name)) {
          values.set(name, []);
        }
        if (!values.get(name).includes(value)) {
          values.get(name).push(value);
        }
      }
      for (const element of form.elements) {
        if (element.name && !elements.has(element.name)) {
          elements.set(element.name, element);
        }
      }
      const parts = [];
      for (const [name, vals] of values) {
        const group = groupOf(name);
        if (group) {
          if (atDefault(group)) {
            continue;
          }
        } else {
          const holder = elements.get(name)?.closest("[data-filter-default]");
          if (holder && sameSet(vals, list(holder.dataset.filterDefault))) {
            continue;
          }
        }
        parts.push(`${encodeURIComponent(name)}=${vals.map(encodeURIComponent).join(",")}`);
      }
      return parts.join("&");
    };

    // --- Chips ------------------------------------------------------------

    const chip = (facet, text, removeLabel, remove) => {
      const span = document.createElement("span");
      span.className = "fchip";
      const words = document.createElement("span");
      words.className = "fchip-text";
      if (facet) {
        const f = document.createElement("span");
        f.className = "fchip-facet";
        f.textContent = `${facet}:`;
        words.append(f);
      }
      words.append(text);
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = "×";
      button.setAttribute("aria-label", removeLabel);
      button.addEventListener("click", () => remove(button));
      span.append(words, button);
      return span;
    };

    // After a chip's removal, focus moves to the chip now in its place, or
    // the one before it, or else to `fallback`.
    const refocus = (container, index, fallback) => {
      const buttons = [...container.querySelectorAll("button")];
      (buttons[index] || buttons[index - 1] || fallback)?.focus();
    };

    // What a group's chip says: up to three values, else a count; a group
    // with a default subset is described against it ("open + done").
    const describe = (group) => {
      const on = lit(group);
      const names = on.map(labelOf);
      const base = defaults(group);
      if (base.length && on.length) {
        const values = on.map((i) => i.value);
        const extra = on.filter((i) => !base.includes(i.value));
        if (base.every((v) => values.includes(v)) && extra.length && extra.length <= 3) {
          return `${group.dataset.filterDefaultLabel || "default"} + ${extra.map(labelOf).join(", ")}`;
        }
      }
      if (names.length <= 3) {
        return names.join(", ");
      }
      return `${names.length} of ${inputs(group).length}`;
    };

    const toggle = form.querySelector("[data-filter-toggle]");
    const drawBar = () => {
      const area = form.querySelector("[data-filter-chips]");
      if (!area) {
        return 0;
      }
      area.replaceChildren();
      let count = 0;
      for (const group of groups) {
        if (atDefault(group)) {
          continue;
        }
        count += 1;
        const label = group.dataset.filterLabel || group.dataset.filterGroup;
        area.append(
          chip(label, describe(group), `Remove the ${label.toLowerCase()} filter`, (button) => {
            const index = [...area.querySelectorAll("button")].indexOf(button);
            reset(group);
            changed();
            refocus(area, index, toggle);
          }),
        );
      }
      for (const input of switches()) {
        if (!input.checked) {
          continue;
        }
        count += 1;
        const text = input.closest("label")?.textContent.trim().replace(/^Only /, "") || input.name;
        const words = text.charAt(0).toUpperCase() + text.slice(1);
        area.append(
          chip("", words, `Remove the filter ${text}`, (button) => {
            const index = [...area.querySelectorAll("button")].indexOf(button);
            input.checked = false;
            changed();
            refocus(area, index, toggle);
          }),
        );
      }
      return count;
    };

    // --- Pills ------------------------------------------------------------

    const drawPills = () => {
      for (const pills of form.querySelectorAll("[data-filter-pills]")) {
        const group = pills.closest("[data-filter-group]");
        if (!group) {
          continue;
        }
        const all = pills.querySelector("[data-pills-all]");
        if (all) {
          all.setAttribute("aria-pressed", String(atDefault(group)));
        }
        // A shortcut shows only while it would change something (JD-70
        // option E): Open only away from the default, All while a value
        // is unlit; their holder goes when both do.
        const back = pills.querySelector("[data-pills-default]");
        const every = pills.querySelector("[data-pills-every]");
        if (back) {
          back.hidden = atDefault(group);
        }
        if (every) {
          every.hidden = lit(group).length === inputs(group).length;
        }
        const tools = pills.querySelector("[data-pills-tools]");
        if (tools) {
          tools.hidden = [back, every].every((b) => !b || b.hidden);
        }
      }
    };

    // A shortcut that hid itself hands focus to the other one, or to the
    // group's first pill, so focus is never left on a hidden button.
    const keepFocus = (button, group) => {
      if (!button.hidden) {
        return;
      }
      const other = [...group.querySelectorAll("[data-pills-default], [data-pills-every]")].find(
        (b) => b !== button && !b.hidden,
      );
      (other || inputs(group)[0])?.focus();
    };

    // --- Pickers ----------------------------------------------------------

    const MAX_CHIPS = 6;
    const drawPicker = (picker) => {
      const group = picker.closest("[data-filter-group]");
      if (!group) {
        return;
      }
      const all = inputs(group);
      const on = lit(group);
      const whole = defaults(group).length === 0 && (on.length === 0 || on.length === all.length);
      const opener = picker.querySelector("[data-picker-open]");
      if (opener?.firstChild?.nodeType === Node.TEXT_NODE) {
        opener.firstChild.textContent = whole ? opener.dataset.allLabel : opener.dataset.someLabel;
      }
      const count = picker.querySelector("[data-picker-count]");
      if (count) {
        count.textContent = `${on.length} of ${all.length} chosen`;
      }
      const chips = picker.querySelector("[data-picker-chips]");
      if (!chips) {
        return;
      }
      chips.replaceChildren();
      if (whole) {
        return;
      }
      for (const input of on.slice(0, MAX_CHIPS)) {
        const name = labelOf(input);
        chips.append(
          chip("", name, `Remove ${name}`, (button) => {
            const index = [...chips.querySelectorAll("button")].indexOf(button);
            input.checked = false;
            changed();
            refocus(chips, index, opener);
          }),
        );
      }
      if (on.length > MAX_CHIPS) {
        const more = document.createElement("button");
        more.type = "button";
        more.className = "fchip-more";
        more.textContent = `+${on.length - MAX_CHIPS} more`;
        more.addEventListener("click", () => open(picker));
        chips.append(more);
      }
    };

    // A popover placed below `anchor` (above it when there is no room),
    // through the CSSOM; on a narrow screen the stylesheet makes it a sheet.
    const placeBelow = (pop, anchor) => {
      if (!pop || matchMedia("(max-width: 720px)").matches) {
        return;
      }
      const rect = anchor.getBoundingClientRect();
      const width = pop.offsetWidth;
      const height = pop.offsetHeight;
      const left = Math.max(8, Math.min(rect.left, innerWidth - width - 8));
      let top = rect.bottom + 4;
      if (top + height > innerHeight - 8 && rect.top - height - 4 > 8) {
        top = rect.top - height - 4;
      }
      pop.style.left = `${left}px`;
      pop.style.top = `${Math.max(8, top)}px`;
    };
    const place = (picker) =>
      placeBelow(picker.querySelector("[data-picker-pop]"), picker.querySelector(".picker-box") || picker);

    const visibleRows = (picker) =>
      [...picker.querySelectorAll("[data-picker-item]")].filter((li) => !li.hidden);

    const filterRows = (picker) => {
      const search = picker.querySelector("[data-picker-search]");
      const words = (search?.value || "").trim().toLowerCase().split(/\s+/).filter(Boolean);
      let shown = 0;
      for (const li of picker.querySelectorAll("[data-picker-item]")) {
        const text = li.dataset.search || "";
        li.hidden = !words.every((w) => text.includes(w));
        shown += li.hidden ? 0 : 1;
      }
      const empty = picker.querySelector("[data-picker-empty]");
      if (empty) {
        empty.hidden = shown > 0;
      }
    };

    function open(picker) {
      const pop = picker.querySelector("[data-picker-pop]");
      if (!pop || pop.matches(":popover-open")) {
        return;
      }
      pop.showPopover();
      place(picker);
      picker.querySelector("[data-picker-search]")?.focus();
    }

    const close = (picker) => {
      const pop = picker.querySelector("[data-picker-pop]");
      if (pop?.matches(":popover-open")) {
        pop.hidePopover();
      }
    };

    for (const picker of form.querySelectorAll("[data-filter-picker]")) {
      const pop = picker.querySelector("[data-picker-pop]");
      const opener = picker.querySelector("[data-picker-open]");
      const search = picker.querySelector("[data-picker-search]");
      const group = picker.closest("[data-filter-group]");
      opener?.addEventListener("click", () => {
        if (pop?.matches(":popover-open")) {
          close(picker);
        } else {
          open(picker);
        }
      });
      // Opened or closed, by this script or by the browser (Escape, a
      // click outside): the button says so, and focus that was inside goes
      // back to it.
      pop?.addEventListener("toggle", (event) => {
        const isOpen = event.newState === "open";
        opener?.setAttribute("aria-expanded", String(isOpen));
        if (!isOpen) {
          if (search && search.value) {
            search.value = "";
            filterRows(picker);
          }
          if (pop.contains(document.activeElement) || document.activeElement === document.body) {
            opener?.focus();
          }
        }
      });
      search?.addEventListener("input", () => filterRows(picker));
      search?.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          // Enter ticks the first row shown, and never submits the form.
          event.preventDefault();
          const input = visibleRows(picker)[0]?.querySelector("input");
          if (input && !input.checked) {
            input.click();
          }
        } else if (event.key === "ArrowDown") {
          event.preventDefault();
          visibleRows(picker)[0]?.querySelector("input")?.focus();
        }
      });
      pop?.addEventListener("keydown", (event) => {
        if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
          return;
        }
        const row = event.target instanceof Element ? event.target.closest("[data-picker-item]") : null;
        if (!row) {
          return;
        }
        event.preventDefault();
        const rows = visibleRows(picker);
        const at = rows.indexOf(row) + (event.key === "ArrowDown" ? 1 : -1);
        if (at < 0) {
          search?.focus();
        } else if (rows[at]) {
          rows[at].querySelector("input")?.focus();
        }
      });
      pop?.addEventListener("click", (event) => {
        const target = event.target instanceof Element ? event.target : null;
        if (!target || !group) {
          return;
        }
        if (target.closest("[data-picker-done]")) {
          close(picker);
        } else if (target.closest("[data-picker-all]")) {
          // Select all ticks the rows shown (all of them with no search).
          for (const li of visibleRows(picker)) {
            li.querySelector("input").checked = true;
          }
          changed();
        } else if (target.closest("[data-picker-clear]")) {
          reset(group);
          changed();
        } else if (target.closest("[data-picker-only]")) {
          setLit(group, [target.closest("[data-picker-only]").dataset.pickerOnly]);
          changed();
        }
      });
      addEventListener("resize", () => pop?.matches(":popover-open") && place(picker));
      addEventListener("scroll", () => pop?.matches(":popover-open") && place(picker), { passive: true });
    }

    // --- View menus -------------------------------------------------------

    // `[data-view-menu]`: a button (`data-view-open`) naming the view in
    // `data-view-summary`, and a popover (`data-view-pop`) of controls. Each
    // `[data-view-part]` (a fieldset of radios, or a select) adds the
    // `data-short` text of its chosen value to the summary, after " · " or
    // its own `data-view-join`.
    const views = [...form.querySelectorAll("[data-view-menu]")];
    const drawViews = () => {
      for (const menu of views) {
        const out = menu.querySelector("[data-view-summary]");
        if (!out) {
          continue;
        }
        let text = "";
        for (const part of menu.querySelectorAll("[data-view-part]")) {
          const chosen = part instanceof HTMLSelectElement ? part.selectedOptions[0] : part.querySelector("input:checked");
          const short = chosen?.dataset.short;
          if (short) {
            text += text ? `${part.dataset.viewJoin ?? " \u00b7 "}${short}` : short;
          }
        }
        out.textContent = text;
      }
    };
    for (const menu of views) {
      const opener = menu.querySelector("[data-view-open]");
      const pop = menu.querySelector("[data-view-pop]");
      if (!opener || !pop) {
        continue;
      }
      opener.addEventListener("click", () => {
        if (pop.matches(":popover-open")) {
          pop.hidePopover();
          return;
        }
        pop.showPopover();
        placeBelow(pop, opener);
        (pop.querySelector("input:checked") || pop.querySelector("input, select"))?.focus();
      });
      pop.addEventListener("toggle", (event) => {
        const isOpen = event.newState === "open";
        opener.setAttribute("aria-expanded", String(isOpen));
        if (!isOpen && (pop.contains(document.activeElement) || document.activeElement === document.body)) {
          opener.focus();
        }
      });
      pop.addEventListener("click", (event) => {
        if (event.target instanceof Element && event.target.closest("[data-view-done]")) {
          pop.hidePopover();
        }
      });
      addEventListener("resize", () => pop.matches(":popover-open") && placeBelow(pop, opener));
      addEventListener("scroll", () => pop.matches(":popover-open") && placeBelow(pop, opener), { passive: true });
    }

    // --- Pills' buttons ---------------------------------------------------

    form.addEventListener("click", (event) => {
      const target = event.target instanceof Element ? event.target : null;
      const group = target?.closest("[data-filter-group]");
      if (!target || !group) {
        return;
      }
      if (target.closest("[data-pills-all]")) {
        setLit(group, []);
        changed();
      } else if (target.closest("[data-pills-default]")) {
        reset(group);
        changed();
        keepFocus(target.closest("[data-pills-default]"), group);
      } else if (target.closest("[data-pills-every]")) {
        setLit(group, inputs(group).map((i) => i.value));
        changed();
        keepFocus(target.closest("[data-pills-every]"), group);
      }
    });

    const clear = form.querySelector("[data-filter-clear]");
    clear?.addEventListener("click", () => {
      for (const group of groups) {
        reset(group);
      }
      for (const input of switches()) {
        input.checked = false;
      }
      changed();
      toggle?.focus();
    });

    // A group with a default subset never shows nothing: unlighting its
    // last value returns it to the default, and says so.
    form.addEventListener(
      "change",
      (event) => {
        const input = event.target instanceof HTMLInputElement ? event.target : null;
        const group = input?.closest("[data-filter-group]");
        if (group && defaults(group).length && lit(group).length === 0) {
          reset(group);
          const note = `${group.dataset.filterLabel || "Filter"}: back to ${group.dataset.filterDefaultLabel || "the defaults"}`;
          // Said now, and kept for a page that announces its own update
          // next, so it can say both.
          form.dataset.filterNote = note;
          announce(note);
        }
        refresh();
      },
      true,
    );

    function refresh() {
      drawPills();
      drawViews();
      for (const picker of form.querySelectorAll("[data-filter-picker]")) {
        drawPicker(picker);
      }
      const count = drawBar();
      const badge = form.querySelector("[data-filter-count]");
      if (badge) {
        badge.textContent = String(count);
        badge.hidden = count === 0;
      }
      if (clear) {
        // From two chips: with one, its own × does the same (JD-70 option E).
        clear.hidden = count < 2;
      }
    }

    form.joedocsFilters = { query, refresh };
    refresh();
  }

  for (const form of document.querySelectorAll("form[data-filter-form]")) {
    setup(form);
  }
})();
