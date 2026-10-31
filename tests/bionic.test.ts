/**
 * Bionic reading: English fixation marks on article text.
 *
 * Unit coverage for the DOM transformer (including the word-boundary cases
 * that regressed once: inline markup, contractions, compounds, URL
 * boundaries, pathological punctuation), then render-level integration for
 * the three host-DOM paths (markdown, bilingual + streaming tail,
 * description fallback) and the sandboxed local-markdown iframe.
 */

import { readFileSync } from "node:fs";
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
  // Golden outputs from text-vide's getFixationLength.ts at commit
  // 1353987f2bf12fae8553a90ab424a9c417a2aaa1, not calculated by our code.
  const lengths = [0,1,2,3,4,5,6,7,8,9,10,12,13,16,17,18,20,24,25,29,30,35,36,42,43,48,49,50,64,128];
  const profiles = [
    [0,0,1,2,3,3,4,5,6,7,8,10,10,13,14,14,16,20,20,24,24,29,29,35,35,40,40,41,55,119],
    [0,1,1,1,2,3,4,5,5,6,7,8,9,10,11,12,13,16,17,19,20,23,24,28,29,32,33,33,47,111],
    [0,1,1,1,2,3,3,4,4,5,5,6,7,8,9,9,10,12,13,15,15,18,18,21,22,24,25,25,39,103],
    [0,0,1,1,2,2,2,2,3,3,3,4,5,6,7,7,8,8,8,10,10,12,12,14,14,16,16,17,31,95],
    [0,0,1,1,1,2,2,2,2,2,3,3,3,4,5,5,6,7,7,8,8,9,9,11,11,12,12,13,27,91],
  ];

  it.each([1, 2, 3, 4, 5])("matches upstream fixation profile %i at every boundary", (point) => {
    expect(lengths.map(len => fixationLength(len, point))).toEqual(profiles[point - 1]);
  });

  it("uses upstream profile 1 by default and for an unknown profile", () => {
    expect(lengths.map(len => fixationLength(len))).toEqual(profiles[0]);
    for (const point of [0, 6, NaN, 1.5]) {
      expect(fixationLength(7, point)).toBe(5);
    }
  });
});

