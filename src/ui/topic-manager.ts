/**
 * Topic workspace: the editable half of the tag feature.
 *
 * Overview shows what the library looks like; this pane is where a human
 * decides what each word belongs to. The split matters because it is what
 * keeps the two promises the design makes: the map never changes on its own,
 * and a topic edit never rewrites a tag name.
 *
 * Edits are applied immediately. Each change still carries the workspace hash
 * and uses the existing atomic backend writer, so a stale view is rejected
 * instead of silently overwriting another edit.
 */

import { tags as tagsApi } from "../api";
import { error as toastError, success as toastSuccess } from "../toast";
import type {
  TopicAssignment,
  TopicCategory,
  TopicSuggestion,
  TopicWorkspace,
  TopicWord,
} from "../types";

const TOPIC_PAGE_SIZE = 50;
const TOPIC_SUGGEST_BATCH_SIZE = 40;

/** A direct decision for one word, applied immediately. */
export interface TopicDecision {
  category_id: number | null;
  state: TopicWord["state"];
  source: "manual" | "ai";
}

/**
 * Build the complete assignment list for an immediate change.
 *
 * Exported because this is the function that defines the write semantics: a
 * changed word replaces its stored row, and untouched words remain unchanged.
 */
export function mergeAssignments(
  stored: TopicAssignment[],
  decisions: Map<string, TopicDecision>,
): TopicAssignment[] {
  const byName = new Map<string, TopicAssignment>();
  for (const assignment of stored) byName.set(assignment.tag_name, assignment);
  for (const [tagName, decision] of decisions) {
    if (decision.state === "undecided") {
      // Back to "nobody has decided": the row is dropped rather than stored as
      // an empty opinion.
      byName.delete(tagName);
      continue;
    }
    byName.set(tagName, {
      tag_name: tagName,
      category_id: decision.state === "assigned" ? decision.category_id : null,
      state: decision.state as TopicAssignment["state"],
      source: decision.source,
    });
  }
  return [...byName.values()].sort((left, right) => left.tag_name.localeCompare(right.tag_name));
}

/** Words grouped by topic, for the list view. `null` is "no topic". */
export function groupByTopic(
  words: TopicWord[],
  decisions: Map<string, TopicDecision>,
): Map<number | null, TopicWord[]> {
  const groups = new Map<number | null, TopicWord[]>();
  for (const word of words) {
    const decision = decisions.get(word.name);
    const categoryId = decision
      ? decision.state === "assigned"
        ? decision.category_id
        : null
      : word.category_id;
    const bucket = groups.get(categoryId) ?? [];
    bucket.push(word);
    groups.set(categoryId, bucket);
  }
  return groups;
}

export class TopicManager {
  private workspace: TopicWorkspace | null = null;
  private stored: TopicAssignment[] = [];
  private applyQueue: Promise<void> = Promise.resolve();
  private selected: number | null = null;
  private query = "";
  private wordPage = 0;
  private suggestRun = 0;
  private suggestionActive = false;
  private suggestionTotal = 0;
  private suggestionDone = 0;
  private suggestionBatch = 0;
  private suggestionText = "";

  constructor(
    private readonly onSaved: () => void | Promise<void>,
  ) {}

  async load(): Promise<void> {
    try {
      const workspace = await tagsApi.topicWorkspace();
      if (!workspace || !Array.isArray(workspace.categories) || !Array.isArray(workspace.words)) {
        this.setStatus("The topic catalog is unavailable right now.", true);
        return;
      }
      this.setWorkspace(workspace);
    } catch (error) {
      this.setStatus(`Could not load topics: ${error}`, true);
      toastError(`Could not load topics: ${error}`);
    }
  }

  /** Adopt a freshly loaded or freshly saved workspace as the baseline. */
  setWorkspace(workspace: TopicWorkspace): void {
    // A refresh triggered by an applied AI page must not cancel the run or
    // erase its progress. A normal load starts a fresh progress display.
    if (!this.suggestionActive) {
      this.suggestRun += 1;
      this.suggestionTotal = 0;
      this.suggestionDone = 0;
      this.suggestionBatch = 0;
      this.suggestionText = "";
    }
    this.adoptWorkspace(workspace);
  }

