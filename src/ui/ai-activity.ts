import { listen } from "@tauri-apps/api/event";
import { ai as aiApi } from "../api";
import type { AiActivitySnapshot } from "../types";

const KIND_LABEL: Record<string, string> = {
  translation: "Translating",
  classification: "Classifying",
  "background-classification": "Classifying",
  recommendations: "Picking reads",
  "connection-test": "Testing connection",
};

let current: AiActivitySnapshot | null = null;
let lastVersion = -1;
let timer: number | null = null;

function element(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function elapsedSeconds(snapshot: AiActivitySnapshot): number {
  if (!snapshot.started_at_ms) return 0;
  return Math.max(0, Math.floor((Date.now() - snapshot.started_at_ms) / 1000));
}

function formatKind(snapshot: AiActivitySnapshot): string {
  return KIND_LABEL[snapshot.kind] || snapshot.kind || "Working";
}

function formatTitle(snapshot: AiActivitySnapshot): string {
  const title = snapshot.title?.trim();
  if (title) return ` · “${title}”`;
  if (snapshot.candidate_count != null) return ` · ${snapshot.candidate_count} articles`;
  if (snapshot.total != null && snapshot.kind === "background-classification") {
    return ` · ${snapshot.total} items`;
  }
  return "";
}

function render(): void {
  const root = element("status-ai");
  const label = element("status-ai-label");
  const kind = element("status-ai-kind");
  const title = element("status-ai-title");
  const progress = element("status-ai-progress");
  const timerEl = element("status-ai-timer");
  const queue = element("status-ai-queue");
  if (!root || !label || !kind || !title || !progress || !timerEl || !queue) return;

  if (!current || current.phase === "idle" || current.task_id == null) {
    root.hidden = true;
    return;
  }

  root.hidden = false;
  kind.textContent = current.phase === "waiting"
    ? `AI · Waiting: ${formatKind(current)}`
    : `AI · ${formatKind(current)}`;
  title.textContent = formatTitle(current);

  if (current.current != null && current.total != null && current.total > 0) {
    progress.textContent = `${current.current}/${current.total}`;
  } else {
    progress.textContent = "";
  }

  timerEl.textContent = `${elapsedSeconds(current)}s`;
  queue.textContent = current.queue_length > 0 ? `queue ${current.queue_length}` : "";
  root.title = `${kind.textContent}${title.textContent}`;
}

function update(snapshot: AiActivitySnapshot): void {
  if (snapshot.version < lastVersion) return;
  lastVersion = snapshot.version;
  current = snapshot;
  render();

  if (current && current.phase !== "idle" && timer === null) {
    timer = window.setInterval(render, 1000);
  } else if ((!current || current.phase === "idle") && timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

/** Subscribe to backend task events and hydrate once after startup. */
export async function initAiActivity(): Promise<void> {
  try {
    await listen<AiActivitySnapshot>("ai-activity", event => update(event.payload));
    const snapshot = await aiApi.getActivity();
    update(snapshot);
  } catch (error) {
    // Frontend-only Vite mode has no Tauri event bridge. The app remains
    // usable there; the status segment simply stays hidden.
    console.debug("AI activity unavailable:", error);
  }
}
