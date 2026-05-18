import { invoke } from "@tauri-apps/api/core";
import { open, save, ask } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { marked, Renderer } from "marked";

// Configure marked for proper link and image rendering.
// Use tokens instead of raw text to properly handle nested elements
// (e.g., images inside links: [![alt](img.jpg)](url)).
const mdRenderer = new Renderer();
mdRenderer.link = function ({ href, title, tokens }) {
  const text = this.parser ? this.parser.parseInline(tokens) : "";
  return `<a target="_blank" rel="noopener noreferrer" href="${href}"${title ? ` title="${title}"` : ""}>${text}</a>`;
};
marked.use({ renderer: mdRenderer, gfm: true });

interface Subscription {
  id: number;
  url: string;
  title: string | null;
  website_url: string | null;
  rsshub_url: string | null;
  use_website: boolean;
  auto_classify: boolean;
  opml_attributes: string | null;
  created_at: string;
  updated_at: string;
}

interface FeedItem {
  id: number;
  subscription_id: number;
  guid: string | null;
  title: string;
  link: string | null;
  content: string | null;
  content_md: string | null;
  description: string | null;
  author: string | null;
  published_at: string | null;
  fetched_at: string;
  is_website_content: boolean;
  is_read: boolean;
  is_favorite: boolean;
  is_read_later: boolean;
  is_ignored: boolean;
  tags: string | null; // JSON array string
  category: string | null;
  translated_title: string | null;
  translated_content: string | null;
  translated_at: string | null; // Translation cache timestamp
}

interface AiClassificationResponse {
  tags: string[];
  category: string | null;
}

let subscriptions: Subscription[] = [];
let currentFilter: "all" | "unread" | "favorites" | "read-later" | "today" | "tag" = "all";
let currentTagFilter: string | null = null; // Current tag filter
let currentSubscriptionId: number | null = null;
let currentItems: FeedItem[] = [];
let selectedItem: FeedItem | null = null;
let unreadFilterEnabled: boolean = false; // Whether unread filter is additionally enabled
let useWebView = false;
// 按订阅源 ID 记住 webview 状态
let webviewPerSubscription: Map<number, boolean> = new Map();
// Track item selection time for ignored detection
let lastSelectedTime: number = 0;
let ignoreTimer: ReturnType<typeof setTimeout> | null = null;
// AI 设置 - 翻译状态按文章ID管理
let translationStateByItemId: Map<number, {
  useTranslation: boolean;
  inProgressContent: string | null;
  abortController: AbortController | null;
  hasError: boolean;
  errorMessage: string | null;
}> = new Map();

// ==================== 类型定义 ====================

interface IframeLoadOptions {
  iframe: HTMLIFrameElement;
  htmlContent: string;
  baseUrl: string;
  loadingElement?: HTMLElement;
  onComplete?: () => void;
  onError?: (error: string) => void;
}

// ==================== 常量定义 ====================

// 微信文章样式修复脚本
const WECHAT_FIX_SCRIPT = `
  (function() {
    // 修复 #js_content
    const jsContent = document.getElementById('js_content');
    if (jsContent) {
      jsContent.style.visibility = 'visible';
      jsContent.style.opacity = '1';
    }

    // 修复所有图片
    document.querySelectorAll('img').forEach(img => {
      img.style.visibility = 'visible';
      img.style.opacity = '1';
      img.style.width = 'auto';
      img.style.height = 'auto';
    });

    // 修复所有 section
    document.querySelectorAll('section').forEach(sec => {
      sec.style.visibility = 'visible';
      sec.style.opacity = '1';
    });

    // 修复所有 .rich_media_content
    document.querySelectorAll('.rich_media_content').forEach(el => {
      el.style.visibility = 'visible';
      el.style.opacity = '1';
    });

    console.log('Fix script applied');
  })();
`;

// 注入的 CSS 样式
const INJECTED_CSS = `
  <style>
    /* 强制显示微信文章内容 - 修复白屏问题 */
    #js_content {
      visibility: visible !important;
      opacity: 1 !important;
    }

    /* 强制显示所有图片 */
    .rich_pages.wxw-img,
    .wxw-img {
      visibility: visible !important;
      opacity: 1 !important;
      width: auto !important;
      min-width: 100px !important;
      max-width: 100% !important;
      height: auto !important;
    }

    * {
      box-sizing: border-box;
    }
    html, body {
      background-color: #ffffff !important;
      color: #333333 !important;
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif !important;
      font-size: 16px !important;
      line-height: 1.8 !important;
      padding: 16px !important;
      margin: 0 !important;
      word-wrap: break-word !important;
      overflow-wrap: break-word !important;
      visibility: visible !important;
    }
    p {
      margin: 12px 0 !important;
      line-height: 1.8 !important;
      visibility: visible !important;
    }
    a {
      color: #1a73e8 !important;
      text-decoration: underline !important;
    }
    a:visited {
      color: #1a0dab !important;
    }
    img {
      max-width: 100% !important;
      height: auto !important;
      display: block !important;
      margin: 10px 0 !important;
      visibility: visible !important;
      opacity: 1 !important;
    }
    pre, code {
      background: #f5f5f5 !important;
      padding: 2px 6px !important;
      border-radius: 3px !important;
      font-family: 'Consolas', 'Monaco', monospace !important;
      font-size: 14px !important;
      word-wrap: break-word !important;
      overflow-x: auto !important;
    }
    pre {
      padding: 12px !important;
      overflow-x: auto !important;
      white-space: pre-wrap !important;
    }
    h1, h2, h3, h4, h5, h6 {
      color: #111111 !important;
      font-weight: 600 !important;
      margin: 24px 0 12px !important;
      line-height: 1.4 !important;
    }
    h1 { font-size: 24px !important; }
    h2 { font-size: 20px !important; }
    h3 { font-size: 18px !important; }
    table {
      border-collapse: collapse !important;
      width: 100% !important;
      margin: 12px 0 !important;
    }
    th, td {
      border: 1px solid #dddddd !important;
      padding: 8px !important;
      text-align: left !important;
    }
    th {
      background-color: #f5f5f5 !important;
    }
    blockquote {
      border-left: 4px solid #dddddd !important;
      margin: 12px 0 !important;
      padding-left: 16px !important;
      color: #666666 !important;
      background: #f9f9f9 !important;
    }
    ul, ol {
      padding-left: 24px !important;
      margin: 12px 0 !important;
    }
    li {
      margin: 6px 0 !important;
    }
    .rich_media_content {
      color: #333333 !important;
      visibility: visible !important;
    }
    /* 微信文章特定样式 */
    section {
      margin: 12px 0 !important;
      visibility: visible !important;
    }
    /* 强制显示所有内容 */
    [style*="display:none"], [style*="display: none"] {
      display: block !important;
    }
    [hidden] {
      display: block !important;
      visibility: visible !important;
    }
  </style>
`;

// 状态管理
interface StatusState {
  loading: boolean;
  text: string;
  url: string;
  startTime: number | null;
  timerInterval: number | null;
  successCount: number;
  errorCount: number;
  current: number;
  total: number;
  errors: string[];
}

const statusState: StatusState = {
  loading: false,
  text: "Ready",
  url: "",
  startTime: null,
  timerInterval: null,
  successCount: 0,
  errorCount: 0,
  current: 0,
  total: 0,
  errors: [],
};

// 更新状态栏
function updateStatusBar() {
  const statusBar = document.getElementById("status-bar");
  const statusText = document.getElementById("status-text");
  const statusTimer = document.getElementById("status-timer");
  const statusProgress = document.getElementById("status-progress");
  const statusCount = document.getElementById("status-count");

  if (!statusBar || !statusText || !statusTimer || !statusProgress || !statusCount) return;

  statusBar.classList.remove("loading", "success", "error");

  if (statusState.loading) {
    statusBar.classList.add("loading");
    // 显示简短的标题，而不是完整URL
    if (statusState.text) {
      statusText.textContent = statusState.text.length > 30
        ? statusState.text.substring(0, 28) + "..."
        : statusState.text;
    } else {
      statusText.textContent = "Loading...";
    }
    if (statusState.startTime) {
      const elapsed = Math.floor((Date.now() - statusState.startTime) / 1000);
      statusTimer.textContent = `${elapsed}s`;
    } else {
      statusTimer.textContent = "";
    }
    if (statusState.total > 0) {
      statusProgress.textContent = `${statusState.current}/${statusState.total}`;
    } else {
      statusProgress.textContent = "";
    }
    // 简化显示：只显示总数和错误数
    const completed = statusState.successCount + statusState.errorCount;
    if (completed > 0) {
      if (statusState.errorCount > 0) {
        statusCount.textContent = `✗${statusState.errorCount}`;
        statusCount.classList.add("error");
      } else {
        statusCount.textContent = `✓${completed}`;
        statusCount.classList.remove("error");
      }
    } else {
      statusCount.textContent = "";
      statusCount.classList.remove("error");
    }
  } else if (statusState.errors.length > 0) {
    statusBar.classList.add("error");
    statusText.textContent = statusState.errors[0];
    statusTimer.textContent = "";
    statusProgress.textContent = "";
    statusCount.textContent = "";
    statusCount.classList.remove("error");
  } else {
    statusBar.classList.add("success");
    statusText.textContent = statusState.text || "Ready";
    statusTimer.textContent = "";
    statusProgress.textContent = "";
    statusCount.textContent = "";
    statusCount.classList.remove("error");
  }
}

