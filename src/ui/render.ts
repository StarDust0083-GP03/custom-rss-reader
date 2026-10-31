/**
 * Rendering layer: subscriptions sidebar, item list, and article detail.
 *
 * All DOM writes for the three panels live here. Data loading (`loadItems`)
 * also lives here because the result is rendered immediately.
 */

import { items as itemsApi } from "../api";
import type { FeedItem, FeedItemSummary } from "../types";
import { setSafeHtml, setText, htmlToPlainText, dedupeImages } from "../sanitize";
import { applyBionic } from "../bionic";
import { normalizeMediaUrls } from "../media";
import { IframeManager } from "../iframe";
import { configureMarked, renderMarkdown } from "../markdown";
import { state } from "../state";
import { isParallelPreferred, setParallelAvailable } from "./reading-layout";
import { resetFiltersForSubscription } from "./filter-state";
// Selection / subscription actions are defined in features/actions.ts and
// invoked from click handlers below. This is a function-level module cycle:
// both sides use hoisted function declarations, so it resolves at call time.
import {
  toggleAutoClassify,
  toggleUseWebsite,
  deleteSubscription,
} from "../features/actions";

configureMarked();
const S = state;

// Inline icons for the subscription row actions (stroke = currentColor).
const SPARKLE_ICON =
  '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l1.9 5.7 5.7 1.9-5.7 1.9L12 18.2l-1.9-5.7-5.7-1.9 5.7-1.9z"/><path d="M18.5 15.5l.8 2.4 2.4.8-2.4.8-.8 2.4-.8-2.4-2.4-.8 2.4-.8z"/></svg>';
const TRASH_ICON =
  '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6M14 11v6"/></svg>';
// Globe icon for the subscription's persistent website-content toggle
// (issue #8) — filled with the accent color when use_website is on.
const GLOBE_ICON =
  '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3a14 14 0 0 1 0 18 14 14 0 0 1 0-18z"/></svg>';

// Iframe manager — every load bumps a generation so stale loads are dropped.
export const iframeManager = new IframeManager();

// Monotonic counter for `loadItems` results, so a slow in-flight query for
// subscription A doesn't paint over the fresh results for subscription B.
let loadItemsSeq = 0;

