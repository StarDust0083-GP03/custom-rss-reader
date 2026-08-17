/**
 * Generic dropdown menu — the "⋯" overflow menus used to declutter the
 * panel headers. Single-open-at-a-time, anchored below its button, closes
 * on outside click / Escape / item activation.
 */

let currentMenu: HTMLElement | null = null;

/** Close any open overflow menu (used by the theme picker). */
export function closeMenu(): void {
  closeCurrentMenu();
}

function closeCurrentMenu() {
  if (currentMenu) {
    currentMenu.classList.remove("open");
    document.removeEventListener("click", onOutsideClick);
    document.removeEventListener("keydown", onEscape);
    currentMenu = null;
  }
}

function onOutsideClick(e: MouseEvent) {
  const menu = currentMenu;
  if (menu && !menu.contains(e.target as Node)) {
    closeCurrentMenu();
  }
}

function onEscape(e: KeyboardEvent) {
  if (e.key === "Escape") closeCurrentMenu();
}

export interface MenuOptions {
  button: HTMLElement;
  menu: HTMLElement;
  /** Called with the item's data-action value when an item is chosen. */
  onAction: (action: string) => void;
}

/** Wire a button to its dropdown menu. */
export function attachMenu(opts: MenuOptions): void {
  const { button, menu, onAction } = opts;

  button.addEventListener("click", (e) => {
    e.stopPropagation();
    if (currentMenu === menu) {
      closeCurrentMenu();
      return;
    }
    closeCurrentMenu();
    currentMenu = menu;
    menu.classList.add("open");

    const rect = button.getBoundingClientRect();
    menu.style.top = `${rect.bottom + 6}px`;
    menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - 240))}px`;

    document.addEventListener("click", onOutsideClick);
    document.addEventListener("keydown", onEscape);
  });

  menu.querySelectorAll<HTMLButtonElement>(".dropdown-item").forEach((item) => {
    item.addEventListener("click", (e) => {
      e.stopPropagation();
      const action = item.dataset.action;
      closeCurrentMenu();
      if (action) onAction(action);
    });
  });
}
