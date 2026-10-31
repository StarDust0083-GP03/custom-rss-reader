import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../src/features/actions", () => ({
  cancelIgnoreTimer: () => {},
  toggleAutoClassify: vi.fn(), toggleUseWebsite: vi.fn(), deleteSubscription: vi.fn(),
}));
vi.mock("../src/ui/filters", () => ({ filterByTag: vi.fn() }));

import { renderItemDetail } from "../src/ui/render";
import { state } from "../src/state";
import type { FeedItem } from "../src/types";
import { normalizeMediaUrls } from "../src/media";
import { configureMarked, renderMarkdown } from "../src/markdown";
import { dedupeImages, escapeHtml, setSafeHtml } from "../src/sanitize";

const ARTICLE = "https://example.com/articles/2026/media.html";

function body(html: string): HTMLElement {
  const el = document.createElement("div");
  el.className = "detail-body";
  setSafeHtml(el, html);
  return el;
}

describe("screenshot regression: cached block links", () => {
  it("renders a linked HTML avatar despite blank lines in the cached Markdown", () => {
    configureMarked();
    const md = '[\n\n<img alt="Author" src="https://example.com/avatar.jpg" width="32" height="32">\n\n](/@author?source=post_page---byline)';
    const el = body(renderMarkdown(md));
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector('a img')).toBeTruthy();
    expect(el.querySelector('a')?.getAttribute('href')).toBe('https://example.com/@author?source=post_page---byline');
    expect(el.textContent).not.toContain('](');
    expect(el.textContent?.trim()).toBe('');
  });

  it("drops empty multiline sign-in controls rather than rendering their URLs", () => {
    configureMarked();
    const el = body(renderMarkdown('[\n\n](/m/signin?actionUrl=https://example.com/vote)\n\nActual prose.'));
    normalizeMediaUrls(el, ARTICLE);
    expect(el.textContent?.trim()).toBe('Actual prose.');
    expect(el.querySelector('a')).toBeNull();
  });

  it("keeps code examples of block links literal", () => {
    const md = '```md\n[\n\n<img src="/image.jpg">\n\n](/article)\n```';
    const el = body(renderMarkdown(md));
    expect(el.querySelector('pre code')?.textContent).toContain('[\n\n<img');
    expect(el.querySelector('img')).toBeNull();
  });

  it("shows a missing-source placeholder instead of silently losing the article image", () => {
    const el = body('<img alt="Diagram" width="700" height="467">');
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector('.image-fallback')?.textContent).toContain('Image source missing');
    expect(el.querySelector('.image-fallback')?.getAttribute('href')).toBe(ARTICLE);
  });
});

const cachedHeading = '[\n\n\\# Screenshots and Showcases\n----------\n\n](#screenshots-and-showcases)';

describe("screenshot regression: linked heading blocks", () => {

  it("restores the actual cached setext heading and its anchor", () => {
    const el = body(renderMarkdown(cachedHeading));
    normalizeMediaUrls(el, ARTICLE);
    const heading = el.querySelector("h2");
    expect(heading?.textContent).toBe("Screenshots and Showcases");
    expect(heading?.id).toBe("art-screenshots-and-showcases");
    expect(el.textContent?.trim()).toBe("Screenshots and Showcases");
  });

  it("restores ATX headings with inline formatting and a non-derived target id", () => {
    const el = body(renderMarkdown('[\n\n### **Installation**\n\n](#custom-install)'));
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector("h3 strong")?.textContent).toBe("Installation");
    expect(el.querySelector("h3")?.id).toBe("art-custom-install");
    expect(el.textContent?.trim()).toBe("Installation");
  });

  it("preserves an external destination when the heading is not a self-link", () => {
    const el = body(renderMarkdown('[\n\nDocumentation\n===\n\n](https://example.com/docs)'));
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector("h1 a")?.getAttribute("href")).toBe("https://example.com/docs");
    expect(el.querySelector("h1")?.textContent).toBe("Documentation");
  });

  it("keeps code samples and ordinary links unchanged", () => {
    const code = `${cachedHeading}\n\n](https://example.com/orphan)`;
    const el = body(renderMarkdown('```markdown\n' + code + '\n```\n\n[Ordinary link](/docs)'));
    expect(el.querySelector("pre code")?.textContent).toBe(code + '\n');
    expect(el.querySelector("h2")).toBeNull();
    expect(el.querySelector("a")?.textContent).toBe("Ordinary link");
  });
});

