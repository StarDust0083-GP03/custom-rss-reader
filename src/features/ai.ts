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
import { selectItem } from "./actions";
import { success as toastSuccess, error as toastError, info as toastInfo } from "../toast";

const S = state;

// 翻译
export async function translateItem(item: FeedItem, htmlContent?: string) {
  // 获取或创建该文章的翻译状态
  let translationState: TranslationState | undefined = S.translationStateByItemId.get(item.id);

  // 如果正在翻译同一篇文章，取消翻译
  if (translationState?.abortController) {
    translationState.abortController.abort();
    S.translationStateByItemId.delete(item.id);
    renderItems(true); // Update badge
    renderItemDetail(item);
    toastSuccess("Translation cancelled");
    return;
  }

  // 如果之前翻译失败，清除错误状态，允许重试
  if (translationState?.hasError) {
    S.translationStateByItemId.delete(item.id);
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
    S.translationStateByItemId.set(item.id, translationState);
    renderItems(true); // Update badge
    renderItemDetail(item);
    toastSuccess("Using cached translation");
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
  S.translationStateByItemId.set(item.id, translationState);

  // 立即更新按钮和徽章状态
  renderItems(true); // Show translating badge
  renderItemDetail(item);

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
          if (currentState) {
            currentState.hasError = true;
            currentState.errorMessage = event.payload.error;
          }
          toastError(`Translation error: ${event.payload.error}`);
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
          if (!currentState) return;

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
              if (S.selectedItem?.id === item.id) {
                renderItemDetail(item);
                if (html_chunk) {
                  toastError(`Translation partially complete: ${currentState.errorMessage}`);
                } else {
                  toastError(`Translation failed: ${currentState.errorMessage}. Check ~/.rss-reader/ai_errors.log for details.`);
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
            // 只有当前选中的文章才渲染
            if (S.selectedItem?.id === item.id) {
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
      const currentState = S.translationStateByItemId.get(item.id);
      if (currentState) {
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
      toastSuccess("Translation cancelled");
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
      toastError(`Translation failed: ${error}`);
      renderItems(true); // Update badge to show error
      renderItemDetail(item); // Update button to show error state
    }
  }
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
