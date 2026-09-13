import { readFileSync } from "node:fs";

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

import { groupByTopic, mergeAssignments, TopicManager } from "../src/ui/topic-manager";
import type { TopicAssignment, TopicWord, TopicWorkspace } from "../src/types";

const WORD = (name: string, usage_count = 3): TopicWord => ({
  name,
  usage_count,
  category_id: null,
  state: "undecided",
  source: "none",
});

const WORKSPACE: TopicWorkspace = {
  expected_hash: "hash-1",
  undecided: 2,
  categories: [
    { id: 5, label: "Programming languages", definition: "", sort_order: 5 },
    { id: 12, label: "Containers & deployment", definition: "", sort_order: 12 },
  ],
  words: [
    { name: "rust", usage_count: 14, category_id: 5, state: "assigned", source: "manual" },
    { name: "docker", usage_count: 12, category_id: 12, state: "assigned", source: "ai" },
    { name: "opinion", usage_count: 20, category_id: null, state: "context_only", source: "manual" },
    WORD("zig", 4),
  ],
};

describe("topic decisions", () => {
  it("lets an immediate decision replace the stored row and leaves the rest alone", () => {
    const stored: TopicAssignment[] = [
      { tag_name: "rust", category_id: 5, state: "assigned", source: "manual" },
      { tag_name: "opinion", category_id: null, state: "context_only", source: "manual" },
    ];
    const merged = mergeAssignments(
      stored,
      new Map([["opinion", { category_id: null, state: "review" as const, source: "manual" as const }]]),
    );

    expect(merged).toEqual([
      { tag_name: "opinion", category_id: null, state: "review", source: "manual" },
      { tag_name: "rust", category_id: 5, state: "assigned", source: "manual" },
    ]);
  });

  it("drops a row when a word goes back to undecided instead of storing an empty opinion", () => {
    const stored: TopicAssignment[] = [
      { tag_name: "rust", category_id: 5, state: "assigned", source: "manual" },
    ];
    expect(
      mergeAssignments(stored, new Map([["rust", { category_id: null, state: "undecided", source: "manual" }]])),
    ).toEqual([]);
  });

  it("never sends a topic for a word that is not assigned", () => {
    const merged = mergeAssignments(
      [],
      new Map([
        [
          "docker",
          { category_id: null, state: "context_only" as const, source: "manual" as const },
        ],
      ]),
    );
    expect(merged[0].category_id).toBeNull();
  });

  it("groups words by their selected topic", () => {
    const groups = groupByTopic(WORKSPACE.words, new Map());
    expect(groups.get(5)?.map(word => word.name)).toEqual(["rust"]);
    expect(groups.get(12)?.map(word => word.name)).toEqual(["docker"]);
    expect(groups.get(null)?.map(word => word.name)).toEqual(["opinion", "zig"]);

    const selected = groupByTopic(
      WORKSPACE.words,
      new Map([["zig", { category_id: 5, state: "assigned" as const, source: "manual" as const }]]),
    );
    expect(selected.get(5)?.map(word => word.name)).toEqual(["rust", "zig"]);
  });
});

