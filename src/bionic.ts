/**
 * Bionic-reading support for English article text.
 *
 * Emphasises the leading "fixation" letters of each word so the eye has
 * anchors while scanning. The pass works on WORD units rather than text
 * nodes: a word keeps one fixation even when the source markup splits it with
 * inline elements (`rea<em>ding</em>` → **read**ing), and URLs, emails,
 * identifiers, code and author bold stay exactly as rendered.
 *
 * Applied as a DOM pass AFTER sanitisation (both the host-DOM article path
 * and the sandboxed-iframe path), so it never re-introduces unsanitised
 * markup: markers are produced by the Range/DOM API only, never by string
 * concatenation.
 *
 * Scanning is a single left-to-right walk per text segment with no
 * backtracking, so a punctuation-heavy paragraph can't trigger repeated
 * rescanning.
 *
 * Idempotent: `applyBionic` first unwraps its own markers (moving children,
 * so inline elements survive) and normalises text nodes, so repeated passes —
 * streaming re-renders, re-selecting an article — are stable.
 */

/**
 * Skip code-like and interactive contexts — fixation bolding only hurts
 * there. `b`/`strong` keep the author's own emphasis un-stacked; they also
 * break a segment, so a word straddling them keeps its own emphasis.
 */
const SKIP_SELECTOR =
  "pre, code, kbd, samp, script, style, b, strong, svg, math, button, input, textarea, select, [contenteditable], .bionic-fix";

/** Elements that end a run of readable inline text. */
const BLOCK_TAGS = new Set([
  "ADDRESS", "ARTICLE", "ASIDE", "BLOCKQUOTE", "BR", "DD", "DETAILS", "DIV",
  "DL", "DT", "FIGCAPTION", "FIGURE", "FOOTER", "H1", "H2", "H3", "H4", "H5",
  "H6", "HEADER", "HR", "LI", "MAIN", "NAV", "OL", "P", "PRE", "SECTION",
  "SUMMARY", "TABLE", "TBODY", "TD", "TFOOT", "TH", "THEAD", "TR", "UL",
]);

