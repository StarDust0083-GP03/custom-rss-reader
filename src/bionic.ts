/**
 * Bionic-reading support for English article text.
 *
 * Bolds the leading "fixation" letters of each Latin word so the eye has
 * anchors while scanning. Only standalone ASCII-letter words are touched —
 * CJK, digits, punctuation, URLs, emails, identifiers, code and existing
 * bold stay exactly as rendered, which keeps this safe over mixed
 * Chinese/English bilingual views.
 *
 * Applied as a DOM pass AFTER sanitisation (both the host-DOM article path
 * and the sandboxed-iframe path), so it never re-introduces unsanitised
 * markup: wrappers are built with DOM APIs (`createElement`/`textContent`)
 * only, never by string concatenation.
 *
 * Idempotent: `applyBionic` first unwraps its own previous wrappers, so
 * repeated passes (streaming re-renders, re-selecting an article) are
 * stable — word tails never get folded into a second fixation pass.
 */

/**
 * Skip code-like and interactive contexts — fixation bolding only hurts
 * there. `b`/`strong` keep the author's own emphasis un-stacked (they are
 * unwrapped-and-rebuilt only for our own `.bionic-fix` markers, which
 * `unwrapFixations` removes before the walk).
 */
const SKIP_SELECTOR =
  "pre, code, kbd, samp, script, style, b, strong, svg, math, button, input, textarea, select, [contenteditable], .bionic-fix";

/**
 * One scan over a text node. Alternation order matters:
 *   1. scheme URLs          -> protected
 *   2. emails               -> protected
 *   3. separator-joined runs (example.com, well-known, v2.0) -> protected
 *   4. plain alnum runs     -> protected only when digits are present
 * Everything not matched (CJK, punctuation, whitespace) is copied as-is.
 */
const TOKEN =
  /(?:https?|ftp):\/\/[^\s<>"']+|[\w.+-]+@[\w-]+(?:\.[\w-]+)+|[A-Za-z0-9]+(?:[._+-][A-Za-z0-9]+)+|[A-Za-z0-9]+/gi;

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

/**
 * URLs, emails and anything joining letters with digits, dots, underscores,
 * plus signs or hyphens is one unit (a link, a domain, an identifier, a
 * version) — bionicising its pieces would corrupt what the reader copies
 * and add noise, so leave it whole.
 */
function isProtectedToken(token: string): boolean {
  return (
    /^(?:https?|ftp):\/\//i.test(token) ||
    token.includes("@") ||
    /[0-9]/.test(token) ||
    /[._+-]/.test(token)
  );
}

/** Remove wrappers from a previous pass and merge the split text nodes. */
function unwrapFixations(root: Element): void {
  const doc = root.ownerDocument;
  for (const el of Array.from(root.querySelectorAll(".bionic-fix"))) {
    el.replaceWith(doc.createTextNode(el.textContent ?? ""));
  }
  root.normalize();
}

/** Replace one text node's content with bionic-wrapped segments. */
function bionicifyTextNode(doc: Document, textNode: Text): void {
  const text = textNode.nodeValue ?? "";
  if (!/[A-Za-z]/.test(text)) return; // no Latin word — leave the node untouched

  const frag = doc.createDocumentFragment();
  let last = 0;

  TOKEN.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = TOKEN.exec(text)) !== null) {
    const token = match[0];
    const start = match.index;
    if (start > last) {
      frag.appendChild(doc.createTextNode(text.slice(last, start)));
    }
    last = start + token.length;

    if (isProtectedToken(token)) {
      frag.appendChild(doc.createTextNode(token));
      continue;
    }

    const fixation = fixationLength(token.length);
    const bold = doc.createElement("b");
    bold.className = "bionic-fix";
    bold.textContent = token.slice(0, fixation);
    frag.appendChild(bold);
    if (fixation < token.length) {
      frag.appendChild(doc.createTextNode(token.slice(fixation)));
    }
  }

  if (last === 0) return;
  if (last < text.length) {
    frag.appendChild(doc.createTextNode(text.slice(last)));
  }
  textNode.replaceWith(frag);
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

  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent) return NodeFilter.FILTER_REJECT;
      if (parent.closest(SKIP_SELECTOR)) return NodeFilter.FILTER_REJECT;
      return /[A-Za-z]/.test(node.nodeValue ?? "")
        ? NodeFilter.FILTER_ACCEPT
        : NodeFilter.FILTER_REJECT;
    },
  });

  // Collect first — mutating text nodes during a TreeWalker walk is unsafe.
  const targets: Text[] = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    targets.push(node as Text);
  }
  for (const textNode of targets) {
    bionicifyTextNode(doc, textNode);
  }
}