  private adoptWorkspace(workspace: TopicWorkspace): void {
    this.workspace = workspace;
    this.stored = workspace.words
      .filter(word => word.state !== "undecided")
      .map(word => ({
        tag_name: word.name,
        category_id: word.category_id,
        state: word.state as TopicAssignment["state"],
        source: word.source === "ai" ? "ai" : "manual",
      }));
    this.wordPage = 0;
    // Open on the bucket that still needs work: a workspace where nothing is
    // decided should show the undecided words, not an empty topic.
    const unassigned = workspace.words.filter(word => word.state === "undecided").length;
    if (this.selected === null || !workspace.categories.some(c => c.id === this.selected)) {
      this.selected = unassigned > 0 ? null : workspace.categories[0]?.id ?? null;
    }
    this.render();
  }

  private decisionsFromSuggestions(suggestions: TopicSuggestion[]): Map<string, TopicDecision> {
    const decisions = new Map<string, TopicDecision>();
    for (const suggestion of suggestions) {
      if (!this.words.some(word => word.name === suggestion.name)) continue;
      decisions.set(suggestion.name, {
        category_id: suggestion.state === "assigned" ? suggestion.category_id : null,
        state: suggestion.state,
        source: "ai",
      });
    }
    return decisions;
  }

  /** Category labels by id, so the overview can name a dot's colour. */
  labels(): Map<number, string> {
    const labels = new Map<number, string>();
    for (const category of this.workspace?.categories ?? []) {
      labels.set(category.id, category.label);
    }
    return labels;
  }

  private get categories(): TopicCategory[] {
    return this.workspace?.categories ?? [];
  }

  private get words(): TopicWord[] {
    return this.workspace?.words ?? [];
  }

  private setStatus(text: string, isError = false): void {
    const status = document.getElementById("topic-status");
    if (!status) return;
    status.textContent = text;
    status.classList.toggle("is-error", isError);
  }

  private enqueueTopicChanges(
    decisions: Map<string, TopicDecision>,
    successMessage: string,
  ): Promise<void> {
    const task = this.applyQueue.then(async () => {
      if (!this.workspace || decisions.size === 0) return;
      this.setStatus("Applying topic changes…");
      try {
        const workspace = await tagsApi.applyTopics(
          this.categories,
          mergeAssignments(this.stored, decisions),
          this.workspace.expected_hash,
        );
        this.adoptWorkspace(workspace);
        await this.onSaved();
        if (successMessage) {
          toastSuccess(successMessage);
          this.setStatus(successMessage);
        }
      } catch (error) {
        this.setStatus(`Not applied: ${error}`, true);
        toastError(`Could not apply topic changes: ${error}`);
        throw error;
      }
    });
    this.applyQueue = task.catch(() => {});
    return task;
  }