describe("cached Medium translation fragments", () => {
  const articleUrl = 'https://medium.com/@author/story-123';

  it("repairs complete byline/control links before cleaning loose cached destinations", () => {
    const source = '[\n\n<img alt="Quan Strata" src="https://example.com/avatar.jpg" width="32" height="32">\n\n](/@quanstrata?source=post_page---byline--123)\n\n[Quan Strata](/@quanstrata?source=post_page---byline--123)\n\n3 min read\n\n[\n\n](/m/signin?actionUrl=https://medium.com/_/vote/p/123)\n\n[\n\n](/m/signin?actionUrl=https://medium.com/_/repost/p/123)\n\nBeyond stability.\n\n**The Equilibrium Fallacy**';
    const el = body(renderMarkdown(source, articleUrl, 'Quan Strata'));
    normalizeMediaUrls(el, articleUrl);
    expect(el.querySelector('a img')).toBeTruthy();
    expect([...el.querySelectorAll('p')].filter(p => p.textContent?.trim() === '[')).toHaveLength(0);
    expect(el.querySelectorAll('figcaption')).toHaveLength(0);
    expect((el.textContent?.match(/Quan Strata/g) ?? [])).toHaveLength(1);
    expect(el.textContent).toContain('Beyond stability.');
    expect(el.textContent).toContain('The Equilibrium Fallacy');
    expect(el.textContent).not.toContain('/m/signin');
  });
  const cached = '(/ @author?source=post_page---byline--123)\n\n[Author Name\n\n9 min read\n\n](/m/signin?actionUrl=https://medium.com/_/repost/p/123)\n\nReal prose with [documentation](/docs).';

  it("cleans old byline/sign-in fragments without discarding author or article links", () => {
    const el = body(renderMarkdown(cached, articleUrl, 'Author Name'));
    normalizeMediaUrls(el, articleUrl);
    expect(el.textContent).toContain('Author Name');
    expect(el.textContent).toContain('Real prose with documentation.');
    expect(el.textContent).not.toMatch(/\[Author|source=post_page|\]\(\/m\/signin/);
    expect(el.querySelector('a')?.getAttribute('href')).toBe('https://medium.com/docs');
  });

  it("does not delete ordinary profile references, arrays, parentheses or code examples", () => {
    const prose = '(https://medium.com/@author?ref=reading)\n\narray [0] and (aside)\n\n```text\n' + cached + '\n```';
    const el = body(renderMarkdown(prose, articleUrl, 'Author Name'));
    expect(el.textContent).toContain('(https://medium.com/@author?ref=reading)');
    expect(el.textContent).toContain('array [0] and (aside)');
    expect(el.querySelector('pre code')?.textContent).toBe(cached + '\n');
  });

  it("preserves source-code examples already wrapped in HTML pre/code", () => {
    const code = cached + '\n\n](/orphan)';
    const el = body(renderMarkdown(`<pre><code>${escapeHtml(code)}</code></pre>`, articleUrl, 'Author Name'));
    expect(el.querySelector('pre code')?.textContent).toBe(code);
    expect(el.querySelector('a')).toBeNull();
  });

  it("does not apply Medium-specific metadata rules to unrelated sites", () => {
    const el = body(renderMarkdown('(/ @author?source=post_page---byline--123)', 'https://example.com/article'));
    expect(el.textContent).toContain('source=post_page');
  });
});

describe("Medium reading header", () => {
  const url = 'https://medium.com/@quanstrata/story';
  const header = '<p><a href="/tag/systems-thinking?source=post_page---header_tags--123">Systems Thinking</a></p><h1>Dancing With the Current</h1><p><a href="/@quanstrata?source=post_page---byline--123"><img src="/avatar.jpg" width="32" height="32" alt="Quan Strata"></a></p><p><a href="/@quanstrata?source=post_page---byline--123">Quan Strata</a></p><p>3 min read</p><p>·</p><p>Just now</p><p>--</p>';
  const article = { title: 'Dancing With the Current', author: 'Quan Strata' };

  it("removes only the repeated site header, preserving prose and later references", () => {
    const el = body(header + '<p>Beyond stability.</p><h2>The Equilibrium Fallacy</h2><p>Quan Strata discusses systems.</p><p>--</p><p><a href="/tag/example">A meaningful reference</a></p>');
    normalizeMediaUrls(el, url, article);
    expect(el.firstElementChild?.textContent).toBe('Beyond stability.');
    expect(el.querySelector('h1')).toBeNull();
    expect(el.querySelector('img')).toBeNull();
    expect(el.textContent).not.toMatch(/3 min read|Just now|Systems Thinking/);
    expect(el.textContent).toContain('The Equilibrium Fallacy');
    expect(el.textContent).toContain('Quan Strata discusses systems.');
    expect(el.textContent).toContain('--');
    expect(el.querySelector('a')?.textContent).toBe('A meaningful reference');
  });

  it("does not remove a header from another site or one with a different title", () => {
    for (const [base, title] of [['https://example.com/story', article.title], [url, 'Different article']]) {
      const el = body(header + '<p>Real prose.</p>');
      normalizeMediaUrls(el, base, { ...article, title });
      expect(el.querySelector('h1')?.textContent).toBe(article.title);
      expect(el.textContent).toContain('3 min read');
      expect(el.textContent).toContain('Real prose.');
    }
  });

  it("does not discard the article's real hero image after the byline", () => {
    const el = body(header + '<p><img src="/hero.png" alt="Real diagram"></p><p>Actual prose.</p>');
    normalizeMediaUrls(el, url, article);
    expect(el.querySelector('figure img')?.getAttribute('src')).toBe('https://medium.com/hero.png');
    expect(el.querySelector('figcaption')?.textContent).toBe('Real diagram');
    expect(el.textContent).toContain('Actual prose.');
  });
});

describe("normalizeMediaUrls", () => {
  it("resolves relative image and link URLs against the article, not the app origin", () => {
    const el = body('<p><a href="/post/2">next</a></p><p><img src="hero.png" alt="Hero"></p>');
    normalizeMediaUrls(el, ARTICLE);

    expect(el.querySelector("a")?.getAttribute("href")).toBe("https://example.com/post/2");
    expect(el.querySelector("img")?.getAttribute("src")).toBe(
      "https://example.com/articles/2026/hero.png",
    );
  });

  it("keeps protocol-relative and absolute URLs, and marks external links safe", () => {
    const el = body('<p><a href="//cdn.example.org/a">a</a> <a href="https://other.test/b">b</a></p>');
    normalizeMediaUrls(el, ARTICLE);

    const [protoRel, absolute] = [...el.querySelectorAll("a")];
    expect(protoRel.getAttribute("href")).toBe("https://cdn.example.org/a");
    expect(absolute.getAttribute("href")).toBe("https://other.test/b");
    for (const link of [protoRel, absolute]) {
      expect(link.getAttribute("target")).toBe("_blank");
      expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    }
  });

  it("drops links and images it cannot resolve instead of pointing them at the app", () => {
    const el = body('<p><a href="/post/2">dead</a></p><p><img src="/local.png" alt="x"></p>');
    normalizeMediaUrls(el, null);

    expect(el.querySelector("a")).toBeNull();
    expect(el.textContent).toContain("dead"); // words survive, link does not
    expect(el.querySelector("img")).toBeNull();
  });

  it("keeps in-article anchors in-page and rewrites them to the namespaced id", () => {
    const el = body('<h2 id="setup">Setup</h2><p><a href="#setup">jump</a></p>');
    normalizeMediaUrls(el, ARTICLE);

    const link = el.querySelector("a")!;
    expect(link.getAttribute("href")).toBe("#art-setup");
    expect(link.hasAttribute("data-article-anchor")).toBe(true);
    expect(link.getAttribute("target")).toBeNull();
  });

  it("unwraps an anchor whose heading is gone", () => {
    const el = body('<p><a href="#missing">see above</a></p>');
    normalizeMediaUrls(el, ARTICLE);

    expect(el.querySelector("a")).toBeNull();
    expect(el.textContent).toBe("see above");
  });

  it("turns a standalone image into a captioned figure", () => {
    const el = body('<p><img src="hero.png" alt="The reading surface"></p><p>After.</p>');
    normalizeMediaUrls(el, ARTICLE);

    const figure = el.querySelector("figure.article-figure")!;
    expect(figure).toBeTruthy();
    expect(figure.querySelector("img")?.getAttribute("src")).toBe(
      "https://example.com/articles/2026/hero.png",
    );
    expect(figure.querySelector("figcaption")?.textContent).toBe("The reading surface");
    // The wrapper paragraph is gone, the following paragraph is untouched.
    expect(el.querySelectorAll("p")).toHaveLength(1);
  });

  it("keeps links before and after a block image when splitting its paragraph", () => {
    const el = body('<p><a href="/before">Before</a><img src="/hero.png" alt="Diagram"><a href="/after">After</a></p>');
    normalizeMediaUrls(el, ARTICLE);
    expect([...el.children].map(e => e.tagName)).toEqual(['P', 'FIGURE', 'P']);
    expect(el.children[0].textContent).toBe('Before');
    expect(el.children[2].textContent).toBe('After');
    expect(el.querySelectorAll('a')).toHaveLength(2);
    expect(el.querySelector('figure img')?.getAttribute('src')).toBe('https://example.com/hero.png');
  });

  it("leaves inline icons inline", () => {
    const el = body('<p>Text <img src="icon.png" alt="icon"> more text.</p>');
    normalizeMediaUrls(el, ARTICLE);

    expect(el.querySelector("figure")).toBeNull();
    expect(el.querySelector("p img")).toBeTruthy();
  });

  it("loads images lazily without leaking a referrer", () => {
    const el = body('<p><img src="hero.png" alt="Hero"></p>');
    normalizeMediaUrls(el, ARTICLE);

    const img = el.querySelector("img")!;
    expect(img.getAttribute("loading")).toBe("lazy");
    expect(img.getAttribute("decoding")).toBe("async");
    expect(img.getAttribute("referrerpolicy")).toBe("no-referrer");
  });

  it("is idempotent", () => {
    const el = body('<p><img src="hero.png" alt="Hero"></p><p><a href="/x">x</a></p>');
    normalizeMediaUrls(el, ARTICLE);
    const first = el.innerHTML;
    normalizeMediaUrls(el, ARTICLE);
    expect(el.innerHTML).toBe(first);
  });
});

describe("media review regressions", () => {
  it("wraps direct images in the body and bilingual original without throwing", () => {
    for (const html of [
      '<img src="/hero.png" alt="Hero">',
      '<div class="paragraph-original"><img src="/hero.png" alt="Hero"></div>',
    ]) {
      const el = body(html);
      expect(() => normalizeMediaUrls(el, ARTICLE)).not.toThrow();
      expect(el.querySelector("figure img")).toBeTruthy();
      expect(el.querySelector("figcaption")?.textContent).toBe("Hero");
    }
  });

  it("preserves anchors across repeated normalisation", () => {
    const el = body('<h2 id="setup">Setup</h2><a href="#setup">Jump</a>');
    normalizeMediaUrls(el, ARTICLE);
    const first = el.innerHTML;
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector('a[href="#art-setup"]')).toBeTruthy();
    expect(el.innerHTML).toBe(first);
  });

  it("preserves Chinese and encoded fragment identifiers", () => {
    const el = body('<h2 id="安装步骤">安装步骤</h2><a href="#%E5%AE%89%E8%A3%85%E6%AD%A5%E9%AA%A4">Jump</a>');
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector("h2")?.id).toBe("art-安装步骤");
    expect(el.querySelector("a")?.getAttribute("href")).toBe("#art-安装步骤");
  });

  it("creates targets for Markdown headings without an explicit id", () => {
    const el = body('<h2>Getting started</h2><a href="#getting-started">Jump</a>');
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector("h2")?.id).toBe("art-getting-started");
    expect(el.querySelector("a")?.getAttribute("href")).toBe("#art-getting-started");
  });

  it("compacts bare long URLs without changing their destination or descriptive labels", () => {
    const url = 'https://example.com/' + 'tracking'.repeat(30);
    const el = body(`<p><a href="${url}">${url}</a><a href="${url}">Read the article</a></p>`);
    normalizeMediaUrls(el, ARTICLE);
    const [bare, descriptive] = el.querySelectorAll("a");
    expect(bare.textContent).toBe("example.com · Open link");
    expect(bare.getAttribute("href")).toBe(url);
    expect(bare.getAttribute("title")).toBe(url);
    expect(descriptive.textContent).toBe("Read the article");
  });

  it("keeps a useful original-image link when image loading fails", () => {
    const el = body('<p><img src="/hero.png" alt="Diagram"></p>');
    normalizeMediaUrls(el, ARTICLE);
    el.querySelector("img")!.dispatchEvent(new Event("error"));
    const link = el.querySelector("a.image-fallback")!;
    expect(link.textContent).toContain("Image unavailable");
    expect(link.getAttribute("href")).toBe("https://example.com/hero.png");
    expect(el.querySelector("figcaption")?.textContent).toBe("Diagram");
  });

  it("retains linked images instead of replacing their navigation with preview", () => {
    const el = body('<p><a href="/original"><img src="/thumb.png" alt="Thumbnail"></a></p>');
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector("a img")).toBeTruthy();
    expect(el.querySelector("a")?.getAttribute("href")).toBe("https://example.com/original");
    expect(el.querySelector("img")?.getAttribute("role")).toBeNull();
  });

  it("does not show error placeholders for hidden translated-side duplicates", () => {
    const el = body('<div class="paragraph-translated"><img src="/hero.png" alt="Diagram"></div>');
    normalizeMediaUrls(el, ARTICLE);
    expect(el.querySelector("img")).toBeTruthy();
    el.querySelector("img")!.dispatchEvent(new Event("error"));
    expect(el.querySelector(".image-fallback")).toBeNull();
  });

  it("removes duplicate figures together with their orphan captions", () => {
    const el = body('<p><img src="/hero.png" alt="Hero"></p><p><img src="/hero.png" alt="Hero"></p>');
    normalizeMediaUrls(el, ARTICLE);
    dedupeImages(el);
    expect(el.querySelectorAll("img")).toHaveLength(1);
    expect(el.querySelectorAll("figcaption")).toHaveLength(1);
  });
});

