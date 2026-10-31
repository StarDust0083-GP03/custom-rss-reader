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
import { normalizeMediaUrls } from "./media";
import { applyBionic } from "./bionic";
import { configureMarked, renderMarkdown } from "./markdown";

configureMarked();

const IFRAME_STYLE_ID = "rss-reader-iframe-style";

/** Return a normalized URL only for network-safe article pages. */
export function safeHttpUrl(raw: string): string | null {
  try {
    const parsed = new URL(raw.trim());
    if (
      !((parsed.protocol === "http:" || parsed.protocol === "https:") && parsed.hostname) ||
      isPrivateOrLocalHost(parsed.hostname)
    ) {
      return null;
    }
    return parsed.href;
  } catch {
    return null;
  }
}

function isPrivateOrLocalHost(rawHost: string): boolean {
  const host = rawHost.replace(/^\[|\]$/g, "").toLowerCase().replace(/\.$/, "");
  if (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host.endsWith(".local") ||
    host === "metadata.google.internal"
  ) {
    return true;
  }

  const octets = host.split(".").map(Number);
  if (octets.length === 4 && octets.every((part) => Number.isInteger(part) && part >= 0 && part <= 255)) {
    const [first, second] = octets;
    return (
      first === 0 ||
      first === 10 ||
      first === 127 ||
      (first === 169 && second === 254) ||
      (first === 172 && second >= 16 && second <= 31) ||
      (first === 192 && second === 168) ||
      first >= 224
    );
  }

  // IPv6 loopback, link-local, unique-local and multicast ranges.
  return host === "::1" || /^(fc|fd|fe8|fe9|fea|feb)/i.test(host) || host.startsWith("ff");
}

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
   * Requested original page. Never navigated inside the app: remote scripts
   * and players bypass the article sanitizer. Render cached Markdown instead.
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
   * Load sanitized Markdown into a sandboxed iframe inside the container.
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
      const safeUrl = safeHttpUrl(url);
      if (!safeUrl) {
        const message = "Refused to load a non-http(s) article URL";
        if (loadingEl?.isConnected) loadingEl.textContent = message;
        onError?.(message);
        return;
      }
    }

    // Never navigate a cross-origin webpage: its scripts can start media
    // downloads before a load handler could stop them, and the parent CSP
    // cannot restrict resources fetched inside a remote document.
    const notice = url
      ? '<p role="status">Audio/video and live webpages are blocked. Showing text and images only. Use “Open in browser” to view the original page.</p>'
      : "";

    // Build the document: minimal head, then the sanitized article body.
    // Pass CSS variables so the iframe matches the parent page styles (including dark mode).
    const safeBody = notice + renderMarkdownBody(markdown, baseUrl);
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
  const html = renderMarkdown(markdown);

  // 2. Sanitise via the shared helper (drops scripts, event handlers, etc.)
  const wrapper = document.createElement("div");
  setSafeHtml(wrapper, html);
  // 3. Same media policy as the host DOM path: resolve article-relative URLs
  //    (this document is a `data:` URL), caption standalone figures, and keep
  //    one occurrence per image URL.
  normalizeMediaUrls(wrapper, baseUrl || null);
  dedupeImages(wrapper);
  // 5. Bionic reading — last pass on the sanitised DOM, before the innerHTML
  //    is serialised into the sandboxed iframe document.
  applyBionic(wrapper);
  return wrapper.innerHTML;
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
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src https: http: data:; style-src 'unsafe-inline'; media-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'">
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>${header?.title ? escapeHtml(header.title) : "Article"}</title>
<style>
:root {
${varDecls}
}
*, *::before, *::after { box-sizing: border-box; }
/* The host-rendered markdown document below has an opaque origin, so it cannot
   fetch the self-hosted woff2 files: these stacks mirror href/host order and
   resolve to the real faces only when installed locally, otherwise to the
   platform UI sans. (The only current caller loads a live URL instead, and a
   live page renders with the site's own fonts.) CJK falls through to
   heiti-style system sans — issue #5: sans only. */
:root {
  --cjk-sans: 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei',
              'Noto Sans CJK SC', 'Noto Sans SC', sans-serif;
  --sans: 'Source Sans 3', 'Segoe UI', 'Helvetica Neue', Arial, var(--cjk-sans);
  --display: 'Instrument Sans', 'Source Sans 3', 'Segoe UI', 'Helvetica Neue',
             Arial, var(--cjk-sans);
  --mono: 'JetBrains Mono', ui-monospace, 'Cascadia Code', Menlo, Consolas,
          'DejaVu Sans Mono', var(--cjk-sans);
}
html, body {
  margin: 0;
  padding: 2.5rem 3rem;
  background: var(--bg-primary);
  color: var(--text-primary);
  font-family: var(--sans);
  font-size: 16px;
  line-height: 1.8;
  min-height: 100%;
}

/* Header section */
.source {
  font-family: var(--sans);
  font-size: 0.72rem;
  color: var(--accent-color);
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.14em;
  margin-bottom: 0.6rem;
}

/* Title styling - matches .detail-content h1 */
h1 {
  font-family: var(--display);
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
  font-family: var(--sans);
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
  font-family: var(--sans);
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
  text-decoration-color: color-mix(in srgb, var(--accent-color) 45%, transparent);
  text-underline-offset: 3px;
  overflow-wrap: anywhere;
}
a:hover {
  text-decoration-color: var(--accent-color);
}

/* Images - matches .detail-body img */
img, video, table { max-width: 100%; }
img, video { height: auto; display: inline-block; }
img {
  max-width: 100%;
  height: auto;
  object-fit: contain;
  border-radius: 3px;
  margin: 0.75rem 0;
}
figure.article-figure { margin: 1.6rem 0; text-align: center; }
figure.article-figure img { display: block; margin: 0 auto; }
figure.article-figure figcaption {
  margin-top: 0.6rem;
  font: 0.76rem/1.45 var(--sans);
  color: var(--text-secondary);
  overflow-wrap: anywhere;
}

/* Paragraphs */
p { margin: 0.85rem 0; }

/* Headings */
h2, h3, h4, h5, h6 {
  font-family: var(--sans);
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

/* Pre and code - matches .detail-body (monospace survives the sans switch) */
pre, code {
  font-family: var(--mono);
  background: var(--bg-secondary);
  border-radius: 2px;
  font-size: 0.88rem;
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
  font-family: var(--sans);
  font-size: 0.72rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

/* Overflow handling */
body { overflow-wrap: anywhere; word-break: break-word; }

/* Hide empty elements */
p:empty, div:empty { display: none; }

/* Bionic reading — word-leading fixation marks (matches host .bionic-fix) */
.bionic-fix {
  font-weight: 700;
  color: inherit;
}
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
