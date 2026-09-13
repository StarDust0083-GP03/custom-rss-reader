/**
 * Regression tests for the correctness findings from the architecture review.
 *
 * Each case failed before its fix, so the assertion is the contract: the
 * failure mode is written in the test name.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const bus = vi.hoisted(() => new Map<string, Set<(e: any) => void>>());
const ipc = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: ipc }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (name: string, cb: any) => {
    if (!bus.has(name)) bus.set(name, new Set());
    bus.get(name)!.add(cb);
    return () => bus.get(name)!.delete(cb);
  },
}));

import { state } from '../src/state';
import { registerDialog } from '../src/ui/dialog';
import { renderIndexStatus } from '../src/features/chroma';
import {
  addSubscription,
  closeDetailPane,
  retryFailedJobs,
  searchItems,
  selectItem,
  toggleFavorite,
  toggleReadLater,
} from '../src/features/actions';
import { loadItems, loadMoreItems, renderItemDetail } from '../src/ui/render';
import { cancelTranslation, translateItem } from '../src/features/ai';
import { renderMarkdown } from '../src/markdown';
import { safeHttpUrl } from '../src/iframe';

const deferred = () => {
  let resolve!: (v: any) => void;
  const promise = new Promise((r) => (resolve = r));
  return { promise, resolve };
};

const item = (id = 1): any => ({
  id,
  subscription_id: 1,
  title: 'Fixture article',
  link: 'https://example.com/posts/article',
  content_md: 'Original paragraph with enough words.',
  description: null,
  is_read: true,
  is_ignored: false,
  is_favorite: false,
  is_read_later: false,
  translated_content: null,
  tags: null,
  has_translation: false,
});

const emit = (name: string, payload: any) =>
  bus.get(name)?.forEach((cb) => cb({ payload }));

beforeEach(() => {
  document.body.innerHTML = `
    <div id="items-list"></div>
    <div id="detail-content"></div>
    <div id="subscription-list"></div>
    <section class="detail-panel"></section>
    <button id="favorite-btn"></button>
    <button id="read-later-btn"></button>
    <!-- The status bar is part of the refresh feedback path; without these
         nodes updateStatusBar() no-ops and the test would assert nothing. -->
    <div id="status-bar">
      <span id="status-text"></span>
      <span id="status-timer"></span>
      <span id="status-progress"></span>
      <span id="status-count"></span>
    </div>
  `;
  Object.assign(state, {
    currentItems: [],
    selectedItem: null,
    subscriptions: [],
    currentFilter: 'all',
    currentSubscriptionId: null,
    searchMode: 'text',
    chromaEnabled: false,
  });
  state.translationStateByItemId.clear();
  bus.clear();
  ipc.mockReset();
  ipc.mockImplementation(async (cmd: string) => (cmd === 'list_subscriptions' ? [] : null));
});

afterEach(() => {
  vi.useRealTimers();
});

describe('IPC contracts', () => {
  it('add-feed sends the camelCase option keys the Tauri command declares', async () => {
    await addSubscription({
      url: 'https://example.com/feed',
      website_url: 'https://example.com',
      rsshub_url: 'https://rsshub.app/test',
      use_website: true,
    });

    const args = ipc.mock.calls.find((c) => c[0] === 'add_subscription')?.[1];
    expect(args).toMatchObject({
      websiteUrl: 'https://example.com',
      rsshubUrl: 'https://rsshub.app/test',
      useWebsite: true,
    });
    // The snake_case form was silently ignored by the backend.
    expect(args).not.toHaveProperty('website_url');
  });
});

describe('library query ownership', () => {
  it('a completed filter load is not overwritten by an older search', async () => {
    const pending = deferred();
    ipc.mockImplementation((cmd: string) =>
      cmd === 'search_items' ? pending.promise : Promise.resolve([item(2)]),
    );

    const search = searchItems('old query');
    await loadItems();
    pending.resolve([item(1)]);
    await search;

    expect(state.currentItems.map((i) => i.id)).toEqual([2]);
  });
});

describe('translation run identity', () => {
  it('drops a cancelled run’s events and accepts the live run’s events', async () => {
    const calls: Array<{ cmd: string; args: any }> = [];
    const old = deferred();
    const fresh = deferred();
    let translations = 0;
    ipc.mockImplementation(async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'translate_item_bilingual_streaming') {
        translations += 1;
        return translations === 1 ? old.promise : fresh.promise;
      }
      return null;
    });

    const article = item();
    state.selectedItem = article;
    const first = translateItem(article);
    await vi.waitFor(() => expect(translations).toBe(1));
    cancelTranslation(article);
    const second = translateItem(article);
    await vi.waitFor(() => expect(translations).toBe(2));

    const [staleRun, liveRun] = calls
      .filter((c) => c.cmd === 'translate_item_bilingual_streaming')
      .map((c) => c.args.runId);
    expect(staleRun).toEqual(expect.any(Number));
    expect(liveRun).not.toBe(staleRun);

    emit('translation-progress', {
      item_id: 1,
      run_id: staleRun,
      completed: 1,
      total: 2,
      html_chunk: '<div class="paragraph-original">OLD RUN</div>',
      is_complete: false,
    });
    expect(state.translationStateByItemId.get(1)?.inProgressContent ?? '').not.toContain('OLD RUN');

    emit('translation-progress', {
      item_id: 1,
      run_id: liveRun,
      completed: 1,
      total: 2,
      html_chunk: '<div class="paragraph-original">NEW RUN</div>',
      is_complete: false,
    });
    expect(state.translationStateByItemId.get(1)?.inProgressContent).toContain('NEW RUN');

    old.resolve('old');
    fresh.resolve('new');
    await Promise.all([first, second]);
  });

  it('finishes from the command result when the final event never arrives', async () => {
    ipc.mockResolvedValue('<div class="bilingual-content">COMPLETE</div>');
    const article = item();
    state.selectedItem = article;

    await translateItem(article);

    expect(state.translationStateByItemId.get(1)?.abortController).toBeNull();
    expect(article.translated_content).toContain('COMPLETE');
  });
});

describe('explicit state, not timers', () => {
  it('does not mark an article ignored just because it was left quiet', async () => {
    vi.useFakeTimers();
    const article = item();
    ipc.mockResolvedValue(article);

    await selectItem(article);
    await vi.advanceTimersByTimeAsync(5000);

    expect(ipc.mock.calls.some((c) => c[0] === 'toggle_ignored')).toBe(false);
  });

  it('removes an item from Favorites when it is un-favorited', async () => {
    const article = { ...item(), is_favorite: true };
    state.currentFilter = 'favorites';
    state.currentItems = [article];
    state.selectedItem = article;
    ipc.mockResolvedValue(false);

    await toggleFavorite(1);

    expect(state.currentItems).toHaveLength(0);
    expect(article.is_favorite).toBe(false);
  });

  it('removes an item from Read Later when its flag is cleared', async () => {
    const article = { ...item(), is_read_later: true };
    state.currentFilter = 'read-later';
    state.currentItems = [article];
    state.selectedItem = article;
    ipc.mockResolvedValue(false);

    await toggleReadLater(1);

    expect(state.currentItems).toHaveLength(0);
  });
});

describe('markdown fidelity', () => {
  it('keeps an image title out of the image URL', () => {
    const div = document.createElement('div');
    div.innerHTML = renderMarkdown('![alt](https://example.com/a.png "A title")');

    expect(div.querySelector('img')?.getAttribute('src')).toBe('https://example.com/a.png');
    expect(div.querySelector('img')?.getAttribute('title')).toBe('A title');
  });

  it('still encodes spaces in a destination with no title', () => {
    const div = document.createElement('div');
    div.innerHTML = renderMarkdown('![alt](https://example.com/a b.png)');

    expect(div.querySelector('img')?.getAttribute('src')).toBe('https://example.com/a%20b.png');
  });

  it('does not rewrite literal syntax inside code', () => {
    const fenced = document.createElement('div');
    fenced.innerHTML = renderMarkdown('```text\n](literal)\n```');
    expect(fenced.querySelector('code')?.textContent).toContain('](literal)');

    const inline = document.createElement('div');
    inline.innerHTML = renderMarkdown('Example: `](literal)` done');
    expect(inline.querySelector('code')?.textContent).toBe('](literal)');
  });

  it('does not rewrite an image inside a code fence', () => {
    const div = document.createElement('div');
    div.innerHTML = renderMarkdown('```md\n![alt](a b.png "A title")\n```');
    expect(div.querySelector('code')?.textContent).toContain('![alt](a b.png "A title")');
    expect(div.querySelector('img')).toBeNull();
  });

  it('resolves relative article images against the article URL', () => {
    const article = { ...item(), content_md: '![diagram](./diagram.png)' };
    state.selectedItem = article;

    renderItemDetail(article);

    expect(document.querySelector<HTMLImageElement>('.detail-body img')?.src).toBe(
      'https://example.com/posts/diagram.png',
    );
  });
});

describe('paged library', () => {
  const page = (start: number, count: number) =>
    Array.from({ length: count }, (_, i) => ({ ...item(start + i), id: start + i }));

  it('reaches articles beyond the first page', async () => {
    const calls: Array<{ cmd: string; args: any }> = [];
    ipc.mockImplementation(async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'get_items') return args.offset === 0 ? page(1, 50) : page(51, 3);
      return null;
    });

    await loadItems();
    expect(state.currentItems).toHaveLength(50);
    expect(state.listPage.hasMore).toBe(true);
    expect(document.querySelector('.load-more-btn')).not.toBeNull();

    await loadMoreItems();

    expect(state.currentItems).toHaveLength(53);
    expect(state.currentItems[50].id).toBe(51);
    expect(state.listPage.hasMore).toBe(false);
    expect(calls.filter((c) => c.cmd === 'get_items').map((c) => c.args.offset)).toEqual([0, 50]);
    expect(document.querySelector('.load-more-btn')).toBeNull();
  });

  it('drops a page that resolves after a newer filter load', async () => {
    const pending = deferred();
    let calls = 0;
    ipc.mockImplementation(async (cmd: string) => {
      if (cmd !== 'get_items') return null;
      calls += 1;
      if (calls === 2) return pending.promise; // the "load more" request
      return page(1, 50);
    });

    await loadItems();
    const more = loadMoreItems();
    // A new filter load takes ownership of the list before the page returns.
    await loadItems();
    pending.resolve(page(51, 3));
    await more;

    expect(state.currentItems).toHaveLength(50);
    expect(state.currentItems.every((i) => i.id <= 50)).toBe(true);
  });
});

describe('reader ownership', () => {
  it('does not paint a different article over the open one', () => {
    const open = item(1);
    state.selectedItem = open;
    renderItemDetail(open);
    const before = document.getElementById('detail-content')?.textContent ?? '';

    renderItemDetail(item(2));

    expect(document.getElementById('detail-content')?.textContent).toBe(before);
  });

  it('activates an article card from the keyboard', async () => {
    ipc.mockImplementation(async (cmd: string) => (cmd === 'get_item' ? item(1) : null));
    state.currentItems = [{ ...item(1) }];
    const { renderItems } = await import('../src/ui/render');
    renderItems();

    const card = document.querySelector<HTMLElement>('.item-card');
    expect(card?.getAttribute('role')).toBe('button');
    expect(card?.getAttribute('tabindex')).toBe('0');
    card?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await vi.waitFor(() =>
      expect(ipc.mock.calls.some((c) => c[0] === 'get_item')).toBe(true),
    );
  });
});

describe('narrow-window navigation', () => {
  it('reveals the reader overlay on selection and hides it on Back', async () => {
    const article = item();
    ipc.mockResolvedValue(article);

    await selectItem(article);
    expect(document.querySelector('.detail-panel')?.classList.contains('visible')).toBe(true);

    closeDetailPane();
    expect(document.querySelector('.detail-panel')?.classList.contains('visible')).toBe(false);
  });
});

describe('article URL policy', () => {
  it('rejects IPv4-mapped and private IPv6 literals', () => {
    expect(safeHttpUrl('http://[::ffff:127.0.0.1]/')).toBeNull();
    expect(safeHttpUrl('http://[::ffff:169.254.169.254]/')).toBeNull();
    expect(safeHttpUrl('http://[::ffff:10.0.0.1]/')).toBeNull();
    expect(safeHttpUrl('http://[::127.0.0.1]/')).toBeNull();
    expect(safeHttpUrl('http://[fd00::1]/')).toBeNull();
    expect(safeHttpUrl('http://[fe80::1]/')).toBeNull();
    expect(safeHttpUrl('http://127.0.0.1/')).toBeNull();
    expect(safeHttpUrl('http://[::1]/')).toBeNull();
  });

  it('still accepts public addresses', () => {
    expect(safeHttpUrl('http://[::ffff:8.8.8.8]/')).not.toBeNull();
    expect(safeHttpUrl('https://example.com/post')).toBe('https://example.com/post');
  });
});

describe('dialog behaviour', () => {
  const buildModal = () => {
    document.body.innerHTML = `
      <button id="opener">open</button>
      <div id="test-modal" class="modal">
        <div class="modal-content">
          <h2>Test dialog</h2>
          <input id="first-field" />
          <button id="close-btn">Close</button>
        </div>
      </div>`;
    const onClose = vi.fn();
    registerDialog('test-modal', onClose);
    return { modal: document.getElementById('test-modal')!, onClose };
  };

  it('announces itself as a dialog and moves focus in when opened', async () => {
    const { modal } = buildModal();
    const opener = document.getElementById('opener') as HTMLButtonElement;
    opener.focus();

    modal.classList.add('visible');
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(modal.getAttribute('role')).toBe('dialog');
    expect(modal.getAttribute('aria-modal')).toBe('true');
    expect(document.activeElement?.id).toBe('first-field');
  });

  it('closes on Escape and returns focus to the opener', async () => {
    const { modal, onClose } = buildModal();
    const opener = document.getElementById('opener') as HTMLButtonElement;
    opener.focus();

    modal.classList.add('visible');
    await new Promise((resolve) => setTimeout(resolve, 0));

    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(onClose).toHaveBeenCalledTimes(1);

    // The caller's close function normally removes the class; do that here and
    // assert focus is restored.
    modal.classList.remove('visible');
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(document.activeElement?.id).toBe('opener');

    // Escape is no longer wired once closed.
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe('background job visibility', () => {
  it('retries failed work and reports how much was requeued', async () => {
    const calls: string[] = [];
    ipc.mockImplementation(async (cmd: string) => {
      calls.push(cmd);
      if (cmd === 'retry_failed_jobs') return 2;
      if (cmd === 'requeue_expired_jobs') return 1;
      return null;
    });

    await retryFailedJobs();

    expect(calls).toEqual(['retry_failed_jobs', 'requeue_expired_jobs']);
  });

  it('refreshes the visible job counts after a refresh', async () => {
    const calls: string[] = [];
    ipc.mockImplementation(async (cmd: string) => {
      calls.push(cmd);
      if (cmd === 'fetch_all_feeds') {
        return { total_subscriptions: 1, success_count: 1, total_items: 1, new_items: 1, errors: [] };
      }
      if (cmd === 'get_job_stats') {
        return { queued: 3, running: 1, failed: 2, succeeded: 0, recent_errors: ['boom'] };
      }
      return [];
    });
    const { refreshAllFeeds } = await import('../src/features/actions');

    await refreshAllFeeds();

    // Enrichment is queued, not awaited — the user must be told, and failures
    // must not look like a clean refresh.
    expect(calls).toContain('get_job_stats');
    expect(document.getElementById('status-text')?.textContent ?? '').toMatch(/background task/);
  });
});

describe('semantic index status panel', () => {
  const panel = () => {
    document.body.innerHTML = `
      <section class="index-status" id="chroma-index-status" data-state="idle">
        <header><h3>Semantic index</h3><span class="index-chip"><span class="index-dot"></span><span id="chroma-chip-text"></span></span></header>
        <div class="index-meter" id="chroma-progress" role="progressbar" aria-valuenow="0"><span id="chroma-progress-bar"></span></div>
        <div class="index-foot">
          <p class="index-summary" id="chroma-summary"></p>
          <p class="index-detail" id="chroma-detail"></p>
        </div>
        <p class="index-note" id="chroma-index-note"></p>
      </section>`;
  };
  const text = (id: string) => document.getElementById(id)?.textContent ?? "";
  const state = () => document.getElementById("chroma-index-status")?.dataset.state ?? "";
  const status = (over: Partial<Parameters<typeof renderIndexStatus>[0]> = {}) => ({
    enabled: true,
    running: false,
    phase: "",
    indexed: 0,
    total: 0,
    queued_jobs: 0,
    pending_upserts: 0,
    pending_deletes: 0,
    collection_name: "rss_articles",
    collection_id: null,
    done: 0,
    scan_total: 0,
    elapsed_ms: 0,
    ...over,
  });

  it('summarises a complete index in one line', () => {
    panel();
    renderIndexStatus(status({ indexed: 7498, total: 7498 }));

    expect(state()).toBe("ready");
    expect(text("chroma-chip-text")).toBe("Up to date");
    expect(text("chroma-summary")).toBe("7,498 of 7,498 articles indexed");
    expect(text("chroma-detail")).toBe("");
    expect(text("chroma-index-note")).toBe("");
    expect(document.getElementById("chroma-progress-bar")?.style.width).toBe("100%");
  });

  it('puts phase, progress, elapsed and queue in the bottom-right while running', () => {
    panel();
    renderIndexStatus(
      status({
        indexed: 4200,
        total: 8400,
        queued_jobs: 9627,
        running: true,
        phase: "upserts",
        done: 400,
        scan_total: 8400,
        elapsed_ms: 12_000,
      }),
    );

    expect(state()).toBe("running");
    expect(text("chroma-chip-text")).toBe("Indexing…");
    expect(text("chroma-summary")).toBe("4,200 of 8,400 articles indexed");
    expect(text("chroma-detail")).toBe("embedding · 400/8,400 · 12s · 9,627 queued");
    expect(text("chroma-index-note")).toBe("");
    expect(document.getElementById("chroma-progress")?.getAttribute("aria-valuenow")).toBe("50");
  });

  it('keeps the detail line short when only queue depth is known', () => {
    panel();
    renderIndexStatus(status({ indexed: 7498, total: 7498, queued_jobs: 12 }));
    expect(state()).toBe("ready");
    expect(text("chroma-detail")).toBe("12 queued");
    expect(text("chroma-index-note")).toBe("");
  });

  it('offers the fix when the index lags and nothing is queued', () => {
    panel();
    renderIndexStatus(status({ indexed: 40, total: 100 }));
    expect(state()).toBe("behind");
    expect(text("chroma-chip-text")).toBe("Behind");
    expect(text("chroma-index-note")).toMatch(/catches up the remaining 60/);

    // Already queued: no advice needed, the queue shows in the detail line.
    renderIndexStatus(status({ indexed: 40, total: 100, queued_jobs: 60 }));
    expect(state()).toBe("ready");
    expect(text("chroma-index-note")).toBe("");
    expect(text("chroma-detail")).toBe("60 queued");
  });

  it('says plainly that nothing is being indexed when disabled', () => {
    panel();
    renderIndexStatus(status({ enabled: false, indexed: 0, total: 100 }));
    expect(state()).toBe("off");
    expect(text("chroma-chip-text")).toBe("Disabled");
    expect(text("chroma-summary")).toBe("Not indexing");
    expect(text("chroma-detail")).toBe("");
    expect(text("chroma-index-note")).toMatch(/search stays keyword-only/);
  });

  it('handles an empty library without dividing by zero', () => {
    panel();
    renderIndexStatus(status({ indexed: 0, total: 0 }));
    expect(text("chroma-summary")).toBe("0 of 0 articles indexed");
    expect(document.getElementById("chroma-progress-bar")?.style.width).toBe("100%");
  });
});
