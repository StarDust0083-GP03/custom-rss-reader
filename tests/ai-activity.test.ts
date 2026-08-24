import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { initAiActivity } from "../src/ui/ai-activity";

function snapshot(overrides: Record<string, unknown> = {}) {
  return {
    version: 1,
    task_id: 1,
    phase: "running",
    kind: "translation",
    title: "Article title",
    current: 3,
    total: 12,
    candidate_count: null,
    queue_length: 0,
    started_at_ms: Date.now() - 8_000,
    ...overrides,
  };
}

describe("AI activity status", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <span id="status-ai" role="status" hidden>
        <span id="status-ai-label"><span id="status-ai-kind"></span><span id="status-ai-title"></span></span>
        <span id="status-ai-progress"></span>
        <span id="status-ai-timer"></span>
        <span id="status-ai-queue"></span>
      </span>`;
  });

  afterEach(() => {
    clearMocks();
    document.body.replaceChildren();
  });

  it("hydrates the task and reacts to backend events", async () => {
    mockIPC((cmd) => {
      if (cmd === "get_ai_activity") return snapshot();
      return null;
    }, { shouldMockEvents: true });

    await initAiActivity();

    expect(document.getElementById("status-ai")?.hidden).toBe(false);
    expect(document.getElementById("status-ai-kind")?.textContent).toBe("AI · Translating");
    expect(document.getElementById("status-ai-title")?.textContent).toBe(" · “Article title”");
    expect(document.getElementById("status-ai-progress")?.textContent).toBe("3/12");

    await emit("ai-activity", snapshot({
      version: 2,
      phase: "waiting",
      kind: "recommendations",
      title: null,
      current: null,
      total: null,
      candidate_count: 60,
      queue_length: 2,
    }));

    expect(document.getElementById("status-ai-kind")?.textContent).toBe("AI · Waiting: Picking reads");
    expect(document.getElementById("status-ai-title")?.textContent).toBe(" · 60 articles");
    expect(document.getElementById("status-ai-queue")?.textContent).toBe("queue 2");

    await emit("ai-activity", snapshot({ version: 3, task_id: null, phase: "idle" }));
    expect(document.getElementById("status-ai")?.hidden).toBe(true);
  });
});
