/**
 * AI features: streaming bilingual translation, single-article
 * classification, and the AI settings modal.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { FeedItem, AiClassificationResponse, Recommendation } from "../types";
import { ai as aiApi } from "../api";
import { state, type TranslationState } from "../state";
import { renderItems, renderItemDetail } from "../ui/render";
import { markAsRead, selectItem } from "./actions";
import { success as toastSuccess, error as toastError, info as toastInfo } from "../toast";

const S = state;

// 翻译
export async function translateItem(
  item: FeedItem,
  htmlContent?: string,
  options: { force?: boolean } = {},
) {
  const force = options.force === true;
  // 获取或创建该文章的翻译状态
  let translationState: TranslationState | undefined = S.translationStateByItemId.get(item.id);

  // 如果正在翻译同一篇文章，取消翻译
  if (translationState?.abortController) {
    cancelTranslation(item);
    return;
  }

  // 如果之前翻译失败，清除错误状态，允许重试
  if (translationState?.hasError) {
    S.translationStateByItemId.delete(item.id);
    translationState = undefined;
  }

  // 强制重译必须先丢弃旧缓存，普通点击则复用有效缓存。
  if (force) {
    item.translated_content = null;
  } else if (item.translated_content?.trim()) {
    // 直接使用已有的翻译
    translationState = {
      useTranslation: true,
      inProgressContent: null,
      abortController: null,
      hasError: false,
      errorMessage: null
    };
    S.translationStateByItemId.set(item.id, translationState);
    renderItems(true); // Update badge
    renderItemDetail(item);
    toastSuccess("Using cached translation");
    return;
  }

  // 翻译动作会把文章标为未读，和详情页的其他已读状态修改共用同一条路径。
  if (item.is_read) {
    await markAsRead(item.id, false);
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
  S.translationStateByItemId.set(item.id, translationState);
  const runState = translationState;

  // 立即更新按钮和徽章状态
  renderItems(true); // Show translating badge
  renderItemDetail(item);

  let errorNotified = false;
  try {
    toastSuccess("Translating...");

    let unlistenProgress: (() => void) | null = null;
    let unlistenError: (() => void) | null = null;

    try {
      // Listen for translation error events
      unlistenError = await listen<{ item_id: number; error: string; paragraph_index: number }>(
        "translation-error",
        (event) => {
          if (event.payload.item_id !== item.id) return;
          const currentState = S.translationStateByItemId.get(item.id);
          if (currentState !== runState) return;
          currentState.hasError = true;
          currentState.errorMessage = event.payload.error;
          currentState.useTranslation = false;
          // The invoke rejection owns the user-facing error toast. Keeping
          // this listener state-only prevents duplicate errors.
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
          const currentState = S.translationStateByItemId.get(item.id);
          if (currentState !== runState) return;

          // 如果是缓存命中，直接设置完整内容
          if (cached && html_chunk) {
            item.translated_content = html_chunk;
            currentState.useTranslation = true;
            currentState.inProgressContent = null;
            currentState.abortController = null;
            currentState.hasError = false;
            // 只有当前选中的文章才渲染和显示提示
            if (S.selectedItem?.id === item.id) {
              renderItems(true);
              renderItemDetail(item);
              toastSuccess("Using cached translation");
            }
            return;
          }

          // Update progress indicator - 只在当前选中时显示
          if (!cached && S.selectedItem?.id === item.id && !currentState.hasError) {
            toastSuccess(`Translating... ${completed}/${total}`);
          }

          // Append the chunk to state storage (not to item yet)
          if (is_complete) {
            // Final event - clean up state
            currentState.abortController = null;

            // Handle error case
            if (has_error) {
              currentState.hasError = true;
              currentState.errorMessage = error_messages?.[0] || "Translation failed";
              // Never expose or cache a partial translation as a successful
              // result. The original article remains the fallback view.
              currentState.inProgressContent = null;
              currentState.useTranslation = false;

              // 更新列表徽章
              renderItems(true);

              // 只有当前选中的文章才渲染
              if (S.selectedItem?.id === item.id) {
                renderItemDetail(item);
                toastError(`Translation failed: ${currentState.errorMessage}. Check ~/.rss-reader/ai_errors.log for details.`);
              }
              errorNotified = true;
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
            if (S.selectedItem?.id === item.id) {
              renderItemDetail(item);
              toastSuccess("Translation complete");
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
            // Show the streaming partial LIVE. Without this, `useTranslation`
            // stays false until completion and the bilingual view never
            // renders mid-stream — in webview mode the pane kept showing the
            // webpage and in text mode the original markdown, so translation
            // looked broken/idle until the very end. Flipping it here makes
            // streaming display identical in both modes; an error mid-stream
            // resets it to false and falls back to the original view.
            currentState.useTranslation = true;
            // 只有当前选中的文章才渲染 — pass the untranslated remainder so
            // the body shows [finished bilingual pairs] + [original tail];
            // nothing is hidden while the stream is in flight.
            if (S.selectedItem?.id === item.id) {
              const source = item.content_md ?? item.description ?? "";
              const tail = untranslatedTail(source, currentState.inProgressContent ?? "");
              renderItemDetail(
                { ...item, translated_content: currentState.inProgressContent },
                { untranslatedTail: tail },
              );
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
          force,
        });
      } else {
        // 使用流式双语对照翻译
        await invoke<string>("translate_item_bilingual_streaming", {
          itemId: item.id,
          force,
        });
      }
    } finally {
      // Always clean up listeners
      if (unlistenProgress) unlistenProgress();
      if (unlistenError) unlistenError();
      // Clean up translation state on error (keep cached success entries)
      const currentState = S.translationStateByItemId.get(item.id);
      if (currentState === runState) {
        if (abortController.signal.aborted) {
          // User cancelled - clean up
          S.translationStateByItemId.delete(item.id);
        } else if (currentState.hasError) {
          // Error - keep entry so user sees the error badge, but clear bulky content
          currentState.inProgressContent = null;
        }
      }
    }
  } catch (error) {
    const currentState = S.translationStateByItemId.get(item.id);
    if (currentState !== runState) return;
    if (abortController.signal.aborted) {
      // User cancellation is handled synchronously by cancelTranslation().
      S.translationStateByItemId.delete(item.id);
      return;
    }

    // Error occurred. Keep the error state for an explicit retry, but never
    // keep the partial HTML assembled during the failed run.
    currentState.hasError = true;
    currentState.errorMessage = String(error);
    currentState.abortController = null;
    currentState.inProgressContent = null;
    currentState.useTranslation = false;
    if (!errorNotified) {
      toastError(`Translation failed: ${error}`);
    }
    renderItems(true);
    if (S.selectedItem?.id === item.id) renderItemDetail(item);
  }
}

/** Cancel the visible translation run without allowing its late events to
 * update a newer run for the same item. The backend request may still finish
 * in the background because Tauri invoke has no transport cancellation here.
 */
