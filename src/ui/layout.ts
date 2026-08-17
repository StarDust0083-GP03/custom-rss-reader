/**
 * Resizable, collapsible columns for the two left panels.
 *
 * The app grid is `var(--col-sidebar) 8px var(--col-items) 8px 1fr` — the
 * 8px tracks are the drag handles. Drag to resize (widths persist in
 * localStorage); double-click a handle to collapse that column to 0 and
 * double-click again to restore its previous width.
 */

const SIDEBAR_VAR = "--col-sidebar";
const ITEMS_VAR = "--col-items";
const SIDEBAR_KEY = "rss.col.sidebar";
const ITEMS_KEY = "rss.col.items";
const SIDEBAR_PREV_KEY = "rss.col.sidebar.prev";
const ITEMS_PREV_KEY = "rss.col.items.prev";

const SIDEBAR_MIN = 160;
const ITEMS_MIN = 240;
const DETAIL_MIN = 320;

function loadWidth(key: string, fallback: number): number {
  const v = parseFloat(localStorage.getItem(key) ?? "");
  return Number.isFinite(v) && v >= 0 ? v : fallback;
}

function saveWidth(key: string, w: number): void {
  localStorage.setItem(key, String(w));
}

export function initColumnLayout(app: HTMLElement): void {
  app.style.setProperty(SIDEBAR_VAR, `${loadWidth(SIDEBAR_KEY, 280)}px`);
  app.style.setProperty(ITEMS_VAR, `${loadWidth(ITEMS_KEY, 380)}px`);

  makeResizer(app, document.getElementById("resizer-sidebar"), {
    variable: SIDEBAR_VAR,
    key: SIDEBAR_KEY,
    prevKey: SIDEBAR_PREV_KEY,
    min: SIDEBAR_MIN,
    defaultWidth: 280,
    otherMin: ITEMS_MIN,
  });
  makeResizer(app, document.getElementById("resizer-items"), {
    variable: ITEMS_VAR,
    key: ITEMS_KEY,
    prevKey: ITEMS_PREV_KEY,
    min: ITEMS_MIN,
    defaultWidth: 380,
    otherMin: SIDEBAR_MIN,
  });
}

interface ResizerOpts {
  variable: string;
  key: string;
  prevKey: string;
  min: number;
  defaultWidth: number;
  /** Minimum width of the OTHER resizable column (detail always needs room). */
  otherMin: number;
}

function makeResizer(app: HTMLElement, el: HTMLElement | null, opts: ResizerOpts): void {
  if (!el) return;

  const width = () => parseFloat(app.style.getPropertyValue(opts.variable)) || 0;
  const setWidth = (w: number) => app.style.setProperty(opts.variable, `${w}px`);

  el.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    el.classList.add("active");
    const startX = e.clientX;
    const startW = width();

    const onMove = (ev: PointerEvent) => {
      const available = app.clientWidth - DETAIL_MIN - opts.otherMin;
      const w = Math.min(Math.max(startW + (ev.clientX - startX), opts.min), available);
      setWidth(w);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      el.classList.remove("active");
      saveWidth(opts.key, width());
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  });

  el.addEventListener("dblclick", () => {
    if (width() > 0) {
      // Collapse — remember the current width for restore.
      saveWidth(opts.prevKey, width());
      setWidth(0);
    } else {
      const prev = loadWidth(opts.prevKey, opts.defaultWidth);
      setWidth(prev);
    }
    saveWidth(opts.key, width());
  });
}
