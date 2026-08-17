/**
 * HTML sanitization helpers.
 *
 * Trusted-string vs untrusted-string split:
 * - `escapeHtml(s)`: escapes every character so the string is safe to embed
 *   inside an HTML attribute or text node.
 * - `setText(el, s)`: writes `s` as plain text (no parsing).
 * - `setSafeHtml(el, html)`: parses `html` in a detached DOM, walks it and
 *   removes any element/attribute we don't trust, then copies the result
 *   into the live element.
 *
 * We deliberately implement the sanitizer in-house (no external dep) and
 * keep its allowlist tiny. The input we sanitize is RSS article HTML and
 * AI-generated HTML — both are untrusted.
 */

const BLOCKED_TAGS = new Set([
  "script", "style", "noscript", "iframe", "frame", "frameset",
  "object", "embed", "applet", "meta", "link", "base",
  "form", "input", "textarea", "select", "button",
  "svg", "math",
  "video", "audio", "source", "track",
]);

const SAFE_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "title", "target", "rel"]),
  img: new Set(["src", "alt", "title", "width", "height", "loading"]),
  blockquote: new Set(["cite"]),
  th: new Set(["colspan", "rowspan", "scope"]),
  td: new Set(["colspan", "rowspan"]),
};

/**
 * Class tokens allowed to survive sanitisation. Only the translation
 * pipeline's known wrapper classes — anything else (attacker-chosen
 * utility/hook classes) is dropped. Without this the bilingual view lost
 * every `paragraph-original` / `paragraph-translated` class, so neither the
 * styling nor the markdown re-rendering below could find its targets.
 */
const SAFE_CLASSES = new Set([
  "bilingual-content",
  "translation-paragraph",
  "paragraph-block",
  "paragraph-original",
  "paragraph-translated",
  "translation-segment",
  "original-content",
  "translated-content",
  "translation-badge",
]);

const ALLOWED_URL_SCHEMES = ["http:", "https:", "mailto:"];

/** Replace every HTML-special character with its entity. */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#x27;");
}

/** Safely set a text node (no HTML parsing). */
export function setText(el: Element, text: string): void {
  el.replaceChildren(document.createTextNode(text));
}

/**
 * Convert an HTML-ish string into plain text for list-view summaries.
 *
 * RSS `<description>` and `<content:encoded>` payloads frequently contain
 * `<p>`, `<img>`, entity references like `&amp;`, or even stripped-down
 * snippets that don't form a valid document. Using `textContent` would
 * show the raw tags and entities to the user; parsing through a DOM
 * parses them into real text.
 *
 * The conversion happens in a detached `<template>` so no scripts can
 * fire and no resources are fetched.
 */
export function htmlToPlainText(html: string): string {
  const template = document.createElement("template");
  template.innerHTML = html;
  // `<br>` should produce a space so "line one<br>line two" doesn't merge
  // into "line oneline two". Block-level elements naturally add newlines
  // via the surrounding whitespace; for inline `<br>` we insert a space.
  template.content.querySelectorAll("br").forEach((br) => {
    br.replaceWith(document.createTextNode(" "));
  });
  const text = template.content.textContent ?? "";
  // Collapse runs of whitespace — entity-decoded `&nbsp;` becomes U+00A0
  // here, which `textContent` preserves but reads as glue. Collapse both.
  return text.replace(/[\s ]+/g, " ").trim();
}

/**
 * Parse `html` in a detached `<template>`, sanitize, then graft the result
 * into `target`. The template approach prevents the parser from running
 * scripts or fetching resources.
 */
export function setSafeHtml(target: Element, html: string): void {
  const template = document.createElement("template");
  // innerHTML assignment in a template is safe — scripts and image fetches
  // do not execute. We still sanitize the resulting tree.
  template.innerHTML = html;
  const cleaned = sanitizeNode(template.content);
  // replaceChildren drops the placeholder
  target.replaceChildren(cleaned);
}

/**
 * Sanitize a DOM node (and its descendants) in place. Returns the same
 * node for chaining. Implemented recursively so we don't allocate extra
 * trees for safe content.
 */
function sanitizeNode(root: DocumentFragment | Element): DocumentFragment | Element {
  const walker = root.ownerDocument!.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
  const toRemove: Element[] = [];
  let node: Node | null = walker.currentNode as Node;
  // Walker doesn't include the root itself, so process children first then
  // walk back up via parent.
  const elements: Element[] = [root as Element];
  // First, collect all elements (DFS) so we can sanitize parents last.
  while (node) {
    elements.push(node as Element);
    node = walker.nextNode();
  }

  // Sanitize attributes on all elements and flag blocked ones for removal.
  for (const el of elements) {
    if (el === root) continue;
    const tag = el.tagName.toLowerCase();
    if (BLOCKED_TAGS.has(tag)) {
      toRemove.push(el);
      continue;
    }
    sanitizeAttributes(el);
  }

  // Remove blocked elements (in document order — child first, so the
  // parent loop doesn't trip on already-removed children).
  toRemove.sort((a, b) => {
    // Compare depths: deeper first
    let depthA = 0, depthB = 0;
    for (let n: Node | null = a; n; n = n.parentNode) depthA++;
    for (let n: Node | null = b; n; n = n.parentNode) depthB++;
    return depthB - depthA;
  });
  for (const el of toRemove) {
    el.parentNode?.removeChild(el);
  }

  return root;
}

function sanitizeAttributes(el: Element): void {
  const tag = el.tagName.toLowerCase();
  const allowed = SAFE_ATTRS[tag];
  // If no allowlist entry, drop ALL attributes (safer than passing through).
  const allowedSet = allowed ?? new Set<string>();

  for (const attr of Array.from(el.attributes)) {
    const name = attr.name.toLowerCase();
    if (name.startsWith("on")) {
      el.removeAttributeNode(attr);
      continue;
    }
    if (name === "class") {
      // Keep only known translation-pipeline classes.
      const safe = attr.value
        .split(/\s+/)
        .filter((c) => SAFE_CLASSES.has(c));
      if (safe.length > 0) {
        el.setAttribute("class", safe.join(" "));
      } else {
        el.removeAttributeNode(attr);
      }
      continue;
    }
    if (!allowedSet.has(name)) {
      el.removeAttributeNode(attr);
      continue;
    }
    if ((name === "href" || name === "src") && !isSafeUrl(attr.value)) {
      el.removeAttributeNode(attr);
      continue;
    }
    if (name === "href" && !attr.value.startsWith("#") && !attr.value.startsWith("/")) {
      // Force external link to open safely
      el.setAttribute("target", "_blank");
      el.setAttribute("rel", "noopener noreferrer");
    }
  }
}

function isSafeUrl(url: string): boolean {
  const trimmed = url.trim();
  if (!trimmed || trimmed.startsWith("#") || trimmed.startsWith("/")) {
    return true;
  }
  try {
    const u = new URL(trimmed, "http://invalid.local/");
    return ALLOWED_URL_SCHEMES.includes(u.protocol);
  } catch {
    return false;
  }
}
