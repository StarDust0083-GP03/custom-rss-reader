/**
 * Tag workspace shell.
 *
 * Overview owns the community point cloud, Topics owns navigation assignment,
 * and this Tags tab owns the vocabulary tools. The old adopted/category graph
 * has been removed: tags are names, topics are navigation, and the Manage
 * vocabulary dialog handles rename/merge/hide/restore.
 */

import { tags as tagsApi } from "../api";
import { refreshTagMatchConfig } from "../features/tags";
import { clearLoadingStatus, setLoadingWithStatus } from "./status";
import { error as toastError, success as toastSuccess } from "../toast";
import { openTagManager } from "../features/tags";
import { CommunityMap } from "./tag-overview";
import { TopicManager } from "./topic-manager";
import type { TagCatalogEntry, TagDictionaryStatus } from "../types";

const MODAL_ID = "tag-graph-modal";
const EXPLAIN_BATCH = 10;
const EXPLAIN_CALL_TIMEOUT_MS = 150_000;

function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(
      () => reject(new Error(`${label} did not answer within ${Math.round(ms / 1000)}s`)),
      ms,
    );
    promise.then(
      value => {
        window.clearTimeout(timer);
        resolve(value);
      },
      error => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });
}

const state = {
  catalog: [] as TagCatalogEntry[],
  search: "",
  selected: null as string | null,
};
let dictionary: TagDictionaryStatus | null = null;
let dictionaryBusy = false;
let consolidationBusy = false;
let footerMessage: string | null = null;

function $<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function hash(name: string): number {
  let value = 2166136261;
  for (let index = 0; index < name.length; index += 1) {
    value ^= name.charCodeAt(index);
    value = Math.imul(value, 16777619);
  }
  return Math.abs(value);
}

const PALETTE = [
  "#2e6f8e", "#b4552f", "#5f7a3a", "#8a4f7d", "#a8802a", "#3e6b5b",
  "#7a5aa0", "#a34d5e", "#3f7f6f", "#8c6a3f", "#5a6ea8", "#9c5f42",
];

function colorOf(name: string): string {
  return PALETTE[hash(name) % PALETTE.length];
}

function renderStats() {
  const stats = $("tag-graph-stats");
  if (!stats) return;
  const rows: [string, string][] = [
    ["Tags", String(state.catalog.length)],
    ["Never used", String(state.catalog.filter(tag => tag.usage_count === 0).length)],
  ];
  stats.replaceChildren();
  for (const [label, value] of rows) {
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    stats.append(dt, dd);
  }
}

function renderDetail() {
  const detail = $("tag-graph-detail");
  if (!detail) return;
  detail.replaceChildren();
  const tag = state.catalog.find(item => item.name === state.selected);
  if (!tag) {
    const hint = document.createElement("p");
    hint.className = "tag-manager-empty";
    hint.textContent = "Pick a tag from the list to inspect it. Rename and merge tags under Manage vocabulary.";
    detail.append(hint);
    return;
  }
  const title = document.createElement("h3");
  title.textContent = tag.name;
  const facts = document.createElement("dl");
  facts.className = "tag-graph-facts";
  for (const [label, value] of [
    ["Articles", String(tag.usage_count)],
    ["Synonyms", tag.aliases.length ? tag.aliases.join(", ") : "None"],
  ]) {
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    facts.append(dt, dd);
  }
  detail.append(title, facts);
}

function renderList() {
  const list = $("tag-graph-list");
  if (!list) return;
  list.replaceChildren();
  const query = state.search.trim().toLowerCase();
  const visible = state.catalog
    .filter(tag => !query || tag.name.toLowerCase().includes(query) || tag.aliases.some(alias => alias.includes(query)))
    .sort((a, b) => b.usage_count - a.usage_count || a.name.localeCompare(b.name, "zh-CN"));
  if (!visible.length) {
    const empty = document.createElement("p");
    empty.className = "tag-manager-empty";
    empty.textContent = state.catalog.length ? "No matching tags." : "No tags yet. Classify an article with AI.";
    list.append(empty);
    return;
  }
  for (const tag of visible) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "tag-graph-row";
    if (state.selected === tag.name) row.classList.add("is-selected");
    const swatch = document.createElement("span");
    swatch.className = "tag-graph-swatch";
    swatch.style.background = colorOf(tag.name);
    const name = document.createElement("span");
    name.className = "tag-graph-row-name";
    name.textContent = tag.name;
    const meta = document.createElement("small");
    meta.textContent = `${tag.usage_count} article${tag.usage_count === 1 ? "" : "s"}`;
    row.append(swatch, name, meta);
    row.addEventListener("click", () => {
      state.selected = tag.name;
      renderList();
      renderDetail();
    });
    list.append(row);
  }
}

