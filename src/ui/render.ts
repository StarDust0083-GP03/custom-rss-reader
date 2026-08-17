/**
 * Rendering layer: subscriptions sidebar, item list, and article detail.
 *
 * All DOM writes for the three panels live here. Data loading (`loadItems`)
 * also lives here because the result is rendered immediately.
 */

import { items as itemsApi } from "../api";
import type { FeedItem, FeedItemSummary } from "../types";
import { setSafeHtml, setText, htmlToPlainText } from "../sanitize";
import { IframeManager } from "../iframe";
import { configureMarked } from "../markdown";
import { state } from "../state";
// Selection / subscription actions are defined in features/actions.ts and
// invoked from click handlers below. This is a function-level module cycle:
// both sides use hoisted function declarations, so it resolves at call time.
import {
  toggleAutoClassify,
  deleteSubscription,
} from "../features/actions";

const marked = configureMarked();
const S = state;

// Iframe manager — every load bumps a generation so stale loads are dropped.
export const iframeManager = new IframeManager();

// Monotonic counter for `loadItems` results, so a slow in-flight query for
// subscription A doesn't paint over the fresh results for subscription B.
let loadItemsSeq = 0;

// 更新切换按钮状态
export function updateToggleButtonStates() {
  const webviewBtn = document.getElementById("toggle-webview-btn") as HTMLButtonElement;

  if (webviewBtn) {
    webviewBtn.textContent = S.useWebView ? "Markdown" : "Web View";
  }
}

// 渲染订阅列表
export function renderSubscriptions() {
  const list = document.getElementById("subscription-list");
  if (!list) return;

  list.replaceChildren();

  // "All Items" entry
  const allItem = document.createElement("div");
  allItem.className = `subscription-item ${S.currentSubscriptionId === null && S.currentFilter === "all" ? "active" : ""}`;
  allItem.dataset.id = "all";
  const allTitle = document.createElement("span");
  allTitle.className = "subscription-title";
  allTitle.textContent = "All Items";
  allItem.appendChild(allTitle);
  allItem.addEventListener("click", () => {
    S.currentSubscriptionId = null;
    S.currentFilter = "all";
    renderSubscriptions();
    loadItems();
  });
  list.appendChild(allItem);

  S.subscriptions.forEach((sub) => {
    const item = document.createElement("div");
    item.className = `subscription-item ${S.currentSubscriptionId === sub.id ? "active" : ""}`;
    item.dataset.id = sub.id.toString();

    const info = document.createElement("div");
    info.className = "subscription-info";
    const title = document.createElement("span");
    title.className = "subscription-title";
    // Titles and URLs come from arbitrary RSS feeds — treat as untrusted text.
    title.textContent = sub.title || sub.url;
    info.appendChild(title);
    if (sub.use_website) info.appendChild(makeBadge("website", "Website"));
    if (!sub.auto_classify) info.appendChild(makeBadge("no-auto", "No Auto"));
    if (sub.rsshub_url) info.appendChild(makeBadge("rsshub", "RSSHub"));
    item.appendChild(info);

    const actions = document.createElement("div");
    actions.className = "subscription-actions";
    const autoBtn = document.createElement("button");
    autoBtn.className = "icon-btn toggle-auto-btn";
    autoBtn.dataset.id = sub.id.toString();
    autoBtn.title = "Toggle auto-classify";
    autoBtn.dataset.auto = sub.auto_classify ? "true" : "false";
    autoBtn.textContent = sub.auto_classify ? "AI" : "ai";
    autoBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleAutoClassify(sub.id);
    });
    actions.appendChild(autoBtn);
    const delBtn = document.createElement("button");
    delBtn.className = "icon-btn delete-sub";
    delBtn.dataset.id = sub.id.toString();
    delBtn.title = "Delete";
    delBtn.textContent = "×";
    delBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      deleteSubscription(sub.id);
    });
    actions.appendChild(delBtn);
    item.appendChild(actions);

    item.addEventListener("click", (e) => {
      const target = e.target as HTMLElement;
      if (target.closest(".delete-sub") || target.closest(".toggle-auto-btn")) return;
      selectSubscription(sub.id);
    });

    list.appendChild(item);
  });
}

