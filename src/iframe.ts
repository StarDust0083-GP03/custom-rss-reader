/**
 * Sandboxed iframe loader for third-party article Markdown.
 *
 * The previous implementation wrote raw article HTML into the iframe and
 * relied on a regex strip + DOM sanitiser + selector-based chrome removal
 * (`<nav>`, `<footer>`, `<h1>`). That doubled up the sanitisation logic
 * that already lives in the host DOM path.
 *
 * Both the host DOM path and this iframe path now share the same pipeline:
 * `html2md` (backend) → Markdown → `marked` → `setSafeHtml`. The iframe
 * exists for layout isolation only — it loads a `data:` URL with a
 * sandboxed `<iframe>` and renders the sanitised Markdown body inside.
 *
 * The generation counter (`this.generation`) still cancels stale loads if
 * the user navigates to a different article while a load is in flight.
 */

import { setSafeHtml, escapeHtml, dedupeImages } from "./sanitize";
import { configureMarked, renderMarkdown } from "./markdown";

configureMarked();

const IFRAME_STYLE_ID = "rss-reader-iframe-style";
function ensureIframeCss(): void {
  if (document.getElementById(IFRAME_STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = IFRAME_STYLE_ID;
  style.textContent = `
    .rss-reader-iframe {
      width: 100%;
      height: 100%;
      border: 0;
      background: var(--bg-color, #fff);
    }
  `;
  document.head.appendChild(style);
}

export interface IframeLoadRequest {
  container: HTMLElement;
  /** Article body in Markdown — produced by the backend `html2md` pipeline. */
  markdown: string;
  /**
   * Load the REAL website URL directly (webview mode, issue #2). When set,
   * `markdown` is ignored and the iframe navigates to this address.
   */
  url?: string;
  /** Article URL; used to resolve any remaining relative URLs. */
  baseUrl: string;
  /** Feed source name (e.g., "Hacker News") */
  sourceName?: string;
  /** Article title */
  title?: string;
  /** Publication date */
  date?: string;
  /** Author name */
  author?: string;
  /** Original article link */
  articleLink?: string;
  /** Called once the iframe's content is rendered. */
  onComplete?: () => void;
  /** Called on any error before the iframe is set. */
  onError?: (msg: string) => void;
  /** Loading placeholder element; removed on success/error. */
  loadingEl?: HTMLElement | null;
}

export class IframeManager {
  private generation = 0;

  /**
   * Load `request.html` into a sandboxed iframe inside `request.container`.
   * If a previous load is still in flight, it is cancelled.
   */
  load(request: IframeLoadRequest): number {
    ensureIframeCss();
    this.generation += 1;
    const gen = this.generation;
    this.renderIframe(request, gen);
    return gen;
  }

  /** Cancel any pending loads. Container is left as-is. */
  cancel(): void {
    this.generation += 1;
  }

  private renderIframe(request: IframeLoadRequest, gen: number): void {
    const { container, markdown, baseUrl, sourceName, title, date, author, articleLink, loadingEl, onComplete, onError, url } = request;
    if (gen !== this.generation) return; // superseded
    if (!container.isConnected) return;

    if (url) {
      this.renderDirectUrl(request, url, gen);
      return;
    }

    // Build the document: minimal head, then the sanitized article body.
    // Pass CSS variables so the iframe matches the parent page styles (including dark mode).
    const safeBody = renderMarkdownBody(markdown, baseUrl);
    const cssVars = getCssVariables();
    const docHtml = buildIframeDocument(safeBody, cssVars, { sourceName, title, date, author, articleLink });

    // Use a data: URL with a unique fragment so concurrent iframes don't
    // collide and successive iframes get a fresh origin.
    const dataUrl =
      "data:text/html;charset=utf-8," + encodeURIComponent(docHtml) + `#g=${gen}`;

    // Keep the loading element across the swap. If the iframe never fires
    // `load` (some WebView2 builds on `data:` + `sandbox=""`), the user
    // still sees something instead of a blank panel. We swap its text
    // on error in `finishError` below.
    const placeholder = loadingEl ?? null;
    container.innerHTML = "";
    if (placeholder) {
      placeholder.dataset.gen = String(gen);
      container.appendChild(placeholder);
    }

    const iframe = document.createElement("iframe");
    iframe.className = "rss-reader-iframe";
    iframe.setAttribute("sandbox", "");
    iframe.setAttribute("referrerpolicy", "no-referrer");
    iframe.setAttribute("data-gen", String(gen));
    iframe.src = dataUrl;

    const finishError = (msg: string) => {
      if (gen !== this.generation) return; // superseded
      if (placeholder?.isConnected) {
        placeholder.textContent = `Failed to load: ${msg}`;
      }
      if (onError) onError(msg);
    };

    iframe.addEventListener("error", () => finishError("iframe load failed"));

    iframe.addEventListener("load", () => {
      // `load` wins on compliant engines; the timeout below acts as a
      // fallback for engines that never fire it.
      if (gen !== this.generation) return;
      if (placeholder?.isConnected) placeholder.remove();
      if (onComplete) onComplete();
    });

    container.appendChild(iframe);

    // Hard floor: data: URLs in sandboxed iframes don't reliably fire
    // `load` on every WebView backend (notably some WebView2 builds
    // shipped with Tauri 2.x). 250ms is short enough not to feel slow
    // and long enough to win on compliant engines when `load` already
    // fired and invoked onComplete — we only do the work again if it's
    // still pending.
    window.setTimeout(() => {
      if (gen !== this.generation) return;
      if (!iframe.isConnected) return;
      if (placeholder?.isConnected) placeholder.remove();
      if (onComplete) onComplete();
    }, 250);
  }

  /**
   * Webview mode (issue #2): load the actual website in the iframe.
   *
   * The sandbox keeps the page isolated from the host app (opaque origin,
   * no top-level navigation) while allowing the scripts, forms and popups
   * real sites need to render. X-Frame-Options-refusing sites can't be
   * framed by any browser — for those the placeholder turns into a hint
   * pointing at the "Open in browser" link in the article header.
   */
  private renderDirectUrl(request: IframeLoadRequest, url: string, gen: number): void {
    const { container, loadingEl, onComplete, onError } = request;
    const placeholder = loadingEl ?? null;
    container.replaceChildren();
    if (placeholder) {
      placeholder.dataset.gen = String(gen);
      placeholder.textContent = "Loading webpage...";
      container.appendChild(placeholder);
    }

    const iframe = document.createElement("iframe");
    iframe.className = "rss-reader-iframe";
    iframe.setAttribute(
      "sandbox",
      "allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox allow-same-origin",
    );
    iframe.setAttribute("referrerpolicy", "no-referrer");
    iframe.setAttribute("data-gen", String(gen));
    iframe.src = url;

    const finishError = (msg: string) => {
      if (gen !== this.generation) return;
      if (placeholder?.isConnected) {
        placeholder.textContent = `Failed to load: ${msg}`;
      }
      if (onError) onError(msg);
    };
    iframe.addEventListener("error", () => finishError("iframe load failed"));
    iframe.addEventListener("load", () => {
      if (gen !== this.generation) return;
      if (placeholder?.isConnected) placeholder.remove();
      if (onComplete) onComplete();
    });

    container.appendChild(iframe);

    // XFO/CSP-refusing sites often fire `load` with an empty/error document
    // or never fire anything. Either way, retire the placeholder after a
    // grace period and hint at the header's "Open in browser" link instead
    // of spinning forever.
    window.setTimeout(() => {
      if (gen !== this.generation) return;
      if (placeholder?.isConnected) {
        placeholder.textContent =
          "Still loading — this site may refuse embedding. Use “Open in browser →” in the article header if it stays blank.";
      }
    }, 6000);
  }
}

/**
 * Render Markdown into a sanitised HTML body string for the iframe.
 *
 * Mirrors the host DOM pipeline: Markdown → `marked` → relative-URL resolve
 * → `setSafeHtml`. Chrome stripping (nav, footer, header, h1, etc.) is no
 * longer needed because the backend's `html_to_markdown_pipeline` already
 * removes those blocks before we ever see the content.
 */
function renderMarkdownBody(markdown: string, baseUrl: string): string {
  if (!markdown.trim()) return "";

  // 1. Markdown -> HTML (gfm on, same as the host DOM path)
  let html = renderMarkdown(markdown);

  // 2. Resolve relative URLs (markdown keeps them as-is; the iframe is
  //    loaded from a `data:` URL so relative hrefs would otherwise 404)
  if (baseUrl) {
    try {
      html = resolveRelativeUrls(html, baseUrl);
    } catch {
      // Ignore URL parse errors; the browser will still handle them.
    }
  }

  // 3. Sanitise via the shared helper (drops scripts, event handlers, etc.)
  const wrapper = document.createElement("div");
  setSafeHtml(wrapper, html);
  // 4. Same image policy as the host DOM path: one occurrence per URL.
  dedupeImages(wrapper);
  return wrapper.innerHTML;
}

/** Rewrite common relative URL patterns to absolute. Best-effort. */
function resolveRelativeUrls(html: string, baseUrl: string): string {
  const base = new URL(baseUrl);
  return html.replace(
    /\b(href|src)=("([^"]*)"|'([^']*)')/gi,
    (_match, attr: string, full: string, dq: string | undefined, sq: string | undefined) => {
      const raw = dq ?? sq ?? "";
      if (!raw || /^(https?:|mailto:|data:|#|\/\/)/i.test(raw)) {
        return `${attr}=${full}`;
      }
      try {
        const absolute = new URL(raw, base).href;
        return `${attr}="${escapeHtml(absolute)}"`;
      } catch {
        return `${attr}=${full}`;
      }
    },
  );
}

/**
 * Extracts CSS variable values from the computed style of the document element.
 * Returns an object with all the CSS variable values needed for the iframe.
 */
function getCssVariables(): Record<string, string> {
  const root = document.documentElement;
  const style = getComputedStyle(root);
  return {
    "--bg-primary": style.getPropertyValue("--bg-primary").trim() || "#ffffff",
    "--bg-secondary": style.getPropertyValue("--bg-secondary").trim() || "#f5f5f5",
    "--bg-tertiary": style.getPropertyValue("--bg-tertiary").trim() || "#e9e9e9",
    "--text-primary": style.getPropertyValue("--text-primary").trim() || "#1a1a1a",
    "--text-secondary": style.getPropertyValue("--text-secondary").trim() || "#666666",
    "--accent-color": style.getPropertyValue("--accent-color").trim() || "#0066cc",
    "--accent-hover": style.getPropertyValue("--accent-hover").trim() || "#0052a3",
    "--border-color": style.getPropertyValue("--border-color").trim() || "#d0d0d0",
  };
}

interface ArticleHeader {
  sourceName?: string;
  title?: string;
  date?: string;
  author?: string;
  articleLink?: string;
}

function buildIframeDocument(body: string, cssVars?: Record<string, string>, header?: ArticleHeader): string {
  // Use provided CSS vars or get from parent
  const vars = cssVars || getCssVariables();
  
  // Build CSS variable declarations for the iframe
  const varDecls = Object.entries(vars)
    .map(([name, value]) => `  ${name}: ${value};`)
    .join("\n");
  
  // Build the header section (title, source, meta)
  let headerHtml = "";
  if (header) {
    const { sourceName, title, date, author, articleLink } = header;
    
    // Source
    if (sourceName) {
      headerHtml += `<div class="source">${escapeHtml(sourceName)}</div>\n`;
    }
    
    // Title
    if (title) {
      headerHtml += `<h1>${escapeHtml(title)}</h1>\n`;
    }
    
    // Meta line
    const metaParts: string[] = [];
    if (date) {
      metaParts.push(`<span class="meta-date">${escapeHtml(date)}</span>`);
    }
    if (author) {
      metaParts.push(`<span class="meta-author">${escapeHtml(author)}</span>`);
    }
    if (articleLink) {
      metaParts.push(`<a href="${escapeHtml(articleLink)}" target="_blank" rel="noopener noreferrer">Open in browser →</a>`);
    }
    
    if (metaParts.length > 0) {
      headerHtml += `<div class="meta">${metaParts.join("<span class=\"meta-sep\">•</span>")}</div>\n`;
    }
  }
  
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>${header?.title ? escapeHtml(header.title) : "Article"}</title>
<style>
:root {
${varDecls}
}
*, *::before, *::after { box-sizing: border-box; }
/* data: iframes can't fetch the self-hosted Radon woff2 (opaque origin),
   so approximate it with the system mono stack; CJK falls through to
   heiti-style system sans (issue #5). */
:root {
  --cjk-sans: 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei',
              'Noto Sans CJK SC', 'Noto Sans SC', sans-serif;
  --mono-hand: 'Monaspace Radon', 'Cascadia Code', 'JetBrains Mono', Menlo,
               Consolas, 'DejaVu Sans Mono', var(--cjk-sans);
}
html, body {
  margin: 0;
  padding: 2.5rem 3rem;
  background: var(--bg-primary);
  color: var(--text-primary);
  font-family: var(--mono-hand);
  font-size: 16px;
  line-height: 1.8;
  min-height: 100%;
}

/* Header section */
.source {
  font-family: 'Public Sans', 'Segoe UI', var(--cjk-sans);
  font-size: 0.72rem;
  color: var(--accent-color);
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.14em;
  margin-bottom: 0.6rem;
}

/* Title styling - matches .detail-content h1 */
h1 {
  font-family: var(--mono-hand);
  font-size: 2rem;
  font-weight: 600;
  letter-spacing: -0.015em;
  line-height: 1.2;
  margin: 0 0 1rem 0;
  color: var(--text-primary);
}

/* Meta styling - matches .detail-meta */
.meta {
  display: flex;
  gap: 0.9rem;
  align-items: baseline;
  flex-wrap: wrap;
  margin-bottom: 1.5rem;
  padding-bottom: 1rem;
  border-bottom: 1px solid var(--border-color);
  color: var(--text-secondary);
  font-family: 'Public Sans', 'Segoe UI', var(--cjk-sans);
  font-size: 0.78rem;
  letter-spacing: 0.03em;
}

.meta-sep {
  color: var(--border-color);
}

.meta-date, .meta-author {
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
}

.meta a {
  color: var(--accent-color);
  text-decoration: none;
  border-bottom: 1px solid transparent;
}

.meta a:hover {
  border-bottom-color: var(--accent-color);
}

/* Article content — fills the panel width at any window size (issue #4) */
.article-body {
  line-height: 1.8;
  font-size: 1rem;
  width: 100%;
}

.article-body > p:first-of-type::first-letter {
  font-family: var(--mono-hand);
  font-weight: 600;
  font-size: 3.4em;
  float: left;
  line-height: 0.82;
  padding: 0.08em 0.12em 0 0;
  color: var(--accent-color);
}

/* Links - matches .detail-body a */
a {
  color: var(--accent-color);
  cursor: pointer;
  text-decoration: underline;
  text-decoration-color: rgba(191, 63, 44, 0.4);
  text-underline-offset: 2px;
}
a:hover {
  text-decoration-color: var(--accent-color);
}

/* Images - matches .detail-body img */
img, video, table { max-width: 100%; }
img, video { height: auto; display: inline-block; }
img {
  max-width: 400px;
  max-height: 300px;
  object-fit: contain;
  border-radius: 2px;
  margin: 0.75rem 0;
  cursor: zoom-in;
  transition: max-width 0.3s ease, max-height 0.3s ease;
}

/* Paragraphs */
p { margin: 0.85rem 0; }

/* Headings */
h2, h3, h4, h5, h6 {
  font-family: var(--mono-hand);
  color: var(--text-primary);
  font-weight: 600;
  letter-spacing: -0.01em;
  line-height: 1.25;
  margin: 1.6rem 0 0.6rem;
}

/* Lists */
ul, ol {
  padding-left: 1.5rem;
  margin: 0.85rem 0;
}

li::marker { color: var(--accent-color); }

/* Blockquote - matches .detail-body blockquote */
blockquote {
  font-style: italic;
  border-left: 3px solid var(--accent-color);
  padding-left: 1.25rem;
  margin: 1.5rem 0;
  color: var(--text-secondary);
}

/* Pre and code - matches .detail-body */
pre, code {
  background: var(--bg-secondary);
  border-radius: 2px;
  font-size: 0.88rem;
  font-family: 'Consolas', 'Monaco', monospace;
}
code { padding: 0.15em 0.35em; }
pre {
  padding: 1rem;
  overflow-x: auto;
  white-space: pre-wrap;
}

/* Tables */
table { border-collapse: collapse; width: 100%; font-size: 0.95rem; }
th, td { border: 1px solid var(--border-color); padding: 0.45rem 0.7rem; }
th {
  background: var(--bg-secondary);
  font-family: 'Public Sans', 'Segoe UI', var(--cjk-sans);
  font-size: 0.72rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

/* Overflow handling */
body { overflow-wrap: anywhere; word-break: break-word; }

/* Hide empty elements */
p:empty, div:empty { display: none; }
</style>
</head>
<body>
${headerHtml}
<div class="article-body">
${body}
</div>
</body>
</html>`;
}
