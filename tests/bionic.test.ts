/**
 * Bionic reading: English fixation marks on article text.
 *
 * Unit coverage for the DOM transformer, then render-level integration for
 * the three host-DOM paths (markdown, bilingual + streaming tail,
 * description fallback) and the sandboxed local-markdown iframe.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// features/actions pulls Tauri dialog/opener plugins and cycles back into
// ui/render; render.ts only needs a handful of symbols from it, none of
// which run in these tests. Mock the heavy graph, keep render.ts real.
vi.mock("../src/features/actions", () => ({
  cancelIgnoreTimer: () => {},
  toggleAutoClassify: vi.fn(),
  toggleUseWebsite: vi.fn(),
  deleteSubscription: vi.fn(),
}));
// ui/filters re-enters ui/render and pulls the tag manager; not under test.
vi.mock("../src/ui/filters", () => ({
  filterByTag: vi.fn(),
}));

import { applyBionic, fixationLength } from "../src/bionic";
import { IframeManager } from "../src/iframe";
import { renderItemDetail } from "../src/ui/render";
import { state } from "../src/state";
import type { FeedItem } from "../src/types";

function wrap(html: string): HTMLElement {
  const div = document.createElement("div");
  div.innerHTML = html;
  return div;
}

function fixationTexts(root: Element): (string | null)[] {
  return Array.from(root.querySelectorAll("b.bionic-fix")).map((b) => b.textContent);
}

describe("fixationLength", () => {
  it("splits short words near the middle and long words at ~45%", () => {
    expect(fixationLength(1)).toBe(1);
    expect(fixationLength(2)).toBe(2);
    expect(fixationLength(3)).toBe(2);
    expect(fixationLength(4)).toBe(2);
    expect(fixationLength(5)).toBe(3);
    expect(fixationLength(6)).toBe(3);
    expect(fixationLength(7)).toBe(4);
    expect(fixationLength(10)).toBe(5);
    expect(fixationLength(20)).toBe(9);
  });
});

describe("applyBionic", () => {
  it("bolds word-leading fixation letters and preserves text exactly", () => {
    const root = wrap("<p>Reading is easy.</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["Read", "is", "ea"]);
  });

  it("leaves URLs, emails and identifiers whole", () => {
    const root = wrap(
      "<p>See https://example.com/read?q=1 or mail reader@example.com; WebView2 and UTF8 stay put.</p>",
    );
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["Se", "or", "ma", "an", "st", "pu"]);
  });

  it("leaves dotted domains and hyphenated runs alone", () => {
    const root = wrap("<p>Visit example.com or a well-known source.</p>");
    applyBionic(root);
    expect(fixationTexts(root)).toEqual(["Vis", "or", "a", "sou"]); // "source" is a plain word
  });

  it("skips code, pre and author bold but still processes link text", () => {
    const root = wrap(
      '<pre>const x = 1;</pre><code>echo hi</code><strong>Important words</strong>' +
        '<p><a href="https://example.com/article">Read the full article here</a></p>',
    );
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(root.querySelector("pre b.bionic-fix, code b.bionic-fix, strong b.bionic-fix")).toBeNull();
    const link = root.querySelector("a")!;
    expect(link.getAttribute("href")).toBe("https://example.com/article");
    expect(link.querySelector("b.bionic-fix")).toBeTruthy();
  });

  it("handles mixed CJK/English without touching Chinese", () => {
    const root = wrap("<p>中文English混排 text。</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["Engl", "te"]);
  });

  it("is idempotent: a second pass yields the same DOM", () => {
    const root = wrap("<p>Stable reading text stays identical after repeated rendering.</p>");
    applyBionic(root);
    const first = root.innerHTML;
    applyBionic(root);
    expect(root.innerHTML).toBe(first);
    expect(root.querySelector("b.bionic-fix b.bionic-fix")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Render-level integration (host DOM paths)
// ---------------------------------------------------------------------------

function item(overrides: Partial<FeedItem> = {}): FeedItem {
  return {
    id: 21,
    subscription_id: 5,
    guid: null,
    title: "A test article",
    link: null,
    content: null,
    content_md: null,
    description: null,
    author: null,
    published_at: null,
    fetched_at: "2026-01-01T00:00:00Z",
    is_website_content: false,
    is_read: false,
    is_favorite: false,
    is_read_later: false,
    is_ignored: false,
    tags: null,
    category: null,
    translated_title: null,
    translated_content: null,
    translated_at: null,
    ...overrides,
  };
}

function detailBody(): HTMLElement {
  return document.querySelector("#detail-content .detail-body") as HTMLElement;
}

beforeEach(() => {
  document.body.innerHTML = '<div id="detail-content"></div>';
});

afterEach(() => {
  state.subscriptions = [];
  state.translationStateByItemId.clear();
  document.body.innerHTML = "";
});

describe("renderItemDetail bionic integration", () => {
  it("processes the plain markdown body, not code, and keeps links intact", () => {
    const body = item({
      content_md:
        "## Intro\n\nEnglish body with [a link](https://example.com/a) and `code`.\n\n中文继续 English。",
    });
    renderItemDetail(body);
    const el = detailBody();
    expect(el.textContent).toContain("中文继续 English。");
    expect(el.querySelector("h2 b.bionic-fix")).toBeTruthy();
    expect(el.querySelector("code b.bionic-fix")).toBeNull();
    expect(el.querySelector("a")?.getAttribute("href")).toBe("https://example.com/a");
    expect(el.querySelector("a b.bionic-fix")).toBeTruthy();
  });

  it("processes bilingual originals and the streaming tail, not pure Chinese", () => {
    const itemWithTranslation = item({
      content_md: "unused",
      translated_content:
        '<div class="bilingual-content"><div class="translation-segment">' +
        '<p class="paragraph-original">A useful reading tool keeps the original structure.</p>' +
        '<p class="paragraph-translated">好的阅读工具保留原文结构。</p>' +
        "</div></div>",
    });
    state.translationStateByItemId.set(itemWithTranslation.id, {
      useTranslation: true,
      inProgressContent: null,
      abortController: null,
      hasError: false,
      errorMessage: null,
    });
    renderItemDetail(itemWithTranslation, {
      untranslatedTail: "Remaining English text stays visible.",
    });

    const el = detailBody();
    // 8 English words in the original — one fixation mark each.
    expect(el.querySelectorAll(".paragraph-original b.bionic-fix")).toHaveLength(8);
    expect(el.querySelector(".paragraph-translated b.bionic-fix")).toBeNull();
    expect(el.querySelector(".untranslated-tail b.bionic-fix")).toBeTruthy();
    expect(el.textContent).toContain("Remaining English text stays visible.");
    expect(el.textContent).toContain("好的阅读工具保留原文结构。");
  });

  it("processes the plain-text description fallback", () => {
    renderItemDetail(item({ description: "<p>Plain English description.</p>" }));
    expect(detailBody().querySelector("b.bionic-fix")).toBeTruthy();
  });

  it("renders identically when the same article is re-selected", () => {
    const same = item({ content_md: "English body text for the second render." });
    renderItemDetail(same);
    const first = document.getElementById("detail-content")!.innerHTML;
    renderItemDetail(same);
    expect(document.getElementById("detail-content")!.innerHTML).toBe(first);
  });
});

describe("local markdown iframe integration", () => {
  it("serialises fixation marks and their CSS into the sandboxed document", () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    new IframeManager().load({
      container,
      markdown: "# Hello\n\nBionic English body with URL https://example.com/x.",
      baseUrl: "https://example.com",
    });

    const iframe = container.querySelector("iframe");
    expect(iframe).not.toBeNull();
    expect(iframe!.getAttribute("sandbox")).toBe("");

    const src = iframe!.getAttribute("src")!;
    const html = decodeURIComponent(src.slice(src.indexOf(",") + 1).split("#")[0]);
    const parsed = new DOMParser().parseFromString(html, "text/html");
    expect(parsed.querySelector(".article-body b.bionic-fix")).toBeTruthy();
    expect(parsed.querySelector(".article-body p")?.textContent).toContain(
      "https://example.com/x",
    );
    expect(html).toContain(".bionic-fix");
  });
});