  private render(): void {
    const list = document.getElementById("topic-category-list");
    const words = document.getElementById("topic-word-list");
    if (!list || !words) return;

    const groups = groupByTopic(this.words, new Map());
    list.replaceChildren();
    // The unassigned bucket has to be reachable, otherwise the words nobody has
    // decided about are invisible in the one pane that exists to decide them.
    const untopicedRow = this.renderCategoryRow(null, "No topic", (groups.get(null) ?? []).length);
    list.append(untopicedRow);
    for (const category of this.categories) {
      const members = groups.get(category.id) ?? [];
      list.append(this.renderCategoryRow(category.id, category.label, members.length));
    }

    const untopiced = groups.get(null) ?? [];
    const undecidedCount = this.words.filter(word => word.state === "undecided").length;

    const detailTitle = document.getElementById("topic-detail-title");
    const detailCount = document.getElementById("topic-detail-count");
    const selectedCategory = this.categories.find(category => category.id === this.selected);
    const shown = this.selected === null ? untopiced : groups.get(this.selected) ?? [];
    const filtered = shown.filter(
      word => !this.query || word.name.toLowerCase().includes(this.query),
    );
    const totalPages = Math.max(1, Math.ceil(filtered.length / TOPIC_PAGE_SIZE));
    this.wordPage = Math.min(this.wordPage, totalPages - 1);
    const pageWords = filtered.slice(
      this.wordPage * TOPIC_PAGE_SIZE,
      (this.wordPage + 1) * TOPIC_PAGE_SIZE,
    );
    if (detailTitle) detailTitle.textContent = selectedCategory?.label ?? "No topic";
    if (detailCount) {
      const page = totalPages > 1 ? ` · page ${this.wordPage + 1}/${totalPages}` : "";
      detailCount.textContent = `${shown.length} word(s) · ${untopiced.length} without a topic · ${undecidedCount} undecided${page}`;
    }

    words.replaceChildren();
    if (!filtered.length) {
      const empty = document.createElement("p");
      empty.className = "topic-empty";
      empty.textContent = this.query
        ? "No word in this topic matches the search."
        : "Nothing here yet. Words arrive as AI classification produces them.";
      words.append(empty);
    }
    for (const word of pageWords) {
      words.append(this.renderWord(word));
    }
    this.renderPagination(filtered.length, totalPages);

    const status = document.getElementById("topic-status");
    if (status && !this.suggestionActive) {
      const total = this.words.length;
      status.textContent = `${this.categories.length} topics · ${total} word(s) · ${this.workspace?.undecided ?? 0} undecided`;
    }
    const suggest = document.getElementById("topic-suggest") as HTMLButtonElement | null;
    if (suggest) {
      // Nothing to propose once every word has a decision.
      suggest.disabled = this.suggestionActive || undecidedCount === 0;
      suggest.textContent = this.suggestionActive
        ? `Matching topics… batch ${this.suggestionBatch}`
        : undecidedCount
          ? `Suggest topics with AI (${undecidedCount})`
          : "Suggest topics with AI";
    }
    const stop = document.getElementById("topic-suggest-stop") as HTMLButtonElement | null;
    if (stop) {
      stop.hidden = !this.suggestionActive;
      stop.disabled = !this.suggestionActive;
    }
    const progress = document.getElementById("topic-suggest-progress");
    const progressBar = document.getElementById("topic-suggest-progress-bar") as HTMLProgressElement | null;
    const progressLabel = document.getElementById("topic-suggest-progress-label");
    if (progress) progress.hidden = this.suggestionTotal === 0;
    if (progressBar) {
      progressBar.max = Math.max(1, this.suggestionTotal);
      progressBar.value = Math.min(this.suggestionDone, this.suggestionTotal);
    }
    if (progressLabel) {
      progressLabel.textContent = this.suggestionText || `${this.suggestionDone}/${this.suggestionTotal} tags processed`;
    }
  }

  private renderPagination(total: number, totalPages: number): void {
    const pager = document.getElementById("topic-word-pagination");
    if (!pager) return;
    pager.replaceChildren();
    pager.hidden = totalPages <= 1;
    if (totalPages <= 1) return;

    const previous = document.createElement("button");
    previous.type = "button";
    previous.className = "topic-page-button";
    previous.textContent = "Previous";
    previous.disabled = this.wordPage === 0;
    previous.addEventListener("click", () => {
      this.wordPage -= 1;
      this.render();
    });

    const label = document.createElement("span");
    label.className = "topic-page-label";
    label.textContent = `Page ${this.wordPage + 1} of ${totalPages} · ${total} words`;
    label.setAttribute("aria-live", "polite");

    const next = document.createElement("button");
    next.type = "button";
    next.className = "topic-page-button";
    next.textContent = "Next";
    next.disabled = this.wordPage >= totalPages - 1;
    next.addEventListener("click", () => {
      this.wordPage += 1;
      this.render();
    });

    pager.append(previous, label, next);
  }

  private renderCategoryRow(
    id: number | null,
    label: string,
    count: number,
  ): HTMLElement {
    const item = document.createElement("li");
    item.className = "topic-category";
    item.classList.toggle("is-selected", id === this.selected);
    const button = document.createElement("button");
    button.type = "button";
    button.className = "topic-category-button";
    button.addEventListener("click", () => {
      this.selected = id;
      this.wordPage = 0;
      this.render();
    });
    const name = document.createElement("span");
    name.className = "topic-category-name";
    name.textContent = label;
    const count_ = document.createElement("span");
    count_.className = "topic-category-count";
    count_.textContent = String(count);
    button.append(name, count_);
    item.append(button);
    return item;
  }

