/**
 * ChromaDB semantic-search features: settings modal, search-mode toggle,
 * similar-articles lookup, reindex and health check.
 */

import { invoke } from "@tauri-apps/api/core";
import type { IndexStatus, SyncProgress } from "../types";
import { chroma as chromaApi } from "../api";
import { state } from "../state";
import { renderItems } from "../ui/render";
import { beginLibraryQuery, isCurrentLibraryQuery } from "../ui/library-query";
import { setLoadingWithStatus, clearLoadingStatus } from "../ui/status";
import { success as toastSuccess, error as toastError, info as toastInfo } from "../toast";
import { searchItems } from "./actions";

const S = state;
let chromaEnableInProgress = false;

export async function loadChromaConfig(): Promise<void> {
  try {
    const config = await chromaApi.getConfig();
    S.chromaEnabled = config.enabled;
    (document.getElementById("chroma-host") as HTMLInputElement).value = config.host;
    (document.getElementById("chroma-port") as HTMLInputElement).value = config.port.toString();
    (document.getElementById("chroma-collection") as HTMLInputElement).value = config.collection_name;
    (document.getElementById("chroma-enabled") as HTMLInputElement).checked = config.enabled;
    updateSearchModeBtn();
    updateChromaSaveButton();
  } catch (error) {
    console.log("No ChromaDB config found, using defaults");
  }
}

export function updateChromaSaveButton() {
  const button = document.getElementById("chroma-save-btn") as HTMLButtonElement | null;
  const enabled = (document.getElementById("chroma-enabled") as HTMLInputElement | null)?.checked;
  if (!button || enabled === undefined) return;
  button.textContent = enabled && !S.chromaEnabled ? "Enable & Index" : "Save Configuration";
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
  const seq = beginLibraryQuery();
  try {
    const items = await chromaApi.findSimilar(S.selectedItem.id, 20);
    // Similar-articles results replace the same list search and filters use.
    if (!isCurrentLibraryQuery(seq)) return;
    S.currentItems = items;
    renderItems();
    clearLoadingStatus(true, `Found ${items.length} similar articles`);
  } catch (error) {
    if (!isCurrentLibraryQuery(seq)) return;
    console.error("Failed to find similar articles:", error);
    clearLoadingStatus(false, "Similar-articles search failed");
    toastError("Similar-articles search failed. Is ChromaDB running and indexed?");
  }
}

export async function saveChromaConfig(data: { host: string; port: number; collection_name: string; enabled: boolean }) {
  try {
    await chromaApi.setConfig({
      host: data.host,
      port: data.port,
      collectionName: data.collection_name,
      enabled: data.enabled,
    });
    S.chromaEnabled = data.enabled;
    updateSearchModeBtn();
    updateChromaSaveButton();
    toastSuccess("ChromaDB configuration saved.");
    return true;
  } catch (error) {
    toastError(`Failed to save ChromaDB configuration: ${error}`);
    return false;
  }
}

function startChromaProgressPolling(label: string): ReturnType<typeof setInterval> {
  return setInterval(async () => {
    try {
      const p = await invoke<SyncProgress>("chroma_sync_progress");
      if (p.running) {
        const pct = p.total > 0 ? Math.round((p.done / p.total) * 100) : 0;
        setLoadingWithStatus(
          "",
          `${label} (${p.phase || "walk"})... ${p.done}/${p.total} items ${pct}%, ` +
            `page ${p.pages}, ${p.elapsed_ms}ms`,
        );
      }
    } catch {
      // transient polling errors are harmless — keep trying
    }
  }, 400);
}

