import { afterEach, describe, expect, it, vi } from 'vitest';
import { readFileSync, rmSync } from 'node:fs';
const ipc = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: ipc }));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }));
import { state } from '../src/state';
import { renderItemDetail } from '../src/ui/render';

const item = (translated: string, md: string): any => ({
  id: 1, subscription_id: 1, title: 'Probe', link: 'https://medium.com/@x/post-1234',
  content_md: md,
  description: null, is_read: true, is_favorite: false, is_read_later: false,
  is_ignored: false, published_at: '2026-10-31T00:00:00Z', tags: null,
  translated_content: translated,
});

const fixture = () =>
  // Model output variants: a bare pair (no wrapper), and richly formatted
  // originals (heading, table, code, list, image, nested link-image, quote).
  '<div class="bilingual-content" data-cached="false">' +
  '<div class="paragraph-original">bare original</div><div class="paragraph-translated">裸对译文</div>' +
  '<div class="translation-paragraph"><div class="paragraph-original">## Metrics\n\n| a | b |\n| --- | --- |\n| 1 | 2 |</div><div class="paragraph-translated">指标表</div></div>' +
  '<div class="translation-paragraph"><div class="paragraph-original">```rust\nfn main() {}\n```</div><div class="paragraph-translated">代码</div></div>' +
  '<div class="translation-paragraph"><div class="paragraph-original">- one\n- two</div><div class="paragraph-translated">列表</div></div>' +
  '<div class="translation-paragraph"><div class="paragraph-original">![chart](https://example.com/c.png)</div><div class="paragraph-translated">图表</div></div>' +
  '</div>';

describe('parallel layout with formatted originals', () => {
  afterEach(() => {
    rmSync('/tmp/parallel-regression.log', { force: true });
    state.translationStateByItemId.clear();
    state.selectedItem = null;
  });

  it('normalizes bare pairs and keeps every original inside a grid row', () => {
    document.body.innerHTML = '<div id="detail-content"></div>';
    const article = item(fixture(), readFileSync('/tmp/probe_article.md', 'utf8'));
    state.selectedItem = article;
    state.translationStateByItemId.set(article.id, {
      runId: 0, useTranslation: true, inProgressContent: null,
      abortController: null, hasError: false, errorMessage: null,
    } as any);
    renderItemDetail(article);

    const bi = document.querySelector('.bilingual-content')!;
    expect(bi.classList.contains('parallel')).toBe(true);

    // The bare pair got its wrapper back: every original now sits directly
    // inside a .translation-paragraph (the parallel grid row).
    const origs = bi.querySelectorAll('.paragraph-original');
    expect(origs.length).toBe(5);
    origs.forEach((o) => {
      expect(o.parentElement!.classList.contains('translation-paragraph')).toBe(true);
      expect(o.parentElement!.querySelector('.paragraph-translated')).toBeTruthy();
    });

    // Formatted blocks stay inside their own cells: heading/table/code/list
    // live in the original column, never as stray grid rows of their own.
    const pairEls = bi.querySelectorAll('.translation-paragraph');
    expect(pairEls.length).toBe(5);
    pairEls.forEach((p) => {
      const kids = Array.from(p.children);
      expect(kids.length).toBe(2);
      for (const k of kids) {
        expect(['paragraph-original', 'paragraph-translated']).toContain(k.className);
      }
    });
    // Rich content survived the markdown pass and sanitiser.
    expect(bi.querySelector('table')).toBeTruthy();
    expect(bi.querySelector('pre')).toBeTruthy();
    expect(bi.querySelector('ul')).toBeTruthy();
    expect(bi.querySelector('.paragraph-original img')).toBeTruthy();
    expect(bi.querySelector('.paragraph-original h2')).toBeTruthy();
  });

  it('renders the real formatted translation without losing content', () => {
    document.body.innerHTML = '<div id="detail-content"></div>';
    const article = item(readFileSync('/tmp/fixed_output.html', 'utf8'), readFileSync('/tmp/probe_article.md', 'utf8'));
    state.selectedItem = article;
    state.translationStateByItemId.set(article.id, {
      runId: 0, useTranslation: true, inProgressContent: null,
      abortController: null, hasError: false, errorMessage: null,
    } as any);
    renderItemDetail(article);

    const bi = document.querySelector('.bilingual-content')!;
    const origs = bi.querySelectorAll('.paragraph-original');
    const text = (bi.textContent ?? '').replace(/\s+/g, ' ');
    // Every source paragraph's plain text survives the parallel render.
    const paras = (article.content_md as string)
      .split('\n\n')
      .map((p: string) => p.trim())
      .filter((p: string) => p.length > 60 && !/\/m\/signin|\/tag\/|^\]\(|^<img|byline|^\[Ron Olsen|authorPhoto|presentation/.test(p.slice(0, 80)) && !/^<img/.test(p));
    const missing = paras.filter((p: string) => !text.includes(p.replace(/[#*>`~_\[\]]/g, '').replace(/\s+/g, ' ').slice(0, 40)));
    expect(missing).toEqual([]);
    expect(origs.length).toBeGreaterThan(0);
  });
});
