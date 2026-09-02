/** Tag catalog management, local similarity clustering, and explicit mutations. */

import { tags as tagsApi } from "../api";
import type { TagCatalogEntry, TagCluster, TagMatchConfig } from "../types";
import { clearLoadingStatus, setLoadingWithStatus } from "../ui/status";
import { error as toastError, info as toastInfo, success as toastSuccess } from "../toast";

let catalog: TagCatalogEntry[] = [];
let blocked: string[] = [];
let clusters: TagCluster[] = [];
let editingTag: string | null = null;
let deletingTag: string | null = null;
let clustering = false;
let matchConfig: TagMatchConfig | null = null;

type TagChangeDetail =
  | { kind: "rename"; oldName: string; newName: string | null }
  | { kind: "merge"; canonicalName: string; members: string[] }
  | { kind: "delete"; name: string };

function notifyTagChange(detail: TagChangeDetail) {
  window.dispatchEvent(new CustomEvent<TagChangeDetail>("rss-tags-changed", { detail }));
}

export async function openTagManager() {
  document.getElementById("tag-manager-modal")?.classList.add("visible");
  await refreshTagManager();
}

export function closeTagManager() {
  document.getElementById("tag-manager-modal")?.classList.remove("visible");
}

async function refreshTagManager() {
  clusters = [];
  editingTag = null;
  deletingTag = null;
  try {
    [catalog, blocked] = await Promise.all([tagsApi.catalog(), tagsApi.blocked()]);
    renderCatalog();
    renderMappings();
    renderBlocked();
    renderClusters();
  } catch (error) {
    toastError(`Failed to load tags: ${error}`);
  }
  // Settings are independent of the catalog; a failure here must not hide
  // the tag list, so load them separately.
  try {
    matchConfig = await tagsApi.matchConfig();
    renderMatchConfig();
  } catch (error) {
    toastError(`Failed to load tag matching settings: ${error}`);
  }
}

function matchFormElements() {
  return {
    form: document.getElementById("tag-match-form") as HTMLFormElement | null,
    enabled: document.getElementById("tag-match-enabled") as HTMLInputElement | null,
    threshold: document.getElementById("tag-match-threshold") as HTMLInputElement | null,
    value: document.getElementById("tag-match-threshold-value") as HTMLOutputElement | null,
  };
}

/** Reflect the slider position and enabled state in the form. */
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

export async function saveMatchConfigFromForm() {
  const { enabled, threshold } = matchFormElements();
  if (!enabled || !threshold) return;
  const similarityThreshold = Number(threshold.value);
  if (!Number.isFinite(similarityThreshold)) {
    toastError("Similarity threshold must be a number.");
    return;
  }
  try {
    matchConfig = await tagsApi.setMatchConfig(enabled.checked, similarityThreshold);
    renderMatchConfig();
    toastSuccess(
      matchConfig.enabled
        ? `Generated tags will be matched at ≥ ${matchConfig.similarity_threshold.toFixed(2)} similarity.`
        : "Automatic tag matching disabled.",
    );
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
    empty.textContent = "No mappings yet. Apply a cluster to create one.";
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

function renderClusters() {
  const section = document.getElementById("tag-clusters-section");
  const list = document.getElementById("tag-clusters-list");
  if (!section || !list) return;
  list.replaceChildren();
  section.hidden = clusters.length === 0;
  for (const [index, cluster] of clusters.entries()) {
    const card = document.createElement("div");
    card.className = "tag-cluster-card";
    const heading = document.createElement("h4");
    heading.textContent = `Cluster ${index + 1}`;
    card.appendChild(heading);
    const hint = document.createElement("p");
    hint.className = "tag-cluster-hint";
    hint.textContent = "Choose the subject name to keep. Other members will map to it.";
    card.appendChild(hint);

    const choices = document.createElement("div");
    choices.className = "tag-cluster-choices";
    for (const [memberIndex, member] of cluster.members.entries()) {
      const label = document.createElement("label");
      label.className = "tag-cluster-choice";
      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = `tag-cluster-${index}`;
      radio.value = member.name;
      radio.checked = memberIndex === 0;
      label.append(radio, document.createTextNode(member.name));
      const usage = document.createElement("small");
      usage.textContent = `${member.usage_count} articles`;
      label.appendChild(usage);
      if (member.aliases.length > 0) {
        const aliases = document.createElement("small");
        aliases.textContent = `known: ${member.aliases.join(", ")}`;
        label.appendChild(aliases);
      }
      choices.appendChild(label);
    }
    card.appendChild(choices);
    card.appendChild(button("Apply selected head", "tag-action-button primary", () => void applyCluster(index)));
    list.appendChild(card);
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
    toastSuccess("Tag removed from articles.");
    await refreshTagManager();
    notifyTagChange({ kind: "delete", name });
  } catch (error) {
    toastError(`Could not remove tag: ${error}`);
  }
}

export async function clusterTags() {
  if (clustering) return;
  clustering = true;
  const clusterButton = document.getElementById("cluster-tags-btn") as HTMLButtonElement | null;
  if (clusterButton) {
    clusterButton.disabled = true;
    clusterButton.textContent = "Clustering...";
  }
  setLoadingWithStatus("", "Comparing tag names with local embeddings...");
  try {
    clusters = await tagsApi.cluster();
    renderClusters();
    clearLoadingStatus(true, `Found ${clusters.length} similar tag cluster${clusters.length === 1 ? "" : "s"}`);
    if (clusters.length === 0) toastInfo("No similar tag groups found.");
  } catch (error) {
    clearLoadingStatus(false, "Tag clustering failed");
    toastError(`Could not cluster tags: ${error}`);
  } finally {
    clustering = false;
    if (clusterButton) {
      clusterButton.disabled = false;
      clusterButton.textContent = "Cluster similar tags";
    }
  }
}

async function applyCluster(index: number) {
  const cluster = clusters[index];
  if (!cluster) return;
  const selected = document.querySelector<HTMLInputElement>(
    `input[name="tag-cluster-${index}"]:checked`,
  )?.value;
  if (!selected) return;
  try {
    const members = cluster.members.map(member => member.name);
    await tagsApi.merge(selected, members);
    clusters = clusters.filter((_, clusterIndex) => clusterIndex !== index);
    toastSuccess(`Mapped cluster to ${selected}.`);
    await refreshTagManager();
    notifyTagChange({ kind: "merge", canonicalName: selected, members });
  } catch (error) {
    toastError(`Could not apply cluster: ${error}`);
  }
}

async function restoreTag(name: string) {
  try {
    await tagsApi.restore(name);
    toastSuccess("Tag restored.");
    await refreshTagManager();
  } catch (error) {
    toastError(`Could not restore tag: ${error}`);
  }
}
