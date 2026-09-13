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

// ---------------------------------------------------------------------------
// Prose-only rewriting
// ---------------------------------------------------------------------------

/** A fence opening line: up to three spaces, then ``` or ~~~. */
const FENCE_RE = /^ {0,3}(`{3,}|~{3,})/;

/**
 * Apply `transform` to prose only.
 *
 * The cleanup below repairs malformed article markdown, but fenced blocks and
 * inline code spans are literal content: a sample showing `](literal)` or
 * `![a b](c.png "t")` must render exactly as written. Code is detected here
 * once, before any rewriting, and copied verbatim.
 */
function transformProse(md: string, transform: (prose: string) => string): string {
  let out = "";
  let prose = "";
  let fence: string | null = null;

  for (const line of splitLines(md)) {
    const match = FENCE_RE.exec(line);
    if (fence) {
      out += line;
      if (match && match[1][0] === fence[0] && match[1].length >= fence.length) {
        fence = null;
      }
      continue;
    }
    if (match) {
      out += transformInlineProse(prose, transform);
      prose = "";
      out += line;
      fence = match[1];
      continue;
    }
    prose += line;
  }

  return out + transformInlineProse(prose, transform);
}

/** Split into lines that keep their trailing newline (so output is byte-exact). */
function splitLines(md: string): string[] {
  const lines: string[] = [];
  let start = 0;
  while (start < md.length) {
    const end = md.indexOf("\n", start);
    if (end === -1) {
      lines.push(md.slice(start));
      break;
    }
    lines.push(md.slice(start, end + 1));
    start = end + 1;
  }
  return lines;
}

/**
 * Apply `transform` to the parts of `text` outside inline code spans.
 * An unmatched backtick run is left in the prose — only a run with a closing
 * run of the same length starts a code span.
 */
function transformInlineProse(text: string, transform: (prose: string) => string): string {
  let out = "";
  let start = 0;
  let i = 0;

  while (i < text.length) {
    if (text[i] !== "`") {
      i++;
      continue;
    }
    let end = i;
    while (text[end] === "`") end++;
    const ticks = text.slice(i, end);
    const close = text.indexOf(ticks, end);
    if (close === -1) {
      i = end;
      continue;
    }
    out += transform(text.slice(start, i));
    out += text.slice(i, close + ticks.length);
    i = close + ticks.length;
    start = i;
  }

  return out + transform(text.slice(start));
}

// ---------------------------------------------------------------------------
// Cleanup passes
// ---------------------------------------------------------------------------

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
  return transformProse(md, stripOrphanLinkFragmentsInProse);
}

function stripOrphanLinkFragmentsInProse(md: string): string {
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
  return marked.parse(cleanMarkdownSource(md), { gfm: true }) as string;
}

/**
 * Pre-parse cleanup applied before every markdown render.
 */
function cleanMarkdownSource(md: string): string {
  return encodeSpaceyImageUrls(stripOrphanLinkFragments(md));
}

/**
 * Percent-encode spaces inside image destinations. A space in `![](a b.png)`
 * is invalid markdown — marked refuses the link and renders the literal
 * `![](<a>url</a> b.png)` garbage, displaying the image URL as text.
 * Spaces can only be fixed by encoding them (`%20`).
 */
export function encodeSpaceyImageUrls(md: string): string {
  return transformProse(md, encodeSpaceyImageUrlsInProse);
}

function encodeSpaceyImageUrlsInProse(md: string): string {
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
          out += encodeImageDestination(md.slice(urlStart, j - 1)) + ")";
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

/** A markdown title tail: whitespace, then a quoted or parenthesized title. */
const TITLE_TAIL_RE = /^[ \t\r\n]+(?:"[^"]*"|'[^']*'|\([^()]*\))[ \t\r\n]*$/;

/**
 * Encode spaces in an image destination, leaving a valid title alone.
 *
 * markdown splits `url "title"` itself; a `replace` over the whole span
 * destroyed both the separator and the title, so
 * `![alt](a.png "A title")` resolved to `a.png%20%22A%20title%22`. The title
 * is the longest tail that parses as a title, so the split is searched from
 * the end.
 */
function encodeImageDestination(inner: string): string {
  for (let i = inner.length - 1; i >= 0; i--) {
    if (!/\s/.test(inner[i])) continue;
    const tail = inner.slice(i);
    if (TITLE_TAIL_RE.test(tail)) {
      return inner.slice(0, i).replace(/ /g, "%20") + tail;
    }
  }
  return inner.replace(/ /g, "%20");
}
