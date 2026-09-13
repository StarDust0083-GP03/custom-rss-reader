/** Tag vocabulary management, synonym cleanup, and explicit mutations. */

import { tags as tagsApi } from "../api";
import type { TagCatalogEntry, TagMatchConfig } from "../types";
import { error as toastError, success as toastSuccess } from "../toast";

let catalog: TagCatalogEntry[] = [];
let blocked: string[] = [];
let editingTag: string | null = null;
let deletingTag: string | null = null;
let matchConfig: TagMatchConfig | null = null;
let reopenTagGraph = false;

type TagChangeDetail =
  | { kind: "rename"; oldName: string; newName: string | null }
  | { kind: "merge"; canonicalName: string; members: string[] }
  | { kind: "delete"; name: string };

function notifyTagChange(detail: TagChangeDetail) {
  window.dispatchEvent(new CustomEvent<TagChangeDetail>("rss-tags-changed", { detail }));
}

export async function openTagManager() {
  const graph = document.getElementById("tag-graph-modal");
  reopenTagGraph = graph?.classList.contains("visible") ?? false;
  // The manager is a follow-on screen, not a second modal stacked under the
  // graph. Hide the parent while it is open so Escape, focus, and backdrop
  // clicks all belong to one dialog.
  graph?.classList.remove("visible");
  document.getElementById("tag-manager-modal")?.classList.add("visible");
  await refreshTagManager();
}

export function closeTagManager() {
  document.getElementById("tag-manager-modal")?.classList.remove("visible");
  if (reopenTagGraph) {
    reopenTagGraph = false;
    document.getElementById("tag-graph-modal")?.classList.add("visible");
  }
}

/** Refresh the matching-settings form. Shared with the Tags workspace. */
export async function refreshTagMatchConfig() {
  try {
    matchConfig = await tagsApi.matchConfig();
    renderMatchConfig();
  } catch (error) {
    toastError(`Failed to load tag matching settings: ${error}`);
  }
}

async function refreshTagManager() {
  editingTag = null;
  deletingTag = null;
  try {
    [catalog, blocked] = await Promise.all([tagsApi.catalog(), tagsApi.blocked()]);
    renderCatalog();
    renderMappings();
    renderBlocked();
  } catch (error) {
    toastError(`Failed to load tags: ${error}`);
  }
  // Settings are independent of the catalog; a failure here must not hide
  // the tag list.
  await refreshTagMatchConfig();
}

function matchFormElements() {
  return {
    form: document.getElementById("tag-match-form") as HTMLFormElement | null,
    enabled: document.getElementById("tag-match-enabled") as HTMLInputElement | null,
    threshold: document.getElementById("tag-match-threshold") as HTMLInputElement | null,
    value: document.getElementById("tag-match-threshold-value") as HTMLOutputElement | null,
  };
}

/**
 * Reflect the slider position and enabled state in the form.
 *
 * Matching is the only knob left here: grouping used to be configurable in two
 * modes, and the community mode has since become the read-only Overview tab,
 * where the structure is drawn rather than merged. A control that stages merges
 * from a partition the user never asked to apply is worse than no control.
 */
export function syncMatchConfigForm() {
  const { form, enabled, threshold, value } = matchFormElements();
  if (!form || !enabled || !threshold || !value) return;
  value.textContent = Number(threshold.value).toFixed(2);
  threshold.disabled = !enabled.checked;
  form.classList.toggle("disabled", !enabled.checked);
}

function renderMatchConfig() {
  const { enabled, threshold } = matchFormElements();
  if (!matchConfig || !enabled || !threshold) return;
  enabled.checked = matchConfig.enabled;
  threshold.value = matchConfig.similarity_threshold.toFixed(2);
  syncMatchConfigForm();
}

/**
 * Wire the matching-settings form once, at bootstrap.
 *
 * Lives next to the form it configures so the shared-article control's
 * visibility rule is one testable unit instead of being split across main.ts.
 */
export function initMatchSettingsForm(): void {
  document.getElementById("tag-match-threshold")?.addEventListener("input", syncMatchConfigForm);
  document.getElementById("tag-match-enabled")?.addEventListener("change", syncMatchConfigForm);
}

export async function saveMatchConfigFromForm() {
  const { enabled, threshold } = matchFormElements();
  if (!enabled || !threshold) return;
  const similarityThreshold = Number(threshold.value);
  if (!Number.isFinite(similarityThreshold)) {
    toastError("Similarity threshold must be a number.");
    return;
  }
  // The two grouping fields are no longer exposed. They are sent back
  // unchanged so an older library keeps whatever it had stored instead of
  // being silently reset by a form that no longer shows them.
  const groupingMethod = matchConfig?.grouping_method ?? "embedding";
  const communityMinWeight = matchConfig?.community_min_weight ?? 1;
  try {
    matchConfig = await tagsApi.setMatchConfig(
      enabled.checked,
      similarityThreshold,
      groupingMethod,
      communityMinWeight,
    );
    renderMatchConfig();
    const grouping =
      matchConfig.grouping_method === "community"
        ? `Auto-group will detect communities from articles sharing ≥ ${matchConfig.community_min_weight} tag(s).`
        : "Auto-group will compare tag names with the local encoder.";
    toastSuccess(matchConfig.enabled ? grouping : "Automatic tag matching disabled.");
  } catch (error) {
    toastError(`Could not save tag matching settings: ${error}`);
  }
}

function button(text: string, className: string, onClick: () => void) {
  const result = document.createElement("button");
  result.type = "button";
  result.className = className;
  result.textContent = text;
  result.addEventListener("click", onClick);
  return result;
}

