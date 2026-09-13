import { readFileSync } from "node:fs";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

const TAGS = ["database", "machine_learning", "gardening"];

function mountPage() {
  const parsed = new DOMParser().parseFromString(readFileSync("index.html", "utf8"), "text/html");
  document.body.replaceChildren(
    ...[...parsed.body.childNodes].map(node => document.importNode(node, true)),
  );
}

beforeEach(() => {
  vi.resetModules();
  mountPage();
});

afterEach(() => {
  clearMocks();
  document.body.replaceChildren();
});

describe("tag filter picker", () => {
  it("keeps the settings gear inside the Tags pill without opening the picker", async () => {
    mockIPC(command => (command === "get_all_tags" ? TAGS : []));
    const { initFilterTabs } = await import("../src/ui/filters");
    initFilterTabs();

    const pill = document.querySelector<HTMLElement>('.filter-tab[data-filter="tag"]')!;
    const gear = document.getElementById("tag-settings-btn")!;
    // One pill: the gear is a child of the tag pill, not a sibling tab.
    expect(pill.contains(gear)).toBe(true);
    expect(pill.tagName).not.toBe("BUTTON");

    gear.click();
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(document.getElementById("tag-filter-menu")).toBeNull();
  });

  it("opens a searchable tag list from the Tags button and filters on selection", async () => {
    const requests: { command: string; args: Record<string, unknown> }[] = [];
    mockIPC((command, args) => {
      requests.push({ command, args: (args ?? {}) as Record<string, unknown> });
      if (command === "get_all_tags") return TAGS;
      return [];
    });

    const { initFilterTabs } = await import("../src/ui/filters");
    const { state } = await import("../src/state");
    initFilterTabs();

    // Clicking the Tags pill is the trigger the user expects.
    const tagTab = document.querySelector<HTMLButtonElement>('.filter-tab[data-filter="tag"]');
    expect(tagTab).not.toBeNull();
    tagTab!.click();
    await vi.waitFor(() => {
      expect(document.getElementById("tag-filter-menu")).not.toBeNull();
    });

    const menu = document.getElementById("tag-filter-menu")!;
    expect(menu.querySelector<HTMLInputElement>(".tag-menu-search")).not.toBeNull();
    expect([...menu.querySelectorAll(".tag-menu-item")].map(node => node.textContent)).toEqual([
      "#database",
      "#machine_learning",
      "#gardening",
    ]);

    // Typing narrows the list instead of hiding the menu.
    const search = menu.querySelector<HTMLInputElement>(".tag-menu-search")!;
    search.value = "machine";
    search.dispatchEvent(new Event("input", { bubbles: true }));
    expect([...menu.querySelectorAll(".tag-menu-item")].map(node => node.textContent)).toEqual([
      "#machine_learning",
    ]);

    // Selecting applies the existing article filter, scoped to the active view.
    menu.querySelector<HTMLButtonElement>(".tag-menu-item")!.click();
    await vi.waitFor(() => {
      expect(state.currentFilter).toBe("tag");
      expect(state.currentTagFilter).toBe("machine_learning");
    });
    expect(requests).toContainEqual({
      command: "get_items_by_tag",
      args: { tag: "machine_learning", subscriptionId: null, limit: 50, offset: 0 },
    });
    expect(document.getElementById("tag-filter-menu")).toBeNull();
    expect(document.querySelector('.filter-tab[data-filter="tag"] .filter-label')?.textContent).toBe(
      "#machine_learning",
    );
  });

  it("toggles closed when the Tags button is clicked again", async () => {
    mockIPC(command => (command === "get_all_tags" ? TAGS : []));
    const { initFilterTabs } = await import("../src/ui/filters");
    initFilterTabs();

    const tagTab = document.querySelector<HTMLButtonElement>('.filter-tab[data-filter="tag"]')!;
    tagTab.click();
    await vi.waitFor(() => expect(document.getElementById("tag-filter-menu")).not.toBeNull());
    tagTab.click();
    expect(document.getElementById("tag-filter-menu")).toBeNull();
  });

  it("still opens after the menu was removed outside the picker", async () => {
    mockIPC(command => (command === "get_all_tags" ? TAGS : []));
    const { initFilterTabs } = await import("../src/ui/filters");
    initFilterTabs();

    const tagTab = document.querySelector<HTMLButtonElement>('.filter-tab[data-filter="tag"]')!;
    tagTab.click();
    await vi.waitFor(() => expect(document.getElementById("tag-filter-menu")).not.toBeNull());

    // Simulate another render dropping the menu without going through the
    // picker. A stale "open" flag used to make the next click close nothing.
    document.getElementById("tag-filter-menu")!.remove();
    tagTab.click();
    await vi.waitFor(() => expect(document.getElementById("tag-filter-menu")).not.toBeNull());
  });

  it("offers clearing the active tag filter", async () => {
    mockIPC(command => (command === "get_all_tags" ? TAGS : []));
    const { initFilterTabs } = await import("../src/ui/filters");
    const { state } = await import("../src/state");
    initFilterTabs();

    const tagTab = document.querySelector<HTMLButtonElement>('.filter-tab[data-filter="tag"]')!;
    tagTab.click();
    await vi.waitFor(() => expect(document.getElementById("tag-filter-menu")).not.toBeNull());
    document.querySelector<HTMLButtonElement>(".tag-menu-item")!.click();
    await vi.waitFor(() => expect(state.currentTagFilter).toBe("database"));

    tagTab.click();
    await vi.waitFor(() => expect(document.getElementById("tag-filter-menu")).not.toBeNull());
    const menu = document.getElementById("tag-filter-menu")!;
    expect(menu.querySelector(".tag-menu-active")?.textContent).toContain("#database");
    menu.querySelector<HTMLButtonElement>(".tag-menu-clear")!.click();
    expect(state.currentFilter).toBe("all");
    expect(state.currentTagFilter).toBeNull();
  });
});