function renderFooter() {
  const status = $("tag-graph-status");
  const subtitle = $("tag-graph-subtitle");
  const consolidate = $<HTMLButtonElement>("tag-consolidate-single-use");
  if (subtitle) subtitle.textContent = `All subscriptions · ${state.catalog.length} tags`;
  if (status) status.textContent = footerMessage ?? "";
  if (consolidate) {
    consolidate.disabled = consolidationBusy || !state.catalog.some(tag => tag.usage_count < 5);
    consolidate.textContent = consolidationBusy ? "Comparing tags…" : "Merge low-use tags";
  }
}

function renderAll() {
  renderStats();
  renderDictionary();
  renderList();
  renderDetail();
  renderFooter();
}

function renderDictionary() {
  const stats = $("tag-graph-dict-stats");
  const build = $<HTMLButtonElement>("tag-dictionary-build");
  if (stats) {
    stats.replaceChildren();
    const rows: [string, string][] = dictionary
      ? [["Tags", String(dictionary.tags)], ["Defined", String(dictionary.explained)], ["Indexed", String(dictionary.indexed)]]
      : [["Status", "unavailable"]];
    for (const [label, value] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = label;
      const dd = document.createElement("dd");
      dd.textContent = value;
      stats.append(dt, dd);
    }
  }
  if (build) {
    build.disabled = dictionaryBusy || dictionary === null || dictionary.tags === 0;
    build.textContent = dictionary && dictionary.indexed >= dictionary.tags ? "Rebuild dictionary" : "Build dictionary";
  }
}

async function loadDictionaryStatus() {
  try {
    const status = await tagsApi.dictionaryStatus();
    dictionary = status && typeof status.tags === "number" ? status : null;
  } catch (error) {
    dictionary = null;
    toastError(`Could not read the tag dictionary: ${error}`);
  }
  renderDictionary();
}

async function buildDictionary() {
  if (dictionaryBusy) return;
  dictionaryBusy = true;
  const progress = $("tag-dictionary-progress");
  const report = (text: string) => {
    if (progress) progress.textContent = text;
  };
  renderDictionary();
  try {
    if (dictionary === null) await loadDictionaryStatus();
    let remaining = dictionary ? Math.max(0, dictionary.tags - dictionary.explained) : 0;
    let explained = dictionary?.explained ?? 0;
    const total = dictionary?.tags ?? 0;
    let batch = 0;
    while (remaining > 0) {
      batch += 1;
      const started = Date.now();
      const tick = () => report(`Generating definitions… ${explained}/${total} · batch ${batch} · ${Math.round((Date.now() - started) / 1000)}s`);
      tick();
      setLoadingWithStatus("", "Generating tag definitions…");
      const timer = window.setInterval(tick, 1000);
      let result;
      try {
        result = await withTimeout(tagsApi.generateExplanations(EXPLAIN_BATCH), EXPLAIN_CALL_TIMEOUT_MS, "the provider");
      } finally {
        window.clearInterval(timer);
      }
      if (result.generated === 0) throw new Error("the model returned no usable definitions");
      explained += result.generated;
      remaining = result.remaining;
      await loadDictionaryStatus();
    }
    report("Indexing definitions…");
    setLoadingWithStatus("", "Indexing tag definitions…");
    const indexed = await tagsApi.indexDictionary();
    clearLoadingStatus(true, `Indexed ${indexed.indexed} tag definitions`);
    report(`Indexed ${indexed.indexed}/${indexed.total}. Semantic map is ready.`);
    toastSuccess(`Dictionary ready: ${indexed.indexed} tags indexed.`);
    await loadDictionaryStatus();
  } catch (error) {
    clearLoadingStatus(false, "Dictionary build failed");
    report("");
    toastError(`Could not build the dictionary: ${error}`);
  } finally {
    dictionaryBusy = false;
    renderDictionary();
  }
}

async function load() {
  try {
    state.catalog = await tagsApi.catalog();
    state.selected = state.catalog.some(tag => tag.name === state.selected) ? state.selected : null;
    renderAll();
  } catch (error) {
    toastError(`Could not load tags: ${error}`);
  }
}

