import { state } from "../state";

const S = state;

/** Reset content filters when the user changes the subscription context. */
export function resetFiltersForSubscription(): void {
  S.currentFilter = "all";
  S.currentTagFilter = null;
  S.unreadFilterEnabled = false;
  updateFilterTabs();
}

/** Keep filter-tab labels and active states in sync with application state. */
export function updateFilterTabs(): void {
  if (typeof document === "undefined") return;

  document.querySelectorAll(".filter-tab").forEach(tab => {
    const tabFilter = tab.getAttribute("data-filter");
    const label = tab.querySelector(".filter-label");
    if (!label) return;

    if (S.currentFilter === "tag" && S.currentTagFilter && tabFilter === "tag") {
      tab.classList.add("active");
      label.textContent = `#${S.currentTagFilter}`;
    } else if (tabFilter === "today" && S.unreadFilterEnabled) {
      tab.classList.add("active");
      label.textContent = "Today + Unread";
    } else if (tabFilter === S.currentFilter && S.currentFilter !== "tag") {
      tab.classList.add("active");
      if (tabFilter === "today") label.textContent = "Today";
    } else {
      tab.classList.remove("active");
      if (tabFilter === "tag") label.textContent = "Tags";
      if (tabFilter === "today") label.textContent = "Today";
    }
  });
}