// 设置加载状态
function setLoadingWithStatus(url: string, text: string) {
  statusState.loading = true;
  statusState.url = url;
  statusState.text = text;
  statusState.startTime = Date.now();
  if (statusState.timerInterval) clearInterval(statusState.timerInterval);
  statusState.timerInterval = window.setInterval(() => updateStatusBar(), 1000);
  updateStatusBar();
}

// 清除加载状态
function clearLoadingStatus(success: boolean, text: string) {
  statusState.loading = false;
  statusState.url = "";
  statusState.text = text;
  statusState.startTime = null;
  if (statusState.timerInterval) {
    clearInterval(statusState.timerInterval);
    statusState.timerInterval = null;
  }
  updateStatusBar();
  if (!success) {
    setTimeout(() => {
      statusState.errors = [];
      updateStatusBar();
    }, 5000);
  }
}

// 更新切换按钮状态
function updateToggleButtonStates() {
  const webviewBtn = document.getElementById("toggle-webview-btn") as HTMLButtonElement;

  if (webviewBtn) {
    webviewBtn.textContent = useWebView ? "Text" : "Web View";
  }
}

// 渲染订阅列表
function renderSubscriptions() {
  const list = document.getElementById("subscription-list");
  if (!list) return;

  list.innerHTML = "";

  // 添加 "All Items" 选项
  const allItem = document.createElement("div");
  // "All Items" is active when: no subscription selected AND filter is "all" (not unread, favorites, etc.)
  allItem.className = `subscription-item ${currentSubscriptionId === null && currentFilter === "all" ? "active" : ""}`;
  allItem.dataset.id = "all";
  allItem.innerHTML = `<span class="subscription-title">All Items</span>`;
  allItem.addEventListener("click", () => {
    currentSubscriptionId = null;
    currentFilter = "all";
    renderSubscriptions();
    loadItems();
  });
  list.appendChild(allItem);

  subscriptions.forEach((sub) => {
    const item = document.createElement("div");
    item.className = `subscription-item ${currentSubscriptionId === sub.id ? "active" : ""}`;
    item.dataset.id = sub.id.toString();
    item.innerHTML = `
      <div class="subscription-info">
        <span class="subscription-title">${sub.title || sub.url}</span>
        ${sub.use_website ? '<span class="badge website">Website</span>' : ""}
        ${!sub.auto_classify ? '<span class="badge no-auto">No Auto</span>' : ""}
        ${sub.rsshub_url ? '<span class="badge rsshub">RSSHub</span>' : ""}
      </div>
      <div class="subscription-actions">
        <button class="icon-btn toggle-auto-btn" data-id="${sub.id}" title="Toggle auto-classify" data-auto="${sub.auto_classify}">
          ${sub.auto_classify ? "AI" : "ai"}
        </button>
        <button class="icon-btn delete-sub" data-id="${sub.id}" title="Delete">×</button>
      </div>
    `;
    item.addEventListener("click", (e) => {
      if (!(e.target as HTMLElement).classList.contains("delete-sub") &&
          !(e.target as HTMLElement).classList.contains("toggle-auto-btn")) {
        selectSubscription(sub.id);
      }
    });

    const deleteBtn = item.querySelector(".delete-sub");
    if (deleteBtn) {
      deleteBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        deleteSubscription(sub.id);
      });
    }

    const toggleAutoBtn = item.querySelector(".toggle-auto-btn");
    if (toggleAutoBtn) {
      toggleAutoBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        toggleAutoClassify(sub.id);
      });
    }

    list.appendChild(item);
  });
}

// 选择订阅
function selectSubscription(id: number | null) {
  currentSubscriptionId = id;
  // When selecting a specific subscription, don't reset filter (allows unread + subscription)
  // When selecting "All Items" (id === null), keep current filter to allow "unread" for all subscriptions
  renderSubscriptions();
  loadItems();
}

// 加载内容
async function loadItems() {
  setLoadingWithStatus("", "Loading items...");
  try {
    let items: FeedItem[] = [];

    if (currentFilter === "unread") {
      items = await invoke<FeedItem[]>("get_unread", {
        subscriptionId: currentSubscriptionId,
        limit: 100,
        offset: 0,
      });
    } else if (currentFilter === "favorites") {
      items = await invoke<FeedItem[]>("get_favorites", { limit: 100, offset: 0 });
    } else if (currentFilter === "read-later") {
      items = await invoke<FeedItem[]>("get_read_later", { limit: 100, offset: 0 });
    } else if (currentFilter === "today") {
      items = await invoke<FeedItem[]>("get_today_items", {
        subscriptionId: currentSubscriptionId,
        unreadOnly: unreadFilterEnabled,
        limit: 100,
        offset: 0,
      });
    } else if (currentFilter === "tag" && currentTagFilter) {
      items = await invoke<FeedItem[]>("get_items_by_tag", {
        tag: currentTagFilter,
        subscriptionId: currentSubscriptionId,
        limit: 100,
        offset: 0,
      });
    } else {
      items = await invoke<FeedItem[]>("get_items", {
        subscriptionId: currentSubscriptionId,
        limit: 100,
        offset: 0,
      });
    }

    currentItems = items;
    renderItems();
    clearLoadingStatus(true, "Ready");
  } catch (error) {
    console.error("Failed to load items:", error);
    clearLoadingStatus(false, "Load failed");
    showError("Failed to load items");
  }
}

