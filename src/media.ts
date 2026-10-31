/**
 * One normalisation pass for article media, shared by every render path.
 *
 * The markdown text view, the bilingual/translation view and the streaming
 * tail all receive untrusted RSS/AI HTML. Two things used to differ between
 * them and between paths:
 *
 * - Relative URLs were resolved nowhere in the host DOM, so `src="/hero.png"`
 *   or `href="/post/2"` pointed at the *app* origin (tauri.localhost) and
 *   rendered a broken image or a dead link.
 * - Images were bare `<img>` tags styled by a hover-to-enlarge rule, so a
 *   figure jumped size under the pointer and had no caption.
 *
 * This module fixes both, in DOM form so it can run after sanitisation and
 * before bionic marking. It is idempotent: running it twice changes nothing.
 */

import { articleAnchorId } from "./sanitize";

const EXTERNAL_REL = "noopener noreferrer";

/** Absolute http(s) URL, or null. */
function absoluteHttpUrl(raw: string, base: URL | null): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  try {
    const parsed = base ? new URL(trimmed, base) : new URL(trimmed);
    return parsed.protocol === "http:" || parsed.protocol === "https:"
      ? parsed.href
      : null;
  } catch {
    return null;
  }
}

/** Remove only a recognisable Medium page header duplicated by the reader's
 * own title/byline. Never scan into prose looking for matching author names. */
function removeRepeatedSiteHeader(root: Element, base: URL, article: { title: string; author?: string | null }): void {
  const isMedium = (host: string) => host === "medium.com" || host.endsWith(".medium.com");
  if (!isMedium(base.hostname)) return;
  const text = (value: string) => value.trim().replace(/\s+/g, " ");
  const children = Array.from(root.children);
  const heading = children.findIndex(el => el.tagName === "H1" && text(el.textContent ?? "") === text(article.title));
  if (heading < 0) return;

  const linkBlock = (el: Element, role: "header_tags" | "byline") => {
    if (el.tagName !== "P" || !el.querySelector("a[href]")) return false;
    const onlyLinks = Array.from(el.childNodes).every(node =>
      node.nodeType === Node.TEXT_NODE ? !(node.textContent ?? "").trim() :
      node.nodeType === Node.ELEMENT_NODE && (node as Element).tagName === "A");
    return onlyLinks && Array.from(el.querySelectorAll("a[href]")).every(link => {
      try {
        const url = new URL(link.getAttribute("href") ?? "", base);
        return isMedium(url.hostname) && (url.searchParams.get("source") ?? "").startsWith(`post_page---${role}--`);
      } catch { return false; }
    });
  };
  // Any genuine content before the heading makes the boundary ambiguous.
  if (!children.slice(0, heading).every(el => linkBlock(el, "header_tags"))) return;
  const repeated = children.slice(0, heading + 1);
  let byline = false;
  for (const el of children.slice(heading + 1)) {
    const value = text(el.textContent ?? "");
    if (linkBlock(el, "byline") || (article.author && value === text(article.author))) {
      byline = true;
      repeated.push(el);
    } else if (byline && /^(?:\d+\s+min read|Just now|\d+\s+(?:second|minute|hour|day|week|month|year)s?\s+ago|[·•]|--)$/i.test(value)) {
      repeated.push(el);
    } else if (!value && !el.querySelector("img,figure,pre,video")) {
      repeated.push(el);
    } else break; // Real hero image, subtitle or prose: the header ends here.
  }
  repeated.forEach(el => el.remove());
}

/** Replace an element with its children, keeping the text. */
function unwrap(el: Element): void {
  const parent = el.parentNode;
  if (!parent) return;
  while (el.firstChild) parent.insertBefore(el.firstChild, el);
  parent.removeChild(el);
}

/** Image blocks may also contain a separate source link after Markdown
 * wraps adjacent HTML into a paragraph. Actual inline prose stays inline. */
function isFigureParagraph(container: Element, img: Element): boolean {
  for (const node of Array.from(container.childNodes)) {
    if (node === img) continue;
    if (node.nodeType === Node.TEXT_NODE && !(node.nodeValue ?? "").trim()) continue;
    if (node.nodeType === Node.ELEMENT_NODE && ["BR", "A"].includes((node as Element).tagName)) continue;
    return false;
  }
  return true;
}

function captionFor(img: Element): string {
  const alt = (img.getAttribute("alt") ?? "").trim();
  const title = (img.getAttribute("title") ?? "").trim();
  return title || alt;
}

/**
 * Give a standalone article image the figure treatment: block layout plus a
 * caption from its alt/title. Inline icons (inside a sentence) are left alone.
 */
function upgradeToFigure(img: Element): void {
  const parent = img.parentElement;
  if (!parent) return;
  if (parent.tagName === "FIGURE") return;
  const standalone = parent.tagName === "P" ? isFigureParagraph(parent, img) : parent.classList.contains("detail-body") || parent.classList.contains("paragraph-original");
  if (!standalone) return;

  const doc = img.ownerDocument;
  const figure = doc.createElement("figure");
  figure.className = "article-figure";
  const caption = captionFor(img);
  // Install the figure before moving the image. Moving first and then
  // replacing a direct-child image would insert an ancestor into itself.
  if (parent.tagName === "P") {
    // Split around the image rather than dropping adjacent links or nesting a
    // block-level figure inside a paragraph. Preserve their document order.
    const before = doc.createElement("p"), after = doc.createElement("p");
    let pastImage = false;
    for (const node of Array.from(parent.childNodes)) {
      if (node === img) { pastImage = true; continue; }
      (pastImage ? after : before).appendChild(node);
    }
    const parts: Element[] = [];
    if (before.textContent?.trim() || before.querySelector("a,img")) parts.push(before);
    parts.push(figure);
    if (after.textContent?.trim() || after.querySelector("a,img")) parts.push(after);
    parent.replaceWith(...parts);
  } else img.replaceWith(figure);
  figure.appendChild(img);
  if (caption) {
    const figcaption = doc.createElement("figcaption");
    figcaption.textContent = caption;
    figure.appendChild(figcaption);
  }
}

