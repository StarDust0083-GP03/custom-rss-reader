use std::collections::BTreeMap;

use serde::Serialize;
use tauri::State;

use crate::error::Result;
use crate::repositories::TagCatalogEntry;
use crate::services::tag_matcher::{cosine_similarity, TagMatchConfig};

use super::AppState;

/// Similarity threshold for the review-only tag clustering suggestion.
///
/// This is intentionally broad because users choose the canonical head before
/// any data changes. The threshold can be tuned from real tag libraries later.
const TAG_CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.55;

#[derive(Debug, Clone, Serialize)]
pub struct TagClusterResponse {
    pub members: Vec<TagCatalogEntry>,
}

#[tauri::command]
pub async fn get_tag_catalog(state: State<'_, AppState>) -> Result<Vec<TagCatalogEntry>> {
    state.feed_repo.find_tag_catalog().await
}

#[tauri::command]
pub async fn get_blocked_tags(state: State<'_, AppState>) -> Result<Vec<String>> {
    state.feed_repo.find_blocked_tags().await
}

#[tauri::command]
pub async fn create_tag(state: State<'_, AppState>, name: String) -> Result<()> {
    state.feed_repo.create_tag(&name).await
}

#[tauri::command]
pub async fn rename_tag(
    state: State<'_, AppState>,
    old_name: String,
    new_name: String,
) -> Result<()> {
    state.feed_repo.rename_tag(&old_name, &new_name).await
}

#[tauri::command]
pub async fn merge_tags(
    state: State<'_, AppState>,
    canonical_name: String,
    members: Vec<String>,
) -> Result<()> {
    state.feed_repo.merge_tags(&canonical_name, &members).await
}

#[tauri::command]
pub async fn delete_tag(state: State<'_, AppState>, name: String) -> Result<()> {
    state.feed_repo.delete_tag(&name).await
}

#[tauri::command]
pub async fn restore_tag(state: State<'_, AppState>, name: String) -> Result<()> {
    state.feed_repo.restore_tag(&name).await
}

#[tauri::command]
pub async fn get_tag_match_config(state: State<'_, AppState>) -> Result<TagMatchConfig> {
    Ok(state.tag_matcher.config().await)
}

/// Save the automatic tag-matching settings used when AI classification
/// returns names that are not yet in the catalog.
#[tauri::command]
pub async fn set_tag_match_config(
    state: State<'_, AppState>,
    enabled: bool,
    similarity_threshold: f32,
) -> Result<TagMatchConfig> {
    let config = TagMatchConfig {
        enabled,
        similarity_threshold,
    };
    state.tag_matcher.set_config(config.clone()).await?;
    Ok(config)
}

/// Cluster active tag names with the local sentence-embedding model.
///
/// No LLM or ChromaDB server is involved. The result is a suggestion only;
/// the caller must explicitly choose a head and call `merge_tags`.
#[tauri::command]
pub async fn cluster_tags(state: State<'_, AppState>) -> Result<Vec<TagClusterResponse>> {
    let catalog = state.feed_repo.find_tag_catalog().await?;
    if catalog.len() < 2 {
        return Ok(Vec::new());
    }

    let names: Vec<String> = catalog.iter().map(|tag| tag.name.clone()).collect();
    let vectors = state.tag_matcher.embed(&names).await?;

    let groups = cluster_indices(&vectors, TAG_CLUSTER_SIMILARITY_THRESHOLD);
    Ok(groups
        .into_iter()
        .filter(|group| group.len() > 1)
        .map(|group| {
            let mut members: Vec<TagCatalogEntry> = group
                .into_iter()
                .filter_map(|index| catalog.get(index).cloned())
                .collect();
            // Put the most-used name first as a harmless default. The UI still
            // requires the user to choose the actual canonical head.
            members.sort_by(|a, b| {
                b.usage_count
                    .cmp(&a.usage_count)
                    .then_with(|| a.name.cmp(&b.name))
            });
            TagClusterResponse { members }
        })
        .collect())
}

/// Return connected components for pairwise cosine similarity above `threshold`.
fn cluster_indices(vectors: &[Vec<f32>], threshold: f32) -> Vec<Vec<usize>> {
    let mut parents: Vec<usize> = (0..vectors.len()).collect();
    for left in 0..vectors.len() {
        for right in (left + 1)..vectors.len() {
            if cosine_similarity(&vectors[left], &vectors[right]) < threshold {
                continue;
            }
            let left_root = find_root(&mut parents, left);
            let right_root = find_root(&mut parents, right);
            if left_root != right_root {
                parents[right_root] = left_root;
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..vectors.len() {
        let root = find_root(&mut parents, index);
        groups.entry(root).or_default().push(index);
    }
    groups.into_values().collect()
}

fn find_root(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        let root = find_root(parents, parents[index]);
        parents[index] = root;
    }
    parents[index]
}

#[cfg(test)]
mod tests {
    use super::cluster_indices;

    #[test]
    fn clustering_is_transitive_and_leaves_distant_tags_alone() {
        let groups = cluster_indices(&[vec![1.0, 0.0], vec![0.8, 0.6], vec![-1.0, 0.0]], 0.7);
        assert_eq!(groups, vec![vec![0, 1], vec![2]]);
    }
}
