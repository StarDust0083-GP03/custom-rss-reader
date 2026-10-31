/**
 * Custom word-prefix emphasis for English article text.
 *
 * Fixation lengths follow text-vide's five boundary tables (MIT), pinned to
 * commit 1353987f2bf12fae8553a90ab424a9c417a2aaa1. See the attribution and
 * full license in public/third-party-licenses.txt. Default fixationPoint is 1,
 * matching text-vide. This is not the official Bionic Reading algorithm and
 * does not add Saccade or Opacity controls. CSS weight is our presentation
 * choice, not an upstream setting or a validated reading-speed improvement.
 *
 * Only the length calculation is adapted: our existing DOM walker continues
 * to protect URLs, identifiers, code, images, Chinese and author emphasis.
 *
 * Emphasises the leading "fixation" letters of each word so the eye has
 * anchors while scanning. The pass works on WORD units rather than text
 * nodes: a word keeps one fixation even when the source markup splits it with
 * inline elements (`rea<em>ding</em>` → **readi**ng), and URLs, emails,
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
 * Adapted from text-vide/packages/text-vide/src/getFixationLength.ts (MIT).
 * Keep these tables in upstream order, including the repeated 0 in profile 4:
 * both the first matching index and the full table length affect the output.
 * Copyright (c) 2022 shj. Full MIT notice: public/third-party-licenses.txt.
 */
const FIXATION_BOUNDARIES = [
  [0, 4, 12, 17, 24, 29, 35, 42, 48],
  [1, 2, 7, 10, 13, 14, 19, 22, 25, 28, 31, 34, 37, 40, 43, 46, 49],
  [1, 2, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 27, 29, 31, 33, 35, 37, 39, 41, 43, 45, 47, 49],
  [0, 2, 4, 5, 6, 8, 9, 11, 14, 15, 17, 18, 20, 0, 21, 23, 24, 26, 27, 29, 30, 32, 33, 35, 36, 38, 39, 41, 42, 44, 45, 47, 48],
  [0, 2, 3, 5, 6, 7, 8, 10, 11, 12, 14, 15, 17, 19, 20, 21, 23, 24, 25, 26, 28, 29, 30, 32, 33, 34, 35, 37, 38, 39, 41, 42, 43, 44, 46, 47, 48],
];

/** Number of leading letters to mark, using text-vide's table lookup. */
export function fixationLength(len: number, fixationPoint = 1): number {
  const boundaries = FIXATION_BOUNDARIES[fixationPoint - 1] ?? FIXATION_BOUNDARIES[0];
  const index = boundaries.findIndex(boundary => len <= boundary);
  return Math.max(len - (index === -1 ? boundaries.length : index), 0);
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
 * contraction keeps a single leading anchor (`don't` -> **don**'t).
 */
function wordFixation(part: string, fixationPoint: number): number {
  let letters = 0;
  for (const ch of part) {
    if (!APOSTROPHE.test(ch)) letters++;
  }
  if (letters === 0) return 0;

  const fixation = fixationLength(letters, fixationPoint);
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
 * compounds (`well-known` -> **wel**l-**kno**wn) and apostrophes stay inside
 * their word.
 */
function lexemeFixations(lexeme: string, fixationPoint: number): Array<[number, number]> {
  if (/[0-9@._+]/.test(lexeme)) return [];

  const ranges: Array<[number, number]> = [];
  let start = 0;
  for (let i = 0; i <= lexeme.length; i++) {
    if (i < lexeme.length && lexeme[i] !== "-") continue;
    if (i > start) {
      const length = wordFixation(lexeme.slice(start, i), fixationPoint);
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
function collectFixations(text: string, fixationPoint: number): Array<[number, number]> {
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
      for (const [from, to] of lexemeFixations(text.slice(i, wordEnd), fixationPoint)) {
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
      // Whitespace and CJK nodes still separate Latin words. Dropping them
      // would turn `<span>Reading</span> <em>text</em>` into "Readingtext".
      nodes.push(node as Text);
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
function markSegment(doc: Document, segment: Segment, fixationPoint: number): void {
  const ranges = collectFixations(segment.text, fixationPoint);
  if (ranges.length === 0) return;

  // Most paragraphs are one text node. Build their markers off-DOM and swap
  // once: thousands of live Ranges make later DOM mutations increasingly
  // expensive. Inline-spanning words still use the format-preserving path.
  if (segment.nodes.length === 1) {
    const fragment = doc.createDocumentFragment();
    let offset = 0;
    for (const [start, end] of ranges) {
      if (start > offset) fragment.appendChild(doc.createTextNode(segment.text.slice(offset, start)));
      const marker = doc.createElement("b");
      marker.className = "bionic-fix";
      marker.textContent = segment.text.slice(start, end);
      fragment.appendChild(marker);
      offset = end;
    }
    if (offset < segment.text.length) fragment.appendChild(doc.createTextNode(segment.text.slice(offset)));
    segment.nodes[0].replaceWith(fragment);
    return;
  }

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
export function applyBionic(root: Element, fixationPoint = 1): void {
  const doc = root.ownerDocument;
  if (!doc) return;

  unwrapFixations(root);
  for (const segment of collectSegments(root)) {
    markSegment(doc, segment, fixationPoint);
  }
}