describe("applyBionic", () => {
  it("bolds word-leading fixation letters and preserves text exactly", () => {
    const root = wrap("<p>Reading is easy.</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["Readi", "i", "eas"]);
  });

  it("leaves URLs, emails and identifiers whole", () => {
    const root = wrap(
      "<p>See https://example.com/read?q=1 or mail reader@example.com; WebView2 and UTF8 stay put.</p>",
    );
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["Se", "o", "mai", "an", "sta", "pu"]);
  });

  it("leaves dotted domains alone but fixates hyphenated prose", () => {
    const root = wrap("<p>Visit example.com or a well-known source.</p>");
    applyBionic(root);
    expect(fixationTexts(root)).toEqual(["Vis", "o", "wel", "kno", "sour"]);
  });

  it("fixates each component of a multi-part compound", () => {
    const root = wrap("<p>state-of-the-art open-source</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["sta", "o", "th", "ar", "ope", "sour"]);
  });

  it("keeps one leading mark on contractions and possessives", () => {
    const root = wrap("<p>don't reader’s can't</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    // No suffix anchors: "don't" must not become **do**n'**t**.
    expect(fixationTexts(root)).toEqual(["don", "reade", "can"]);
  });

  it("uses one word unit across inline markup", () => {
    const root = wrap("<p>rea<em>ding</em> text</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    // The same word must get the same fixation no matter how it is marked up.
    expect(fixationTexts(root)).toEqual(["readi", "tex"]);
    expect(root.querySelector("b.bionic-fix em")).toBeTruthy();
    const marked = root.querySelector("b.bionic-fix em")!.textContent ?? "";
    const rest = root.querySelectorAll("em")[1]?.textContent ?? "";
    expect(marked + rest).toBe("ding");
  });

  it("does not merge words across whitespace-only or Chinese text nodes", () => {
    for (const html of [
      '<p><span>Reading</span> <em>text</em></p>',
      '<p><span>Reading</span>中文<em>text</em></p>',
    ]) {
      const root = wrap(html);
      const before = root.textContent;
      applyBionic(root);
      expect(fixationTexts(root)).toEqual(["Readi", "tex"]);
      expect(root.textContent).toBe(before);
    }
  });

  it("stops URL protection at prose punctuation", () => {
    const root = wrap("<p>https://example.com。English正文</p>");
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["Engli"]);
  });

  it("protects a URL that inline markup splits", () => {
    const root = wrap('<p>https://example.com/<span>article</span> here</p>');
    const before = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual(["her"]);
    expect(root.querySelector("span")?.textContent).toBe("article");
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
    expect(fixationTexts(root)).toEqual(["Engli", "tex"]);
  });

  it("changes fixation profile without losing markup or stacking marks", () => {
    const root = wrap('<p><a href="https://example.com">Reading</a> is easy.</p><img src="https://example.com/hero.png">');
    const before = root.textContent;
    applyBionic(root);
    const normal = root.innerHTML;
    applyBionic(root, 5);
    expect(fixationTexts(root)).toEqual(["Re", "i", "e"]);
    expect(root.textContent).toBe(before);
    expect(root.querySelector("a")?.getAttribute("href")).toBe("https://example.com");
    expect(root.querySelector("img")?.getAttribute("src")).toBe("https://example.com/hero.png");
    applyBionic(root);
    expect(root.innerHTML).toBe(normal);
  });

  it("is idempotent: a second pass yields the same DOM", () => {
    const root = wrap("<p>Stable reading text stays identical after repeated rendering.</p>");
    applyBionic(root);
    const first = root.innerHTML;
    applyBionic(root);
    expect(root.innerHTML).toBe(first);
    expect(root.querySelector("b.bionic-fix b.bionic-fix")).toBeNull();
  });

  it("is idempotent across inline markup too", () => {
    const root = wrap("<p>rea<em>ding</em> text</p>");
    applyBionic(root);
    const first = root.innerHTML;
    applyBionic(root);
    expect(root.innerHTML).toBe(first);
  });

  it("stays linear on pathological punctuation runs", () => {
    // A long run of separator characters must be scanned as ONE protected
    // lexeme. The earlier regex-based version re-tried an email candidate at
    // every position: ~800 ms at this size.
    const root = wrap("<p></p>");
    root.firstElementChild!.textContent = "+".repeat(32000) + "a";
    const before = root.textContent;
    const started = performance.now();
    applyBionic(root);
    const elapsed = performance.now() - started;

    expect(root.textContent).toBe(before);
    expect(fixationTexts(root)).toEqual([]);
    expect(elapsed).toBeLessThan(400);
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

  it("keeps emphasised words whole in rendered markdown", () => {
    renderItemDetail(item({ content_md: "A rea*ding* test." }));
    const el = detailBody();
    // Same word unit across <em>: one mark, not "re" + "di".
    expect(fixationTexts(el)).toEqual(["readi", "tes"]);
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
    // Profile 1 leaves single-letter words alone; other words use the table.
    expect(fixationTexts(el.querySelector(".paragraph-original")!))
      .toEqual(["usef", "readi", "too", "kee", "th", "origin", "structu"]);
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

describe("bionic typography", () => {
  it("uses bold fixation marks consistently in both render paths", () => {
    // Preserve the text-vide lengths; tune only the contrast against 400 body text.
    const rule = /\.bionic-fix\s*\{[^}]*font-weight:\s*700/;
    expect(readFileSync("src/styles.css", "utf8")).toMatch(rule);
    expect(readFileSync("src/iframe.ts", "utf8")).toMatch(rule);
  });
});