// Sync the Markdown / Web View buttons to the current render mode.
//
// Two separate buttons with distinct semantics (user clarification):
//   - Web View button  -> toggles the SUBSCRIPTION's persistent webview
//     mode (use_website): whether content is fetched from the website vs RSS
//     text. Active state = the subscription's use_website value.
//   - Markdown button  -> within webview mode, switches the transient render
//     between host Markdown and an isolated, media-blocked reader frame.
//     In text mode (use_website off), it's disabled because only text is shown.
export function updateToggleButtonStates() {
  const markdownBtn = document.getElementById("markdown-btn") as HTMLButtonElement | null;
  const webviewBtn = document.getElementById("webview-btn") as HTMLButtonElement | null;

  const item = S.selectedItem;
  const subId = item?.subscription_id;
  const subscription = subId != null
    ? S.subscriptions.find(s => s.id === subId)
    : undefined;

  // Persistent subscription webview mode — controlled by the Web View button.
  const webviewMode = subscription?.use_website ?? false;
  // Within webview mode, host DOM <-> isolated reader — controlled by the Markdown
  // button, remembered in memory for this session only.
  const wantLiveWeb = subId != null
    ? (S.webviewPerSubscription.get(subId) ?? false)
    : false;
  // The isolated reader is available when the subscription uses website content AND the
  // item has a link.
  const canWeb = webviewMode && !!item?.link;
  const showWeb = canWeb && wantLiveWeb;

  if (webviewBtn) {
    webviewBtn.classList.toggle("active", webviewMode);
    webviewBtn.disabled = false;
    webviewBtn.title = webviewMode
      ? "Webview mode on — click to switch to text (RSS)"
      : "Webview mode off — click to fetch content from the website";
  }
  if (markdownBtn) {
    // In text mode there's no markdown/web switch — only text is shown.
    markdownBtn.disabled = !canWeb;
    markdownBtn.classList.toggle("active", !showWeb);
    markdownBtn.title = canWeb
      ? (showWeb
          ? "Viewing isolated reader — click for Markdown"
          : "Showing Markdown — click for isolated reader (audio/video blocked)")
      : "Text mode — no separate reader frame";
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
    selectSubscription(null);
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
    autoBtn.title = sub.auto_classify ? "Auto-classify on — click to disable" : "Auto-classify off — click to enable";
    autoBtn.dataset.auto = sub.auto_classify ? "true" : "false";
    autoBtn.setAttribute("aria-label", "Toggle auto-classify");
    // Sparkle icon — filled (accent) when auto-classify is on.
    autoBtn.innerHTML = SPARKLE_ICON;
    autoBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleAutoClassify(sub.id);
    });
    actions.appendChild(autoBtn);
    const webBtn = document.createElement("button");
    webBtn.className = "icon-btn toggle-web-btn";
    webBtn.dataset.id = sub.id.toString();
    webBtn.title = sub.use_website
      ? "Website content on — click to use RSS content"
      : "Website content off — click to fetch content from the website";
    webBtn.dataset.web = sub.use_website ? "true" : "false";
    webBtn.setAttribute("aria-label", "Toggle website content");
    // Globe icon — filled (accent) when website content is enabled.
    webBtn.innerHTML = GLOBE_ICON;
    webBtn.classList.toggle("filled", sub.use_website);
    webBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleUseWebsite(sub.id);
    });
    actions.appendChild(webBtn);
    const delBtn = document.createElement("button");
    delBtn.className = "icon-btn delete-sub";
    delBtn.dataset.id = sub.id.toString();
    delBtn.title = "Delete";
    delBtn.setAttribute("aria-label", "Delete subscription");
    delBtn.innerHTML = TRASH_ICON;
    delBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      deleteSubscription(sub.id);
    });
    actions.appendChild(delBtn);
    item.appendChild(actions);

    item.addEventListener("click", (e) => {
      const target = e.target as HTMLElement;
      if (target.closest(".delete-sub") || target.closest(".toggle-auto-btn") || target.closest(".toggle-web-btn")) return;
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
  // A subscription click changes the content context. Start from the full
  // subscription view instead of carrying over Unread, Today, or Tags.
  resetFiltersForSubscription();
  renderSubscriptions();
  void loadItems();
}

// Peek navigation: scroll the sidebar to a subscription row and flash-
// highlight it WITHOUT selecting it (no filter change, no item reload).
// Triggered by clicking a source label in the article list / detail panel.
export function revealSubscription(subscriptionId: number) {
  const list = document.getElementById("subscription-list");
  const row = list?.querySelector<HTMLElement>(
    `.subscription-item[data-id="${subscriptionId}"]`,
  );
  if (!row) return;

  row.scrollIntoView({ block: "center", behavior: "smooth" });
  row.classList.add("reveal");
  window.setTimeout(() => row.classList.remove("reveal"), 1600);
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
      items = await itemsApi.favorites(subId);
    } else if (S.currentFilter === "read-later") {
      items = await itemsApi.readLater(subId);
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

    // Quiet Desk hierarchy: title → excerpt → one source footer. The date
    // moved out of the title row so the headline is the only thing competing
    // for attention; provenance is a single quiet line at the bottom.
    const header = document.createElement("div");
    header.className = "item-header";
    const titleEl = document.createElement("h3");
    titleEl.className = "item-title";
    titleEl.textContent = item.title;
    header.appendChild(titleEl);
    div.appendChild(header);

    if (item.description) {
      const desc = document.createElement("div");
      desc.className = "item-description";
      // RSS `description` is HTML (often with `<p>`, `<img>`, entities).
      // Strip tags + decode entities; otherwise literal "<p>" or "&amp;"
      // would be visible to the user.
      desc.textContent = htmlToPlainText(item.description);
      div.appendChild(desc);
    }

    // Meta footer: source · time · first tag · state badges · author. One
    // line instead of four stacked rows — fewer labels, same information.
    const meta = document.createElement("div");
    meta.className = "item-meta";
    const itemTranslationState = S.translationStateByItemId.get(item.id);
    const isTranslating = !!itemTranslationState?.abortController;
    const hasTranslationError = !!itemTranslationState?.hasError;
    const hasTranslation = item.has_translation;
    // Source kicker (issue #3) leads the footer: it is the "where" of the
    // title → excerpt → source hierarchy. Skipped when the summary carries no
    // source (e.g. synthesized semantic-search hits).
    const sourceName = item.source_title || item.source_url;
    if (sourceName) {
      const src = document.createElement("span");
      src.className = "item-source";
      src.textContent = sourceName;
      src.title = "Go to source";
      // Peek navigation: scroll + highlight the source in the sidebar
      // WITHOUT selecting it (and without selecting this article).
      src.addEventListener("click", (e) => {
        e.stopPropagation();
        revealSubscription(item.subscription_id);
      });
      meta.appendChild(src);
    }

    if (item.published_at) {
      const date = document.createElement("span");
      date.className = "item-date";
      date.textContent = formatDate(item.published_at);
      meta.appendChild(date);
    }

    // One tag in the list; the rest live in the tag picker and the detail
    // view. A "+N" hint keeps the count discoverable without a chip wall.
    if (item.tags) {
      try {
        const tags = JSON.parse(item.tags) as string[];
        if (Array.isArray(tags) && tags.length > 0) {
          const first = document.createElement("span");
          first.className = "tag clickable-tag";
          first.dataset.tag = tags[0];
          first.textContent = "#" + tags[0];
          first.addEventListener("click", (e) => {
            e.stopPropagation();
            filterByTagLocal(tags[0]);
          });
          meta.appendChild(first);
          if (tags.length > 1) {
            const more = document.createElement("span");
            more.className = "item-more-tags";
            more.textContent = `+${tags.length - 1}`;
            more.title = tags.slice(1).map(t => "#" + t).join(" ");
            meta.appendChild(more);
          }
        }
      } catch {
        // Invalid JSON — ignore silently
      }
    }

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
// ---------------------------------------------------------------------------
// Streaming tail computation
// ---------------------------------------------------------------------------

/**
 * Compute the untranslated remainder of `source` given the bilingual HTML
 * accumulated so far.
 *
 * Each completed chunk carries its ORIGINAL text inside a
 * `.paragraph-original` div (the LLM is instructed to preserve it verbatim).
 * We walk the originals in document order and sequentially cut each one out
 * of the source; whatever remains after the last completed original is the
 * untranslated tail that must stay visible while streaming.
 *
 * An original that the LLM paraphrased (verbatim match not found) is simply
 * skipped — worst case one paragraph appears both translated and in the
 * tail, which is far better than hiding untranslated content.
 */
function untranslatedTail(source: string, bilingualHtml: string): string {
  if (!source.trim() || !bilingualHtml.trim()) {
    return "";
  }

  const template = document.createElement("template");
  template.innerHTML = bilingualHtml;
  const originals = Array.from(
    template.content.querySelectorAll(".paragraph-original"),
  );

  let remaining = source;
  for (const el of originals) {
    const needle = (el.textContent ?? "").trim();
    if (!needle) continue;
    const idx = remaining.indexOf(needle);
    if (idx === -1) continue; // paraphrased — skip rather than mis-cut
    remaining = remaining.slice(idx + needle.length);
  }
  return remaining.trim();
}

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
  // Persistent subscription webview mode — controlled by the Web View button
  // (use_website: content fetched from website vs RSS text). Within webview
  // mode the Markdown button switches between host Markdown and an isolated
  // reader frame; remote scripts and media are never loaded.
  const webviewMode = subscription?.use_website ?? false;
  const wantLiveWeb = S.webviewPerSubscription.get(item.subscription_id) ?? false;
  const showWebView = webviewMode && wantLiveWeb && item.link !== null;

  // Sync the Markdown / Web View mode buttons to the rendered mode.
  updateToggleButtonStates();

  // Top action buttons (read / favorite / read-later / open)
  const markReadBtn = document.getElementById("mark-read-btn");
  const favoriteBtn = document.getElementById("favorite-btn");
  const readLaterBtn = document.getElementById("read-later-btn");
  const openLinkBtn = document.getElementById("open-link-btn") as HTMLAnchorElement | null;
  const translateBtn = document.getElementById("translate-btn");
  if (markReadBtn) {
    // Icon-only button (✓) — the active state carries the read styling.
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

  const ts = S.translationStateByItemId.get(item.id);
  // Keep the control's DOM and active class stable across streaming renders.
  // Removing/readding the class or text restarts painting and animations.
  if (translateBtn) {
    const translating = !!ts?.abortController;
    const failed = !translating && !!ts?.hasError;
    const cached = !translating && !failed && !!item.translated_content?.trim();
    translateBtn.classList.toggle("translating", translating);
    translateBtn.classList.toggle("has-error", failed);
    translateBtn.classList.toggle("has-cache", cached);
    translateBtn.setAttribute("aria-busy", String(translating));

    let label = "Translate";
    let title = "Right-click or long-press to re-translate";
    if (translating) {
      label = "Cancel";
      title = "Cancel translation";
    } else if (failed) {
      label = "Retry";
      title = ts?.errorMessage || "Translation failed";
    } else if (cached && ts?.useTranslation) {
      label = "Show Original";
    }
    if (translateBtn.textContent !== label) translateBtn.textContent = label;
    if (translateBtn.title !== title) translateBtn.title = title;
  }

  // ---- Header (source + category) ----
  const source = document.createElement("div");
  source.className = "detail-source";
  source.textContent = subName + (item.category ? ` • ${item.category}` : "");
  source.title = "Go to source";
  // Peek navigation (same rule as the list kicker): scroll + highlight the
  // source in the sidebar, but do NOT select it.
  source.addEventListener("click", () => {
    revealSubscription(item.subscription_id);
  });
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

  const useTranslationForItem = ts?.useTranslation ?? false;
  // Every caller (layout toggles, reselect, source changes, progress events)
  // must resolve the same live content, not just the completed item cache.
  const translatedContent = ts?.inProgressContent ?? item.translated_content;
  const tail = opts.untranslatedTail ?? (ts?.abortController && ts.inProgressContent
    ? untranslatedTail(item.content_md ?? item.description ?? "", ts.inProgressContent)
    : "");

  // Translation FIRST (issue #2): the webview branch below returns early, so
  // when it came first an active translation was never rendered in webview
  // mode — the user clicked Translate and nothing happened. With this order,
  // an active bilingual view always displays; toggling "Show Original"
  // returns to the media-blocked reader.
  // Parallel Text is offered only while a bilingual view is actually shown.
  setParallelAvailable(useTranslationForItem && !!translatedContent);
  if (useTranslationForItem && translatedContent) {
    const badge = document.createElement("span");
    badge.className = "translation-badge";
    badge.textContent = "Bilingual View";
    meta.appendChild(badge);
    // The translation content comes from the LLM — untrusted. Sanitise.
    const bilingualEl = document.createElement("div");
    bilingualEl.className = "bilingual-content";
    if (isParallelPreferred()) bilingualEl.classList.add("parallel");
    setSafeHtml(bilingualEl, translatedContent);
    // A Markdown link may span text + a real <img> + text. Processing those
    // text nodes separately loses the bracket pairing, so normalise the whole
    // serialised block. Using innerHTML preserves existing images and links;
    // the result is sanitised again before insertion.
    bilingualEl.querySelectorAll(".paragraph-original, .paragraph-translated").forEach((el) => {
      const raw = el.children.length ? el.innerHTML : el.textContent ?? "";
      if (raw.trim()) setSafeHtml(el, renderMarkdown(raw, item.link, item.author));
    });
    body.appendChild(bilingualEl);

    // Streaming: keep the untranslated remainder visible below the
    // finished paragraphs. Completed paragraphs substitute their originals
    // (the bilingual pairs above); the rest of the article is NOT removed.
    if (tail.trim()) {
      const tailEl = document.createElement("div");
      tailEl.className = "untranslated-tail";
      const html = renderMarkdown(tail, item.link, item.author);
      setSafeHtml(tailEl, html);
      body.appendChild(tailEl);
    }

    // Relative URLs inside the bilingual pairs resolve against the ARTICLE,
    // not the app origin, and standalone images become captioned figures.
    // Include the streaming tail so cross-section anchors see every target.
    normalizeMediaUrls(body, item.link, { title: item.title, author: item.author });

    // One image per URL across the whole bilingual view: the same figure
    // often appears in several translated pairs and again in the
    // untranslated tail — keep only the first occurrence. The hidden
    // translated side never claims a URL.
    dedupeImages(body, ".paragraph-translated");
    // Bionic reading (English fixation marks) — last DOM pass so sanitising
    // and re-rendering can't strip it; covers originals, translated side
    // and the streaming tail in one sweep.
    applyBionic(body);
    return;
  }

  // Isolated reader only: never navigate the original webpage inside the
  // app, since remote scripts and players can fetch media before onload.
  // Reuse the cached Markdown; the iframe sanitizer and CSP block players.
  if (showWebView && item.link) {
    detail.classList.add("webview-mode");
    const container = document.createElement("div");
    container.className = "webview-container";
    const loading = document.createElement("div");
    loading.className = "webview-loading";
    loading.textContent = "Loading reader (audio/video blocked)...";
    container.appendChild(loading);
    body.appendChild(container);

    iframeManager.load({
      container,
      markdown: item.content_md ?? htmlToPlainText(item.description ?? ""),
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
    const html = renderMarkdown(item.content_md, item.link, item.author);
    setSafeHtml(body, html);
    // Same media policy as the bilingual view: article-relative URLs, real
    // captions on standalone figures. Drop a recognisable site header only
    // when it duplicates the title/byline already displayed by the app.
    normalizeMediaUrls(body, item.link, { title: item.title, author: item.author });
    // The same image often appears twice in one article (hero + inline) —
    // render it once.
    dedupeImages(body);
    // Bionic reading — after sanitise + dedupe, on the whole body.
    applyBionic(body);
  } else if (item.description) {
    // Same untrusted-HTML handling as the list-item summary above.
    setText(body, htmlToPlainText(item.description));
    applyBionic(body);
  } else {
    setText(body, "No content available");
  }
}

// cancelIgnoreTimer lives with the selection logic in features/actions.ts.
import { cancelIgnoreTimer as cancelIgnoreTimerLocal } from "../features/actions";
// filterByTag lives in ui/filters.ts (which imports loadItems from here).
import { filterByTag as filterByTagLocal } from "./filters";
