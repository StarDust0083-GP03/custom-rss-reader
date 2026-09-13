/**
 * Filter logic: the filter-tab row, the "Today + Unread" combination mode,
 * and the tag filter (including the in-DOM tag picker).
 *
 * The tag picker replaces the old `window.prompt()` approach — native
 * prompts are unsupported in Tauri webviews, so the Tags button was a no-op
 * in the packaged app.
 */

import { items as itemsApi } from "../api";
import { openTagGraph } from "./tag-graph";
import { state } from "../state";
import { loadItems, renderSubscriptions } from "./render";
import { error as toastError } from "../toast";
import { updateFilterTabs } from "./filter-state";

export { resetFiltersForSubscription, updateFilterTabs } from "./filter-state";

const S = state;

const TAG_MENU_ID = "tag-filter-menu";

// 切换筛选
export function setFilter(filter: typeof S.currentFilter) {
  // Capture the previous filter BEFORE overwriting `currentFilter`, otherwise
  // the "switching from unread to today" check below always sees the new value
  // and never fires.
  const prevFilter = S.currentFilter;
  S.currentFilter = filter;
  // Don't reset tag filter when switching to "tag" filter type
  if (filter !== "tag") {
    S.currentTagFilter = null;
  }
  // Don't reset subscription when switching to/from unread or today filters
  // This allows combining unread/today with specific subscription
  if (filter !== "unread" && filter !== "today") {
    S.currentSubscriptionId = null;
    S.unreadFilterEnabled = false;
  } else if (filter === "today") {
    // When switching to today, keep unread filter state if it was enabled
    // But reset it when switching from unread to today (to avoid double unread)
    if (prevFilter === "unread") {
      S.unreadFilterEnabled = false;
    }
  }

  // 更新筛选标签 — updateFilterTabs() also resets the tag tab's label from
  // "#foo" back to "Tags" and the today tab's label from "Today + Unread"
  // back to "Today".
  updateFilterTabs();

  renderSubscriptions();
  loadItems();
}

// Filter items by tag
export function filterByTag(tag: string) {
  S.currentFilter = "tag";
  S.currentTagFilter = tag;
  // Leaving "Today + Unread" combination mode — otherwise the today tab
  // keeps rendering as an active "Today + Unread" while a tag filter is on.
  S.unreadFilterEnabled = false;
  updateFilterTabs();
  loadItems();
}

// ---------------------------------------------------------------------------
// Tag picker (in-DOM dropdown anchored to the Tags tab)
//
// The picker lives in the DOM rather than window.prompt because native prompts
// are unsupported in the Tauri webview, which made the Tags button a no-op in
// the packaged app.
// ---------------------------------------------------------------------------

function closeTagMenu() {
  document.getElementById(TAG_MENU_ID)?.remove();
  // Always detach, regardless of the flag: if the menu was removed by some
  // other render, a stale flag used to swallow the next click.
  document.removeEventListener("click", onTagMenuOutsideClick);
  document.removeEventListener("keydown", onTagMenuEscape);
}

function onTagMenuOutsideClick(e: MouseEvent) {
  const menu = document.getElementById(TAG_MENU_ID);
  if (menu && !menu.contains(e.target as Node)) {
    closeTagMenu();
  }
}

function onTagMenuEscape(e: KeyboardEvent) {
  if (e.key === "Escape") closeTagMenu();
}

function tagMenuButton(className: string, label: string, onSelect: () => void): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.textContent = label;
  // The document-level outside-click listener would otherwise close the menu
  // before the option's own handler runs.
  button.addEventListener("click", e => {
    e.stopPropagation();
    onSelect();
  });
  return button;
}

