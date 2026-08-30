/**
 * Filter logic: the filter-tab row, the "Today + Unread" combination mode,
 * and the tag filter (including the in-DOM tag picker).
 *
 * The tag picker replaces the old `window.prompt()` approach — native
 * prompts are unsupported in Tauri webviews, so the Tags button was a no-op
 * in the packaged app.
 */

import { items as itemsApi } from "../api";
import { openTagManager } from "../features/tags";
import { state } from "../state";
import { loadItems, renderSubscriptions } from "./render";
import { error as toastError } from "../toast";
import { updateFilterTabs } from "./filter-state";

export { resetFiltersForSubscription, updateFilterTabs } from "./filter-state";

const S = state;

const TAG_MENU_ID = "tag-filter-menu";
let tagMenuOpen = false;

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
// ---------------------------------------------------------------------------

function closeTagMenu() {
  document.getElementById(TAG_MENU_ID)?.remove();
  if (tagMenuOpen) {
    document.removeEventListener("click", onTagMenuOutsideClick);
    document.removeEventListener("keydown", onTagMenuEscape);
    tagMenuOpen = false;
  }
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

/** Show the tag-selection dropdown anchored below `anchor`. */
export async function showTagSelector(anchor: HTMLElement) {
  if (tagMenuOpen) {
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

  const menu = document.createElement("div");
  menu.className = "tag-menu";
  menu.id = TAG_MENU_ID;
  menu.setAttribute("role", "listbox");

  const search = document.createElement("input");
  search.type = "search";
  search.className = "tag-menu-search";
  search.placeholder = "Search tags...";
  search.setAttribute("aria-label", "Search tags");
  menu.appendChild(search);

  const options = document.createElement("div");
  options.className = "tag-menu-options";
  menu.appendChild(options);

  const renderOptions = () => {
    options.replaceChildren();
    const query = search.value.trim().toLowerCase();
    const visible = tags.filter(tag => tag.toLowerCase().includes(query));
    if (visible.length === 0) {
      const empty = document.createElement("p");
      empty.className = "tag-menu-empty";
      empty.textContent = tags.length === 0 ? "No used tags yet." : "No matching tags.";
      options.appendChild(empty);
    } else {
      for (const tag of visible) {
        const opt = document.createElement("button");
        opt.type = "button";
        opt.className = "tag-menu-item";
        opt.textContent = "#" + tag;
        opt.addEventListener("click", (e) => {
          e.stopPropagation();
          closeTagMenu();
          filterByTag(tag);
        });
        options.appendChild(opt);
      }
    }
  };
  search.addEventListener("input", renderOptions);
  renderOptions();

  const manage = document.createElement("button");
  manage.type = "button";
  manage.className = "tag-menu-manage";
  manage.textContent = "Manage tags";
  manage.addEventListener("click", (e) => {
    e.stopPropagation();
    closeTagMenu();
    void openTagManager();
  });
  menu.appendChild(manage);

  document.body.appendChild(menu);
  const rect = anchor.getBoundingClientRect();
  menu.style.top = `${rect.bottom + 6}px`;
  menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - 300))}px`;

  tagMenuOpen = true;
  document.addEventListener("click", onTagMenuOutsideClick);
  document.addEventListener("keydown", onTagMenuEscape);
  search.focus();
}
