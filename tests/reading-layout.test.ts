import { beforeEach, describe, expect, it, vi } from "vitest";
import { initReadingLayout, isParallelPreferred, setParallelAvailable } from "../src/ui/reading-layout";

// The jsdom environment has no storage without a URL origin; provide a
// minimal in-memory Storage so the persistence paths run for real.
const memory = new Map<string, string>();
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: {
    getItem: (k: string) => (memory.has(k) ? memory.get(k)! : null),
    setItem: (k: string, v: string) => void memory.set(k, String(v)),
    removeItem: (k: string) => void memory.delete(k),
    clear: () => memory.clear(),
  },
});

function mountShell(): void {
  document.body.innerHTML = `
    <div class="app-container">
      <aside class="sidebar"></aside>
      <section class="items-panel"></section>
      <section class="detail-panel">
        <button id="focus-btn" aria-pressed="false">Focus</button>
        <button id="parallel-btn" aria-pressed="false" hidden>Parallel</button>
      </section>
    </div>`;
}

const container = () => document.querySelector(".app-container") as HTMLElement;
const focusBtn = () => document.getElementById("focus-btn") as HTMLButtonElement;
const parallelBtn = () => document.getElementById("parallel-btn") as HTMLButtonElement;

describe("reading layout states", () => {
  let compact = false;
  let resize = () => {};
  beforeEach(() => {
    compact = false;
    resize = () => {};
    vi.stubGlobal('matchMedia', () => ({
      get matches() { return compact; },
      addEventListener: (_event: string, handler: () => void) => { resize = handler; },
    }));
    localStorage.clear();
    mountShell();
  });

  it("uses the focus control to return to the article list in a compact window", () => {
    compact = true;
    initReadingLayout();
    const detail = document.querySelector('.detail-panel')!;
    detail.classList.add('visible');
    expect(focusBtn().textContent).toBe('← Article list');
    expect(focusBtn().getAttribute('aria-label')).toBe('Back to article list');
    focusBtn().click();
    expect(detail.classList.contains('visible')).toBe(false);
    expect(container().classList.contains('reading-focus')).toBe(false);
  });

  it("automatically shows an existing article on crossing into a compact window", () => {
    initReadingLayout();
    document.querySelector('.detail-panel')!.insertAdjacentHTML('beforeend', '<div class="detail-body">Article</div>');
    compact = true;
    resize();
    expect(document.querySelector('.detail-panel')?.classList.contains('visible')).toBe(true);
    expect(focusBtn().textContent).toBe('← Article list');
    compact = false;
    resize();
    expect(focusBtn().textContent).toBe('Focus');
    expect(focusBtn().getAttribute('aria-label')).toBeNull();
  });

  it("returns to the compact list with Escape even when manual focus is off", () => {
    compact = true;
    initReadingLayout();
    const detail = document.querySelector('.detail-panel')!;
    detail.classList.add('visible');
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    expect(detail.classList.contains('visible')).toBe(false);
  });

  it("starts in Quiet Desk: three panes, Parallel hidden until translation is active", () => {
    initReadingLayout();
    expect(container().classList.contains("reading-focus")).toBe(false);
    expect(focusBtn().getAttribute("aria-pressed")).toBe("false");
    expect(parallelBtn().hidden).toBe(true);
  });

  it("toggles Reading Room from the Focus button and persists the choice", () => {
    initReadingLayout();
    focusBtn().click();
    expect(container().classList.contains("reading-focus")).toBe(true);
    expect(focusBtn().textContent).toBe("Library");
    expect(localStorage.getItem("rss.reading.focus")).toBe("1");

    focusBtn().click();
    expect(container().classList.contains("reading-focus")).toBe(false);
    expect(localStorage.getItem("rss.reading.focus")).toBe("0");
  });

  it("restores Reading Room from storage on startup", () => {
    localStorage.setItem("rss.reading.focus", "1");
    initReadingLayout();
    expect(container().classList.contains("reading-focus")).toBe(true);
  });

  it("exits Reading Room on Escape", () => {
    initReadingLayout();
    focusBtn().click();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(container().classList.contains("reading-focus")).toBe(false);
  });

  it("does not exit Reading Room on Escape while a dialog is open", () => {
    initReadingLayout();
    focusBtn().click();
    const modal = document.createElement("div");
    modal.className = "modal visible";
    document.body.appendChild(modal);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(container().classList.contains("reading-focus")).toBe(true);
  });

  it("shows Parallel only when a bilingual view is available, and defaults it on", () => {
    initReadingLayout();
    setParallelAvailable(true);
    expect(parallelBtn().hidden).toBe(false);
    expect(parallelBtn().getAttribute("aria-pressed")).toBe("true");
    expect(isParallelPreferred()).toBe(true);

    setParallelAvailable(false);
    expect(parallelBtn().hidden).toBe(true);
  });

  it("remembers the Parallel preference across toggles", () => {
    initReadingLayout();
    setParallelAvailable(true);
    parallelBtn().click();
    expect(isParallelPreferred()).toBe(false);
    expect(localStorage.getItem("rss.reading.parallel")).toBe("0");
    expect(parallelBtn().getAttribute("aria-pressed")).toBe("false");
  });
});
