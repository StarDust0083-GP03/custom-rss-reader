import { afterEach, describe, expect, it } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { items } from "../src/api";
import { safeHttpUrl } from "../src/iframe";

afterEach(() => {
  clearMocks();
});

describe("frontend security and IPC contracts", () => {
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
});
