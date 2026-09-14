import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

const CATALOG = [
  { name: "rust", usage_count: 4, aliases: [], adopted: true },
  { name: "docker", usage_count: 3, aliases: ["containerization"], adopted: true },
  { name: "opinion", usage_count: 1, aliases: [], adopted: true },
];
const WORKSPACE = {
  expected_hash: "h",
  undecided: 3,
  categories: [{ id: 5, label: "Programming languages", definition: "", sort_order: 5 }],
  words: CATALOG.map(word => ({ name: word.name, usage_count: word.usage_count, category_id: null, state: "undecided", source: "none" })),
};
const OVERVIEW = {
  snapshot_id: "s",
  scope_label: "all subscriptions",
  coverage: { total_items: 3, tagged_items: 3, unreadable_items: 0 },
  nodes: CATALOG.map(word => ({ name: word.name, usage_count: word.usage_count, category_id: null })),
  edges: [], communities: [], singletons: CATALOG.map(word => word.name), blocked_excluded: 0,
  structuring: "semantic", warnings: [],
};

function mountPage() {
  const parsed = new DOMParser().parseFromString(readFileSync("index.html", "utf8"), "text/html");
  document.body.replaceChildren(...[...parsed.body.childNodes].map(node => document.importNode(node, true)));
}

beforeEach(() => mountPage());
afterEach(() => clearMocks());

describe("tag workspace after category removal", () => {
  it("keeps vocabulary actions in the Tags footer", () => {
    expect(document.getElementById("tag-graph-advanced")).toBeNull();
    expect(document.getElementById("tag-consolidate-single-use")).not.toBeNull();
    expect(document.getElementById("tag-graph-manage")).not.toBeNull();
  });

  it("closes the workspace from its close button", async () => {
    const { initTagGraph } = await import("../src/ui/tag-graph");
    const modal = document.getElementById("tag-graph-modal")!;
    initTagGraph();
    modal.classList.add("visible");
    modal.querySelector<HTMLButtonElement>(".close-modal")!.click();
    expect(modal.classList.contains("visible")).toBe(false);
  });

  it("uses the vocabulary list, not the removed category graph", async () => {
    const commands: string[] = [];
    mockIPC(command => {
      commands.push(command);
      if (command === "get_tag_catalog") return CATALOG;
      if (command === "get_topic_workspace") return WORKSPACE;
      if (command === "get_tag_overview") return OVERVIEW;
      if (command === "tag_dictionary_status") return { tags: 3, explained: 3, indexed: 3 };
      if (command === "get_tag_match_config") return { enabled: true, similarity_threshold: 0.85, grouping_method: "embedding", community_min_weight: 1 };
      return undefined;
    });
    const { initTagGraph, openTagGraph } = await import("../src/ui/tag-graph");
    initTagGraph();
    await openTagGraph();
    expect(document.querySelectorAll("#tag-graph-list .tag-graph-row")).toHaveLength(3);
    expect(document.getElementById("tag-graph-canvas")).toBeNull();
    expect(document.getElementById("tag-grouping-method")).toBeNull();
    expect(commands).not.toContain("get_tag_graph");
    expect(commands).not.toContain("set_tag_adopted");
    expect(commands).not.toContain("map_tag");
    expect(commands).not.toContain("unmap_tag");
  });

  it("consolidates single-use tags from the Tags footer", async () => {
    const commands: string[] = [];
    mockIPC(command => {
      commands.push(command);
      if (command === "get_tag_catalog") return CATALOG;
      if (command === "get_topic_workspace") return WORKSPACE;
      if (command === "get_tag_overview") return OVERVIEW;
      if (command === "tag_dictionary_status") return { tags: 3, explained: 3, indexed: 3 };
      if (command === "get_tag_match_config") return { enabled: true, similarity_threshold: 0.85, grouping_method: "embedding", community_min_weight: 1 };
      if (command === "consolidate_single_use_tags") return { single_use: 1, merged: 1, unmatched: 0 };
      return undefined;
    });
    const { initTagGraph, openTagGraph } = await import("../src/ui/tag-graph");
    initTagGraph();
    await openTagGraph();
    document.getElementById("tag-consolidate-single-use")!.click();
    await vi.waitFor(() => expect(commands).toContain("consolidate_single_use_tags"));
  });

  it("opens vocabulary management as a separate layer and returns to the workspace", async () => {
    mockIPC(command => {
      if (command === "get_tag_catalog") return CATALOG;
      if (command === "get_blocked_tags") return [];
      if (command === "get_tag_match_config") {
        return { enabled: true, similarity_threshold: 0.85, grouping_method: "embedding", community_min_weight: 1 };
      }
      return undefined;
    });
    const { openTagManager, closeTagManager } = await import("../src/features/tags");
    const graph = document.getElementById("tag-graph-modal")!;
    const manager = document.getElementById("tag-manager-modal")!;
    graph.classList.add("visible");

    await openTagManager();
    expect(graph.classList.contains("visible")).toBe(false);
    expect(manager.classList.contains("visible")).toBe(true);

    closeTagManager();
    expect(manager.classList.contains("visible")).toBe(false);
    expect(graph.classList.contains("visible")).toBe(true);
  });

  it("keeps the map and topics as the only structural views", async () => {
    mockIPC(command => {
      if (command === "get_tag_catalog") return CATALOG;
      if (command === "get_topic_workspace") return WORKSPACE;
      if (command === "get_tag_overview") return OVERVIEW;
      if (command === "tag_dictionary_status") return { tags: 3, explained: 3, indexed: 3 };
      if (command === "get_tag_match_config") return { enabled: true, similarity_threshold: 0.85, grouping_method: "embedding", community_min_weight: 1 };
      return undefined;
    });
    const { initTagGraph, openTagGraph } = await import("../src/ui/tag-graph");
    initTagGraph();
    await openTagGraph();
    expect([...document.querySelectorAll("[data-tag-view]")].map(tab => tab.textContent?.trim())).toEqual(["Overview", "Topics", "Tags"]);
    expect(document.getElementById("tag-graph-apply")).toBeNull();
    expect(document.getElementById("tag-graph-discard")).toBeNull();
    const consolidate = document.getElementById("tag-consolidate-single-use")!;
    expect(consolidate.hidden).toBe(true);
    document.querySelector<HTMLButtonElement>('[data-tag-view="tags"]')!.click();
    expect(consolidate.hidden).toBe(false);
  });
});
