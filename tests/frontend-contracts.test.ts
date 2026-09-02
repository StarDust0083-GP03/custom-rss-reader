import { readFileSync } from "node:fs";

import { afterEach, describe, expect, it } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { chroma, items, tags } from "../src/api";
import { safeHttpUrl } from "../src/iframe";

afterEach(() => {
  clearMocks();
});

describe("frontend security and IPC contracts", () => {
  it("allows a blank AI key field so saving can preserve the existing secret", () => {
    const html = readFileSync("index.html", "utf8");
    const page = new DOMParser().parseFromString(html, "text/html");
    const input = page.querySelector<HTMLInputElement>("#ai-api-key");
    expect(input?.required).toBe(false);
  });

  it("accepts public article URLs and rejects active or private destinations", () => {
    expect(safeHttpUrl(" https://example.com/article ")).toBe("https://example.com/article");
    expect(safeHttpUrl("http://127.0.0.1:8000/admin")).toBeNull();
    expect(safeHttpUrl("javascript:alert(1)")).toBeNull();
    expect(safeHttpUrl("data:text/html,<script>alert(1)</script>")).toBeNull();
  });

  it("sends structured classification tags through the typed command contract", async () => {
    let request: { command: string; args: Record<string, unknown> } | undefined;
    mockIPC((command, args) => {
      request = { command, args: args as Record<string, unknown> };
      return { id: 9 };
    });

    await items.saveTags(9, ["rust", "rss"], null);

    expect(request).toEqual({
      command: "save_item_tags",
      args: { itemId: 9, tags: ["rust", "rss"], category: null },
    });
  });

  it("passes the active subscription into scoped favorite queries", async () => {
    let request: { command: string; args: Record<string, unknown> } | undefined;
    mockIPC((command, args) => {
      request = { command, args: args as Record<string, unknown> };
      return [];
    });

    await items.favorites(42);

    expect(request).toEqual({
      command: "get_favorites",
      args: { subscriptionId: 42, limit: 50, offset: 0 },
    });
  });

  it("sends canonical tag merge operations through the typed command contract", async () => {
    let request: { command: string; args: Record<string, unknown> } | undefined;
    mockIPC((command, args) => {
      request = { command, args: args as Record<string, unknown> };
      return undefined;
    });

    await tags.merge("machine_learning", ["ai", "deep_learning"]);

    expect(request).toEqual({
      command: "merge_tags",
      args: {
        canonicalName: "machine_learning",
        members: ["ai", "deep_learning"],
      },
    });
  });

  it("sends tag matching settings with camelCase arguments and returns the saved config", async () => {
    let request: { command: string; args: Record<string, unknown> } | undefined;
    mockIPC((command, args) => {
      request = { command, args: args as Record<string, unknown> };
      return { enabled: true, similarity_threshold: 0.9 };
    });

    const saved = await tags.setMatchConfig(true, 0.9);

    expect(request).toEqual({
      command: "set_tag_match_config",
      args: { enabled: true, similarityThreshold: 0.9 },
    });
    expect(saved).toEqual({ enabled: true, similarity_threshold: 0.9 });
  });

  it("bounds the tag matching threshold slider to the backend's accepted range", () => {
    const html = readFileSync("index.html", "utf8");
    const page = new DOMParser().parseFromString(html, "text/html");
    const slider = page.querySelector<HTMLInputElement>("#tag-match-threshold");
    expect(slider?.min).toBe("0.5");
    expect(slider?.max).toBe("1");
    expect(page.querySelector("#tag-match-form")).not.toBeNull();
  });

  it("uses the one-click Chroma initialization command contract", async () => {
    let request: { command: string; args: Record<string, unknown> } | undefined;
    mockIPC((command, args) => {
      request = { command, args: args as Record<string, unknown> };
      return {
        config: {
          host: "http://localhost",
          port: 8000,
          collection_name: "rss_articles",
          enabled: true,
        },
        sync: { indexed: 3, deleted: 0, pages: 1, duration_ms: 42 },
      };
    });

    const result = await chroma.enableAndIndex({
      host: "http://localhost",
      port: 8000,
      collectionName: "rss_articles",
    });

    expect(request).toEqual({
      command: "enable_chroma_and_index",
      args: {
        host: "http://localhost",
        port: 8000,
        collectionName: "rss_articles",
      },
    });
    expect(result.sync.indexed).toBe(3);
  });
});
