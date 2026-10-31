import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FeedItem } from "../src/types";

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Map<string, (event: { payload: unknown }) => void>() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    ipc.listeners.set(name, handler);
    return () => ipc.listeners.delete(name);
  }),
}));
vi.mock("../src/features/actions", () => ({
  markAsRead: vi.fn(), selectItem: vi.fn(), cancelIgnoreTimer: vi.fn(),
  toggleAutoClassify: vi.fn(), toggleUseWebsite: vi.fn(), deleteSubscription: vi.fn(),
}));
vi.mock("../src/ui/filters", () => ({ filterByTag: vi.fn() }));
vi.mock("../src/toast", () => ({ success: vi.fn(), error: vi.fn(), info: vi.fn() }));

import { state } from "../src/state";
import { translateItem, cancelTranslation } from "../src/features/ai";
import { renderItemDetail } from "../src/ui/render";
import { initReadingLayout } from "../src/ui/reading-layout";

const FIRST = '<div class="translation-paragraph"><div class="paragraph-original">First paragraph.</div><div class="paragraph-translated">第一段。</div></div>';
let article: FeedItem;
let finish: () => void;
let run: Promise<void> | undefined;
const rerender = () => { if (state.selectedItem) renderItemDetail(state.selectedItem); };
const parallel = () => document.getElementById("parallel-btn") as HTMLButtonElement;
const translate = () => document.getElementById("translate-btn") as HTMLButtonElement;

beforeEach(() => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: vi.fn() });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener: vi.fn() }));
  document.body.innerHTML = '<div class="app-container"><button id="translate-btn">Translate</button><button id="parallel-btn" hidden>Parallel</button><div id="detail-content"></div></div>';
  state.translationStateByItemId.clear();
  state.currentItems = [];
  state.subscriptions = [];
  article = { id: 7, subscription_id: 3, title: "Article", link: null, is_read: false,
    content_md: "First paragraph.\n\nRemaining paragraph.", translated_content: null } as FeedItem;
  state.selectedItem = article;
  ipc.listeners.clear();
  ipc.invoke.mockReset();
  ipc.invoke.mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
  initReadingLayout();
  document.addEventListener("reading-layout-change", rerender);
});

afterEach(async () => {
  if (run) { finish(); await run; run = undefined; }
  document.removeEventListener("reading-layout-change", rerender);
  state.translationStateByItemId.clear();
  state.selectedItem = null;
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

async function start() {
  run = translateItem(article);
  await vi.waitFor(() => expect(ipc.listeners.has("translation-progress")).toBe(true));
}

function progress(patch: Record<string, unknown> = {}) {
  ipc.listeners.get("translation-progress")!({ payload: {
    item_id: article.id, total: 2, completed: 1, html_chunk: FIRST, is_complete: false, ...patch,
  } });
}

describe("streaming translation controls", () => {
  it("keeps Parallel and the original tail when toggling layout or rerendering the selected article", async () => {
    await start();
    progress();
    expect(parallel().hidden).toBe(false);
    expect(article.translated_content).toBeNull(); // A partial run is not a completed cache.
    for (let i = 0; i < 3; i++) {
      parallel().click();
      expect(parallel().hidden).toBe(false);
      expect(document.querySelector(".paragraph-translated")?.textContent).toContain("第一段");
      expect(document.querySelector(".untranslated-tail")?.textContent).toContain("Remaining paragraph.");
      expect(document.querySelector(".untranslated-tail")?.textContent).not.toContain("First paragraph.");
    }
    renderItemDetail(state.selectedItem!);
    expect(parallel().hidden).toBe(false);
    expect(document.querySelector(".untranslated-tail")?.textContent).toContain("Remaining paragraph.");
  });

  it("does not restart the Cancel state or replace its text node on every paragraph", async () => {
    await start();
    progress();
    const label = translate().firstChild;
    const observer = new MutationObserver(() => {});
    observer.observe(translate(), { attributes: true, attributeFilter: ["class"], childList: true });
    progress({ html_chunk: "", completed: 1 });
    renderItemDetail(state.selectedItem!);
    const changes = observer.takeRecords();
    observer.disconnect();
    expect(translate().textContent).toBe("Cancel");
    expect(translate().firstChild).toBe(label);
    expect(changes).toHaveLength(0);
    expect(translate().getAttribute("aria-busy")).toBe("true");
  });

  it.each(["complete", "cancel", "error"])("leaves the correct controls and content after %s", async ending => {
    await start();
    progress();
    if (ending === "cancel") cancelTranslation(article);
    else progress({ is_complete: true, html_chunk: "", has_error: ending === "error", error_messages: ["Test failure"] });
    expect(parallel().hidden).toBe(ending !== "complete");
    expect(translate().classList.contains("translating")).toBe(false);
    expect(translate().getAttribute("aria-busy")).toBe("false");
    expect(translate().textContent).toBe(ending === "complete" ? "Show Original" : ending === "error" ? "Retry" : "Translate");
    expect(document.querySelector("#detail-content")?.textContent).toContain("First paragraph.");
    if (ending !== "complete") {
      expect(article.translated_content).toBeNull();
      expect(document.querySelector(".untranslated-tail")).toBeNull();
    }
  });

  it("keeps the opacity pulse while disabling native WebKit button painting", () => {
    const css = readFileSync("src/styles.css", "utf8");
    const base = css.match(/#translate-btn\s*\{([^}]+)\}/)?.[1];
    const busy = css.match(/#translate-btn\.translating\s*\{([^}]+)\}/)?.[1];
    expect(base).toMatch(/-webkit-appearance:\s*none/);
    expect(base).toMatch(/(?:^|\n)\s*appearance:\s*none/);
    expect(busy).toMatch(/animation:\s*pulse\s+1\.5s\s+infinite/);
    expect(busy).toMatch(/background:\s*var\(--accent-color\)/);
  });
});