async function consolidateSingleUseTags() {
  if (consolidationBusy) return;
  consolidationBusy = true;
  footerMessage = "Comparing tags used fewer than five times with established vocabulary…";
  renderFooter();
  try {
    const result = await tagsApi.consolidateSingleUse();
    footerMessage = result.merged
      ? `Merged ${result.merged}/${result.candidates} low-use tags · ${result.unmatched} below the similarity threshold`
      : `${result.candidates} low-use tags checked · none were similar enough to merge`;
    toastSuccess(result.merged ? `Merged ${result.merged} low-use tags.` : "No safe low-use tag merges found.");
    await Promise.all([load(), loadTopics(), loadOverview()]);
    window.dispatchEvent(new CustomEvent("rss-tags-changed", { detail: { kind: "consolidate" } }));
  } catch (error) {
    footerMessage = "Low-use tag cleanup failed.";
    toastError(`Could not merge low-use tags: ${error}`);
  } finally {
    consolidationBusy = false;
    renderFooter();
  }
}

let communityMap: CommunityMap | null = null;
let topicManager: TopicManager | null = null;

async function loadTopics() {
  if (!topicManager) {
    topicManager = new TopicManager(async () => {
      await Promise.all([loadOverview(), load()]);
      communityMap?.setTopicLabels(topicManager?.labels() ?? new Map());
    });
    topicManager.bind();
  }
  await topicManager.load();
  communityMap?.setTopicLabels(topicManager.labels());
}

async function loadOverview() {
  if (!communityMap) {
    const canvas = $<HTMLCanvasElement>("tag-map-canvas");
    const search = $<HTMLInputElement>("tag-map-search");
    const results = $("tag-map-results");
    const status = $("tag-map-status");
    const legend = $("tag-map-legend");
    if (!canvas || !search || !results || !status || !legend) return;
    communityMap = new CommunityMap({ canvas, search, results, status, legend });
    communityMap.attach();
    communityMap.setTheme(window.matchMedia?.("(prefers-color-scheme: dark)").matches === true);
    $("tag-map-in")?.addEventListener("click", () => communityMap?.zoom(1.25));
    $("tag-map-out")?.addEventListener("click", () => communityMap?.zoom(0.8));
    $("tag-map-fit")?.addEventListener("click", () => communityMap?.fit());
  }
  communityMap.setTopicLabels(topicManager?.labels() ?? new Map());
  await communityMap.load(null);
}

function showView(view: string) {
  const panels: Record<string, string> = { overview: "tag-view-overview", topics: "tag-view-topics", tags: "tag-view-tags" };
  for (const [name, id] of Object.entries(panels)) document.getElementById(id)?.toggleAttribute("hidden", name !== view);
  document.querySelectorAll<HTMLButtonElement>("[data-tag-view]").forEach(tab => {
    const active = tab.dataset.tagView === view;
    tab.classList.toggle("is-active", active);
    tab.setAttribute("aria-selected", String(active));
  });
  document.querySelectorAll<HTMLElement>("[data-tags-only]").forEach(control => {
    control.hidden = view !== "tags";
  });
  if (view === "overview") communityMap?.fit();
}

export async function openTagGraph() {
  document.getElementById(MODAL_ID)?.classList.add("visible");
  await Promise.all([load(), refreshTagMatchConfig(), loadDictionaryStatus(), loadTopics(), loadOverview()]);
}

export function closeTagGraph() {
  document.getElementById(MODAL_ID)?.classList.remove("visible");
}

export function initTagGraph() {
  const modal = $("tag-graph-modal");
  modal?.querySelector(".close-modal")?.addEventListener("click", closeTagGraph);
  modal?.addEventListener("click", event => {
    if (event.target === event.currentTarget) closeTagGraph();
  });
  $("tag-graph-search")?.addEventListener("input", event => {
    state.search = (event.target as HTMLInputElement).value;
    renderList();
  });
  $("tag-graph-manage")?.addEventListener("click", () => void openTagManager());
  $("tag-consolidate-single-use")?.addEventListener("click", () => void consolidateSingleUseTags());
  $("tag-dictionary-build")?.addEventListener("click", () => void buildDictionary());
  document.querySelectorAll<HTMLButtonElement>("[data-tag-view]").forEach(tab => {
    tab.addEventListener("click", () => showView(tab.dataset.tagView ?? "overview"));
  });
}