// 渲染内容列表
function renderItems(preserveScroll = false) {
  const list = document.getElementById("items-list");
  if (!list) return;

  // 保存滚动位置
  const scrollPos = preserveScroll ? list.scrollTop : 0;

  if (currentItems.length === 0) {
    list.innerHTML = `<div class="empty-state">No items found</div>`;
    return;
  }

  list.innerHTML = "";
  currentItems.forEach((item) => {
    const div = document.createElement("div");
    div.className = `item-card ${!item.is_read ? "unread" : ""} ${selectedItem?.id === item.id ? "active" : ""}`;

    // Parse tags
    let tagsHtml = "";
    if (item.tags) {
      try {
        const tags = JSON.parse(item.tags) as string[];
        if (tags.length > 0) {
          tagsHtml = `<div class="item-tags">${tags.map((tag: string) =>
            `<span class="tag clickable-tag" data-tag="${tag}">#${tag}</span>`
          ).join("")}</div>`;
        }
      } catch (e) {
        // Invalid JSON, ignore
      }
    }

    // 检查翻译状态
    const itemTranslationState = translationStateByItemId.get(item.id);
    const isTranslating = !!(itemTranslationState && itemTranslationState.abortController);
    const hasTranslationError = !!(itemTranslationState && itemTranslationState.hasError);
    const hasTranslation = item.translated_content !== null;
    const translationBadge = isTranslating
      ? '<span class="badge translating-badge">Translating...</span>'
      : hasTranslationError
        ? '<span class="badge translation-error-badge">Error</span>'
        : hasTranslation
          ? '<span class="badge translated-badge">Translated</span>'
          : '';

    div.innerHTML = `
      <div class="item-header">
        <h3 class="item-title">${item.title}</h3>
        ${item.published_at ? `<span class="item-date">${formatDate(item.published_at)}</span>` : ""}
      </div>
      ${item.description ? `<div class="item-description">${item.description}</div>` : ""}
      ${tagsHtml}
      <div class="item-meta">
        ${item.is_ignored ? '<span class="badge ignored-badge">Ignored</span>' : ""}
        ${item.is_favorite ? '<span class="badge">★ Favorite</span>' : ""}
        ${item.is_read_later ? '<span class="badge">Later</span>' : ""}
        ${translationBadge}
        ${item.author ? `<span class="item-author">${item.author}</span>` : ""}
      </div>
    `;
    div.addEventListener("click", (e) => {
      // If clicked on a tag, filter by tag instead of selecting item
      const target = e.target as HTMLElement;
      if (target.classList.contains("clickable-tag")) {
        const tag = target.getAttribute("data-tag");
        if (tag) filterByTag(tag);
        return;
      }
      selectItem(item);
    });
    list.appendChild(div);
  });

  // 恢复滚动位置
  if (preserveScroll) {
    list.scrollTop = scrollPos;
  }
}

// Filter items by tag
function filterByTag(tag: string) {
  currentFilter = "tag";
  currentTagFilter = tag;
  updateFilterTabs();
  loadItems();
}

// 选择内容
function selectItem(item: FeedItem) {
  selectedItem = item;

  // 根据订阅源设置 webview 状态和图片缩放状态
  const subId = item.subscription_id;

  // 优先使用用户手动切换的状态（Map中记录的）
  // 如果Map中没有记录，则使用订阅源的默认设置（use_website）
  if (webviewPerSubscription.has(subId)) {
    useWebView = webviewPerSubscription.get(subId)!;
  } else {
    // 查找订阅源配置，如果设置了 use_website=true，则默认使用 webview 模式
    const subscription = subscriptions.find(s => s.id === subId);
    useWebView = subscription?.use_website ?? false;
    // Persist to map so renderItemDetail reads the same value
    webviewPerSubscription.set(subId, useWebView);
  }

  // 更新按钮状态
  updateToggleButtonStates();

  renderItems(true); // 保存滚动位置
  renderItemDetail(item);

  // 标记为已读
  if (!item.is_read) {
    markAsRead(item.id, true);
  }

  // Set up ignore timer - if user reads for less than 1 second and takes no action, mark as ignored
  setupIgnoreTimer(item);
}

// Set up timer to detect if user quickly abandons the article
function setupIgnoreTimer(item: FeedItem) {
  // Clear any existing timer
  if (ignoreTimer !== null) {
    clearTimeout(ignoreTimer);
    ignoreTimer = null;
  }

  // Don't set up timer for already ignored items
  if (item.is_ignored) {
    return;
  }

  lastSelectedTime = Date.now();

  ignoreTimer = setTimeout(async () => {
    // Only mark as ignored if:
    // 1. The same item is still selected
    // 2. The item has not been marked as read (is_read should be true by now if user actually read it)
    // 3. Less than 1 second elapsed between selection and timer fire
    if (selectedItem?.id === item.id && !item.is_ignored) {
      const elapsed = Date.now() - lastSelectedTime;
      if (elapsed < 1000) {
        try {
          await invoke<boolean>("toggle_ignored", { itemId: item.id });
          item.is_ignored = true;
          renderItems(true);
          console.log(`[Ignore] Article "${item.title}" marked as ignored (read for ${elapsed}ms)`);
        } catch (error) {
          console.error('[Ignore] Failed to toggle ignored:', error);
        }
      }
    }
    ignoreTimer = null;
  }, 1000);
}

// Cancel ignore timer when user takes an action (scroll, translate, etc.)
function cancelIgnoreTimer() {
  if (ignoreTimer !== null) {
    clearTimeout(ignoreTimer);
    ignoreTimer = null;
    console.log('[Ignore] Timer cancelled due to user action');
  }
}

// Check if text contains markdown syntax that marked can render.
function containsMarkdown(text: string): boolean {
  return /(\*\*|__|^#{1,6}\s|\[.+\]\(.+\)|!\[.+\]\(.+\)|`{1,3})/m.test(text);
}

// 显示内容详情
function renderItemDetail(item: FeedItem) {
  // Cancel ignore timer since user is now viewing the article (engagement detected)
  cancelIgnoreTimer();

  const detail = document.getElementById("detail-content");
  if (!detail) return;

  // 获取订阅源名称
  const subscription = subscriptions.find(s => s.id === item.subscription_id);
  const subName = subscription?.title || subscription?.url || "Unknown";
  // 从该文章的订阅源获取 webview 状态
  const useWebViewForItem = webviewPerSubscription.get(item.subscription_id) ?? false;

  // 更新操作按钮状态
  const markReadBtn = document.getElementById("mark-read-btn");
  const favoriteBtn = document.getElementById("favorite-btn");
  const readLaterBtn = document.getElementById("read-later-btn");
  const openLinkBtn = document.getElementById("open-link-btn") as HTMLAnchorElement;
  const translateBtn = document.getElementById("translate-btn");

  if (markReadBtn) {
    markReadBtn.textContent = item.is_read ? "Unread" : "Read";
    markReadBtn.classList.toggle("active", item.is_read);
  }
  if (favoriteBtn) {
    favoriteBtn.classList.toggle("active", item.is_favorite);
  }
  if (readLaterBtn) {
    readLaterBtn.classList.toggle("active", item.is_read_later);
  }
  if (openLinkBtn && item.link) {
    openLinkBtn.href = item.link;
  }

  // 更新翻译按钮状态
  if (translateBtn) {
    const translationState = translationStateByItemId.get(item.id);
    const isTranslating = translationState?.abortController != null;
    const hasError = translationState?.hasError ?? false;
    const hasCache = item.translated_content !== null;

    // 移除所有状态类
    translateBtn.classList.remove("translating", "has-cache", "has-error");

    if (isTranslating) {
      // 正在翻译 - 高亮显示，点击可取消
      translateBtn.classList.add("translating");
      translateBtn.textContent = "Cancel";
    } else if (hasError) {
      // 翻译失败 - 红色显示，点击重试
      translateBtn.classList.add("has-error");
      translateBtn.textContent = "Retry";
      translateBtn.title = translationState?.errorMessage || "Translation failed";
    } else if (hasCache) {
      // 有缓存 - 黄色显示，点击切换
      translateBtn.classList.add("has-cache");
      const useTranslationForItem = translationState?.useTranslation ?? false;
      translateBtn.textContent = useTranslationForItem ? "Show Original" : "Translate";
      translateBtn.title = "";
    } else {
      // 无缓存 - 暗色，点击开始翻译
      translateBtn.textContent = "Translate";
      translateBtn.title = "";
    }
  }

  // 显示内容
  // 从该文章的翻译状态中获取是否使用翻译
  const translationState = translationStateByItemId.get(item.id);
  const useTranslationForItem = translationState?.useTranslation ?? false;

  // 如果启用了翻译且有翻译内容，则显示翻译内容（无论 webview 还是 text 模式）
  if (useTranslationForItem && item.translated_content) {
    detail.classList.remove('webview-mode');
    // 解析标签
    let tagsHtml = "";
    if (item.tags) {
      try {
        const tags = JSON.parse(item.tags);
        if (Array.isArray(tags) && tags.length > 0) {
          tagsHtml = `<div class="detail-tags">${tags.map((tag: string) => `<span class="tag">#${tag}</span>`).join(" ")}</div>`;
        }
      } catch (e) {
        // Ignore parse errors
      }
    }

    detail.innerHTML = `
      <div class="detail-source">${subName}${item.category ? ` • ${item.category}` : ""}</div>
      <h1>${item.title}</h1>
      ${tagsHtml}
      <div class="detail-meta">
        ${item.published_at ? `<span>${formatDate(item.published_at)}</span>` : ""}
        ${item.author ? `<span>${item.author}</span>` : ""}
        ${item.link ? `<a href="${item.link}" target="_blank">Open in browser →</a>` : ""}
        <span class="translation-badge">Bilingual View</span>
      </div>
      <div class="detail-body">
        ${item.translated_content}
      </div>
    `;

    // AI outputs markdown syntax as plain text in both original and
    // translated paragraphs — render it so links/formatting are visible.
    // Only parse when markdown patterns are detected (avoids double-processing
    // content that's already HTML).
    const detailBody = detail.querySelector('.detail-body');
    if (detailBody) {
      detailBody.querySelectorAll('.paragraph-original, .paragraph-translated').forEach(para => {
        if (containsMarkdown(para.innerHTML)) {
          try {
            para.innerHTML = marked.parse(para.innerHTML) as string;
          } catch (_e) {
            // Leave as-is if parsing fails
          }
        }
      });
    }
  } else if (useWebViewForItem && item.link) {
    try {
      // 创建 webview 容器
      const { iframe, loading } = createWebviewContainer(detail);

      // 从后端获取网页内容并加载到 iframe
      invoke<string>("fetch_website_content", { url: item.link, itemId: item.id })
        .then((htmlContent) => {
          loadIframeContent({
            iframe,
            htmlContent,
            baseUrl: item.link || '',
            loadingElement: loading,
            onError: (error) => {
              console.error('[WebView] Error:', error);
              loading.textContent = 'Failed to load: ' + error;
            }
          });
        })
        .catch((error) => {
          console.error('[WebView] Error fetching content:', error);
          loading.textContent = 'Failed to load: ' + error;
        });
    } catch (error) {
      console.error('[WebView] Error creating webview:', error);
      detail.textContent = 'Failed to create webview: ' + error;
    }
  } else {
    detail.classList.remove('webview-mode');
    // 解析标签
    let tagsHtml = "";
    if (item.tags) {
      try {
        const tags = JSON.parse(item.tags);
        if (Array.isArray(tags) && tags.length > 0) {
          tagsHtml = `<div class="detail-tags">${tags.map((tag: string) => `<span class="tag">#${tag}</span>`).join(" ")}</div>`;
        }
      } catch (e) {
        // Ignore parse errors
      }
    }

    // 显示文本内容（支持翻译）
    // Prefer markdown content (content_md), fall back to raw HTML or description
    const originalContent = item.content_md
      ? marked.parse(item.content_md)
      : (item.content || item.description || "No content available");

    detail.innerHTML = `
      <div class="detail-source">${subName}${item.category ? ` • ${item.category}` : ""}</div>
      <h1>${item.title}</h1>
      ${tagsHtml}
      <div class="detail-meta">
        ${item.published_at ? `<span>${formatDate(item.published_at)}</span>` : ""}
        ${item.author ? `<span>${item.author}</span>` : ""}
        ${item.link ? `<a href="${item.link}" target="_blank">Open in browser →</a>` : ""}
        ${useTranslationForItem ? `<span class="translation-badge">Bilingual View</span>` : ""}
      </div>
      <div class="detail-body">
        ${useTranslationForItem && item.translated_content ? item.translated_content : originalContent}
      </div>
    `;
  }
}

// 格式化日期
function formatDate(dateString: string): string {
  const date = new Date(dateString);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMs / 3600000);
  const diffDays = Math.floor(diffMs / 86400000);

  if (diffMins < 60) return `${diffMins}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;
  if (diffDays < 7) return `${diffDays}d ago`;
  return date.toLocaleDateString();
}

// ==================== HTML 处理辅助函数 ====================

/**
 * 加载内容到 iframe
 * @param options - iframe 加载选项
 */
function loadIframeContent(options: IframeLoadOptions): void {
  const { iframe, htmlContent, baseUrl, loadingElement, onComplete, onError } = options;

  console.log('[Iframe] Loading content, length:', htmlContent.length);

  const fixedHtml = fixHtmlContent(htmlContent, baseUrl);
  console.log('[Iframe] Fixed HTML length:', fixedHtml.length);

  // 使用 about:blank + document.write 方法
  iframe.src = 'about:blank';

  iframe.onload = () => {
    console.log('[Iframe] iframe loaded');

    try {
      const iframeDoc = iframe.contentDocument || iframe.contentWindow?.document;
      if (iframeDoc) {
        iframeDoc.open();
        iframeDoc.write(fixedHtml);
        iframeDoc.close();
        console.log('[Iframe] Content written to iframe');

        // 添加修复脚本
        const fixScript = iframeDoc.createElement('script');
        fixScript.textContent = WECHAT_FIX_SCRIPT;
        iframeDoc.head.appendChild(fixScript);

        // 隐藏加载提示
        if (loadingElement) {
          loadingElement.style.display = 'none';
        }

        // 验证内容
        setTimeout(() => {
          const bodyLength = iframeDoc.body?.innerHTML?.length || 0;
          if (bodyLength === 0) {
            console.warn('[Iframe] Warning: iframe body is empty');
            onError?.('Content appears to be empty');
          } else {
            console.log('[Iframe] ✓ Success: Content loaded');
          }
          onComplete?.();
        }, 100);
      } else {
        const error = 'Cannot access iframe document';
        console.error('[Iframe]', error);
        onError?.(error);
        if (loadingElement) {
          loadingElement.textContent = 'Failed: Cannot access iframe';
        }
      }
    } catch (e) {
      const error = `Error writing to iframe: ${e}`;
      console.error('[Iframe]', error);
      onError?.(error);
      if (loadingElement) {
        loadingElement.textContent = 'Failed: ' + e;
      }
    }
  };

  // 设置超时保护
  setTimeout(() => {
    if (loadingElement && loadingElement.style.display !== 'none') {
      console.log('[Iframe] Timeout - hiding loading indicator');
      loadingElement.style.display = 'none';
    }
  }, 3000);
}

/**
 * 创建 webview 容器
 * @param container - 容器元素
 * @returns iframe 元素和加载元素
 */
function createWebviewContainer(container: HTMLElement): { iframe: HTMLIFrameElement; loading: HTMLElement } {
  container.classList.add('webview-mode');
  container.innerHTML = `
    <div class="webview-container">
      <div class="webview-loading">Loading webpage...</div>
      <iframe id="content-iframe" name="content-iframe" style="width: 100%; height: 100%; border: none;"></iframe>
    </div>
  `;

  const iframe = container.querySelector('#content-iframe') as HTMLIFrameElement;
  const loading = container.querySelector('.webview-loading') as HTMLElement;

  if (!iframe) {
    throw new Error('Failed to create iframe element');
  }

  return { iframe, loading };
}

// ==================== HTML 内容修复 ====================
function fixHtmlContent(html: string, baseUrl: string): string {
  // 提取 base URL
  let base = '';
  try {
    const urlObj = new URL(baseUrl);
    base = urlObj.origin;
  } catch (e) {
    // 如果 URL 无效，使用空字符串
  }

  let fixed = html;

  // 修复微信文章的隐藏样式问题 - 在处理前先替换掉隐藏样式
  fixed = fixed
    // 修复 #js_content 的 visibility 和 opacity
    .replace(/id=["']js_content["'][^>]*style=["'][^"']*visibility:\s*hidden[^"']*["']/gi, 'id="js_content" style="visibility: visible; opacity: 1;"')
    .replace(/id=["']js_content["'][^>]*style=["'][^"']*opacity:\s*0[^"']*["']/gi, 'id="js_content" style="visibility: visible; opacity: 1;"')
    // 修复 img 标签的 width: 0px
    .replace(/(<img[^>]*style=["'][^"']*width:\s*)0px([^"']*["'])/gi, '$1auto$2')
    .replace(/(<img[^>]*style=["'][^"']*width:\s*)31\.5px([^"']*["'])/gi, '$1auto$2')
    // 修复图片的 visibility: hidden
    .replace(/(<img[^>]*style=["'][^"']*visibility:\s*)hidden([^"']*["'])/gi, '$1visible$2');

  // 移除 script, style, noscript 标签及其内容
  fixed = fixed
    // 移除 <script>...</script>
    .replace(/<script\b[^<]*(?:(?!<\/script>)<[^<]*)*<\/script>/gi, '')
    // 移除 <style>...</style>
    .replace(/<style\b[^<]*(?:(?!<\/style>)<[^<]*)*<\/style>/gi, '')
    // 移除 <noscript>...</noscript>
    .replace(/<noscript\b[^<]*(?:(?!<\/noscript>)<[^<]*)*<\/noscript>/gi, '')
    // 移除独立的 </script> 和 </style> 标签（可能残留）
    .replace(/<\/script>/gi, '')
    .replace(/<\/style>/gi, '')
    .replace(/<\/noscript>/gi, '');
  
  // 修复相对链接（支持更多协议和格式）
  // 修复 <img src="/...">
  fixed = fixed.replace(/(<img[^>]*src=["'])\/([^"']+)(["'])/gi, `$1${base}/$2$3`);
  // 修复 <a href="/...">
  fixed = fixed.replace(/(<a[^>]*href=["'])\/([^"']+)(["'])/gi, `$1${base}/$2$3`);
  // 修复 <link href="/...">
  fixed = fixed.replace(/(<link[^>]*href=["'])\/([^"']+)(["'])/gi, `$1${base}/$2$3`);
  // 修复 srcset 属性
  fixed = fixed.replace(/(<img[^>]*srcset=["'])\/([^"']+)(["'])/gi, `$1${base}/$2$3`);
  // 修复 data-src 属性（懒加载）
  fixed = fixed.replace(/(<img[^>]*data-src=["'])\/([^"']+)(["'])/gi, `$1${base}/$2$3`);

  // 创建一个完整的 HTML 文档
  const fullHtml = `<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  ${INJECTED_CSS}
</head>
<body>
${fixed}
</body>
</html>`;

  return fullHtml;
}

// 标记已读/未读
async function markAsRead(itemId: number, isRead: boolean) {
  try {
    await invoke("mark_item_read", { itemId, isRead });
    // 更新本地状态和单个 DOM 元素
    const item = currentItems.find(i => i.id === itemId);
    if (item) {
      item.is_read = isRead;

      // 只更新单个卡片的状态，不重新渲染整个列表
      const list = document.getElementById("items-list");
      if (list) {
        const cards = list.querySelectorAll(".item-card");
        cards.forEach((card, index) => {
          if (currentItems[index]?.id === itemId) {
            if (isRead) {
              card.classList.remove("unread");
            } else {
              card.classList.add("unread");
            }
          }
        });
      }

      if (selectedItem?.id === itemId) {
        selectedItem.is_read = isRead;
        const markReadBtn = document.getElementById("mark-read-btn");
        if (markReadBtn) {
          markReadBtn.textContent = isRead ? "Unread" : "Read";
          markReadBtn.classList.toggle("active", isRead);
        }
      }
    }
  } catch (error) {
    console.error("Failed to mark as read:", error);
  }
}

// 切换收藏
async function toggleFavorite(itemId: number) {
  try {
    const isFavorite = await invoke<boolean>("toggle_favorite", { itemId });
    // 更新本地状态
    const item = currentItems.find(i => i.id === itemId);
    if (item) {
      item.is_favorite = isFavorite;
      renderItems();
      if (selectedItem?.id === itemId) {
        selectedItem.is_favorite = isFavorite;
        const favoriteBtn = document.getElementById("favorite-btn");
        if (favoriteBtn) {
          favoriteBtn.classList.toggle("active", isFavorite);
        }
      }
      showSuccess(isFavorite ? "Added to favorites" : "Removed from favorites");
    }
  } catch (error) {
    console.error("Failed to toggle favorite:", error);
    showError("Failed to toggle favorite");
  }
}

// 切换稍后读
async function toggleReadLater(itemId: number) {
  try {
    const isReadLater = await invoke<boolean>("toggle_read_later", { itemId });
    // 更新本地状态
    const item = currentItems.find(i => i.id === itemId);
    if (item) {
      item.is_read_later = isReadLater;
      renderItems();
      if (selectedItem?.id === itemId) {
        selectedItem.is_read_later = isReadLater;
        const readLaterBtn = document.getElementById("read-later-btn");
        if (readLaterBtn) {
          readLaterBtn.classList.toggle("active", isReadLater);
        }
      }
      showSuccess(isReadLater ? "Added to Read Later" : "Removed from Read Later");
    }
  } catch (error) {
    console.error("Failed to toggle read later:", error);
    showError("Failed to toggle read later");
  }
}

// 批量标记已读
async function markAllAsRead() {
  try {
    await invoke("mark_all_read", { subscriptionId: currentSubscriptionId });
    currentItems.forEach(item => item.is_read = true);
    renderItems();
    showSuccess("All items marked as read");
  } catch (error) {
    console.error("Failed to mark all as read:", error);
    showError("Failed to mark all as read");
  }
}

// 添加订阅
async function addSubscription(data: {
  url: string;
  title?: string;
  website_url?: string;
  rsshub_url?: string;
  use_website?: boolean;
}) {
  setLoadingWithStatus(data.url, "Adding subscription...");
  try {
    await invoke("add_subscription", data);
    await loadSubscriptions();
    closeAddFeedModal();
    clearLoadingStatus(true, "Subscription added");
    showSuccess("Subscription added successfully");
  } catch (error) {
    console.error("Failed to add subscription:", error);
    clearLoadingStatus(false, "Add failed");
    showError("Failed to add subscription");
  }
}

// 删除订阅
async function deleteSubscription(id: number) {
  // 使用 Tauri 的原生对话框
  const confirmed = await ask("Are you sure you want to delete this subscription?", {
    title: "Confirm Delete",
    kind: "warning"
  });

  if (!confirmed) return;

  setLoadingWithStatus("", "Deleting subscription...");
  try {
    await invoke("remove_subscription", { id });
    await loadSubscriptions();
    if (currentSubscriptionId === id) {
      selectSubscription(null);
    }
    clearLoadingStatus(true, "Subscription deleted");
    showSuccess("Subscription deleted");
  } catch (error) {
    console.error("Failed to delete subscription:", error);
    clearLoadingStatus(false, "Delete failed");
    showError("Failed to delete subscription");
  }
}

// Toggle auto-classify for subscription
async function toggleAutoClassify(id: number) {
  try {
    const updated = await invoke<Subscription>("toggle_auto_classify", { id });
    // Update local subscription list
    const index = subscriptions.findIndex(s => s.id === id);
    if (index !== -1) {
      subscriptions[index] = updated;
    }
    renderSubscriptions();
    showSuccess(updated.auto_classify ? "Auto-classify enabled" : "Auto-classify disabled");
  } catch (error) {
    console.error("Failed to toggle auto-classify:", error);
    showError("Failed to toggle auto-classify");
  }
}

// 加载订阅列表
async function loadSubscriptions() {
  setLoadingWithStatus("", "Loading subscriptions...");
  try {
    subscriptions = await invoke<Subscription[]>("list_subscriptions");
    renderSubscriptions();
    clearLoadingStatus(true, "Ready");
  } catch (error) {
    console.error("Failed to load subscriptions:", error);
    clearLoadingStatus(false, "Load failed");
    showError("Failed to load subscriptions");
  }
}

// 刷新所有订阅
async function refreshAllFeeds() {
  resetCounts();
  setLoadingWithStatus("", "Starting refresh...");

  const unlistenProgress = await listen<FetchProgress>("fetch-progress", (event) => {
    const { current, total, status } = event.payload;
    statusState.current = current;
    statusState.total = total;
    if (status === "completed") {
      statusState.url = "";
      statusState.text = `Completed ${current}/${total}`;
    }
    updateStatusBar();
  });

  const unlistenSuccess = await listen<FetchSuccess>("fetch-success", () => {
    incrementSuccess();
  });

  const unlistenError = await listen<FetchError>("fetch-error", (event) => {
    const { title, error } = event.payload;
    incrementError(`${title}: ${error}`);
  });

  try {
    const result = await invoke<string>("fetch_all_feeds");
    unlistenProgress();
    unlistenSuccess();
    unlistenError();

    const match = result.match(/Fetched (\d+) new items/);
    if (match) {
      const count = parseInt(match[1], 10);
      statusState.successCount = count;
    }

    clearLoadingStatus(true, `Refresh complete: ${result}`);
    await loadItems();
  } catch (error) {
    console.error("Failed to refresh feeds:", error);
    incrementError(`Failed to refresh`);
    clearLoadingStatus(false, "Refresh failed");
    unlistenProgress();
    unlistenSuccess();
    unlistenError();
    showError("Failed to refresh feeds");
  }
}

// 搜索
async function searchItems(query: string) {
  if (!query.trim()) {
    loadItems();
    return;
  }

  setLoadingWithStatus("", `Searching: "${query}"`);
  try {
    const items = await invoke<FeedItem[]>("search_items", { query, limit: 100 });
    currentItems = items;
    renderItems();
    clearLoadingStatus(true, `Found ${items.length} items`);
  } catch (error) {
    console.error("Failed to search items:", error);
    clearLoadingStatus(false, "Search failed");
    showError("Failed to search items");
  }
}

// 导入 OPML
async function importOpml() {
  try {
    const selected = await open({
      multiple: false,
      filters: [{ name: "OPML", extensions: ["opml", "xml"] }],
    });

    if (!selected) return;

    const filePath = typeof selected === "string" ? selected : (selected as any).path;
    setLoadingWithStatus(filePath, "Importing OPML...");
    const result = (await invoke("import_opml", { filePath })) as {
      imported: number;
      skipped: number;
      errors?: string[];
    };

    clearLoadingStatus(true, "Import complete");
    showSuccess(`Imported ${result.imported} subscriptions. Skipped ${result.skipped}.`);
    if (result.errors && result.errors.length > 0) {
      console.warn("Import errors:", result.errors);
    }
    await loadSubscriptions();
  } catch (error) {
    console.error("Failed to import OPML:", error);
    clearLoadingStatus(false, "Import failed");
    showError(`Failed to import OPML: ${error}`);
  }
}

// 导出 OPML
async function exportOpml() {
  try {
    const filePath = await save({
      defaultPath: "subscriptions.opml",
      filters: [{ name: "OPML", extensions: ["opml"] }],
    });

    if (!filePath) return;

    setLoadingWithStatus(filePath, "Exporting OPML...");
    await invoke("export_opml", { filePath });
    clearLoadingStatus(true, "Export complete");
    showSuccess("OPML exported successfully");
  } catch (error) {
    console.error("Failed to export OPML:", error);
    clearLoadingStatus(false, "Export failed");
    showError(`Failed to export OPML: ${error}`);
  }
}

// Modal functions
function openAddFeedModal() {
  const modal = document.getElementById("add-feed-modal");
  if (modal) modal.classList.add("visible");
}

function closeAddFeedModal() {
  const modal = document.getElementById("add-feed-modal");
  if (modal) {
    modal.classList.remove("visible");
    const form = document.getElementById("add-feed-form") as HTMLFormElement;
    if (form) form.reset();
  }
}

// AI Settings Modal
async function openAiSettingsModal() {
  const modal = document.getElementById("ai-settings-modal");
  if (modal) modal.classList.add("visible");

  // Load current AI config and fill the form
  try {
    const config = await invoke<{ api_key: string; base_url: string; model: string; max_chars_per_segment: number | null }>("get_ai_config");
    (document.getElementById("ai-api-key") as HTMLInputElement).value = config.api_key || "";
    (document.getElementById("ai-base-url") as HTMLInputElement).value = config.base_url || "";
    (document.getElementById("ai-model") as HTMLInputElement).value = config.model || "";
    (document.getElementById("ai-max-chars") as HTMLInputElement).value = config.max_chars_per_segment?.toString() || "3000";
  } catch (error) {
    // If no config exists, just leave the fields empty or with defaults
    console.log("No AI config found, using defaults");
  }
}

function closeAiSettingsModal() {
  const modal = document.getElementById("ai-settings-modal");
  if (modal) {
    modal.classList.remove("visible");
  }
}

// AI Functions
async function translateItem(item: FeedItem, htmlContent?: string) {
  // 获取或创建该文章的翻译状态
  let translationState = translationStateByItemId.get(item.id);

  // 如果正在翻译同一篇文章，取消翻译
  if (translationState?.abortController) {
    translationState.abortController.abort();
    translationStateByItemId.delete(item.id);
    renderItems(true); // Update badge
    renderItemDetail(item);
    showSuccess("Translation cancelled");
    return;
  }

  // 如果之前翻译失败，清除错误状态，允许重试
  if (translationState?.hasError) {
    translationStateByItemId.delete(item.id);
    translationState = undefined;
  }

  // 检查是否已经有翻译内容（缓存）
  if (item.translated_content) {
    // 直接使用已有的翻译
    translationState = {
      useTranslation: true,
      inProgressContent: null,
      abortController: null,
      hasError: false,
      errorMessage: null
    };
    translationStateByItemId.set(item.id, translationState);
    renderItems(true); // Update badge
    renderItemDetail(item);
    showSuccess("Using cached translation");
    return;
  }

  // 创建新的 AbortController 和翻译状态
  const abortController = new AbortController();
  translationState = {
    useTranslation: false, // Don't show translation until complete
    inProgressContent: null,
    abortController,
    hasError: false,
    errorMessage: null
  };
  translationStateByItemId.set(item.id, translationState);

  // 立即更新按钮和徽章状态
  renderItems(true); // Show translating badge
  renderItemDetail(item);

  try {
    showSuccess("Translating...");

    let unlistenProgress: (() => void) | null = null;
    let unlistenError: (() => void) | null = null;

    try {
      // Listen for translation error events
      unlistenError = await listen<{ item_id: number; error: string; paragraph_index: number }>(
        "translation-error",
        (event) => {
          if (event.payload.item_id !== item.id) return;
          const currentState = translationStateByItemId.get(item.id);
          if (currentState) {
            currentState.hasError = true;
            currentState.errorMessage = event.payload.error;
          }
          showError(`Translation error: ${event.payload.error}`);
        }
      );

      // Listen for translation progress events
      unlistenProgress = await listen<{ item_id: number; total: number; completed: number; html_chunk: string; is_complete: boolean; cached?: boolean; has_error?: boolean; error_messages?: string[]; partial_content?: string }>(
        "translation-progress",
        (event) => {
          const { item_id, completed, total, html_chunk, is_complete, cached, has_error, error_messages } = event.payload;

          // 确保事件属于正确的文章
          if (item_id !== item.id) {
            return;
          }

          // 检查是否已取消
          if (abortController.signal.aborted) {
            return;
          }

          // 获取最新的翻译状态
          const currentState = translationStateByItemId.get(item.id);
          if (!currentState) return;

          // 如果是缓存命中，直接设置完整内容
          if (cached && html_chunk) {
            item.translated_content = html_chunk;
            currentState.useTranslation = true;
            currentState.inProgressContent = null;
            currentState.abortController = null;
            currentState.hasError = false;
            // 只有当前选中的文章才渲染和显示提示
            if (selectedItem?.id === item.id) {
              renderItems(true);
              renderItemDetail(item);
              showSuccess("Using cached translation");
            }
            return;
          }

          // Update progress indicator - 只在当前选中时显示
          if (!cached && selectedItem?.id === item.id && !currentState.hasError) {
            showSuccess(`Translating... ${completed}/${total}`);
          }

          // Append the chunk to state storage (not to item yet)
          if (is_complete) {
            // Final event - clean up state
            currentState.abortController = null;

            // Handle error case
            if (has_error) {
              currentState.hasError = true;
              currentState.errorMessage = error_messages?.[0] || "Translation failed";
              // Use the partial content from html_chunk if available
              if (html_chunk) {
                currentState.inProgressContent = html_chunk;
                item.translated_content = html_chunk;
              } else {
                currentState.inProgressContent = null;
              }
              currentState.useTranslation = false;

              // 更新列表徽章
              renderItems(true);

              // 只有当前选中的文章才渲染
              if (selectedItem?.id === item.id) {
                renderItemDetail(item);
                if (html_chunk) {
                  showError(`Translation partially complete: ${currentState.errorMessage}`);
                } else {
                  showError(`Translation failed: ${currentState.errorMessage}. Check ~/.rss-reader/ai_errors.log for details.`);
                }
              }
              return;
            }

            // Success case
            if (currentState.inProgressContent && !currentState.inProgressContent.endsWith("</div>")) {
              currentState.inProgressContent += "</div>";
            }

            // 缓存翻译结果到 item
            item.translated_content = currentState.inProgressContent;
            currentState.useTranslation = true;
            currentState.inProgressContent = null;
            currentState.hasError = false;

            // 更新列表徽章
            renderItems(true);

            // 只有当前选中的文章才渲染
            if (selectedItem?.id === item.id) {
              renderItemDetail(item);
              showSuccess("Translation complete");
            }
          } else if (html_chunk) {
            // Append this paragraph chunk to state storage
            // Normalize the chunk - ensure it's properly wrapped and closed
            let normalizedChunk = html_chunk;
            if (!normalizedChunk.endsWith("</div>")) {
              normalizedChunk += "</div>";
            }
            if (!currentState.inProgressContent) {
              currentState.inProgressContent = `<div class="bilingual-content">\n${normalizedChunk}`;
            } else {
              // Ensure previous content is properly closed before adding new chunk
              if (!currentState.inProgressContent.endsWith("</div>")) {
                currentState.inProgressContent += "</div>";
              }
              currentState.inProgressContent += `\n${normalizedChunk}`;
            }
            // 只有当前选中的文章才渲染
            if (selectedItem?.id === item.id) {
              renderItemDetail({ ...item, translated_content: currentState.inProgressContent });
            }
          }
        }
      );

      // 如果提供了 htmlContent（从 webview iframe 获取），使用新的翻译命令
      // 否则使用原来的翻译命令（翻译 RSS 内容）
      if (htmlContent) {
        await invoke<string>("translate_html_content_streaming", {
          itemId: item.id,
          content: htmlContent,
        });
      } else {
        // 使用流式双语对照翻译
        await invoke<string>("translate_item_bilingual_streaming", {
          itemId: item.id,
        });
      }
    } finally {
      // Always clean up listeners
      if (unlistenProgress) unlistenProgress();
      if (unlistenError) unlistenError();
      // Clean up translation state on error (keep cached success entries)
      const currentState = translationStateByItemId.get(item.id);
      if (currentState) {
        if (abortController.signal.aborted) {
          // User cancelled - clean up
          translationStateByItemId.delete(item.id);
        } else if (currentState.hasError) {
          // Error - keep entry so user sees the error badge, but clear bulky content
          currentState.inProgressContent = null;
        }
      }
    }
  } catch (error) {
    const currentState = translationStateByItemId.get(item.id);
    if (abortController.signal.aborted) {
      // User cancelled
      if (currentState) {
        currentState.abortController = null;
        currentState.hasError = false;
        // Clean up any open divs from partial content
        if (currentState.inProgressContent && !currentState.inProgressContent.endsWith("</div>")) {
          currentState.inProgressContent += "</div>";
        }
      }
      showSuccess("Translation cancelled");
    } else {
      // Error occurred
      if (currentState) {
        currentState.hasError = true;
        currentState.errorMessage = String(error);
        currentState.abortController = null;
        // Clean up any open divs from partial content
        if (currentState.inProgressContent && !currentState.inProgressContent.endsWith("</div>")) {
          currentState.inProgressContent += "</div>";
        }
      }
      showError(`Translation failed: ${error}`);
      renderItems(true); // Update badge to show error
      renderItemDetail(item); // Update button to show error state
    }
  }
}

async function classifyItem(item: FeedItem) {
  try {
    const contentSnippet = item.content ? item.content.slice(0, 500) : null;

    showSuccess("Classifying...");
    const result = await invoke<AiClassificationResponse>("classify_item", {
      title: item.title,
      description: item.description,
      contentSnippet,
    });

    // Save tags to database
    await invoke("save_item_tags", {
      itemId: item.id,
      tags: result.tags,
      category: result.category,
    });

    // Update local state
    item.tags = JSON.stringify(result.tags);
    item.category = result.category || null;

    // Re-render detail and list items to show updated tags
    renderItemDetail(item);
    renderItems(true); // Preserve scroll position
    showSuccess(`Classified: ${result.tags.join(", ")}`);
  } catch (error) {
    showError(`Classification failed: ${error}`);
  }
}

// Toast notifications
function showSuccess(message: string) {
  showToast(message, "success");
}

function showError(message: string) {
  showToast(message, "error");
}

function showToast(message: string, type: "success" | "error") {
  const toast = document.createElement("div");
  toast.className = `toast ${type}`;
  toast.textContent = message;
  document.body.appendChild(toast);
  window.setTimeout(() => toast.remove(), 3000);
}

// Reset counts
function resetCounts() {
  statusState.successCount = 0;
  statusState.errorCount = 0;
  statusState.current = 0;
  statusState.total = 0;
  statusState.errors = [];
  updateStatusBar();
}

function incrementSuccess() {
  statusState.successCount++;
  updateStatusBar();
}

function incrementError(error: string) {
  statusState.errorCount++;
  statusState.errors.push(error);
  if (statusState.errors.length > 5) {
    statusState.errors.shift();
  }
  updateStatusBar();
}

// 事件类型定义
interface FetchProgress {
  current: number;
  total: number;
  title?: string;
  url?: string;
  status?: "processing" | "completed";
}

interface FetchSuccess {
  title: string;
  count: number;
}

interface FetchError {
  title: string;
  error: string;
}

// 切换筛选
function setFilter(filter: typeof currentFilter) {
  currentFilter = filter;
  // Don't reset tag filter when switching to "tag" filter type
  if (filter !== "tag") {
    currentTagFilter = null;
  }
  // Don't reset subscription when switching to/from unread or today filters
  // This allows combining unread/today with specific subscription
  if (filter !== "unread" && filter !== "today") {
    currentSubscriptionId = null;
    unreadFilterEnabled = false;
  } else if (filter === "today") {
    // When switching to today, keep unread filter state if it was enabled
    // But reset it when switching from unread to today (to avoid double unread)
    if (currentFilter === "unread") {
      unreadFilterEnabled = false;
    }
  }

  // 更新筛选标签
  document.querySelectorAll(".filter-tab").forEach(tab => {
    tab.classList.toggle("active", tab.getAttribute("data-filter") === filter);
  });

  renderSubscriptions();
  loadItems();
}

// Update filter tabs styling
function updateFilterTabs() {
  document.querySelectorAll(".filter-tab").forEach(tab => {
    const tabFilter = tab.getAttribute("data-filter");
    if (currentFilter === "tag" && currentTagFilter && tabFilter === "tag") {
      tab.classList.add("active");
      tab.textContent = `#${currentTagFilter}`;
    } else if (tabFilter === "today" && unreadFilterEnabled) {
      // Show "Today + Unread" when both filters are active
      tab.classList.add("active");
      tab.textContent = "Today + Unread";
    } else if (tabFilter === currentFilter && currentFilter !== "tag") {
      tab.classList.add("active");
      // Reset text to default
      if (tabFilter === "today") {
        tab.textContent = "Today";
      }
    } else {
      tab.classList.remove("active");
      if (tabFilter === "tag") {
        tab.textContent = "Tags";
      }
      if (tabFilter === "today") {
        tab.textContent = "Today";
      }
    }
  });
}