function makeBadge(className: string, text: string): HTMLElement {
  const el = document.createElement("span");
  el.className = `badge ${className}`;
  el.textContent = text;
  return el;
}

// 选择订阅
export function selectSubscription(id: number | null) {
  S.currentSubscriptionId = id;
  // When selecting a specific subscription, don't reset filter (allows unread + subscription)
  // When selecting "All Items" (id === null), keep current filter to allow "unread" for all subscriptions
  renderSubscriptions();
  loadItems();
}

// 加载内容
export async function loadItems() {
  const seq = ++loadItemsSeq;
  setLoadingWithStatusLocal("", "Loading items...");
  try {
    let items: FeedItemSummary[] = [];
    const subId = S.currentSubscriptionId;

    if (S.currentFilter === "unread") {
      items = await itemsApi.unread(subId);
    } else if (S.currentFilter === "favorites") {
      items = await itemsApi.favorites();
    } else if (S.currentFilter === "read-later") {
      items = await itemsApi.readLater();
    } else if (S.currentFilter === "today") {
      // unreadFilterEnabled drives the "Today + Unread" combination; without
      // threading it through, the toggle had no functional effect.
      items = await itemsApi.today(subId, S.unreadFilterEnabled);
    } else if (S.currentFilter === "tag" && S.currentTagFilter) {
      items = await itemsApi.byTag(S.currentTagFilter, subId);
    } else {
      items = await itemsApi.list({ subscriptionId: subId });
    }

    // Drop a stale response if the user switched filters or subscriptions
    // while the request was in flight.
    if (seq !== loadItemsSeq) return;

    S.currentItems = items;
    renderItems();
    clearLoadingStatusLocal(true, "Ready");
  } catch (error) {
    if (seq !== loadItemsSeq) return;
    console.error("Failed to load items:", error);
    clearLoadingStatusLocal(false, "Load failed");
    toastErrorLocal("Failed to load items");
  }
}

// Local re-exports to avoid a ui/render -> ui/status -> ... import chain in
// circular contexts; status.ts is dependency-free so this is equivalent.
import { setLoadingWithStatus as setLoadingWithStatusLocal, clearLoadingStatus as clearLoadingStatusLocal } from "./status";
import { error as toastErrorLocal } from "../toast";