export function cancelTranslation(item: FeedItem): boolean {
  const translationState = S.translationStateByItemId.get(item.id);
  if (!translationState?.abortController) return false;

  translationState.abortController.abort();
  S.translationStateByItemId.delete(item.id);
  renderItems(true);
  if (S.selectedItem?.id === item.id) renderItemDetail(item);
  toastSuccess("Translation cancelled");
  return true;
}

/** Toggle a completed translation without starting another model request. */
export function toggleCachedTranslation(item: FeedItem): boolean {
  if (!item.translated_content?.trim()) return false;

  const translationState = S.translationStateByItemId.get(item.id);
  if (!translationState) {
    S.translationStateByItemId.set(item.id, {
      useTranslation: true,
      inProgressContent: null,
      abortController: null,
      hasError: false,
      errorMessage: null,
    });
    toastSuccess("Showing translation");
  } else {
    translationState.useTranslation = !translationState.useTranslation;
    toastSuccess(translationState.useTranslation ? "Showing translation" : "Showing original");
  }
  renderItemDetail(item);
  return true;
}

async function ensureWebsiteContent(item: FeedItem): Promise<void> {
  const subscription = S.subscriptions.find(s => s.id === item.subscription_id);
  if (!subscription?.use_website || !item.link || item.content_md) return;

  try {
    const markdown = await invoke<string>("fetch_website_markdown", {
      url: item.link,
      itemId: item.id,
    });
    item.content_md = markdown;
  } catch (error) {
    console.error("[Translate] Failed to fetch website content:", error);
  }
}