export async function enableAndIndexChroma(data: {
  host: string;
  port: number;
  collection_name: string;
}): Promise<boolean> {
  if (chromaEnableInProgress) return false;
  chromaEnableInProgress = true;
  const saveButton = document.getElementById("chroma-save-btn") as HTMLButtonElement | null;
  if (saveButton) saveButton.disabled = true;
  setLoadingWithStatus("", "Connecting to ChromaDB and preparing semantic search...");
  let poll: ReturnType<typeof setInterval> | undefined;
  try {
    poll = startChromaProgressPolling("Indexing");
    const result = await chromaApi.enableAndIndex({
      host: data.host,
      port: data.port,
      collectionName: data.collection_name,
    });
    S.chromaEnabled = true;
    updateSearchModeBtn();
    updateChromaSaveButton();
    clearLoadingStatus(
      true,
      `Semantic search enabled: indexed ${result.sync.indexed} items in ${result.sync.duration_ms}ms`,
    );
    toastSuccess(`Semantic search enabled; indexed ${result.sync.indexed} items.`);
    return true;
  } catch (error) {
    await loadChromaConfig();
    clearLoadingStatus(false, "Semantic search setup failed");
    toastError(`Could not enable semantic search: ${error}. Is ChromaDB running?`);
    return false;
  } finally {
    if (poll !== undefined) clearInterval(poll);
    chromaEnableInProgress = false;
    if (saveButton) {
      saveButton.disabled = false;
      updateChromaSaveButton();
    }
  }
}

// ---------------------------------------------------------------------------
// Index status panel
// ---------------------------------------------------------------------------

/** Poll handle for the status panel; only runs while the modal is open. */
let indexStatusTimer: number | null = null;


/** State the card is rendering, mapped to the visual treatment in CSS. */
type IndexState = "ready" | "running" | "behind" | "off";

/** Sync phase, one word: the chip already says "Indexing…". */
const PHASE_LABELS: Record<string, string> = {
  deletes: "deleting",
  upserts: "embedding",
  walk: "scanning",
  reconcile: "reconciling",
};

/**
 * Render one status snapshot.
 *
 * Three rows, nothing more: a state chip, a meter, and one line of numbers.
 * The dialog answers "is the index up to date?"; queue depth, sync phase and
 * the collection id stay in the log, where they are actually useful.
 */
export function renderIndexStatus(status: IndexStatus): void {
  const panel = document.getElementById("chroma-index-status");
  const set = (id: string, value: string) => {
    const el = document.getElementById(id);
    if (el) el.textContent = value;
  };

  const remaining = Math.max(0, status.total - status.indexed - status.queued_jobs);
  const percent =
    status.total > 0 ? Math.min(100, Math.round((status.indexed / status.total) * 100)) : 100;
  const behind = status.enabled && remaining > 0 && status.queued_jobs === 0;
  const state: IndexState = !status.enabled
    ? "off"
    : status.running
      ? "running"
      : behind
        ? "behind"
        : "ready";

  if (panel) panel.dataset.state = state;

  set("chroma-chip-text", {
    ready: "Up to date",
    running: "Indexing…",
    behind: "Behind",
    off: "Disabled",
  }[state]);

  const bar = document.getElementById("chroma-progress-bar");
  if (bar) bar.style.width = `${percent}%`;
  document.getElementById("chroma-progress")?.setAttribute("aria-valuenow", String(percent));

  if (!status.enabled) {
    set("chroma-summary", "Not indexing");
    set("chroma-detail", "");
    set("chroma-index-note", "Semantic search is off — search stays keyword-only.");
    return;
  }

  const count = status.indexed.toLocaleString();
  const total = status.total.toLocaleString();
  // Use a strong element for the number without rebuilding the whole line.
  const summary = document.getElementById("chroma-summary");
  if (summary) {
    summary.replaceChildren();
    const strong = document.createElement("strong");
    strong.textContent = count;
    summary.append(strong, document.createTextNode(` of ${total} articles indexed`));
  }

  // Bottom-right, small: only what is moving or waiting.
  const detail: string[] = [];
  if (status.running) {
    detail.push(PHASE_LABELS[status.phase] ?? (status.phase || "working"));
    if (status.scan_total > 0) {
      detail.push(`${status.done.toLocaleString()}/${status.scan_total.toLocaleString()}`);
    }
    if (status.elapsed_ms > 0) {
      detail.push(`${Math.round(status.elapsed_ms / 1000)}s`);
    }
  }
  if (status.queued_jobs > 0) {
    detail.push(`${status.queued_jobs.toLocaleString()} queued`);
  }
  set("chroma-detail", detail.join(" · "));

  set(
    "chroma-index-note",
    behind ? `Re-Index All catches up the remaining ${remaining.toLocaleString()}.` : "",
  );
}