// 渲染内容列表 — every text node from the feed goes through `textContent` /
// `setText`, so a malicious RSS payload can no longer inject HTML or event
// attributes. Cards carry `data-id` for future targeted updates.
export function renderItems(preserveScroll = false) {
  const list = document.getElementById("items-list");
  if (!list) return;

  const scrollPos = preserveScroll ? list.scrollTop : 0;

  list.replaceChildren();

  if (S.currentItems.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = "No items found";
    list.appendChild(empty);
    return;
  }

  S.currentItems.forEach((item) => {
    const div = document.createElement("div");
    div.className = `item-card ${!item.is_read ? "unread" : ""} ${S.selectedItem?.id === item.id ? "active" : ""}`;
    div.dataset.id = item.id.toString();

    // Header: title + date
    const header = document.createElement("div");
    header.className = "item-header";
    const titleEl = document.createElement("h3");
    titleEl.className = "item-title";
    titleEl.textContent = item.title;
    header.appendChild(titleEl);
    if (item.published_at) {
      const date = document.createElement("span");
      date.className = "item-date";
      date.textContent = formatDate(item.published_at);
      header.appendChild(date);
    }
    div.appendChild(header);

    // Source line (issue #3) — small-caps kicker above the description so
    // every card shows WHERE the article came from, matching the detail
    // view's .detail-source. Skipped when the summary carries no source
    // (e.g. synthesized semantic-search hits).
    const sourceName = item.source_title || item.source_url;
    if (sourceName) {
      const src = document.createElement("div");
      src.className = "item-source";
      src.textContent = sourceName;
      div.appendChild(src);
    }

    if (item.description) {
      const desc = document.createElement("div");
      desc.className = "item-description";
      // RSS `description` is HTML (often with `<p>`, `<img>`, entities).
      // Strip tags + decode entities; otherwise literal "<p>" or "&amp;"
      // would be visible to the user.
      desc.textContent = htmlToPlainText(item.description);
      div.appendChild(desc);
    }

    // Tags — the clickable filter is built with data attributes; the tag
    // string itself goes into textContent.
    if (item.tags) {
      try {
        const tags = JSON.parse(item.tags) as string[];
        if (Array.isArray(tags) && tags.length > 0) {
          const tagWrap = document.createElement("div");
          tagWrap.className = "item-tags";
          for (const t of tags) {
            const tagEl = document.createElement("span");
            tagEl.className = "tag clickable-tag";
            tagEl.dataset.tag = t;
            tagEl.textContent = "#" + t;
            tagEl.addEventListener("click", (e) => {
              e.stopPropagation();
              filterByTagLocal(t);
            });
            tagWrap.appendChild(tagEl);
          }
          div.appendChild(tagWrap);
        }
      } catch {
        // Invalid JSON — ignore silently
      }
    }

    // Meta: badges + author
    const meta = document.createElement("div");
    meta.className = "item-meta";
    const itemTranslationState = S.translationStateByItemId.get(item.id);
    const isTranslating = !!itemTranslationState?.abortController;
    const hasTranslationError = !!itemTranslationState?.hasError;
    const hasTranslation = item.has_translation;
    if (isTranslating) meta.appendChild(makeBadge("translating-badge", "Translating..."));
    else if (hasTranslationError) meta.appendChild(makeBadge("translation-error-badge", "Error"));
    else if (hasTranslation) meta.appendChild(makeBadge("translated-badge", "Translated"));
    if (item.is_ignored) meta.appendChild(makeBadge("ignored-badge", "Ignored"));
    if (item.is_favorite) meta.appendChild(makeBadge("", "Favorite"));
    if (item.is_read_later) meta.appendChild(makeBadge("", "Later"));
    if (item.author) {
      const authorEl = document.createElement("span");
      authorEl.className = "item-author";
      authorEl.textContent = item.author;
      meta.appendChild(authorEl);
    }
    div.appendChild(meta);

    div.addEventListener("click", () => selectItemLocal(item));
    list.appendChild(div);
  });

  if (preserveScroll) {
    list.scrollTop = scrollPos;
  }
}

// selectItem lives in features/actions.ts (it performs read/ignore side
// effects). Imported lazily by name to keep the module cycle safe.
import { selectItem as selectItemLocal } from "../features/actions";