// Show tag selector
async function showTagSelector() {
  try {
    const tags = await invoke<string[]>("get_all_tags", {
      subscriptionId: currentSubscriptionId,
    });

    if (tags.length === 0) {
      showError("No tags found. Classify some items first.");
      return;
    }

    // Simple prompt to select a tag
    const tagList = tags.map((t, i) => `${i + 1}. ${t}`).join("\n");
    const index = prompt(`Select a tag:\n${tagList}\n\nEnter tag number (1-${tags.length}):`);

    if (index) {
      const num = parseInt(index);
      if (num >= 1 && num <= tags.length) {
        filterByTag(tags[num - 1]);
      }
    }
  } catch (error) {
    showError(`Failed to load tags: ${error}`);
  }
}

// 初始化
async function init() {
  await loadSubscriptions();
  await loadItems();

  // 拦截所有链接点击，在系统浏览器中打开
  document.addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    const link = target.closest("a");
    if (link && link.href && link.target !== "_self") {
      e.preventDefault();
      // 使用 shell.open 在系统浏览器中打开
      invoke("open_url_in_browser", { url: link.href });
    }
  });

  // 事件监听
  document.getElementById("add-feed-btn")?.addEventListener("click", openAddFeedModal);
  document.getElementById("import-opml-btn")?.addEventListener("click", importOpml);
  document.getElementById("export-opml-btn")?.addEventListener("click", exportOpml);

  // 刷新所有订阅 - 带防抖 (防止快速重复点击)
  let refreshDebounceTimer: number | null = null;
  document.getElementById("refresh-all-btn")?.addEventListener("click", () => {
    if (refreshDebounceTimer !== null) {
      showSuccess("Refresh already in progress...");
      return;
    }
    refreshAllFeeds().finally(() => {
      refreshDebounceTimer = window.setTimeout(() => {
        refreshDebounceTimer = null;
      }, 2000);
    });
  });

  // 筛选标签
  document.querySelectorAll(".filter-tab").forEach(tab => {
    tab.addEventListener("click", () => {
      const filter = tab.getAttribute("data-filter") as typeof currentFilter;
      if (filter === "tag") {
        // For tag filter, show tag selection instead of just setting filter
        showTagSelector();
      } else if (filter === "unread" && currentFilter === "today") {
        // Special case: clicking Unread while in Today mode toggles "Today + Unread"
        unreadFilterEnabled = !unreadFilterEnabled;
        updateFilterTabs();
        loadItems();
      } else {
        setFilter(filter);
      }
    });
  });

  // 标记所有已读
  document.getElementById("mark-all-read-btn")?.addEventListener("click", markAllAsRead);

  // 搜索
  let searchTimeout: number;
  document.getElementById("search-input")?.addEventListener("input", (e) => {
    window.clearTimeout(searchTimeout);
    searchTimeout = window.setTimeout(() => {
      searchItems((e.target as HTMLInputElement).value);
    }, 300);
  });

  // 添加订阅表单
  document.getElementById("add-feed-form")?.addEventListener("submit", (e) => {
    e.preventDefault();
    const url = (document.getElementById("feed-url") as HTMLInputElement).value;
    const title = (document.getElementById("feed-title") as HTMLInputElement).value;
    const website_url = (document.getElementById("website-url") as HTMLInputElement).value;
    const rsshub_url = (document.getElementById("rsshub-url") as HTMLInputElement).value;
    const use_website = (document.getElementById("use-website") as HTMLInputElement).checked;

    addSubscription({ url, title: title || undefined, website_url: website_url || undefined, rsshub_url: rsshub_url || undefined, use_website });
  });

  document.querySelector(".cancel-btn")?.addEventListener("click", closeAddFeedModal);
  document.querySelector(".close-modal")?.addEventListener("click", closeAddFeedModal);

  // AI settings modal close buttons
  const aiModal = document.getElementById("ai-settings-modal");
  aiModal?.querySelector(".close-modal")?.addEventListener("click", closeAiSettingsModal);
  aiModal?.querySelector(".cancel-btn")?.addEventListener("click", closeAiSettingsModal);

  // 详情操作按钮
  document.getElementById("toggle-webview-btn")?.addEventListener("click", async () => {
    if (selectedItem) {
      const subId = selectedItem.subscription_id;
      const subscription = subscriptions.find(s => s.id === subId);
      const currentUseWebsite = subscription?.use_website ?? false;
      
      // 先立即切换前端状态
      useWebView = !currentUseWebsite;
      webviewPerSubscription.set(subId, useWebView);
      
      // 更新按钮状态
      const btn = document.getElementById("toggle-webview-btn") as HTMLButtonElement;
      btn.textContent = useWebView ? "Text" : "Web View";
      
      // 重新渲染详情
      renderItemDetail(selectedItem);
      
      // 然后在后台更新后端状态
      try {
        const updated = await invoke<Subscription>("toggle_use_website", { id: subId });
        // 使用后端返回的新状态来确保一致性
        useWebView = updated.use_website;
        webviewPerSubscription.set(subId, useWebView);
        // 更新本地订阅源列表
        const index = subscriptions.findIndex(s => s.id === subId);
        if (index !== -1) {
          subscriptions[index] = updated;
        }
        renderSubscriptions();
        // 如果后端返回的状态与本地不一致，需要重新渲染
        if (selectedItem) renderItemDetail(selectedItem);
      } catch (error) {
        console.error("Failed to update subscription:", error);
      }
    }
  });

  document.getElementById("mark-read-btn")?.addEventListener("click", () => {
    if (selectedItem) {
      markAsRead(selectedItem.id, !selectedItem.is_read);
    }
  });

  document.getElementById("favorite-btn")?.addEventListener("click", () => {
    if (selectedItem) {
      toggleFavorite(selectedItem.id);
    }
  });

  document.getElementById("read-later-btn")?.addEventListener("click", () => {
    if (selectedItem) {
      toggleReadLater(selectedItem.id);
    }
  });

  // AI 功能按钮
  document.getElementById("translate-btn")?.addEventListener("click", async () => {
    if (!selectedItem) return;

    // 获取当前文章的翻译状态
    let translationState = translationStateByItemId.get(selectedItem.id);
    const isTranslating = !!(translationState && translationState.abortController);
    const hasCache = selectedItem.translated_content !== null;

    // 1. 如果正在翻译，点击取消
    if (isTranslating) {
      translationState!.abortController!.abort();
      translationStateByItemId.delete(selectedItem.id);
      renderItemDetail(selectedItem);
      showSuccess("Translation cancelled");
      return;
    }

    // 2. 如果有缓存，切换显示模式
    if (hasCache) {
      if (!translationState) {
        const newState = { useTranslation: true, inProgressContent: null, abortController: null, hasError: false, errorMessage: null };
        translationStateByItemId.set(selectedItem.id, newState);
        showSuccess("Showing translation");
      } else {
        translationState.useTranslation = !translationState.useTranslation;
        showSuccess(translationState.useTranslation ? "Showing translation" : "Showing original");
      }
      renderItemDetail(selectedItem);
      return;
    }

    // 3. 开始新的翻译
    // 标记为未读
    if (selectedItem.is_read) {
      selectedItem.is_read = false;
      await invoke("mark_item_read", { itemId: selectedItem.id, isRead: false });
      renderItems();
    }

    // 检查是否在 webview 模式下，如果是则先将网页内容保存为 content_md
    const useWebViewForItem = webviewPerSubscription.get(selectedItem.subscription_id) ?? false;

    if (useWebViewForItem && selectedItem.link) {
      // 从后端获取网站内容（会自动保存 content_md 并标记 is_website_content）
      try {
        console.log('[Translate] Fetching website content for markdown');
        await invoke<string>("fetch_website_content", { url: selectedItem.link, itemId: selectedItem.id });
      } catch (error) {
        console.error('[Translate] Failed to fetch website content:', error);
      }
    }

    // 开始翻译 — 使用 translate_item_bilingual_streaming 从数据库读取
    // WebView 模式：优先使用 content_md；RSS 模式：优先使用 content
    await translateItem(selectedItem, undefined);
  });

  document.getElementById("classify-btn")?.addEventListener("click", async () => {
    if (!selectedItem) return;
    await classifyItem(selectedItem);
  });

  document.getElementById("ai-settings-btn")?.addEventListener("click", openAiSettingsModal);

  // 测试 AI 连接按钮 - 测试连接并保存
  document.getElementById("test-ai-btn")?.addEventListener("click", async () => {
    const apiKey = (document.getElementById("ai-api-key") as HTMLInputElement).value;
    const baseUrl = (document.getElementById("ai-base-url") as HTMLInputElement).value;
    const model = (document.getElementById("ai-model") as HTMLInputElement).value;
    const maxCharsPerSegment = parseInt((document.getElementById("ai-max-chars") as HTMLInputElement).value) || undefined;

    if (!apiKey) {
      showError("Please enter an API key first");
      return;
    }

    const btn = document.getElementById("test-ai-btn") as HTMLButtonElement;
    const originalText = btn.textContent;
    btn.textContent = "Testing...";
    btn.disabled = true;

    try {
      // 测试连接（会同时保存配置）
      await invoke("set_ai_config", { apiKey, baseUrl: baseUrl || undefined, model: model || undefined, maxCharsPerSegment, skipTest: false });
      showSuccess("API connection successful! Configuration saved.");
    } catch (error) {
      showError(`Connection test failed: ${error}`);
    } finally {
      btn.textContent = originalText;
      btn.disabled = false;
    }
  });

  // AI 设置表单 - 直接保存（跳过测试）
  document.getElementById("ai-settings-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const apiKey = (document.getElementById("ai-api-key") as HTMLInputElement).value;
    const baseUrl = (document.getElementById("ai-base-url") as HTMLInputElement).value;
    const model = (document.getElementById("ai-model") as HTMLInputElement).value;
    const maxCharsPerSegment = parseInt((document.getElementById("ai-max-chars") as HTMLInputElement).value) || undefined;

    if (!apiKey) {
      showError("Please enter an API key");
      return;
    }

    try {
      // 直接保存，跳过连接测试
      await invoke("set_ai_config", { apiKey, baseUrl: baseUrl || undefined, model: model || undefined, maxCharsPerSegment, skipTest: true });
      closeAiSettingsModal();
      showSuccess("AI configuration saved (not tested)");
    } catch (error) {
      showError(`Failed to save AI configuration: ${error}`);
    }
  });

  // Modal 点击外部关闭
  document.getElementById("add-feed-modal")?.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) {
      closeAddFeedModal();
    }
  });

  document.getElementById("ai-settings-modal")?.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) {
      closeAiSettingsModal();
    }
  });

  // Cancel ignore timer for any user interaction (indicates engagement)
  cancelIgnoreTimer();

  // 初始化图片缩放功能
  initImageZoom();
  addZoomHintToImages();
}