describe("article media rendering across views", () => {
  afterEach(() => {
    document.body.replaceChildren();
    state.translationStateByItemId.clear();
    state.subscriptions = [];
  });

  function article(): FeedItem {
    document.body.innerHTML = '<div id="detail-content"></div>';
    return {
      id: 1, subscription_id: 1, title: "Media", link: ARTICLE,
      content_md: '![Diagram](/hero.png)\n\n[Next](/next)',
      translated_content: null,
    } as FeedItem;
  }

  it.each(["markdown", "translation", "streaming"])("repairs linked headings in the %s render path", (mode) => {
    const item = article();
    item.content_md = cachedHeading;
    if (mode !== "markdown") {
      item.translated_content = mode === "translation"
        ? `<div class="paragraph-original">${cachedHeading}</div>`
        : '<div class="paragraph-original">Completed paragraph.</div>';
      state.translationStateByItemId.set(1, {
        useTranslation: true, inProgressContent: null, abortController: null,
        hasError: false, errorMessage: null,
      });
    }
    renderItemDetail(item, mode === "streaming" ? { untranslatedTail: cachedHeading } : {});
    const root = document.querySelector(".detail-body")!;
    expect(root.querySelector("h2")?.textContent).toBe("Screenshots and Showcases");
    expect(root.querySelector("h2")?.id).toBe("art-screenshots-and-showcases");
    expect(root.textContent).not.toContain("](");
  });

  it("cleans cached fragments even when the bilingual original already contains HTML", () => {
    const item = article();
    item.link = 'https://medium.com/@author/story';
    item.author = 'Author Name';
    item.translated_content = '<div class="paragraph-original"><a href="/docs">Documentation</a>\n\n(/ @author?source=post_page---byline--123)\n\n[Author Name\n\n](/m/signin?actionUrl=x)\n\nActual prose.</div><div class="paragraph-translated">译文段落。</div>';
    state.translationStateByItemId.set(1, {
      useTranslation: true, inProgressContent: null, abortController: null,
      hasError: false, errorMessage: null,
    });
    renderItemDetail(item);
    const root = document.querySelector('.paragraph-original')!;
    expect(root.textContent).toContain('Documentation');
    expect(root.textContent).toContain('Actual prose.');
    expect(root.textContent).not.toMatch(/\[Author|source=post_page|\]\(\/m\/signin/);
    expect(root.querySelector('a')?.getAttribute('href')).toBe('https://medium.com/docs');
    expect(document.querySelector('.paragraph-translated')?.textContent).toContain('译文段落。');
  });

  it.each(["markdown", "translation", "streaming"])("preserves complete Medium avatar links in %s, without orphan brackets or duplicate captions", (mode) => {
    const item = article();
    item.link = 'https://medium.com/@quanstrata/story';
    item.author = 'Quan Strata';
    const source = '[\n\n<img alt="Quan Strata" src="https://example.com/avatar.jpg" width="32" height="32">\n\n](/@quanstrata?source=post_page---byline--123)\n\n[Quan Strata](/@quanstrata?source=post_page---byline--123)\n\n[\n\n](/m/signin?actionUrl=x)\n\nBeyond stability.';
    item.content_md = source;
    if (mode !== 'markdown') {
      item.translated_content = mode === 'translation'
        ? `<div class="paragraph-original">${source}</div><div class="paragraph-translated">译文。</div>`
        : '<div class="paragraph-original">Completed paragraph.</div>';
      state.translationStateByItemId.set(1, {
        useTranslation: true, inProgressContent: null, abortController: null,
        hasError: false, errorMessage: null,
      });
    }
    renderItemDetail(item, mode === 'streaming' ? { untranslatedTail: source } : {});
    const root = document.querySelector('.detail-body')!;
    expect(root.querySelector('a img')).toBeTruthy();
    expect(root.querySelector('figcaption')).toBeNull();
    expect([...root.querySelectorAll('p')].filter(p => p.textContent?.trim() === '[')).toHaveLength(0);
    expect((root.textContent?.match(/Quan Strata/g) ?? [])).toHaveLength(1);
    expect(root.textContent).toContain('Beyond stability.');
  });

  it("renders captioned images and article-relative links in Markdown", () => {
    renderItemDetail(article());
    const body = document.querySelector(".detail-body")!;
    expect(body.querySelector("figure img")?.getAttribute("src")).toBe("https://example.com/hero.png");
    expect(body.querySelector("a")?.getAttribute("href")).toBe("https://example.com/next");
  });

  it("renders raw HTML images and links on both sides of bilingual text", () => {
    const item = article();
    item.translated_content = '<div class="translation-paragraph"><div class="paragraph-original"><img src="/hero.png" alt="Diagram"><a href="/original">Original</a></div><div class="paragraph-translated"><a href="/translated">译文链接</a></div></div>';
    state.translationStateByItemId.set(1, {
      useTranslation: true, inProgressContent: null, abortController: null,
      hasError: false, errorMessage: null,
    });
    expect(() => renderItemDetail(item)).not.toThrow();
    expect(document.querySelector(".paragraph-original figure img")).toBeTruthy();
    expect(document.querySelector(".paragraph-translated a")?.getAttribute("href")).toBe("https://example.com/translated");
  });

  it("resolves cross-section anchors and deduplicates figures in the streaming tail", () => {
    const item = article();
    item.translated_content = '<div class="paragraph-original"><a href="#尾部">Next</a><img src="/hero.png" alt="Diagram"></div>';
    state.translationStateByItemId.set(1, {
      useTranslation: true, inProgressContent: null, abortController: null,
      hasError: false, errorMessage: null,
    });
    renderItemDetail(item, { untranslatedTail: '<h2 id="尾部">尾部</h2>\n\n![Diagram](/hero.png)\n\n[Tail link](/tail)' });
    expect(document.querySelector('.paragraph-original a')?.getAttribute("href")).toBe("#art-尾部");
    expect(document.querySelector('.untranslated-tail a')?.getAttribute("href")).toBe("https://example.com/tail");
    expect(document.querySelectorAll(".detail-body figure")).toHaveLength(1);
  });
});

describe("sanitiser namespacing that the media pass relies on", () => {
  it("prefixes heading ids and never lets an article claim an app id", () => {
    const el = body('<h2 id="detail-content">Clash</h2>');
    expect(el.querySelector("h2")?.getAttribute("id")).toBe("art-detail-content");
  });
});