function replaceUnavailableImage(img: HTMLImageElement, href: string | null, label: string): void {
  if (img.closest(".paragraph-translated")) return;
  const fallback = img.ownerDocument.createElement(href && !img.closest("a") ? "a" : "span");
  fallback.className = "image-fallback";
  fallback.textContent = label;
  if (img.alt) fallback.title = img.alt;
  if (fallback instanceof HTMLAnchorElement && href) {
    fallback.href = href;
    fallback.target = "_blank";
    fallback.rel = EXTERNAL_REL;
  }
  img.replaceWith(fallback);
}

function normalizeImage(img: HTMLImageElement, base: URL | null): boolean {
  const raw = img.getAttribute("src") ?? "";
  const resolved = absoluteHttpUrl(raw, base);
  if (!resolved) {
    replaceUnavailableImage(img, base?.href ?? null,
      base ? "Image source missing · Open article" : "Image source missing");
    return false;
  }
  img.setAttribute("src", resolved);
  img.setAttribute("loading", "lazy");
  img.setAttribute("decoding", "async");
  img.setAttribute("referrerpolicy", "no-referrer");
  if (!img.closest("a")) {
    img.tabIndex = 0;
    img.setAttribute("role", "button");
    img.setAttribute("aria-label", `${img.alt || "Image"}: open preview`);
  }
  // Network failures must not leave a tiny broken-image icon. Reassigning
  // onerror keeps repeated normalisation from installing duplicate handlers.
  img.onerror = () => replaceUnavailableImage(img, resolved, "Image unavailable · Open original");
  return true;
}

function normalizeAnchor(anchor: Element, base: URL | null, ids: Set<string>): void {
  const raw = (anchor.getAttribute("href") ?? "").trim();

  // In-article jump: keep it local and never open a new tab. Heading ids are
  // namespaced by the sanitiser (`#intro` -> `#art-intro`); a link whose target
  // no longer exists is unwrapped rather than left dead.
  if (raw.startsWith("#")) {
    let fragment = raw.slice(1);
    try { fragment = decodeURIComponent(fragment); } catch { /* malformed escape: keep literal */ }
    const id = articleAnchorId(fragment);
    // `root` is often detached, so collect ids from the subtree, not the doc.
    const exists = raw.length > 1 && ids.has(id);
    if (!exists) {
      unwrap(anchor);
      return;
    }
    anchor.setAttribute("href", `#${id}`);
    anchor.removeAttribute("target");
    anchor.removeAttribute("rel");
    anchor.setAttribute("data-article-anchor", "");
    return;
  }

  if (raw.toLowerCase().startsWith("mailto:")) {
    anchor.setAttribute("rel", EXTERNAL_REL);
    return;
  }

  const resolved = absoluteHttpUrl(raw, base);
  if (!resolved) {
    // A relative URL with no article link to resolve against would point into
    // the app itself (tauri.localhost/…) and 404. Keep the words, drop the
    // dead link.
    unwrap(anchor);
    return;
  }
  // Keep descriptive author labels, but don't turn a tracking URL into a
  // screenful of underlined text. The full address stays in href and title.
  const label = (anchor.textContent ?? "").trim();
  if (label.length > 96 && (label === raw || label === resolved)) {
    anchor.textContent = `${new URL(resolved).hostname} · Open link`;
    anchor.setAttribute("title", resolved);
  }
  anchor.setAttribute("href", resolved);
  anchor.setAttribute("target", "_blank");
  anchor.setAttribute("rel", EXTERNAL_REL);
}

/**
 * Resolve relative URLs, decorate external links, and turn standalone images
 * into figures. Safe to call on any sanitised article container.
 *
 * @param baseUrl the article's own link — relative paths resolve against it.
 *   Without it, relative links are unwrapped and relative images dropped
 *   rather than pointing at the app origin.
 */
export function normalizeMediaUrls(root: Element, baseUrl: string | null, article?: { title: string; author?: string | null }): void {
  let base: URL | null = null;
  if (baseUrl) {
    const safe = absoluteHttpUrl(baseUrl, null);
    if (safe) base = new URL(safe);
  }

  if (base && article) removeRepeatedSiteHeader(root, base, article);

  root.querySelectorAll("img").forEach((img) => {
    if (!normalizeImage(img, base)) img.remove();
  });

  // Marked does not generate heading ids. Give Markdown headings the same
  // jump targets as source HTML, and disambiguate repeated bilingual titles.
  const ids = new Set<string>();
  root.querySelectorAll("h1,h2,h3,h4,h5,h6").forEach((heading) => {
    const slug = (heading.textContent ?? "").trim().toLowerCase().replace(/\s+/g, "-");
    const original = articleAnchorId(heading.id || slug);
    if (!original) return;
    let id = original, suffix = 2;
    while (ids.has(id)) id = `${original}-${suffix++}`;
    heading.id = id;
    ids.add(id);
  });
  root.querySelectorAll("a[href]").forEach((anchor) => normalizeAnchor(anchor, base, ids));

  // Figures last: the image pass may have removed broken images.
  root.querySelectorAll("img").forEach(upgradeToFigure);
}