// ==================== 图片缩放功能 ====================

/**
 * 初始化图片缩放功能
 */
function initImageZoom(): void {
  // 使用事件委托处理图片点击
  document.getElementById('detail-content')?.addEventListener('click', (e) => {
    // Cancel ignore timer on user interaction
    cancelIgnoreTimer();
    const target = e.target as HTMLElement;

    // 检查是否点击了图片
    if (target.tagName === 'IMG') {
      const img = target as HTMLImageElement;

      // 切换缩放状态
      if (img.classList.contains('zoomed')) {
        // 缩小图片
        img.classList.remove('zoomed');
        img.style.maxWidth = '400px';
        img.style.maxHeight = '300px';
      } else {
        // 放大图片
        img.classList.add('zoomed');
        img.style.maxWidth = '100%';
        img.style.maxHeight = 'none';
      }
    }
  });

  // 添加键盘支持：ESC 键退出缩放
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      const zoomedImages = document.querySelectorAll('.detail-body img.zoomed');
      zoomedImages.forEach(img => {
        img.classList.remove('zoomed');
        (img as HTMLImageElement).style.maxWidth = '400px';
        (img as HTMLImageElement).style.maxHeight = '300px';
      });
    }
  });
}

/**
 * 为图片容器添加缩放提示
 */
function addZoomHintToImages(): void {
  const detailBody = document.querySelector('.detail-body');
  if (!detailBody) return;

  detailBody.querySelectorAll('img').forEach(img => {
    // 添加 title 提示用户可以点击缩放
    if (!img.title) {
      img.title = '点击图片查看大图，按 ESC 键退出';
    }

    // 添加 data 属性标记
    img.dataset.zoomable = 'true';
  });
}

window.addEventListener("DOMContentLoaded", init);
