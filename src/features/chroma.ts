/**
 * ChromaDB semantic-search features: settings modal, search-mode toggle,
 * similar-articles lookup, reindex and health check.
 */

import { invoke } from "@tauri-apps/api/core";
import type { ChromaConfigResponse, SyncProgress } from "../types";
import { chroma as chromaApi } from "../api";
import { state } from "../state";
import { renderItems } from "../ui/render";
import { setLoadingWithStatus, clearLoadingStatus } from "../ui/status";
import { success as toastSuccess, error as toastError, info as toastInfo } from "../toast";
import { searchItems } from "./actions";

const S = state;

export async function loadChromaConfig(): Promise<void> {
  try {
    const config = await invoke<ChromaConfigResponse>("get_chroma_config");
    S.chromaEnabled = config.enabled;
    (document.getElementById("chroma-host") as HTMLInputElement).value = config.host;
    (document.getElementById("chroma-port") as HTMLInputElement).value = config.port.toString();
    (document.getElementById("chroma-collection") as HTMLInputElement).value = config.collection_name;
    (document.getElementById("chroma-enabled") as HTMLInputElement).checked = config.enabled;
    updateSearchModeBtn();
  } catch (error) {
    console.log("No ChromaDB config found, using defaults");
  }
}

export function updateSearchModeBtn() {
  const btn = document.getElementById("search-mode-btn") as HTMLButtonElement;
  if (!btn) return;
  if (S.chromaEnabled) {
    btn.style.display = "";
    btn.textContent = S.searchMode === "semantic" ? "Semantic" : "Text";
    btn.classList.toggle("semantic", S.searchMode === "semantic");
  } else {
    btn.style.display = "none";
  }
  // "Find Similar" lives in the detail ⋯ menu — only useful with Chroma
  const similarItem = document.getElementById("similar-menu-item") as HTMLButtonElement | null;
  if (similarItem) similarItem.disabled = !S.chromaEnabled;
}

/// Replace the item list with articles semantically similar to the
/// currently selected one. Results are real summaries (unlike semantic
/// search hits), so clicking a result opens its detail as usual.
export async function findSimilarArticles() {
  if (!S.selectedItem) {
    toastInfo("Select an article first");
    return;
  }
  setLoadingWithStatus("", `Finding articles similar to "${S.selectedItem.title}"...`);
  try {
    const items = await chromaApi.findSimilar(S.selectedItem.id, 20);
    S.currentItems = items;
    renderItems();
    clearLoadingStatus(true, `Found ${items.length} similar articles`);
  } catch (error) {
    console.error("Failed to find similar articles:", error);
    clearLoadingStatus(false, "Similar-articles search failed");
    toastError("Similar-articles search failed. Is ChromaDB running and indexed?");
  }
}

export async function saveChromaConfig(data: { host: string; port: number; collection_name: string; enabled: boolean }) {
  try {
    await invoke("set_chroma_config", {
      host: data.host,
      port: data.port,
      collectionName: data.collection_name,
      enabled: data.enabled,
    });
    S.chromaEnabled = data.enabled;
    updateSearchModeBtn();
    toastSuccess("ChromaDB configuration saved. Restart the app for changes to take effect.");
    return true;
  } catch (error) {
    toastError(`Failed to save ChromaDB configuration: ${error}`);
    return false;
  }
}

export async function openChromaSettingsModal() {
  const modal = document.getElementById("chroma-settings-modal");
  if (modal) modal.classList.add("visible");
  await loadChromaConfig();
}

export function closeChromaSettingsModal() {
  const modal = document.getElementById("chroma-settings-modal");
  if (modal) modal.classList.remove("visible");
}

export async function reindexChroma() {
  setLoadingWithStatus("", "Re-indexing...");
  let poll: ReturnType<typeof setInterval> | undefined;
  try {
    toastSuccess("Re-indexing started...");
    // Poll live progress while the (potentially long) reindex runs, so the
    // status bar shows movement instead of sitting on "Re-indexing...".
    poll = setInterval(async () => {
      try {
        const p = await invoke<SyncProgress>("chroma_sync_progress");
        if (p.running) {
          const pct = p.total > 0 ? Math.round((p.done / p.total) * 100) : 0;
          setLoadingWithStatus(
            "",
            `Re-indexing (${p.phase || "walk"})... ${p.done}/${p.total} items ${pct}%, ` +
              `page ${p.pages}, ${p.elapsed_ms}ms`,
          );
        }
      } catch {
        // transient polling errors are harmless — keep trying
      }
    }, 400);
    const result = await invoke<string>("reindex_chromadb");
    clearInterval(poll);
    poll = undefined;
    clearLoadingStatus(true, result);
    toastSuccess(result);
  } catch (error) {
    if (poll !== undefined) clearInterval(poll);
    clearLoadingStatus(false, "Re-index failed");
    toastError(`Re-index failed: ${error}`);
  }
}

export async function chromaHealthCheck() {
  try {
    const ok = await invoke<boolean>("chroma_health_check");
    if (ok) {
      toastSuccess("ChromaDB is reachable.");
    } else {
      toastError("ChromaDB is not reachable.");
    }
  } catch (error) {
    toastError(`Health check failed: ${error}`);
  }
}

export function toggleSearchMode() {
  S.searchMode = S.searchMode === "text" ? "semantic" : "text";
  updateSearchModeBtn();
  const searchInput = document.getElementById("search-input") as HTMLInputElement;
  if (searchInput && searchInput.value.trim()) {
    searchItems(searchInput.value);
  }
}