function renderCatalog() {
  const list = document.getElementById("tag-catalog-list");
  if (!list) return;
  list.replaceChildren();

  if (catalog.length === 0) {
    const empty = document.createElement("p");
    empty.className = "tag-manager-empty";
    empty.textContent = "No tags yet. Create one or classify an article with AI.";
    list.appendChild(empty);
    return;
  }

  for (const entry of catalog) {
    const row = document.createElement("div");
    row.className = "tag-catalog-row";

    const details = document.createElement("div");
    details.className = "tag-catalog-details";
    const name = document.createElement("code");
    name.className = "tag-catalog-name";
    name.textContent = entry.name;
    details.appendChild(name);

    const usage = document.createElement("span");
    usage.className = "tag-catalog-usage";
    usage.textContent = `${entry.usage_count} article${entry.usage_count === 1 ? "" : "s"}`;
    details.appendChild(usage);

    if (entry.aliases.length > 0) {
      const aliases = document.createElement("div");
      aliases.className = "tag-catalog-aliases";
      aliases.textContent = `mapped from: ${entry.aliases.join(", ")}`;
      details.appendChild(aliases);
    }
    row.appendChild(details);

    const actions = document.createElement("div");
    actions.className = "tag-catalog-actions";
    if (editingTag === entry.name) {
      const input = document.createElement("input");
      input.type = "text";
      input.className = "tag-inline-input";
      input.value = entry.name;
      input.setAttribute("aria-label", `New name for ${entry.name}`);
      actions.appendChild(input);
      actions.appendChild(button("Save", "tag-action-button", () => void renameTag(entry.name, input.value)));
      actions.appendChild(button("Cancel", "tag-action-button muted", () => {
        editingTag = null;
        renderCatalog();
      }));
      window.setTimeout(() => input.focus(), 0);
    } else if (deletingTag === entry.name) {
      const warning = document.createElement("span");
      warning.className = "tag-delete-warning";
      warning.textContent = "Remove from articles?";
      actions.appendChild(warning);
      actions.appendChild(button("Remove", "tag-action-button danger", () => void removeTag(entry.name)));
      actions.appendChild(button("Cancel", "tag-action-button muted", () => {
        deletingTag = null;
        renderCatalog();
      }));
    } else {
      actions.appendChild(button("Rename", "tag-action-button", () => {
        editingTag = entry.name;
        deletingTag = null;
        renderCatalog();
      }));
      actions.appendChild(button("Remove", "tag-action-button danger", () => {
        deletingTag = entry.name;
        editingTag = null;
        renderCatalog();
      }));
    }
    row.appendChild(actions);
    list.appendChild(row);
  }
}

function renderMappings() {
  const list = document.getElementById("tag-mappings-list");
  if (!list) return;
  list.replaceChildren();
  const mappings = catalog.flatMap(entry => entry.aliases.map(alias => ({ alias, head: entry.name })));
  if (mappings.length === 0) {
    const empty = document.createElement("p");
    empty.className = "tag-manager-empty";
    empty.textContent = "No synonym mappings yet.";
    list.appendChild(empty);
    return;
  }
  for (const mapping of mappings) {
    const row = document.createElement("div");
    row.className = "tag-mapping-row";
    const alias = document.createElement("code");
    alias.textContent = mapping.alias;
    const arrow = document.createElement("span");
    arrow.textContent = "→";
    const head = document.createElement("code");
    head.textContent = mapping.head;
    row.append(alias, arrow, head);
    list.appendChild(row);
  }
}

function renderBlocked() {
  const list = document.getElementById("tag-blocked-list");
  if (!list) return;
  list.replaceChildren();
  if (blocked.length === 0) {
    const empty = document.createElement("p");
    empty.className = "tag-manager-empty";
    empty.textContent = "No blocked names.";
    list.appendChild(empty);
    return;
  }
  for (const name of blocked) {
    const row = document.createElement("div");
    row.className = "tag-blocked-row";
    const label = document.createElement("code");
    label.textContent = name;
    row.appendChild(label);
    row.appendChild(button("Restore", "tag-action-button", () => void restoreTag(name)));
    list.appendChild(row);
  }
}

export async function createTagFromForm() {
  const input = document.getElementById("tag-create-name") as HTMLInputElement | null;
  if (!input || !input.value.trim()) return;
  try {
    await tagsApi.create(input.value);
    input.value = "";
    toastSuccess("Tag created.");
    await refreshTagManager();
  } catch (error) {
    toastError(`Could not create tag: ${error}`);
  }
}

async function renameTag(oldName: string, newName: string) {
  try {
    await tagsApi.rename(oldName, newName);
    editingTag = null;
    toastSuccess("Tag renamed.");
    await refreshTagManager();
    const canonical = catalog.find(entry => entry.aliases.includes(oldName))?.name ?? null;
    notifyTagChange({ kind: "rename", oldName, newName: canonical });
  } catch (error) {
    toastError(`Could not rename tag: ${error}`);
  }
}

async function removeTag(name: string) {
  try {
    await tagsApi.remove(name);
    deletingTag = null;
    toastSuccess("Tag hidden from articles and future classification.");
    await refreshTagManager();
    notifyTagChange({ kind: "delete", name });
  } catch (error) {
    toastError(`Could not remove tag: ${error}`);
  }
}

async function restoreTag(name: string) {
  try {
    await tagsApi.restore(name);
    toastSuccess("Tag restored to the vocabulary.");
    await refreshTagManager();
  } catch (error) {
    toastError(`Could not restore tag: ${error}`);
  }
}