/** Show the tag-selection dropdown anchored below `anchor`, or close it. */
export async function showTagSelector(anchor: HTMLElement) {
  // Toggle from the DOM, not from a module flag: the menu is removed by
  // outside clicks, Escape, and selection, and the flag must never drift.
  if (document.getElementById(TAG_MENU_ID)) {
    closeTagMenu();
    return;
  }

  let tags: string[] = [];
  try {
    tags = await itemsApi.tags(S.currentSubscriptionId);
  } catch (error) {
    toastError(`Failed to load tags: ${error}`);
    return;
  }
  // The list may have arrived after the user closed something else; make sure
  // the document-level listeners are not left installed from a prior open.
  closeTagMenu();

  const menu = document.createElement("div");
  menu.className = "tag-menu";
  menu.id = TAG_MENU_ID;
  menu.setAttribute("role", "menu");
  menu.setAttribute("aria-label", "Filter by tag");

  const active = S.currentFilter === "tag" ? S.currentTagFilter : null;
  if (active) {
    const header = document.createElement("div");
    header.className = "tag-menu-active";
    header.append(document.createTextNode(`Showing #${active}`));
    header.appendChild(
      tagMenuButton("tag-menu-clear", "Clear", () => {
        closeTagMenu();
        setFilter("all");
      }),
    );
    menu.appendChild(header);
  }

  const search = document.createElement("input");
  search.type = "search";
  search.className = "tag-menu-search";
  search.placeholder = "Search tags...";
  search.setAttribute("aria-label", "Search tags");
  menu.appendChild(search);

  const options = document.createElement("div");
  options.className = "tag-menu-options";
  menu.appendChild(options);

  let optionButtons: HTMLButtonElement[] = [];

  const renderOptions = () => {
    options.replaceChildren();
    const query = search.value.trim().toLowerCase();
    const visible = tags.filter(tag => tag.toLowerCase().includes(query));
    optionButtons = [];
    if (visible.length === 0) {
      const empty = document.createElement("p");
      empty.className = "tag-menu-empty";
      empty.textContent = tags.length === 0 ? "No used tags yet." : "No matching tags.";
      options.appendChild(empty);
      return;
    }
    for (const tag of visible) {
      const option = tagMenuButton("tag-menu-item", `#${tag}`, () => {
        closeTagMenu();
        filterByTag(tag);
      });
      option.setAttribute("role", "menuitem");
      option.dataset.tag = tag;
      options.appendChild(option);
      optionButtons.push(option);
    }
  };

  search.addEventListener("input", renderOptions);
  search.addEventListener("keydown", event => {
    if (event.key === "ArrowDown" && optionButtons[0]) {
      event.preventDefault();
      optionButtons[0].focus();
    }
  });
  options.addEventListener("keydown", event => {
    const index = optionButtons.indexOf(document.activeElement as HTMLButtonElement);
    if (index === -1) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const next = (index + (event.key === "ArrowDown" ? 1 : -1) + optionButtons.length) % optionButtons.length;
      optionButtons[next].focus();
    }
  });
  renderOptions();

  const manage = tagMenuButton("tag-menu-manage", "Manage tags", () => {
    closeTagMenu();
    void openTagGraph();
  });
  menu.appendChild(manage);

  document.body.appendChild(menu);
  const rect = anchor.getBoundingClientRect();
  menu.style.top = `${rect.bottom + 6}px`;
  menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - 300))}px`;

  document.addEventListener("click", onTagMenuOutsideClick);
  document.addEventListener("keydown", onTagMenuEscape);
  search.focus();
}

/**
 * Wire the filter-tab row.
 *
 * This lives here, next to filter state, so the tag picker's trigger and its
 * behaviour are one testable unit instead of being split across main.ts.
 */
export function initFilterTabs(): void {
  document.querySelectorAll<HTMLElement>(".filter-tab").forEach(tab => {
    const activate = () => {
      const filter = tab.dataset.filter as typeof S.currentFilter | undefined;
      if (!filter) return;
      if (filter === "tag") {
        void showTagSelector(tab);
        return;
      }
      if (filter === "unread" && S.currentFilter === "today") {
        // Clicking Unread while in Today mode toggles "Today + Unread".
        S.unreadFilterEnabled = !S.unreadFilterEnabled;
        updateFilterTabs();
        void loadItems();
        return;
      }
      setFilter(filter);
    };

    tab.addEventListener("click", event => {
      // The gear lives inside this pill; its own handler opens the workspace,
      // and the same click must not also toggle the filter picker.
      if ((event.target as HTMLElement | null)?.closest("#tag-settings-btn")) return;
      activate();
    });
    // The Tags pill is a container rather than a button, because its settings
    // gear has to sit inside it. A non-button needs explicit key activation.
    if (tab.tagName !== "BUTTON") {
      tab.addEventListener("keydown", event => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          activate();
        }
      });
    }
  });
}