/** Handle click, right-click, and long-press semantics for the translate button. */
export async function handleTranslateAction(item: FeedItem, force = false): Promise<void> {
  const translationState = S.translationStateByItemId.get(item.id);
  if (force && translationState?.abortController) return;

  if (!force && translationState?.abortController) {
    cancelTranslation(item);
    return;
  }

  if (!force && toggleCachedTranslation(item)) return;

  await ensureWebsiteContent(item);
  if (force) {
    await retranslateItem(item);
  } else {
    await translateItem(item);
  }
}

/** Force a fresh translation, bypassing and clearing the old cache. */
export async function retranslateItem(item: FeedItem): Promise<void> {
  const translationState = S.translationStateByItemId.get(item.id);
  if (translationState?.abortController) return;

  S.translationStateByItemId.delete(item.id);
  item.translated_content = null;
  renderItems(true);
  await translateItem(item, undefined, { force: true });
}

export async function classifyItem(item: FeedItem) {
  try {
    const contentSnippet = item.content ? item.content.slice(0, 500) : null;

    toastSuccess("Classifying...");
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
    toastSuccess(`Classified: ${result.tags.join(", ")}`);
  } catch (error) {
    toastError(`Classification failed: ${error}`);
  }
}

// ---------------------------------------------------------------------------
// AI settings modal
// ---------------------------------------------------------------------------

export async function openAiSettingsModal() {
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

export function closeAiSettingsModal() {
  const modal = document.getElementById("ai-settings-modal");
  if (modal) {
    modal.classList.remove("visible");
  }
}

// ---------------------------------------------------------------------------
// Recommended reads (manual trigger, first version)
// ---------------------------------------------------------------------------

const RECOMMEND_LIST_ID = "recommendations-list";

/** Open the recommendations modal and fetch picks (one LLM call). */
export async function openRecommendations() {
  const modal = document.getElementById("recommend-modal");
  const list = document.getElementById(RECOMMEND_LIST_ID);
  const btn = document.getElementById("recommend-btn") as HTMLButtonElement | null;
  if (!modal || !list) return;

  modal.classList.add("visible");

  // Loading state
  list.replaceChildren();
  const loading = document.createElement("div");
  loading.className = "recommend-loading";
  loading.textContent = "Reading your unread articles...";
  list.appendChild(loading);
  if (btn) {
    btn.disabled = true;
    btn.textContent = "Picking...";
  }

  try {
    const recs = await aiApi.recommendReads();
    renderRecommendations(recs);
  } catch (error) {
    list.replaceChildren();
    const err = document.createElement("div");
    err.className = "recommend-loading";
    // Most common cause: no AI key configured (backend returns a validation
    // error from AiConfig::is_valid).
    err.textContent = `Failed to get recommendations: ${error}`;
    list.appendChild(err);
    toastError(`Recommendation failed: ${error}`);
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = "★ Picks";
    }
  }
}

export function closeRecommendModal() {
  const modal = document.getElementById("recommend-modal");
  if (modal) modal.classList.remove("visible");
}

function renderRecommendations(recs: Recommendation[]) {
  const list = document.getElementById(RECOMMEND_LIST_ID);
  if (!list) return;
  list.replaceChildren();

  if (recs.length === 0) {
    const empty = document.createElement("div");
    empty.className = "recommend-loading";
    empty.textContent = "No articles to recommend yet — refresh your feeds first.";
    list.appendChild(empty);
    return;
  }

  toastInfo(`Editor picked ${recs.length} articles for you`);

  recs.forEach((rec, rank) => {
    const row = document.createElement("div");
    row.className = "recommend-item";

    const head = document.createElement("div");
    head.className = "recommend-head";
    const rankEl = document.createElement("span");
    rankEl.className = "recommend-rank";
    rankEl.textContent = `№${rank + 1}`;
    head.appendChild(rankEl);
    const src = document.createElement("span");
    src.className = "recommend-source";
    src.textContent = rec.source;
    head.appendChild(src);
    row.appendChild(head);

    const title = document.createElement("div");
    title.className = "recommend-title";
    title.textContent = rec.title;
    row.appendChild(title);

    const reason = document.createElement("div");
    reason.className = "recommend-reason";
    reason.textContent = rec.reason;
    row.appendChild(reason);

    row.addEventListener("click", () => {
      closeRecommendModal();
      // selectItem re-fetches the full item by id, so a minimal summary is
      // enough to open the article in the reading pane.
      selectItem({ id: rec.item_id } as never);
    });
    list.appendChild(row);
  });
}

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
