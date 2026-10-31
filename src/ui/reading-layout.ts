/**
 * Reading layout states.
 *
 * - Quiet Desk (default): the three-pane layout (sidebar · items · detail).
 * - Reading Room (optional focus state): the sidebar and item list are hidden
 *   so the article fills the window. Column widths are left untouched, so
 *   leaving focus restores the exact previous layout. Esc also exits.
 * - Parallel Text: only offered while the selected article has an active
 *   bilingual translation. Each original/translated paragraph pair sits side
 *   by side and stacks on narrow panes (see `.bilingual-content.parallel`).
 */

const FOCUS_KEY = "rss.reading.focus";
const PARALLEL_KEY = "rss.reading.parallel";

let focusOn = false;
let parallelOn = true;

function readFlag(key: string, fallback: boolean): boolean {
  const v = localStorage.getItem(key);
  return v === null ? fallback : v === "1";
}

function writeFlag(key: string, value: boolean): void {
  localStorage.setItem(key, value ? "1" : "0");
}

export function isReadingRoom(): boolean {
  return focusOn;
}

/** Parallel layout is preferred by default, but only ever shown when available. */
export function isParallelPreferred(): boolean {
  return parallelOn;
}

function appContainer(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".app-container");
}

const COMPACT_QUERY = "(max-width: 1024px)";
const isCompact = () => window.matchMedia?.(COMPACT_QUERY).matches ?? false;

function syncFocusControl(): void {
  const btn = document.getElementById("focus-btn");
  if (!btn) return;
  const compact = isCompact();
  btn.classList.toggle("active", !compact && focusOn);
  if (compact) btn.removeAttribute("aria-pressed");
  else btn.setAttribute("aria-pressed", String(focusOn));
  btn.textContent = compact ? "← Article list" : focusOn ? "Library" : "Focus";
  if (compact) btn.setAttribute("aria-label", "Back to article list");
  else btn.removeAttribute("aria-label");
  btn.title = compact ? "Return to the article list (Esc)" : focusOn
    ? "Show the library and item list (Esc)"
    : "Reading Room: hide the library and read full width (Esc to exit)";
}

function applyFocus(on: boolean): void {
  focusOn = on;
  writeFlag(FOCUS_KEY, on);
  appContainer()?.classList.toggle("reading-focus", on);
  syncFocusControl();
}

/**
 * Show or hide the Parallel control. Called from the detail renderer whenever
 * an article is rendered, so the control tracks translation state exactly.
 */
export function setParallelAvailable(available: boolean): void {
  const btn = document.getElementById("parallel-btn");
  if (!btn) return;
  btn.hidden = !available;
  btn.classList.toggle("active", available && parallelOn);
  btn.setAttribute("aria-pressed", String(available && parallelOn));
}

export function initReadingLayout(): void {
  focusOn = readFlag(FOCUS_KEY, false);
  parallelOn = readFlag(PARALLEL_KEY, true);
  applyFocus(focusOn);

  const onViewportChange = () => {
    // A desktop article must remain readable when the window shrinks. The
    // CSS compact layout hides .detail-panel unless explicitly made visible.
    const detail = document.querySelector(".detail-panel");
    if (isCompact() && detail?.querySelector(".detail-body")) detail.classList.add("visible");
    syncFocusControl();
  };
  window.matchMedia?.(COMPACT_QUERY).addEventListener("change", onViewportChange);
  onViewportChange();

  document.getElementById("focus-btn")?.addEventListener("click", () => {
    if (isCompact()) document.querySelector(".detail-panel")?.classList.remove("visible");
    else applyFocus(!focusOn);
  });

  document.getElementById("parallel-btn")?.addEventListener("click", () => {
    parallelOn = !parallelOn;
    writeFlag(PARALLEL_KEY, parallelOn);
    setParallelAvailable(true);
    // Re-render the bilingual body in place; the detail renderer reads
    // parallelOn through isParallelPreferred().
    document.dispatchEvent(new CustomEvent("reading-layout-change"));
  });

  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    // Let open dialogs and menus handle Escape themselves.
    if (document.querySelector(".modal.visible, .dropdown-menu.open, dialog[open]")) return;
    if (isCompact()) document.querySelector(".detail-panel")?.classList.remove("visible");
    else if (focusOn) applyFocus(false);
  });
}
