import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import { IframeManager } from "../src/iframe";
import { setSafeHtml } from "../src/sanitize";
import { renderItemDetail } from "../src/ui/render";
import { state } from "../src/state";
import type { FeedItem, Subscription } from "../src/types";

vi.mock("../src/features/actions", () => ({
  cancelIgnoreTimer: vi.fn(), toggleAutoClassify: vi.fn(),
  toggleUseWebsite: vi.fn(), deleteSubscription: vi.fn(),
}));
vi.mock("../src/ui/filters", () => ({ filterByTag: vi.fn() }));

const ARTICLE = "https://example.com/article";
const MARKDOWN = `Readable article.\n\n![Diagram](https://example.com/diagram.png)\n\n<video autoplay src="https://example.com/video.mp4"></video>\n<audio autoplay src="https://example.com/audio.mp3"></audio>\n<iframe src="https://example.com/player"></iframe>`;

afterEach(() => {
  document.body.replaceChildren();
  state.subscriptions = [];
  state.webviewPerSubscription.clear();
  state.translationStateByItemId.clear();
  state.selectedItem = null;
  vi.useRealTimers();
});

function load(markdown: string, url?: string) {
  vi.useFakeTimers();
  const container = document.createElement("div");
  document.body.appendChild(container);
  new IframeManager().load({ container, markdown, baseUrl: ARTICLE, url });
  return container;
}

function iframeDocument(container: HTMLElement): Document {
  const iframe = container.querySelector("iframe")!;
  expect(iframe).not.toBeNull();
  expect(iframe.getAttribute("sandbox")).toBe("");
  const source = iframe.getAttribute("src")!;
  expect(source).toMatch(/^data:text\/html/);
  const html = decodeURIComponent(source.slice(source.indexOf(",") + 1).replace(/#g=\d+$/, ""));
  return new DOMParser().parseFromString(html, "text/html");
}

function expectReaderOnly(doc: Document) {
  expect(doc.body.textContent).toContain("Readable article.");
  expect(doc.querySelector("img")?.getAttribute("src")).toBe("https://example.com/diagram.png");
  expect(doc.querySelector("audio,video,source,track,iframe,object,embed,script")).toBeNull();
  const csp = doc.querySelector('meta[http-equiv="Content-Security-Policy"]')?.getAttribute("content");
  expect(csp).toContain("media-src 'none'");
  expect(csp).toContain("frame-src 'none'");
  expect(csp).toContain("default-src 'none'");
}

describe("reader media blocking", () => {
  it("drops players before inserting article content into the host document", () => {
    const container = document.createElement("div");
    setSafeHtml(container, '<p>Readable article.</p><img src="https://example.com/diagram.png"><video autoplay><source src="/movie.mp4"></video><audio src="/audio.mp3"></audio><iframe src="/player"></iframe><object data="/movie.mp4"></object>');
    expect(container.querySelector("video,audio,source,track,iframe,object,embed")).toBeNull();
    expect(container.textContent).toBe("Readable article.");
    expect(container.querySelector("img")).not.toBeNull();
  });

  it("blocks media and nested frames in the local Markdown iframe", () => {
    expectReaderOnly(iframeDocument(load(MARKDOWN)));
  });

  it("never navigates to a live webpage, even when explicitly requested", () => {
    const doc = iframeDocument(load(MARKDOWN, ARTICLE));
    expectReaderOnly(doc);
    expect(doc.body.textContent).toContain("Audio/video and live webpages are blocked");
  });

  it("shows an explanation instead of fetching a page when no cached text exists", () => {
    const doc = iframeDocument(load("", ARTICLE));
    expect(doc.body.textContent).toContain("Open in browser");
    expect(doc.querySelector("video,audio,iframe,script")).toBeNull();
  });

  it.each(["markdown", "reader", "translation", "streaming"])("blocks players in the real %s detail-rendering path", (mode) => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="detail-content"></div>';
    const item = { id: 1, subscription_id: 1, title: "Article", link: ARTICLE,
      content_md: MARKDOWN, translated_content: null } as FeedItem;
    state.selectedItem = item;
    state.subscriptions = [{ id: 1, use_website: mode === "reader" }] as Subscription[];
    state.webviewPerSubscription.set(1, mode === "reader");
    if (mode === "translation" || mode === "streaming") {
      item.translated_content = '<div class="paragraph-original"><p>Readable article.</p><audio src="/a.mp3"></audio><video src="/v.mp4"></video></div>';
      state.translationStateByItemId.set(1, { useTranslation: true, inProgressContent: null,
        abortController: null, hasError: false, errorMessage: null });
    }
    renderItemDetail(item, mode === "streaming" ? { untranslatedTail: MARKDOWN } : {});
    const detail = document.getElementById("detail-content")!;
    if (mode === "reader") expectReaderOnly(iframeDocument(detail));
    else {
      expect(detail.querySelector("audio,video,source,track,iframe,object,embed")).toBeNull();
      expect(detail.textContent).toContain("Readable article.");
    }
  });

  it("rejects unsafe article URLs without creating a network iframe", () => {
    const container = load(MARKDOWN, "javascript:alert(1)");
    expect(container.querySelector("iframe")).toBeNull();
  });

  it("enforces media blocking and disallows remote frames in the application CSP", () => {
    const config = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
    const directives = new Map<string, string[]>(config.app.security.csp.split(";").map((part: string) => {
      const [name, ...values] = part.trim().split(/\s+/);
      return [name, values];
    }));
    expect(directives.get("media-src")).toEqual(["'none'"]);
    expect(directives.get("frame-src")).toEqual(["'self'", "data:"]);
  });
});
