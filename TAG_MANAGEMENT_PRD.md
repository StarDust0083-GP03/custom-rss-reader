# Tag management and canonicalization PRD

**Status:** Approved for implementation  
**Product:** RSS Reader  
**Scope:** Global article-tag vocabulary, local similarity clustering, and tag filtering

## Problem

AI classification currently writes free-form JSON tag arrays directly to `feed_items.tags`. The classifier is asked for one to three tags, but the application does not normalize names, reuse a controlled vocabulary, or preserve relationships between similar names. As the library grows, users can end up with separate tags such as `ai`, `artificial_intelligence`, `machine_learning`, `deep_learning`, `database`, and `postgresql`, which makes filtering unreliable.

The current database snapshot has 250 articles and no tags. Automatic classification is enabled for 265 of 270 subscriptions, but no AI configuration is present on this machine, so classification is currently inactive.

## Goals

1. Keep one global vocabulary for the whole database.
2. Keep tags focused on durable subjects.
3. Have AI generate article tags, while requiring it to reuse the existing vocabulary when possible.
4. Normalize all stored tags to lowercase English `snake_case`.
5. Let users create, rename, merge, delete, and restore tags without AI.
6. Provide a local-embedding action that clusters similar tag names for user review.
7. Preserve approved mappings so future AI output is resolved to the selected cluster head.
8. Make the left-side tag filter searchable and provide a direct path to tag management.

## Non-goals

- No tag-count policy or maximum number of global tags.
- No automatic merge or deletion of *existing* catalog tags based on embedding similarity. Automatic matching applies only to newly generated names that are not yet in the catalog (see "Generated-tag matching").
- No AI call from the tag manager.
- No rejected-pair or exclusion system in the first version.
- No per-subscription vocabularies.
- No format, quality, or temporary tags as a separate taxonomy. The existing category field remains separate.
- No automatic backlog classification command in this release.

## User decisions

- Matching during article classification uses existing tag names.
- The vocabulary is global.
- Similarity clustering includes synonyms and broader related subjects. Examples such as `ai` / `artificial_intelligence`, `machine_learning` / `deep_learning`, and `database` / `postgresql` may appear in one review cluster.
- The user chooses a cluster head. Every other selected member is rewritten to that head.
- A mapping is persisted and displayed later. Future generated names resolve to the cluster head.
- Removing a tag, selecting a cluster head, and renaming a tag are distinct operations.
- Manual creation of an unused tag is supported.
- The filter needs tag-name search and a **Manage tags** action. Counts and extra filter controls are not required.
- Local embeddings are used for two things: the review-only similarity clustering, and silently matching newly generated names onto the catalog. Article tag generation itself remains an AI classification feature.
- Generated-tag matching is silent. The similarity threshold is a user setting in the tag manager, not an implementation constant.

## User experience

### Search and filter

Clicking **Tags** in the left filter row opens an in-DOM picker. The picker contains:

- A search field that filters tag names locally.
- The tags used by the current subscription, or all used tags when viewing all subscriptions.
- A **Manage tags** button.

Selecting a tag keeps the existing subscription scope and loads matching articles. An empty result still shows the management action instead of failing silently.

### Tag manager

The manager is opened from the picker and contains:

1. **Create tag**: enter a subject name and save it as normalized `snake_case`.
2. **Cluster similar tags**: run the existing local multilingual ONNX embedding model against active tag names. The action does not use the configured LLM or ChromaDB server.
3. **Cluster review**: each cluster shows its members, known aliases, and usage information. The user selects one member as the head and applies the mapping.
4. **Tag list**: rename or remove active tags. Removal rewrites articles immediately and places the removed name and its known aliases in the blocked list.
5. **Known mappings**: display `alias -> cluster head` mappings.
6. **Blocked names**: display removed names and allow restoration as unused active tags.
7. **AI tag matching settings**: an on/off switch and a similarity threshold slider (0.50–1.00, default 0.85) that control how aggressively newly generated names are snapped onto existing tags.

All mutations of existing tags are explicit user actions. A cluster result alone never changes article tags.

## Data model

Add three SQLite tables:

```sql
CREATE TABLE tag_catalog (
    name TEXT PRIMARY KEY COLLATE NOCASE,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE tag_aliases (
    alias TEXT PRIMARY KEY COLLATE NOCASE,
    canonical_name TEXT NOT NULL COLLATE NOCASE,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE blocked_tags (
    name TEXT PRIMARY KEY COLLATE NOCASE,
    blocked_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
```

`feed_items.tags` remains a JSON array for compatibility. Migration backfills the catalog from existing arrays, normalizes names, and rewrites legacy arrays. Every write path uses the catalog and mapping tables.