describe("topic workspace writes", () => {
  beforeEach(() => {
    const parsed = new DOMParser().parseFromString(readFileSync("index.html", "utf8"), "text/html");
    document.body.replaceChildren(
      ...[...parsed.body.childNodes].map(node => document.importNode(node, true)),
    );
  });

  afterEach(() => {
    clearMocks();
  });

  it("paginates a long topic word list", () => {
    const words = Array.from({ length: 52 }, (_, index) => WORD(`word_${index}`, index));
    const manager = new TopicManager(() => undefined);
    manager.setWorkspace({ ...WORKSPACE, undecided: 52, words });

    expect(document.querySelectorAll("#topic-word-list .topic-word")).toHaveLength(50);
    expect(document.getElementById("topic-word-pagination")?.hidden).toBe(false);
    expect(document.querySelector(".topic-page-label")?.textContent).toContain("Page 1 of 2");

    const buttons = document.querySelectorAll<HTMLButtonElement>(".topic-page-button");
    buttons[1].click();
    expect(document.querySelectorAll("#topic-word-list .topic-word")).toHaveLength(2);
    expect(document.querySelector(".topic-word-name")?.textContent).toBe("word_50");
  });

  it("applies each AI topic page and reports progress", async () => {
    const words = Array.from({ length: 45 }, (_, index) => WORD(`ai_word_${index}`, index));
    const initial: TopicWorkspace = { ...WORKSPACE, expected_hash: "h1", undecided: 45, words };
    const firstApplied: TopicWorkspace = {
      ...initial,
      expected_hash: "h2",
      undecided: 5,
      words: words.map((word, index) => index < 40
        ? { ...word, category_id: 5, state: "assigned", source: "ai" }
        : word),
    };
    const finalApplied: TopicWorkspace = {
      ...firstApplied,
      expected_hash: "h3",
      undecided: 0,
      words: firstApplied.words.map(word => ({
        ...word,
        category_id: 5,
        state: "assigned",
        source: "ai",
      })),
    };
    const suggestionCalls: Record<string, unknown>[] = [];
    const applyCalls: Record<string, unknown>[] = [];
    let suggestionPage = 0;
    mockIPC((command, args) => {
      if (command === "get_topic_workspace") return initial;
      if (command === "suggest_topic_assignments") {
        suggestionCalls.push(args as Record<string, unknown>);
        const start = suggestionPage++ * 40;
        const page = words.slice(start, start + 40);
        return {
          suggestions: page.map(word => ({
            name: word.name,
            category_id: 5,
            state: "assigned",
            reason: "Matches the programming topic",
          })),
          remaining: 45 - Math.min(45, start + page.length),
          considered: page.length,
          skipped: 0,
        };
      }
      if (command === "apply_topic_changes") {
        applyCalls.push(args as Record<string, unknown>);
        return applyCalls.length === 1 ? firstApplied : finalApplied;
      }
      return undefined;
    });

    const manager = new TopicManager(() => undefined);
    await manager.load();
    await manager.suggestWithAi();

    expect(suggestionCalls).toEqual([{ limit: 40 }, { limit: 40 }]);
    expect(applyCalls).toHaveLength(2);
    expect((applyCalls[0].assignments as TopicAssignment[])).toHaveLength(40);
    expect((applyCalls[1].assignments as TopicAssignment[])).toHaveLength(45);
    expect(document.getElementById("topic-suggest-progress-label")?.textContent).toContain("Complete: 45/45");
  });

  it("applies a manual topic decision immediately", async () => {
    const commands: string[] = [];
    let payload: Record<string, unknown> | undefined;
    mockIPC((command, args) => {
      commands.push(command);
      if (command === "get_topic_workspace") return WORKSPACE;
      if (command === "apply_topic_changes") {
        payload = args as Record<string, unknown>;
        return WORKSPACE;
      }
      return undefined;
    });

    const manager = new TopicManager(() => undefined);
    manager.bind();
    await manager.load();

    const categorySelect = document.querySelector<HTMLSelectElement>(
      "#topic-word-list .topic-word-select",
    );
    expect(categorySelect).not.toBeNull();
    categorySelect!.value = "12";
    categorySelect!.dispatchEvent(new Event("change", { bubbles: true }));
    await new Promise(resolve => setTimeout(resolve, 0));

    expect(commands).toEqual(["get_topic_workspace", "apply_topic_changes"]);
    expect(payload?.expectedHash).toBe("hash-1");
    const assignments = payload?.assignments as TopicAssignment[];
    expect(assignments.find(row => row.tag_name === "opinion")).toEqual({
      tag_name: "opinion",
      category_id: 12,
      state: "assigned",
      source: "manual",
    });
    expect(document.getElementById("topic-save")).toBeNull();
    expect(document.getElementById("topic-discard")).toBeNull();
  });

  it("reports a failed immediate topic decision", async () => {
    let attempts = 0;
    mockIPC(command => {
      if (command === "get_topic_workspace") return WORKSPACE;
      if (command === "apply_topic_changes") {
        attempts += 1;
        throw new Error("The tag library changed since this edit was prepared");
      }
      return undefined;
    });

    const manager = new TopicManager(() => undefined);
    manager.bind();
    await manager.load();

    const status = document.getElementById("topic-status");
    const select = document.querySelector<HTMLSelectElement>("#topic-word-list .topic-word-select");
    expect(select).not.toBeNull();
    select!.value = "12";
    select!.dispatchEvent(new Event("change", { bubbles: true }));
    await new Promise(resolve => setTimeout(resolve, 0));

    expect(attempts).toBe(1);
    expect(status?.classList.contains("is-error")).toBe(true);
    expect(status?.textContent).toMatch(/changed since/);
  });
});