const APOSTROPHE = /['\u2019]/;

/** Characters that make up one contiguous "lexeme" inside a text segment. */
const LEXEME_CHAR = /[A-Za-z\u00C0-\u024F0-9@._+'\u2019-]/;

/** Characters allowed inside a URL body (no prose punctuation, no brackets). */
const URL_CHAR = /[A-Za-z\u00C0-\u024F0-9\-._~:/?#@!$&*+=%;]/;

/** Characters trimmed from the end of a URL so "see https://x.com." keeps the dot. */
const URL_TRAILING = /[.,:;!?]/;

/**
 * Separators that can trail a lexeme without belonging to the word itself:
 * `easy.` and `pre-` are prose, not identifiers.
 */
const LEXEME_TRAILING = /[._@+-]/;

/**
 * Number of leading letters to bold for a word of `len` letters.
 *
 * Short words are split roughly in half ("the" -> **th**e); longer words
 * bold a slightly smaller share (~45%) so a paragraph doesn't turn into a
 * wall of bold.
 */
export function fixationLength(len: number): number {
  if (len <= 2) return len;
  if (len <= 5) return Math.ceil(len / 2);
  return Math.ceil(len * 0.45);
}

/** Scheme or bare-"www." URL start at `index`, if any. */
function urlLengthAt(text: string, index: number): number {
  const head = text.slice(index, index + 8).toLowerCase();
  const starts =
    head.startsWith("http://") ||
    head.startsWith("https://") ||
    head.startsWith("ftp://") ||
    head.startsWith("www.");
  if (!starts) return 0;

  let end = index;
  while (end < text.length && URL_CHAR.test(text[end])) end++;
  while (end > index && URL_TRAILING.test(text[end - 1])) end--;
  return end - index;
}

/**
 * Length of the fixation prefix of one word, counted in letters so a
 * contraction keeps a single leading anchor (`don't` -> **do**n't, not
 * **do**n'**t**).
 */
function wordFixation(part: string): number {
  let letters = 0;
  for (const ch of part) {
    if (!APOSTROPHE.test(ch)) letters++;
  }
  if (letters === 0) return 0;

  const fixation = fixationLength(letters);
  let seen = 0;
  let end = 0;
  while (end < part.length && seen < fixation) {
    if (!APOSTROPHE.test(part[end])) seen++;
    end++;
  }
  return end;
}

/**
 * Fixation ranges (relative to the lexeme) for one contiguous run of
 * lexeme characters.
 *
 * Digits, `@`, `.`, `_` and `+` mark identifiers, emails, domains and
 * versions — those stay whole. Everything else is prose: hyphens separate
 * compounds (`well-known` -> **we**ll-**kno**wn) and apostrophes stay inside
 * their word.
 */
function lexemeFixations(lexeme: string): Array<[number, number]> {
  if (/[0-9@._+]/.test(lexeme)) return [];

  const ranges: Array<[number, number]> = [];
  let start = 0;
  for (let i = 0; i <= lexeme.length; i++) {
    if (i < lexeme.length && lexeme[i] !== "-") continue;
    if (i > start) {
      const length = wordFixation(lexeme.slice(start, i));
      if (length > 0) ranges.push([start, start + length]);
    }
    start = i + 1;
  }
  return ranges;
}

/**
 * Fixation ranges over a whole readable segment (words may span nodes).
 * One linear walk: no per-position rescanning.
 */
function collectFixations(text: string): Array<[number, number]> {
  const ranges: Array<[number, number]> = [];
  let i = 0;
  while (i < text.length) {
    if (!LEXEME_CHAR.test(text[i])) {
      i++;
      continue;
    }
    const urlLength = urlLengthAt(text, i);
    if (urlLength > 0) {
      i += urlLength;
      continue;
    }
    let end = i;
    while (end < text.length && LEXEME_CHAR.test(text[end])) end++;
    // Trailing separators belong to the surrounding prose (a sentence-final
    // period, a trailing hyphen), not to the word being fixated.
    let wordEnd = end;
    while (wordEnd > i && LEXEME_TRAILING.test(text[wordEnd - 1])) wordEnd--;
    if (wordEnd > i) {
      for (const [from, to] of lexemeFixations(text.slice(i, wordEnd))) {
        ranges.push([i + from, i + to]);
      }
    }
    // Always advance: trailing separators are re-scanned as plain lexemes and
    // yield no fixation.
    i = wordEnd > i ? wordEnd : end;
  }
  return ranges;
}

/** A run of text nodes that reads as one line of prose, with its concatenation. */
interface Segment {
  nodes: Text[];
  /** Index in `text` where each node starts. */
  offsets: number[];
  text: string;
}

/**
 * Collect readable segments. Block elements and skipped subtrees end a
 * segment, so a word is never assumed to continue across a paragraph break,
 * a code block or author bold.
 */
function collectSegments(root: Element): Segment[] {
  const segments: Segment[] = [];
  let nodes: Text[] = [];

  const flush = () => {
    if (nodes.length === 0) return;
    const offsets: number[] = [];
    let text = "";
    for (const node of nodes) {
      offsets.push(text.length);
      text += node.nodeValue ?? "";
    }
    segments.push({ nodes, offsets, text });
    nodes = [];
  };

  const visit = (node: Node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      if (/[A-Za-z\u00C0-\u024F]/.test(node.nodeValue ?? "")) nodes.push(node as Text);
      return;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return;

    const el = node as Element;
    if (el.matches(SKIP_SELECTOR)) {
      flush();
      return;
    }
    const isBlock = BLOCK_TAGS.has(el.tagName);
    if (isBlock) flush();
    for (let child = el.firstChild; child; child = child.nextSibling) visit(child);
    if (isBlock) flush();
  };

  for (let child = root.firstChild; child; child = child.nextSibling) visit(child);
  flush();
  return segments;
}

/** Remove markers from a previous pass, keeping any inline elements inside them. */
function unwrapFixations(root: Element): void {
  for (const marker of Array.from(root.querySelectorAll(".bionic-fix"))) {
    const parent = marker.parentNode;
    if (!parent) continue;
    while (marker.firstChild) parent.insertBefore(marker.firstChild, marker);
    parent.removeChild(marker);
  }
  root.normalize();
}

/** Map a segment offset onto its text node and node-local offset. */
function locate(segment: Segment, index: number): { node: Text; offset: number } {
  let low = 0;
  let high = segment.nodes.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >> 1;
    if (segment.offsets[middle] <= index) low = middle;
    else high = middle - 1;
  }
  const node = segment.nodes[low];
  const offset = Math.min(index - segment.offsets[low], (node.nodeValue ?? "").length);
  return { node, offset };
}

/** Elements that count as content even with no text (never pruned). */
const MEDIA_SELECTOR = "img, br, hr, video, audio, source, svg, canvas";

/**
 * Drop empty element clones from an extracted fragment. A range that ends
 * exactly at an inline element boundary makes `extractContents` clone that
 * element with nothing in it; left in place, repeated passes would not be
 * byte-identical.
 */
function pruneEmptyClones(fragment: DocumentFragment): void {
  const elements = Array.from(fragment.querySelectorAll("*"));
  for (let i = elements.length - 1; i >= 0; i--) {
    const element = elements[i];
    if (element.querySelector(MEDIA_SELECTOR)) continue;
    if ((element.textContent ?? "").trim() === "") element.remove();
  }
}

/** Wrap each fixation range of one segment in `<b class="bionic-fix">`. */
function markSegment(doc: Document, segment: Segment): void {
  const ranges = collectFixations(segment.text);

  // Right-to-left: marking a later range only splits or truncates text nodes
  // at or after its own start, so the offsets of earlier ranges stay valid.
  for (let i = ranges.length - 1; i >= 0; i--) {
    const [start, end] = ranges[i];
    const from = locate(segment, start);
    const to = locate(segment, end);

    const range = doc.createRange();
    range.setStart(from.node, from.offset);
    range.setEnd(to.node, to.offset);

    // extractContents clones partially covered inline elements, so a word
    // split by <em>/<a>/<span> keeps its markup inside the marker.
    const contents = range.extractContents();
    pruneEmptyClones(contents);
    const marker = doc.createElement("b");
    marker.className = "bionic-fix";
    marker.appendChild(contents);
    range.insertNode(marker);
  }
}

/**
 * Bold the fixation letters of every English word inside `root`.
 *
 * `textContent` is preserved exactly; only the markup granularity changes.
 */
export function applyBionic(root: Element): void {
  const doc = root.ownerDocument;
  if (!doc) return;

  unwrapFixations(root);
  for (const segment of collectSegments(root)) {
    markSegment(doc, segment);
  }
}