## Tag rules

- Canonical storage form: lowercase ASCII `snake_case`.
- Whitespace, punctuation, and hyphens normalize to underscores; repeated underscores collapse.
- Empty names are rejected. Manual names are bounded to a reasonable input length.
- Article writes deduplicate tags and retain at most three tags per article.
- New, unblocked names returned by AI are inserted into `tag_catalog`.
- Existing aliases resolve to their canonical name before persistence.
- Blocked names are discarded from AI and manual article-tag writes until restored.
- Renaming keeps the old name as an alias of the new name.
- Merging rewrites all affected articles, removes merged catalog rows, and stores each old name as an alias of the selected head.
- Deleting removes the name from articles, deletes its active mapping, and blocks the name and known aliases.

## AI classification contract

The existing classifier remains responsible for generating tags. Its prompt is updated to:

- Treat the catalog supplied by the backend as the preferred vocabulary.
- Reuse an exact catalog name for the same or closely related subject.
- Return a new name only when no catalog name represents the subject.
- Return lowercase English `snake_case` durable subjects.
- Return one to three tags and the existing category.

Manual and batch classification receive current active catalog names from the backend. The repository reconciles every response before saving, so the UI and automatic feed pipeline use the same rules.

## Generated-tag matching

When classification returns a name that, after normalization and alias resolution, is not an active catalog tag:

1. The backend embeds the name and every active catalog name with the local ONNX model (cached per name for the process lifetime).
2. If the best cosine similarity is at or above the configured threshold, the generated name is silently rewritten to that catalog tag and `generated_name -> catalog_tag` is stored in `tag_aliases`. Later occurrences resolve exactly without embedding.
3. Otherwise the name is kept and enters the catalog through the normal `save_tags` path, where it will appear in future cluster suggestions.

The step runs before `save_tags` in both the manual `classify_item` command (so the UI shows the final tags) and the automatic batch classifier. Matching never maps onto blocked names or shadows an active tag, and embedding failures degrade to exact resolution rather than blocking the save. Settings live in `~/.rss-reader/tag_config.json` and apply immediately to the next classification.

## Local clustering contract

The manager fetches active catalog entries, embeds their names with the existing multilingual sentence-transformer model, computes pairwise cosine similarity, and returns connected clusters above a conservative similarity threshold. Clustering is deliberately review-only. The threshold is an implementation constant, not a user setting, and can be tuned after observing real tag data.

The first clustering run may download and load the existing ONNX model. If the model cannot be downloaded or loaded, the manager reports the error and leaves all data unchanged.

## IPC surface

Add typed wrappers and Tauri commands for:

- `get_tag_catalog`
- `get_blocked_tags`
- `create_tag`
- `rename_tag`
- `merge_tags`
- `delete_tag`
- `restore_tag`
- `cluster_tags`
- `get_tag_match_config`
- `set_tag_match_config`

The existing `get_all_tags`, `save_item_tags`, and classification commands remain compatible at the frontend boundary. Their backend behavior becomes canonicalization-aware.

## Verification

Automated checks must cover:

- Migration creates the tag tables and backfills normalized legacy arrays.
- Names normalize to `snake_case` and invalid manual names fail.
- Article tag writes deduplicate, cap at three, resolve aliases, and ignore blocked names.
- Rename rewrites articles and creates an alias.
- Merge rewrites articles, removes merged catalog entries, and maps aliases to the selected head.
- Delete removes tags, blocks names, and restore recreates an unused active tag.
- Catalog and subscription-scoped filter queries return canonical used names.
- Cluster grouping is deterministic for representative vectors, including transitive membership.
- Generated-tag matching snaps names at or above the threshold, persists the alias, skips blocked names, respects the enabled flag, and survives embedder failures.
- Frontend IPC wrappers send the intended command names and camelCase arguments.

Manual acceptance:

1. Configure AI and classify an article. A matching catalog tag is reused.
2. Create two or more tags, cluster them, choose a head, and apply the cluster.
3. Classify another article using an old member name and verify it is stored under the head.
4. Search for a tag from the left picker and verify the article list remains subscription-scoped.
5. Rename, remove, and restore a tag from the manager.
6. Run the manager with no AI configuration. Manual operations and local clustering remain available; article classification reports that AI is not configured.

## Rollback

The feature is additive at the schema level. The old JSON column remains in place. A code rollback leaves the catalog tables and normalized arrays readable by the previous code, but a data rollback is not automatic. Before release, export or copy the SQLite database if reverting after users have merged or deleted tags.
