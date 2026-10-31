/**
 * Shared `marked` configuration.
 *
 * Used by BOTH the host DOM text path and the iframe webview path — the
 * backend runs every article through the same html_to_markdown_pipeline, so
 * any renderer here is exercised for every article regardless of mode.
 * Configuring once here keeps the two paths from drifting apart.
 */

import { marked, Renderer } from "marked";
import { escapeHtml } from "./sanitize";

// Use tokens instead of raw text to properly handle nested elements
// (e.g., images inside links: [![alt](img.jpg)](url)).
const mdRenderer = new Renderer();
mdRenderer.link = function ({ href, title, tokens }) {
  const text = this.parser ? this.parser.parseInline(tokens) : "";
  // Empty controls from website action bars have no readable link label.
  if (!text.trim()) return "";
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
    if (c === "\n") {
      let next = i + 1;
      while (md[next] === " " || md[next] === "\t" || md[next] === "\r") next++;
      // A blank line ends an inline link label. Otherwise an unmatched '['
      // in an old byline can mask orphan destinations in later paragraphs.
      if (md[next] === "\n") bracketDepth = 0;
    }
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
export function renderMarkdown(md: string, articleUrl?: string | null, author?: string | null): string {
  return marked.parse(cleanMarkdownSource(md, articleUrl, author), { gfm: true }) as string;
}

/**
 * Pre-parse cleanup applied before every markdown render.
 */
function cleanMarkdownSource(md: string, articleUrl?: string | null, author?: string | null): string {
  // Protect code from ALL cleanup passes, not just block-link repair.
  const clean = (text: string) => encodeSpaceyImageUrls(
    // Repair complete links first. Cleaning their closing destination line
    // before this step leaves a bare '[' and turns linked avatars into figures.
    stripOrphanLinkFragments(cleanCachedArticleFragments(repairBlockLinks(text), articleUrl, author)),
  );
  let pending = "", out = "";
  for (const token of marked.lexer(md)) {
    if (token.type === "code" || (token.type === "html" && /^\s*<(?:pre|code)\b/i.test(token.raw))) {
      out += clean(pending) + token.raw;
      pending = "";
    } else pending += token.raw;
  }
  return out + clean(pending);
}

/** Compatibility for old Medium translations whose link stripping left naked
 * byline/sign-in destinations. Only whole metadata lines on that site are
 * removed; ordinary citations, prose, arrays and protected code stay intact. */
function cleanCachedArticleFragments(md: string, articleUrl?: string | null, author?: string | null): string {
  if (!articleUrl) return md;
  let base: URL;
  try { base = new URL(articleUrl); } catch { return md; }
  const isMedium = (host: string) => host === "medium.com" || host.endsWith(".medium.com");
  if (!isMedium(base.hostname)) return md;
  return md.split("\n").map(line => {
    if (author?.trim() && line.trim() === `[${author.trim()}`) return line.replace("[", "");
    // Only naked destinations here. A '](...)' line may still belong to a
    // complete link; orphan closing syntax is handled by the bracket parser.
    const fragment = line.match(/^[ \t]*\(([^)\r\n]+)\)[ \t]*$/);
    if (!fragment) return line;
    try {
      const target = new URL(fragment[1].trim().replace(/^\/\s+@/, "/@"), base);
      const byline = target.pathname.startsWith("/@") &&
        (target.searchParams.get("source") ?? "").startsWith("post_page---byline--");
      if (isMedium(target.hostname) && (target.pathname === "/m/signin" || byline)) return "";
    } catch { /* Not a URL: preserve the original words. */ }
    return line;
  }).join("\n");
}

/**
 * html2md can wrap a heading/image/empty control in a link whose brackets
 * occupy separate blocks. Identify the label with Marked instead of guessing
 * from the destination or adding one text-specific fix per broken article.
 */
function repairBlockLinks(md: string): string {
  return md.replace(
    /^\[[ \t]*\r?\n([\s\S]*?)^\][ \t]*\(([^\r\n]*)\)[ \t]*(?=\r?\n|$)/gm,
    (original: string, label: string, destination: string) => {
      const content = label.trim();
      if (!content) return "";
      // Let Marked validate nested parentheses, optional titles and escapes.
      const links = marked.Lexer.lexInline(`[heading](${destination})`);
      if (links.length !== 1 || links[0].type !== "link") return original;
      const link = links[0];
      const blocks = marked.lexer(content).filter(token => token.type !== "space");
      if (blocks.length === 1 && blocks[0].type === "heading") {
        const heading = blocks[0];
        // Some sites put a visible permalink '#' before the actual title.
        const title = heading.text.replace(/^(?:\\#|#)\s+/, "");
        const html = marked.parseInline(title);
        if (link.href.startsWith("#")) {
          let id = link.href.slice(1);
          try { id = decodeURIComponent(id); } catch { /* keep malformed escapes literal */ }
          // The sanitiser namespaces this id; media normalisation keeps it.
          return `<h${heading.depth} id="${escapeHtml(id)}">${html}</h${heading.depth}>\n`;
        }
        const titleAttr = link.title ? ` title="${escapeHtml(link.title)}"` : "";
        return `<h${heading.depth}><a href="${escapeHtml(link.href)}"${titleAttr}>${html}</a></h${heading.depth}>\n`;
      }
      if (/^<img\b[^>]*>$/i.test(content)) return `[${content}](${destination})`;
      if (blocks.length === 1 && blocks[0].type === "paragraph") {
        return `[${content.replace(/\r?\n/g, " ")}](${destination})`;
      }
      return original; // Unknown block structures stay intact, not deleted.
    },
  ).replace(/^Press enter or click to view image in full size[ \t]*\r?\n(?:[ \t]*\r?\n)*(?=<img\b)/gm, "");
}

/**
 * Percent-encode spaces inside image URLs. A space in `![](a b.png)` is
 * invalid markdown — marked refuses the link and renders the literal
 * `![](<a>url</a> b.png)` garbage, displaying the image URL as text.
 * Spaces can only be fixed by encoding them (`%20`).
 */
function encodeSpaceyImageUrls(md: string): string {
  let out = "";
  let i = 0;
  const n = md.length;
  while (i < n) {
    if (md[i] === "!" && md[i + 1] === "[") {
      const after = md.indexOf("](", i + 2);
      if (after !== -1) {
        const urlStart = after + 2;
        let depth = 1;
        let j = urlStart;
        while (j < n && depth > 0) {
          if (md[j] === "(") depth++;
          else if (md[j] === ")") depth--;
          j++;
        }
        if (depth === 0) {
          out += md.slice(i, urlStart);
          out += md.slice(urlStart, j - 1).replace(/ /g, "%20") + ")";
          i = j;
          continue;
        }
      }
    }
    out += md[i];
    i++;
  }
  return out;
}
