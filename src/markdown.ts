/**
 * Shared `marked` configuration.
 *
 * Used by BOTH the host DOM text path and the iframe webview path — the
 * backend runs every article through the same html_to_markdown_pipeline, so
 * any renderer here is exercised for every article regardless of mode.
 * Configuring once here keeps the two paths from drifting apart.
 */

import { marked, Renderer } from "marked";

// Use tokens instead of raw text to properly handle nested elements
// (e.g., images inside links: [![alt](img.jpg)](url)).
const mdRenderer = new Renderer();
mdRenderer.link = function ({ href, title, tokens }) {
  const text = this.parser ? this.parser.parseInline(tokens) : "";
  return `<a target="_blank" rel="noopener noreferrer" href="${href}"${title ? ` title="${title}"` : ""}>${text}</a>`;
};

let configured = false;

/** Idempotently apply the shared renderer config; returns the marked instance. */
export function configureMarked(): typeof marked {
  if (!configured) {
    marked.use({ renderer: mdRenderer, gfm: true });
    configured = true;
  }
  return marked;
}

/**
 * Remove orphan link fragments — a `](url)` span that has no matching
 * opening `[` (broken links in the source HTML, or the LLM's echo of a
 * linked image dropping the opener). Rendered through marked, such a
 * fragment shows as literal `](https://...)` text; without the opener the
 * URL is useless, so the whole `](...)` span is dropped instead.
 *
 * Bracket depth tracking keeps every VALID construct intact: `[text](url)`,
 * `[![alt](img)](url)`, `![](img)`, multiple links, and parens nested in
 * URLs. Only `](` at depth 0 is an orphan.
 */
export function stripOrphanLinkFragments(md: string): string {
  let out = "";
  let bracketDepth = 0;
  let i = 0;
  const n = md.length;
  while (i < n) {
    const c = md[i];
    if (c === "[") {
      bracketDepth++;
      out += c;
      i++;
      continue;
    }
    if (c === "]" && md[i + 1] === "(") {
      if (bracketDepth === 0) {
        // Orphan `](` — skip through the depth-counted closing `)`.
        let depth = 1;
        let j = i + 2;
        while (j < n && depth > 0) {
          if (md[j] === "(") depth++;
          else if (md[j] === ")") depth--;
          j++;
        }
        i = j;
      } else {
        bracketDepth--;
        out += c;
        i++;
      }
      continue;
    }
    if (c === "]") {
      if (bracketDepth > 0) bracketDepth--;
      out += c;
      i++;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

/**
 * The single markdown→HTML choke point used by every render path (text
 * mode, bilingual originals, bilingual translations, streaming tail, and
 * the sandboxed iframe). Applies the orphan-fragment cleanup before marked
 * so no path can ever display a dangling `](url)`.
 */
export function renderMarkdown(md: string): string {
  return marked.parse(stripOrphanLinkFragments(md), { gfm: true }) as string;
}