/** Fetch and render one snapshot. Never throws into the UI. */
export async function refreshIndexStatus(): Promise<void> {
  try {
    renderIndexStatus(await chromaApi.indexStatus());
  } catch (error) {
    // The modal must stay usable when the backend is mid-restart.
    const note = document.getElementById("chroma-index-note");
    if (note) note.textContent = `Index status unavailable: ${error}`;
    document.getElementById("chroma-index-status")?.setAttribute("data-state", "off");
    const chip = document.getElementById("chroma-chip-text");
    if (chip) chip.textContent = "Unavailable";
  }
}

function startIndexStatusPolling(): void {
  stopIndexStatusPolling();
  void refreshIndexStatus();
  indexStatusTimer = window.setInterval(() => void refreshIndexStatus(), 1500);
}

function stopIndexStatusPolling(): void {
  if (indexStatusTimer !== null) {
    window.clearInterval(indexStatusTimer);
    indexStatusTimer = null;
  }
}

export async function openChromaSettingsModal() {
  const modal = document.getElementById("chroma-settings-modal");
  if (modal) modal.classList.add("visible");
  // Indexing is a long background job; show live progress instead of making the
  // reader reopen the dialog to see whether anything happened.
  startIndexStatusPolling();
  await loadChromaConfig();
}

export function closeChromaSettingsModal() {
  stopIndexStatusPolling();
  const modal = document.getElementById("chroma-settings-modal");
  if (modal) modal.classList.remove("visible");
}

export async function reindexChroma() {
  void refreshIndexStatus();
  setLoadingWithStatus("", "Re-indexing...");
  let poll: ReturnType<typeof setInterval> | undefined;
  try {
    toastSuccess("Re-indexing started...");
    // Poll live progress while the (potentially long) reindex runs, so the
    // status bar shows movement instead of sitting on "Re-indexing...".
    poll = startChromaProgressPolling("Re-indexing");
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
  void refreshIndexStatus();
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
  // Entering semantic search is also a good moment to catch up on missing
  // website Markdown (fire-and-forget — never blocks the mode switch).
  if (S.searchMode === "semantic") {
    void ensureMarkdownBackfill();
  }
  const searchInput = document.getElementById("search-input") as HTMLInputElement;
  if (searchInput && searchInput.value.trim()) {
    searchItems(searchInput.value);
  }
}

/// One-shot per app session: refresh the website Markdown for articles that
/// were imported from a feed's history without it, so semantic search can
/// match their full text once the background re-index finishes.
///
/// The backend pass is strictly rate-limited (QPS cap, small batch, no
/// retries — see the contract in `chroma/backfill.rs`), so this is safe to
/// fire whenever the search view loads. It runs unawaited: the current
/// search proceeds on whatever is indexed, and later searches see
/// the backfilled articles.
let markdownBackfillTriggered = false;
export async function ensureMarkdownBackfill(): Promise<void> {
  if (markdownBackfillTriggered || !S.chromaEnabled) return;
  markdownBackfillTriggered = true;
  try {
    const report = await chromaApi.backfillMarkdown();
    if (report.already_running) return;
    console.log("[chroma] markdown backfill:", report);
    if (report.fetched > 0) {
      toastInfo(
        `Refreshed website text for ${report.fetched} article${report.fetched === 1 ? "" : "s"} ` +
        `and re-indexed ${report.queued_reindex} for semantic search`,
      );
    }
    // A full batch means the backlog isn't drained — re-arm so the next
    // search-page load triggers another (paced) pass and we keep catching
    // up instead of stalling at 20 articles per app session.
    if (report.more_pending) {
      markdownBackfillTriggered = false;
    }
  } catch (error) {
    console.error("Markdown backfill failed:", error);
    // Allow a retry on the next trigger — a transient failure shouldn't
    // permanently disable the catch-up for this session.
    markdownBackfillTriggered = false;
  }
}
