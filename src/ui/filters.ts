/**
 * Filter logic: the filter-tab row, the "Today + Unread" combination mode,
 * and the tag filter (including the in-DOM tag picker).
 *
 * The tag picker replaces the old `window.prompt()` approach — native
 * prompts are unsupported in Tauri webviews, so the Tags button was a no-op
 * in the packaged app.
 */

import { items as itemsApi } from "../api";
import { state } from "../state";
import { loadItems, renderSubscriptions } from "./render";
import { error as toastError } from "../toast";

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

// Update filter tabs styling
export function updateFilterTabs() {
  document.querySelectorAll(".filter-tab").forEach(tab => {
    const tabFilter = tab.getAttribute("data-filter");
    if (S.currentFilter === "tag" && S.currentTagFilter && tabFilter === "tag") {
      tab.classList.add("active");
      tab.textContent = `#${S.currentTagFilter}`;
    } else if (tabFilter === "today" && S.unreadFilterEnabled) {
      // Show "Today + Unread" when both filters are active
      tab.classList.add("active");
      tab.textContent = "Today + Unread";
    } else if (tabFilter === S.currentFilter && S.currentFilter !== "tag") {
      tab.classList.add("active");
      // Reset text to default
      if (tabFilter === "today") {
        tab.textContent = "Today";
      }
    } else {
      tab.classList.remove("active");
      if (tabFilter === "tag") {
        tab.textContent = "Tags";
      }
      if (tabFilter === "today") {
        tab.textContent = "Today";
      }
    }
  });
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

  let tags: string[];
  try {
    tags = await itemsApi.tags(S.currentSubscriptionId);
  } catch (error) {
    toastError(`Failed to load tags: ${error}`);
    return;
  }

  if (tags.length === 0) {
    toastError("No tags found. Classify some items first.");
    return;
  }

  const menu = document.createElement("div");
  menu.className = "tag-menu";
  menu.id = TAG_MENU_ID;

  for (const t of tags) {
    const opt = document.createElement("button");
    opt.type = "button";
    opt.className = "tag-menu-item";
    opt.textContent = "#" + t;
    opt.addEventListener("click", (e) => {
      e.stopPropagation();
      closeTagMenu();
      filterByTag(t);
    });
    menu.appendChild(opt);
  }

  document.body.appendChild(menu);
  const rect = anchor.getBoundingClientRect();
  menu.style.top = `${rect.bottom + 6}px`;
  menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - 220))}px`;

  tagMenuOpen = true;
  document.addEventListener("click", onTagMenuOutsideClick);
  document.addEventListener("keydown", onTagMenuEscape);
}
