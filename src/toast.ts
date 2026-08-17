/**
 * Toast / status-bar notification queue.
 *
 * The previous implementation stacked multiple toasts at the same position,
 * making them visually merge. This version is a single-instance queue: the
 * most recent toast replaces the previous one, with a brief fade.
 */

const TOAST_ID = "rss-reader-toast";

interface ToastEntry {
  text: string;
  level: "info" | "success" | "error";
  timeout: number;
}

let current: ToastEntry | null = null;
let clearTimer: number | null = null;

function ensureContainer(): HTMLElement {
  let el = document.getElementById(TOAST_ID);
  if (el) return el;
  el = document.createElement("div");
  el.id = TOAST_ID;
  el.style.cssText = `
    position: fixed;
    right: 16px;
    bottom: 32px;
    z-index: 9999;
    pointer-events: none;
    font-size: 13px;
    padding: 10px 16px;
    border-radius: 8px;
    color: #fff;
    background: rgba(0, 0, 0, .82);
    box-shadow: 0 6px 18px rgba(0, 0, 0, .25);
    opacity: 0;
    transform: translateY(8px);
    transition: opacity .2s ease, transform .2s ease;
    max-width: 360px;
    word-wrap: break-word;
  `;
  document.body.appendChild(el);
  return el;
}

const LEVEL_COLOR: Record<ToastEntry["level"], string> = {
  info: "rgba(34, 28, 20, .92)",
  success: "rgba(95, 122, 61, .96)",
  error: "rgba(176, 58, 42, .96)",
};

export function toast(text: string, level: ToastEntry["level"] = "info", timeoutMs = 3000): void {
  const el = ensureContainer();
  if (current && clearTimer !== null) {
    window.clearTimeout(clearTimer);
  }
  current = { text, level, timeout: timeoutMs };
  el.textContent = text;
  el.style.background = LEVEL_COLOR[level];
  // Trigger the fade-in (use rAF so the browser applies the initial state)
  requestAnimationFrame(() => {
    el.style.opacity = "1";
    el.style.transform = "translateY(0)";
  });
  clearTimer = window.setTimeout(() => {
    el.style.opacity = "0";
    el.style.transform = "translateY(8px)";
    current = null;
    clearTimer = null;
  }, timeoutMs);
}

export const info = (text: string, ms?: number) => toast(text, "info", ms);
export const success = (text: string, ms?: number) => toast(text, "success", ms);
export const error = (text: string, ms?: number) => toast(text, "error", ms ?? 4500);