  private renderWord(word: TopicWord): HTMLElement {
    const state = word.state;
    const categoryId = word.category_id;

    const row = document.createElement("div");
    row.className = "topic-word";

    const name = document.createElement("span");
    name.className = "topic-word-name";
    name.textContent = word.name;
    const usage = document.createElement("span");
    usage.className = "topic-word-usage";
    usage.textContent = `${word.usage_count}`;
    usage.title = `${word.usage_count} article(s)`;

    const select = document.createElement("select");
    select.className = "topic-word-select";
    select.setAttribute("aria-label", `Topic for ${word.name}`);
    const options: { value: string; label: string }[] = [
      { value: "none", label: "No topic" },
      ...this.categories.map(category => ({
        value: String(category.id),
        label: category.label,
      })),
      { value: "context_only", label: "Context only (needs the article)" },
      { value: "undecided", label: "Undecided" },
    ];
    for (const option of options) {
      const element = document.createElement("option");
      element.value = option.value;
      element.textContent = option.label;
      select.append(element);
    }
    select.value =
      state === "assigned" && categoryId ? String(categoryId) : state === "undecided" ? "undecided" : state;
    select.addEventListener("change", () => {
      const value = select.value;
      const decision: TopicDecision = value === "context_only"
        ? { category_id: null, state: "context_only", source: "manual" }
        : value === "undecided"
          ? { category_id: null, state: "undecided", source: "manual" }
          : value === "none"
            ? { category_id: null, state: "review", source: "manual" }
            : { category_id: Number(value), state: "assigned", source: "manual" };
      void this.enqueueTopicChanges(
        new Map([[word.name, decision]]),
        "Topic change applied.",
      ).catch(() => undefined);
    });

    row.append(name, usage, select);
    return row;
  }

  /**
   * Ask the model to file undecided tags, 40 at a time.
   *
   * Each page is applied immediately. The next request reads the updated
   * workspace, so it naturally advances to the next undecided page.
   */
  async suggestWithAi(): Promise<void> {
    if (!this.workspace || this.suggestionActive) return;
    this.suggestionActive = true;
    this.suggestionTotal = this.workspace.undecided;
    this.suggestionDone = 0;
    this.suggestionBatch = 0;
    this.suggestionText = `0/${this.suggestionTotal} tags processed`;
    const run = ++this.suggestRun;
    let totalApplied = 0;
    let completed = false;
    this.render();
    try {
      for (;;) {
        if (run !== this.suggestRun) return;
        this.suggestionBatch += 1;
        const progress = await tagsApi.suggestTopics(TOPIC_SUGGEST_BATCH_SIZE);
        if (run !== this.suggestRun) return;
        const decisions = this.decisionsFromSuggestions(progress.suggestions);
        const matched = decisions.size;
        if (matched > 0) {
          await this.enqueueTopicChanges(decisions, "");
        }
        totalApplied += matched;
        this.suggestionDone = Math.min(
          this.suggestionTotal,
          Math.max(0, this.suggestionTotal - progress.remaining),
        );
        this.suggestionText = `Batch ${this.suggestionBatch}: ${this.suggestionDone}/${this.suggestionTotal} processed · ${progress.remaining} remaining · ${matched} matched and applied`;
        this.setStatus(this.suggestionText);
        this.render();
        if (progress.remaining <= 0 || progress.considered === 0) {
          completed = progress.remaining <= 0;
          break;
        }
        if (progress.suggestions.length === 0) {
          this.setStatus(
            `AI could not place the last ${progress.considered} tag(s); stopping to avoid an endless retry`,
            true,
          );
          break;
        }
      }
      this.suggestionText = completed
        ? `Complete: ${this.suggestionDone}/${this.suggestionTotal} tags processed`
        : `Stopped at ${this.suggestionDone}/${this.suggestionTotal} tags`;
      this.render();
      if (completed) {
        toastSuccess(`AI applied ${totalApplied} topic change(s).`);
        this.setStatus(`${totalApplied} topic change(s) applied by AI`);
      }
    } catch (error) {
      this.suggestionText = `Stopped at ${this.suggestionDone}/${this.suggestionTotal} tags`;
      this.render();
      this.setStatus(`Suggestion run stopped: ${error}`, true);
      toastError(`Could not get topic suggestions: ${error}`);
    } finally {
      this.suggestionActive = false;
      this.render();
    }
  }

  stopSuggesting(): void {
    if (!this.suggestionActive) return;
    this.suggestRun += 1;
    this.suggestionActive = false;
    this.suggestionText = `Stopped at ${this.suggestionDone}/${this.suggestionTotal} tags`;
    this.setStatus(this.suggestionText);
    this.render();
  }

  bind(): void {
    document
      .getElementById("topic-suggest")
      ?.addEventListener("click", () => void this.suggestWithAi());
    document
      .getElementById("topic-suggest-stop")
      ?.addEventListener("click", () => this.stopSuggesting());
    const search = document.getElementById("topic-search") as HTMLInputElement | null;
    search?.addEventListener("input", () => {
      this.query = search.value.trim().toLowerCase();
      this.wordPage = 0;
      this.render();
    });
  }
}
