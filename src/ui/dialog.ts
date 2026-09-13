/**
 * Modal dialog behaviour for every modal in the app.
 *
 * The modals already open/close by toggling a `.visible` class. Rather than
 * rewriting all five open/close functions, this watches that class and adds
 * what a dialog needs to be usable without a mouse:
 *
 * - `role="dialog"` / `aria-modal="true"` so assistive technology announces
 *   it as a dialog instead of a stray region.
 * - Escape closes it (the same `onClose` the visible Close button uses).
 * - Focus moves to the first field when it opens and returns to the control
 *   that opened it when it closes, instead of being left on a hidden element.
 */

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function registerDialog(modalId: string, onClose: () => void): void {
  const modal = document.getElementById(modalId);
  if (!modal) return;

  modal.setAttribute("role", "dialog");
  modal.setAttribute("aria-modal", "true");
  const label =
    modal.querySelector("h2, h3")?.textContent?.trim() || modalId.replace(/-/g, " ");
  modal.setAttribute("aria-label", label);

  let restoreFocusTo: HTMLElement | null = null;

  const focusFirst = () => {
    const target = modal.querySelector<HTMLElement>(FOCUSABLE);
    target?.focus({ preventScroll: true });
  };

  const isOpen = () => modal.classList.contains("visible");

  const onKeydown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      onClose();
    }
  };

  new MutationObserver(() => {
    const open = isOpen();
    if (open) {
      const active = document.activeElement;
      restoreFocusTo = active instanceof HTMLElement ? active : null;
      document.addEventListener("keydown", onKeydown, true);
      // Let the caller finish toggling before moving focus.
      queueMicrotask(focusFirst);
    } else {
      document.removeEventListener("keydown", onKeydown, true);
      const target = restoreFocusTo;
      restoreFocusTo = null;
      if (target && document.contains(target)) {
        target.focus({ preventScroll: true });
      }
    }
  }).observe(modal, { attributes: true, attributeFilter: ["class"] });
}
