/**
 * Centralized application state.
 *
 * This is the single source of truth for all shared frontend state. Modules
 * mutate it directly (`S.currentFilter = "unread"`) exactly like the old
 * module-level `let`s — the object reference is stable, so no re-wiring of
 * call sites is needed. `subscribe`/`notify` exist for incremental adoption
 * of reactive rendering.
 */

import type {
  FeedItem,
  FeedItemSummary,
  Subscription,
  FeedFilter,
  SearchMode,
} from "./types";

export type Listener = () => void;

export interface TranslationState {
  /**
   * Identity of this translation attempt. Events and terminal results carry
   * the same id, so a listener can reject progress from a run the user has
   * already cancelled or replaced. 0 means "no active run" (cache display).
   */
  runId: number;
  useTranslation: boolean;
  inProgressContent: string | null;
  abortController: AbortController | null;
  hasError: boolean;
  errorMessage: string | null;
}

export interface AppState {
  subscriptions: Subscription[];
  currentFilter: FeedFilter;
  currentTagFilter: string | null;
  currentSubscriptionId: number | null;
  currentItems: FeedItemSummary[];
  selectedItem: FeedItem | null;
  /** "Today + Unread" combination mode. */
  unreadFilterEnabled: boolean;
  useWebView: boolean;
  searchMode: SearchMode;
  chromaEnabled: boolean;
  /** Per-subscription webview on/off preference. */
  webviewPerSubscription: Map<number, boolean>;
  /** Per-item translation progress, keyed by item id. */
  translationStateByItemId: Map<number, TranslationState>;
  /** Paging state for the visible list (offset + whether more rows exist). */
  listPage: ListPageState;
}

export interface ListPageState {
  /** How many rows are already held in `currentItems`. */
  offset: number;
  /** The last page came back full, so another page may exist. */
  hasMore: boolean;
  /** A `loadMoreItems` request is in flight. */
  loading: boolean;
}

export const state: AppState = {
  subscriptions: [],
  currentFilter: "all",
  currentTagFilter: null,
  currentSubscriptionId: null,
  currentItems: [],
  selectedItem: null,
  unreadFilterEnabled: false,
  useWebView: false,
  searchMode: "text",
  chromaEnabled: false,
  webviewPerSubscription: new Map(),
  translationStateByItemId: new Map(),
  listPage: { offset: 0, hasMore: false, loading: false },
};

const listeners = new Set<Listener>();

export function getState(): AppState {
  return state;
}

export function subscribe(l: Listener): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function notify(): void {
  for (const l of listeners) {
    try {
      l();
    } catch (err) {
      console.error("state listener failed:", err);
    }
  }
}

/** Apply a batch of updates atomically and notify once at the end. */
export function update(patch: Partial<AppState>): void {
  Object.assign(state, patch);
  notify();
}