// 格式化日期
export function formatDate(dateString: string): string {
  const date = new Date(dateString);
  const now = new Date();
  // Some feeds publish future-dated entries (scheduled posts, clock skew).
  // Clamp so we never render a confusing negative "−5m ago".
  const diffMs = Math.max(0, now.getTime() - date.getTime());
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMs / 3600000);
  const diffDays = Math.floor(diffMs / 86400000);

  if (diffMins < 60) return `${diffMins}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;
  if (diffDays < 7) return `${diffDays}d ago`;
  return date.toLocaleDateString();
}

// Check if text contains markdown syntax that marked can render.
// Untrusted fields use textContent; only known-safe HTML is inserted via
// setSafeHtml (which sanitises via src/sanitize.ts).
/** Optional rendering context for streaming translations. */
export interface RenderDetailOptions {
  /**
   * Untranslated remainder of the source markdown during streaming. When
   * set, the bilingual view renders the completed paragraphs AND this tail
   * below them — untranslated content must stay visible while streaming,
   * with each finished paragraph substituting its original in place.
   */
  untranslatedTail?: string;
}

export function renderItemDetail(item: FeedItem, opts: RenderDetailOptions = {}) {
  cancelIgnoreTimerLocal();
  const detail = document.getElementById("detail-content");
  if (!detail) return;

  iframeManager.cancel(); // drop any webview load for the previous article
  detail.classList.remove("webview-mode");
  detail.replaceChildren();

  const subscription = S.subscriptions.find(s => s.id === item.subscription_id);
  const subName = subscription?.title || subscription?.url || "Unknown";
  const useWebViewForItem = S.webviewPerSubscription.get(item.subscription_id) ?? false;
  // Webview mode = show the REAL webpage whenever the subscription asks for
  // it and the item has a link (issue #2). The old cached-content/override
  // gymnastics made both modes render markdown — indistinguishable views.
  const showWebView = useWebViewForItem && item.link !== null;

  // Toggle button label
  const toggleBtn = document.getElementById("toggle-webview-btn") as HTMLButtonElement | null;
  if (toggleBtn) toggleBtn.textContent = showWebView ? "Markdown" : "Web View";

  // Top action buttons (read / favorite / read-later / open)
  const markReadBtn = document.getElementById("mark-read-btn");
  const favoriteBtn = document.getElementById("favorite-btn");
  const readLaterBtn = document.getElementById("read-later-btn");
  const openLinkBtn = document.getElementById("open-link-btn") as HTMLAnchorElement | null;
  const translateBtn = document.getElementById("translate-btn");
  if (markReadBtn) {
    markReadBtn.textContent = item.is_read ? "Unread" : "Read";
    markReadBtn.classList.toggle("active", item.is_read);
  }
  if (favoriteBtn) favoriteBtn.classList.toggle("active", item.is_favorite);
  if (readLaterBtn) readLaterBtn.classList.toggle("active", item.is_read_later);
  if (openLinkBtn) {
    if (item.link) {
      openLinkBtn.href = item.link;
      openLinkBtn.setAttribute("target", "_blank");
      openLinkBtn.setAttribute("rel", "noopener noreferrer");
    } else {
      openLinkBtn.href = "#";
    }
  }

  // Translation button state
  if (translateBtn) {
    const ts = S.translationStateByItemId.get(item.id);
    translateBtn.classList.remove("translating", "has-cache", "has-error");
    if (ts?.abortController) {
      translateBtn.classList.add("translating");
      translateBtn.textContent = "Cancel";
    } else if (ts?.hasError) {
      translateBtn.classList.add("has-error");
      translateBtn.textContent = "Retry";
      translateBtn.title = ts.errorMessage || "Translation failed";
    } else if (item.translated_content) {
      translateBtn.classList.add("has-cache");
      translateBtn.textContent = ts?.useTranslation ? "Show Original" : "Translate";
      translateBtn.title = "";
    } else {
      translateBtn.textContent = "Translate";
      translateBtn.title = "";
    }
  }

  // ---- Header (source + category) ----
  const source = document.createElement("div");
  source.className = "detail-source";
  source.textContent = subName + (item.category ? ` • ${item.category}` : "");
  detail.appendChild(source);

  // ---- Title ----
  const h1 = document.createElement("h1");
  h1.textContent = item.title;
  detail.appendChild(h1);

  // ---- Tags ----
  if (item.tags) {
    try {
      const tags = JSON.parse(item.tags) as string[];
      if (Array.isArray(tags) && tags.length > 0) {
        const tagWrap = document.createElement("div");
        tagWrap.className = "detail-tags";
        for (const t of tags) {
          const tagEl = document.createElement("span");
          tagEl.className = "tag";
          tagEl.textContent = "#" + t;
          tagWrap.appendChild(tagEl);
        }
        detail.appendChild(tagWrap);
      }
    } catch { /* ignore */ }
  }

  // ---- Meta line ----
  const meta = document.createElement("div");
  meta.className = "detail-meta";
  if (item.published_at) {
    const dateEl = document.createElement("span");
    dateEl.textContent = formatDate(item.published_at);
    meta.appendChild(dateEl);
  }
  if (item.author) {
    const authorEl = document.createElement("span");
    authorEl.textContent = item.author;
    meta.appendChild(authorEl);
  }
  if (item.link) {
    const linkEl = document.createElement("a");
    linkEl.href = item.link;
    linkEl.target = "_blank";
    linkEl.rel = "noopener noreferrer";
    linkEl.textContent = "Open in browser →";
    meta.appendChild(linkEl);
  }
  detail.appendChild(meta);

  // ---- Body ----
  const body = document.createElement("div");
  body.className = "detail-body";
  detail.appendChild(body);

  const ts = S.translationStateByItemId.get(item.id);
  const useTranslationForItem = ts?.useTranslation ?? false;

  // Translation FIRST (issue #2): the webview branch below returns early, so
  // when it came first an active translation was never rendered in webview
  // mode — the user clicked Translate and nothing happened. With this order,
  // an active bilingual view always displays; toggling "Show Original"
  // returns to the webpage.
  if (useTranslationForItem && item.translated_content) {
    const badge = document.createElement("span");
    badge.className = "translation-badge";
    badge.textContent = "Bilingual View";
    meta.appendChild(badge);
    // The translation content comes from the LLM — untrusted. Sanitise.
    const bilingualEl = document.createElement("div");
    bilingualEl.className = "bilingual-content";
    setSafeHtml(bilingualEl, item.translated_content);
    // The original side carries the article's MARKDOWN (the translation
    // source is content_md). Render it through marked so `**bold**`,
    // `[links](…)`, `![images](…)`, `# headers` etc. display as formatting
    // instead of literal syntax; sanitise again because the markdown is
    // untrusted.
    // ONLY pure-text originals are re-parsed: if the original already
    // contains elements (HTML-mode translations keep real `<img>`/`<a>`
    // inside paragraph-original), textContent would DROP them — leave
    // element-bearing originals exactly as sanitised.
    bilingualEl.querySelectorAll(".paragraph-original").forEach((el) => {
      if (el.children.length > 0) return; // real HTML (e.g. <img>) — keep
      const raw = el.textContent ?? "";
      if (!raw.trim() || !/[\\*\[\]#`_>|~-]/.test(raw)) return; // plain text
      const html = marked.parse(raw, { gfm: true }) as string;
      setSafeHtml(el, html);
    });
    body.appendChild(bilingualEl);

    // Streaming: keep the untranslated remainder visible below the
    // finished paragraphs. Completed paragraphs substitute their originals
    // (the bilingual pairs above); the rest of the article is NOT removed.
    if (opts.untranslatedTail && opts.untranslatedTail.trim()) {
      const tailEl = document.createElement("div");
      tailEl.className = "untranslated-tail";
      const html = marked.parse(opts.untranslatedTail, { gfm: true }) as string;
      setSafeHtml(tailEl, html);
      body.appendChild(tailEl);
    }
    return;
  }

  // Webview mode — load the actual website in a sandboxed iframe. Both
  // display paths still converge on the same backend pipeline when the
  // user switches to text: fetch website HTML → html_to_markdown_pipeline
  // → Markdown → `marked` → `setSafeHtml`.
  if (showWebView && item.link) {
    detail.classList.add("webview-mode");
    const container = document.createElement("div");
    container.className = "webview-container";
    const loading = document.createElement("div");
    loading.className = "webview-loading";
    loading.textContent = "Loading webpage...";
    container.appendChild(loading);
    body.appendChild(container);

    iframeManager.load({
      container,
      markdown: "",
      url: item.link,
      baseUrl: item.link,
      loadingEl: loading,
      onError: (msg) => {
        loading.textContent = "Failed to load: " + msg;
      },
    });
    return;
  }

  // Plain text / markdown path. The backend now lazily converts `content`
  // to `content_md` on first read, so every article always has markdown
  // by the time it reaches here. Both display paths (this host DOM text
  // branch and the iframe webview branch above) consume the same markdown.
  if (item.content_md) {
    // marked returns sanitised-ish HTML, but we still pass it through our
    // sanitiser to drop event handlers and dangerous schemes.
    const html = marked.parse(item.content_md, { gfm: true }) as string;
    setSafeHtml(body, html);
  } else if (item.description) {
    // Same untrusted-HTML handling as the list-item summary above.
    setText(body, htmlToPlainText(item.description));
  } else {
    setText(body, "No content available");
  }
}

// cancelIgnoreTimer lives with the selection logic in features/actions.ts.
import { cancelIgnoreTimer as cancelIgnoreTimerLocal } from "../features/actions";
// filterByTag lives in ui/filters.ts (which imports loadItems from here).
import { filterByTag as filterByTagLocal } from "./filters";
