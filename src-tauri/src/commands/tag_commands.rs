use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use tauri::State;

use crate::error::{AppError, Result};
use crate::ai::activity::{with_ai_task, AiTaskSpec};
use crate::models::tag::normalize_tag;
use crate::repositories::TagCatalogEntry;
use crate::services::tag_matcher::{cosine_similarity, TagMatchConfig};

use super::ai_commands::get_ai_service;
use super::AppState;

/// Similarity floor for grouping tag names into one region of the map.
///
/// Used only when shared articles fail to separate the library at all (one
/// community holding nearly every tag). Measured on a real library: 0.55 forms
/// a few dozen regions and leaves the long tail alone, which is what a map
/// needs; lowering it far enough to make one region per subject merges
/// unrelated subjects instead.
const SEMANTIC_REGION_SIMILARITY_THRESHOLD: f32 = 0.55;

/// Identity of the stored dictionary vectors.
///
/// The encoder model plus a format version: changing the model, or how the
/// input text is composed, must recompute the index rather than compare two
/// different vector spaces.
fn dictionary_embedding_key() -> String {
    format!("{}#v1", crate::chroma::embeddings::model_id())
}

/// Vectors for tag names, preferring the dictionary index.
///
/// Definitions give the encoder real semantics, so any name the dictionary
/// covers is grouped by its definition; names it does not cover yet are
/// embedded from the bare name as before.
async fn tag_vectors(state: &AppState, names: &[String]) -> Result<Vec<Vec<f32>>> {
    let stored = state
        .feed_repo
        .find_tag_embeddings(&dictionary_embedding_key())
        .await?;
    let missing: Vec<String> = names
        .iter()
        .filter(|name| !stored.contains_key(name.as_str()))
        .cloned()
        .collect();
    let mut fresh = if missing.is_empty() {
        Vec::new()
    } else {
        state.tag_matcher.embed(&missing).await?
    }
    .into_iter();

    Ok(names
        .iter()
        .map(|name| match stored.get(name) {
            Some(vector) => vector.clone(),
            // `fresh` is consumed in the same order as `missing` was built.
            None => fresh.next().unwrap_or_default(),
        })
        .collect())
}

/// Dictionary coverage for the tag workspace.
#[derive(Debug, Clone, Serialize)]
pub struct TagDictionaryStatus {
    pub tags: i64,
    pub explained: i64,
    pub indexed: i64,
}

/// Progress of one explanation batch.
#[derive(Debug, Clone, Serialize)]
pub struct TagExplanationProgress {
    /// Definitions written by this call.
    pub generated: i64,
    /// Tags still lacking a definition after this call.
    pub remaining: i64,
    pub tags: i64,
}

/// Result of indexing the dictionary.
#[derive(Debug, Clone, Serialize)]
pub struct TagIndexResult {
    pub indexed: i64,
    pub total: i64,
}

#[tauri::command]
pub async fn tag_dictionary_status(state: State<'_, AppState>) -> Result<TagDictionaryStatus> {
    let (tags, explained, indexed) = state.feed_repo.tag_dictionary_status().await?;
    Ok(TagDictionaryStatus {
        tags,
        explained,
        indexed,
    })
}

/// Step 1 of the dictionary: ask the LLM to define the next batch of tags.
///
/// Batched so the caller can show progress and so one slow call cannot run for
/// minutes without feedback. Already-defined tags are skipped, which makes the
/// whole build resumable.
#[tauri::command]
pub async fn generate_tag_explanations(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<TagExplanationProgress> {
    let limit = limit.unwrap_or(crate::ai::EXPLAIN_TAGS_BATCH_SIZE).clamp(1, 200);
    let (tags, _, _) = state.feed_repo.tag_dictionary_status().await?;
    let names = state.feed_repo.find_tags_missing_explanation(limit).await?;
    if names.is_empty() {
        return Ok(TagExplanationProgress {
            generated: 0,
            remaining: 0,
            tags,
        });
    }

    let ai = get_ai_service(&state).await?;
    let explanations = ai.explain_tags(&names).await?;
    let entries: Vec<(String, String)> = explanations
        .into_iter()
        .map(|explanation| (explanation.name, explanation.explanation))
        .collect();
    let generated = entries.len() as i64;
    state
        .feed_repo
        .save_tag_explanations(&entries, crate::ai::TAG_EXPLANATION_PROMPT_VERSION)
        .await?;

    let (_, explained, _) = state.feed_repo.tag_dictionary_status().await?;
    Ok(TagExplanationProgress {
        generated,
        remaining: (tags - explained).max(0),
        tags,
    })
}

/// Step 2 of the dictionary: embed every definition and store the vectors.
///
/// This is what makes the explanations usable — grouping, layout, and matching
/// read the index instead of re-embedding bare names.
#[tauri::command]
pub async fn index_tag_dictionary(state: State<'_, AppState>) -> Result<TagIndexResult> {
    let entries = state.feed_repo.find_tag_explanations().await?;
    if entries.is_empty() {
        return Ok(TagIndexResult {
            indexed: 0,
            total: 0,
        });
    }

    // The definition rides with the name so the vector describes the subject,
    // not the spelling.
    let texts: Vec<String> = entries
        .iter()
        .map(|(name, explanation)| format!("{name} {explanation}"))
        .collect();
    let vectors = state.tag_matcher.embed(&texts).await?;
    let pairs: Vec<(String, Vec<f32>)> = entries
        .iter()
        .zip(vectors)
        .map(|((name, _), vector)| (name.clone(), vector))
        .collect();
    state
        .feed_repo
        .save_tag_embeddings(&dictionary_embedding_key(), &pairs)
        .await?;

    Ok(TagIndexResult {
        indexed: pairs.len() as i64,
        total: entries.len() as i64,
    })
}

/// Two-level Infomap: minimise the map equation by local moving.
///
/// The map equation is the description length of a random walker on the tag
/// co-occurrence graph, so the objective is "which tags does a reader actually
/// move between", not how similar two names look. Modularity was tried first
/// and is a different objective: on a real 384-node library it left a 126-node
/// community of unrelated tags, because a cut is charged only by degree, while
/// the map equation also charges the extra module name a walker must encode.
///
/// Greedy and flat: a node moves to the neighbouring module that shortens the
/// code most, with no hierarchical recursion or annealing. Deterministic by
/// construction: nodes are visited in a fixed order (weighted degree, then
/// name) and only strictly improving moves are accepted.
fn map_equation_modules(nodes: &[String], edges: &[(String, String, i64)]) -> Vec<Vec<String>> {
    const EPSILON: f64 = 1e-9;

    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(position, name)| (name.as_str(), position))
        .collect();

    let mut adjacency: Vec<HashMap<usize, f64>> = vec![HashMap::new(); nodes.len()];
    let mut degree = vec![0.0f64; nodes.len()];
    let mut total_weight = 0.0f64;
    for (left, right, weight) in edges {
        let (Some(&a), Some(&b)) = (index.get(left.as_str()), index.get(right.as_str())) else {
            continue;
        };
        let weight = *weight as f64;
        *adjacency[a].entry(b).or_default() += weight;
        *adjacency[b].entry(a).or_default() += weight;
        degree[a] += weight;
        degree[b] += weight;
        total_weight += weight;
    }
    if total_weight <= 0.0 || nodes.len() < 2 {
        return Vec::new();
    }

    let mut community: Vec<usize> = (0..nodes.len()).collect();
    let mut switch_total = module_terms(&[0], &adjacency, &degree, total_weight).0 * nodes.len() as f64;

    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| {
        degree[b]
            .partial_cmp(&degree[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| nodes[a].cmp(&nodes[b]))
    });

    let members_of = |community: &Vec<usize>, label: usize| -> Vec<usize> {
        (0..community.len())
            .filter(|other| community[*other] == label)
            .collect()
    };

    for _ in 0..MAX_PROPAGATION_ROUNDS {
        let mut improved = false;
        for &node in &order {
            if adjacency[node].is_empty() {
                continue;
            }
            let current = community[node];
            let current_members = members_of(&community, current);
            let (q_current, t_current) =
                module_terms(&current_members, &adjacency, &degree, total_weight);

            let mut candidates: Vec<usize> = adjacency[node]
                .keys()
                .map(|neighbour| community[*neighbour])
                .filter(|candidate| *candidate != current)
                .collect();
            candidates.sort_unstable();
            candidates.dedup();

            let mut best = current;
            let mut best_gain = 0.0;
            for candidate in candidates {
                let mut shrunk = current_members.clone();
                shrunk.retain(|member| *member != node);
                let mut grown = members_of(&community, candidate);
                grown.push(node);

                let (q_candidate, t_candidate) =
                    module_terms(&members_of(&community, candidate), &adjacency, &degree, total_weight);
                let (q_shrunk, t_shrunk) =
                    module_terms(&shrunk, &adjacency, &degree, total_weight);
                let (q_grown, t_grown) = module_terms(&grown, &adjacency, &degree, total_weight);

                // Only these two modules and the global switch probability move.
                let switch_before = switch_total;
                let switch_after = switch_before - q_current - q_candidate + q_shrunk + q_grown;
                let before = -xlogx(q_current) - xlogx(q_candidate) + xlogx(switch_before)
                    - (t_current + t_candidate) / (2.0 * total_weight);
                let after = -xlogx(q_shrunk) - xlogx(q_grown) + xlogx(switch_after)
                    - (t_shrunk + t_grown) / (2.0 * total_weight);

                let gain = before - after;
                if gain > best_gain + EPSILON {
                    best = candidate;
                    best_gain = gain;
                }
            }

            if best != current {
                community[node] = best;
                improved = true;
            }
        }
        if !improved {
            break;
        }
        // Refresh the cached switch probability after a full pass.
        switch_total = (0..nodes.len())
            .map(|node| community[node])
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|label| {
                module_terms(&members_of(&community, label), &adjacency, &degree, total_weight).0
            })
            .sum();
    }

    let mut buckets: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (position, label) in community.iter().enumerate() {
        buckets.entry(*label).or_default().push(nodes[position].clone());
    }
    buckets.into_values().collect()
}

/// `x·log2(x)`, defined as 0 at 0 so empty modules contribute nothing.
fn xlogx(x: f64) -> f64 {
    if x > 0.0 {
        x * x.log2()
    } else {
        0.0
    }
}

/// Map-equation quantities for one module: `(exit flow, internal transitions)`.
///
/// The transition term is `Σ w·log2(w/k_α)` over directed internal moves, which
/// is what makes a module's own code short when the walker stays put.
fn module_terms(
    members: &[usize],
    adjacency: &[HashMap<usize, f64>],
    degree: &[f64],
    total_weight: f64,
) -> (f64, f64) {
    if members.is_empty() || total_weight <= 0.0 {
        return (0.0, 0.0);
    }
    let set: HashSet<usize> = members.iter().copied().collect();
    let mut internal = 0.0;
    let mut transitions = 0.0;
    for &node in members {
        for (&neighbour, &weight) in &adjacency[node] {
            if !set.contains(&neighbour) || weight <= 0.0 {
                continue;
            }
            internal += weight;
            transitions += weight * (weight / degree[node]).log2();
        }
    }
    let internal = internal / 2.0;
    let degree_sum: f64 = members.iter().map(|node| degree[*node]).sum();
    let exit_flow = (degree_sum - 2.0 * internal) / (2.0 * total_weight);
    (exit_flow, transitions)
}

/// Upper bound on refinement rounds. Local moving converges quickly; the cap
/// only stops a pathological oscillation from running forever.
const MAX_PROPAGATION_ROUNDS: usize = 50;

#[tauri::command]
pub async fn get_tag_catalog(state: State<'_, AppState>) -> Result<Vec<TagCatalogEntry>> {
    state.feed_repo.find_tag_catalog().await
}

#[tauri::command]
pub async fn get_blocked_tags(state: State<'_, AppState>) -> Result<Vec<String>> {
    state.feed_repo.find_blocked_tags().await
}

/// One tag on the community map.
#[derive(Debug, Clone, Serialize)]
pub struct TagOverviewNode {
    pub name: String,
    pub usage_count: i64,
    /// Saved topic, when a human decided this word's place.
    pub category_id: Option<i64>,
}

/// One observed relation: two tags that shared articles.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TagOverviewEdge {
    pub source: String,
    pub target: String,
    pub shared_articles: i64,
}

/// One territory of the map.
#[derive(Debug, Clone, Serialize)]
pub struct TagOverviewCommunity {
    /// Only stable within one snapshot: derived from the sorted members, so a
    /// re-count of the same library keeps the same identity while a changed
    /// membership is allowed to produce a different one. Nothing persists it.
    pub id: String,
    pub members: Vec<String>,
    /// Up to three names that say what the territory is about, by weighted
    /// degree. A territory may well span several topics: that is the point of
    /// drawing both encodings at once.
    pub summary_tags: Vec<String>,
    pub article_count: i64,
    /// One level of structure inside this territory.
    ///
    /// A single co-occurrence blob is the normal case on a real library (one
    /// hub tag ties everything together), and drawing 400 dots in one even
    /// field says nothing. Subdividing the induced subgraph gives the inside
    /// of the blob a shape, which is the difference between an overview and a
    /// fishnet.
    pub children: Vec<TagOverviewCommunity>,
}

/// The read-only community map. Everything the view needs, and nothing that
/// changes the library.
#[derive(Debug, Clone, Serialize)]
pub struct TagOverview {
    pub snapshot_id: String,
    pub scope_label: String,
    pub coverage: crate::repositories::TagOverviewCoverage,
    pub nodes: Vec<TagOverviewNode>,
    pub edges: Vec<TagOverviewEdge>,
    pub communities: Vec<TagOverviewCommunity>,
    /// Names with no co-occurrence edge at all. They are drawn as islands
    /// rather than being folded into a fake community.
    pub singletons: Vec<String>,
    /// Blocked names left out of the map, reported instead of disappearing.
    pub blocked_excluded: i64,
    /// Which signal the territories come from: `cooccurrence` when shared
    /// articles already split the library, `semantic` when they do not.
    pub structuring: String,
    pub warnings: Vec<String>,
}

/// Articles only count once per territory, so the map states coverage as
/// distinct articles rather than the sum of its members' counts.
const OVERVIEW_SUMMARY_TAGS: usize = 3;
/// A territory larger than this gets one level of internal structure. Below it
/// the subdivision would produce slivers, which reads as noise rather than
/// structure.
const OVERVIEW_SUBDIVIDE_MIN: usize = 30;
/// Levels of subdivision. One extra level is what turns a 400-member blob into
/// readable regions; deeper recursion would need labels nobody will read and
/// would multiply the per-region count queries.
const OVERVIEW_MAX_DEPTH: usize = 2;
/// When one co-occurrence community holds at least this share of the tags, the
/// partition is describing the library as "everything", which is not an
/// overview. Measured on a 454-tag library: one community held 441 names
/// (97%), and subdividing it produced a 390-name sub-community — the hub
/// structure defeats both. The map then reports articles as edges and uses the
/// dictionary embeddings for the territories instead.
const OVERVIEW_DEGENERATE_SHARE: usize = 65;
const OVERVIEW_DEGENERATE_MIN: usize = 30;

/// How much a frequently-used leader outbids a rarely-used one.
///
/// The boost only chooses among leaders that already clear the similarity
/// threshold; it never lowers that threshold. Usage decides *which* hub a name
/// belongs to, not whether it belongs to one — loosening admission is how a
/// partition turns into one blob again, which is the failure this fallback
/// exists to fix.
const USAGE_LIFT: f64 = 0.06;

/// Leader clustering over tag vectors: a name joins the closest leader it is
/// similar enough to, otherwise it becomes one.
///
/// Leader clustering rather than transitive similarity on purpose: single
/// linkage chains through a hub tag and re-creates the one-blob result this
/// fallback exists to avoid. Deterministic given the input order, which is by
/// usage then name; ties inside one name's candidate set break on leader order,
/// which that same ordering makes stable.
fn semantic_partition(
    names: &[String],
    vectors: &[Vec<f32>],
    usage: &HashMap<String, i64>,
    threshold: f32,
) -> Vec<Vec<String>> {
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|&left, &right| {
        usage
            .get(&names[right])
            .copied()
            .unwrap_or(0)
            .cmp(&usage.get(&names[left]).copied().unwrap_or(0))
            .then_with(|| names[left].cmp(&names[right]))
    });

    let heaviest = order
        .first()
        .map(|index| usage.get(&names[*index]).copied().unwrap_or(0))
        .unwrap_or(1)
        .max(1) as f64;

    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut leaders: Vec<usize> = Vec::new();
    for index in order {
        let vector = &vectors[index];
        let mut best: Option<(usize, f64)> = None;
        for (slot, leader) in leaders.iter().enumerate() {
            let similarity = cosine_similarity(vector, &vectors[*leader]) as f64;
            if similarity < threshold as f64 {
                continue;
            }
            // A tag that carries more articles speaks for more of the library,
            // so it wins a contested name over a tag used once.
            let lift = USAGE_LIFT
                * (usage.get(&names[*leader]).copied().unwrap_or(0) as f64 / heaviest);
            let score = similarity + lift;
            if best.is_none_or(|(_, best_score)| score > best_score) {
                best = Some((slot, score));
            }
        }
        match best {
            Some((slot, _)) => groups[slot].push(names[index].clone()),
            None => {
                leaders.push(index);
                groups.push(vec![names[index].clone()]);
            }
        }
    }
    groups
}

/// Names of one community, ranked so the most telling ones come first.
fn rank_summary(
    members: &[String],
    degree: &HashMap<String, f64>,
    usage: &HashMap<String, i64>,
) -> Vec<String> {
    let mut ranked = members.to_vec();
    ranked.sort_by(|left, right| {
        degree
            .get(right)
            .copied()
            .unwrap_or(0.0)
            .partial_cmp(&degree.get(left).copied().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                usage
                    .get(right)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&usage.get(left).copied().unwrap_or(0))
            })
            .then_with(|| left.cmp(right))
    });
    ranked.truncate(OVERVIEW_SUMMARY_TAGS);
    ranked
}

/// Weighted degree per name: how strongly a node is tied to its neighbours.
/// Used only to pick a territory's summary words, never to rank importance.
fn weighted_degrees(names: &[String], edges: &[(String, String, i64)]) -> HashMap<String, f64> {
    let mut degrees: HashMap<String, f64> = names.iter().map(|name| (name.clone(), 0.0)).collect();
    for (left, right, weight) in edges {
        *degrees.entry(left.clone()).or_default() += *weight as f64;
        *degrees.entry(right.clone()).or_default() += *weight as f64;
    }
    degrees
}

/// Index raw tag membership once so every community count is a set union,
/// rather than another full `feed_items` + `json_each` scan.
fn build_article_ids_by_tag(rows: &[(i64, String)]) -> HashMap<String, HashSet<i64>> {
    let mut index: HashMap<String, HashSet<i64>> = HashMap::new();
    for (item_id, tag) in rows {
        index.entry(tag.clone()).or_default().insert(*item_id);
    }
    index
}

fn count_articles_for_members(
    article_ids_by_tag: &HashMap<String, HashSet<i64>>,
    members: &[String],
) -> i64 {
    let mut item_ids: HashSet<i64> = HashSet::new();
    for member in members {
        if let Some(ids) = article_ids_by_tag.get(member) {
            item_ids.extend(ids);
        }
    }
    item_ids.len() as i64
}

/// The community map: raw-tag co-occurrence, partitioned locally.
///
/// Read-only by construction: it never calls `map_tag`, `set_tag_adopted` or
/// the topic writer, so looking at the map cannot change the library. The
/// partition reuses the same map-equation implementation as the old
/// community command, but keeps singletons and reports coverage, because a
/// map that silently drops most of its nodes is not an overview.
#[tauri::command]
pub async fn get_tag_overview(
    state: State<'_, AppState>,
    subscription_id: Option<i64>,
) -> Result<TagOverview> {
    let repo = state.feed_repo.as_ref();
    let usage = repo.find_raw_tag_usage(subscription_id).await?;
    let edges = repo.find_raw_tag_cooccurrence(subscription_id).await?;
    let raw_tag_items = repo.find_raw_tag_items(subscription_id).await?;
    let article_ids_by_tag = build_article_ids_by_tag(&raw_tag_items);
    let blocked: HashSet<String> = repo.find_blocked_tags().await?.into_iter().collect();
    let coverage = repo.tag_overview_coverage(subscription_id).await?;
    let assignments: HashMap<String, i64> = repo
        .find_topic_assignments()
        .await?
        .into_iter()
        .filter_map(|assignment| assignment.category_id.map(|id| (assignment.tag_name, id)))
        .collect();

    // A blocked name is excluded from the map, but the count is reported: a
    // silent disappearance is exactly the behaviour the tag work was meant to
    // end.
    let blocked_excluded = blocked.iter().filter(|name| usage.contains_key(*name)).count() as i64;

    let mut names: Vec<String> = usage
        .keys()
        .filter(|name| !blocked.contains(*name))
        .cloned()
        .collect();
    names.sort();

    let kept: Vec<(String, String, i64)> = edges
        .into_iter()
        .filter(|(left, right, _)| !blocked.contains(left) && !blocked.contains(right))
        .collect();

    let mut communities: Vec<TagOverviewCommunity> = Vec::new();
    let mut singletons: Vec<String> = Vec::new();
    let mut structuring = "cooccurrence".to_string();
    if names.len() >= 2 {
        let weighted_degree = weighted_degrees(&names, &kept);
        let mut parts = map_equation_modules(&names, &kept);
        let largest = parts.iter().map(Vec::len).max().unwrap_or(0);
        if names.len() > OVERVIEW_DEGENERATE_MIN
            && largest * 100 / names.len().max(1) >= OVERVIEW_DEGENERATE_SHARE
        {
            if let Ok(vectors) = tag_vectors(&state, &names).await {
                if vectors.len() == names.len() {
                    let semantic =
                        semantic_partition(&names, &vectors, &usage, SEMANTIC_REGION_SIMILARITY_THRESHOLD);
                    if semantic.iter().filter(|group| group.len() > 1).count() >= 4 {
                        parts = semantic;
                        structuring = "semantic".to_string();
                    }
                }
            }
        }
        let subdivide_regions = structuring == "cooccurrence";
        for members in parts {
            if members.len() < 2 {
                singletons.extend(members);
                continue;
            }
            let children = if subdivide_regions {
                subdivide(&members, &kept, &weighted_degree, &usage, &article_ids_by_tag, 1)
            } else {
                Vec::new()
            };
            communities.push(TagOverviewCommunity {
                id: crate::models::tag::state_hash(&members),
                summary_tags: rank_summary(&members, &weighted_degree, &usage),
                article_count: count_articles_for_members(&article_ids_by_tag, &members),
                members,
                children,
            });
        }
    } else {
        singletons = names.clone();
    }
    // Biggest territory first: the map's first job is answering "what is most
    // of my library about", and the layout keeps that order stable.
    communities.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| left.summary_tags.cmp(&right.summary_tags))
    });

    let mut warnings = Vec::new();
    if coverage.unreadable_items > 0 {
        warnings.push(format!(
            "{} article(s) have unreadable tag data and are not on the map",
            coverage.unreadable_items
        ));
    }
    if coverage.tagged_items == 0 {
        warnings.push("No article in this scope carries tags yet".to_string());
    }
    if structuring == "semantic" {
        warnings.push(
            "Shared articles do not separate this library (one community held almost every tag), \
             so the territories come from tag meaning instead; the lines on hover still show \
             co-occurrence"
                .to_string(),
        );
    }

    let snapshot_parts: Vec<String> = std::iter::once(format!("scope:{subscription_id:?}"))
        .chain(names.iter().map(|name| format!("{name}:{}", usage.get(name).copied().unwrap_or(0))))
        .chain(kept.iter().map(|(left, right, weight)| format!("{left}|{right}|{weight}")))
        .collect();

    let nodes = names
        .iter()
        .map(|name| TagOverviewNode {
            name: name.clone(),
            usage_count: usage.get(name).copied().unwrap_or(0),
            category_id: assignments.get(name).copied(),
        })
        .collect();
    let edges = kept
        .into_iter()
        .map(|(source, target, shared_articles)| TagOverviewEdge {
            source,
            target,
            shared_articles,
        })
        .collect();

    Ok(TagOverview {
        snapshot_id: crate::models::tag::state_hash(&snapshot_parts),
        scope_label: match subscription_id {
            Some(id) => format!("subscription {id}"),
            None => "all subscriptions".to_string(),
        },
        coverage,
        nodes,
        edges,
        communities,
        singletons,
        blocked_excluded,
        structuring,
        warnings,
    })
}

/// One word as the topic workspace sees it.
#[derive(Debug, Clone, Serialize)]
pub struct TopicWord {
    pub name: String,
    pub usage_count: i64,
    /// `None` means no human decision exists yet. `state` then reports
    /// `undecided`, which is a response-only value: the table stores only the
    /// three decided states, so "not looked at" can never be mistaken for
    /// "reviewed and left without a topic".
    pub category_id: Option<i64>,
    pub state: String,
    pub source: String,
}

/// Everything the topic workspace shows, plus the hash a save must quote.
#[derive(Debug, Clone, Serialize)]
pub struct TopicWorkspace {
    pub categories: Vec<crate::repositories::TopicCategory>,
    pub words: Vec<TopicWord>,
    pub expected_hash: String,
    /// Words with an article record but no human decision yet.
    pub undecided: i64,
}

/// State a save request is checked against. Built from the same inputs the
/// workspace sends, so a save only lands when nothing moved underneath it.
fn topic_state_hash(
    categories: &[crate::repositories::TopicCategory],
    assignments: &[crate::repositories::TopicAssignment],
    words: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for category in categories {
        parts.push(format!(
            "c:{}|{}|{}|{}",
            category.id, category.label, category.definition, category.sort_order
        ));
    }
    for assignment in assignments {
        parts.push(format!(
            "a:{}|{:?}|{}|{}",
            assignment.tag_name, assignment.category_id, assignment.state, assignment.source
        ));
    }
    for word in words {
        parts.push(format!("w:{word}"));
    }
    crate::models::tag::state_hash(&parts)
}

/// Load the workspace from the database; shared by the reader and the writer
/// so a save returns exactly what a reload would show.
async fn load_topic_workspace(state: &State<'_, AppState>) -> Result<TopicWorkspace> {
    let repo = state.feed_repo.as_ref();
    let categories = repo.find_topic_categories().await?;
    let assignments = repo.find_topic_assignments().await?;
    let usage = repo.find_raw_tag_usage(None).await?;
    let catalog = repo.find_tag_catalog().await?;

    let mut names: Vec<String> = usage.keys().cloned().collect();
    for entry in &catalog {
        if !names.contains(&entry.name) {
            names.push(entry.name.clone());
        }
    }
    names.sort();

    let by_name: HashMap<&str, &crate::repositories::TopicAssignment> = assignments
        .iter()
        .map(|assignment| (assignment.tag_name.as_str(), assignment))
        .collect();
    let words: Vec<TopicWord> = names
        .iter()
        .map(|name| match by_name.get(name.as_str()) {
            Some(assignment) => TopicWord {
                name: name.clone(),
                usage_count: usage.get(name).copied().unwrap_or(0),
                category_id: assignment.category_id,
                state: assignment.state.clone(),
                source: assignment.source.clone(),
            },
            None => TopicWord {
                name: name.clone(),
                usage_count: usage.get(name).copied().unwrap_or(0),
                category_id: None,
                state: "undecided".to_string(),
                source: "none".to_string(),
            },
        })
        .collect();

    let undecided = words.iter().filter(|word| word.state == "undecided").count() as i64;
    Ok(TopicWorkspace {
        expected_hash: topic_state_hash(&categories, &assignments, &names),
        categories,
        words,
        undecided,
    })
}

/// The topic catalog, every known word and the current decisions.
#[tauri::command]
pub async fn get_topic_workspace(state: State<'_, AppState>) -> Result<TopicWorkspace> {
    load_topic_workspace(&state).await
}

/// Ceiling on one submitted catalog, mirroring the `≤50` navigation promise.
const MAX_TOPIC_LABEL_CHARS: usize = 80;
const MAX_TOPIC_DEFINITION_CHARS: usize = 400;
const TOPIC_STATES: [&str; 3] = ["assigned", "context_only", "review"];
const TOPIC_SOURCES: [&str; 2] = ["manual", "ai"];

/// Validate a submitted catalog and assignment set.
///
/// Every rejection is a `Validation` error naming the offending entry: the
/// workspace is the only writer of this table, so a malformed request means a
/// client bug, not user input to sanitize silently.
fn validate_topic_changes(
    categories: &[crate::repositories::TopicCategory],
    assignments: &[crate::repositories::TopicAssignment],
) -> Result<()> {
    if categories.is_empty() {
        return Err(AppError::Validation(
            "A topic save must include the catalog".to_string(),
        ));
    }
    let mut seen_ids = HashSet::new();
    let mut seen_labels = HashSet::new();
    for category in categories {
        if !(1..=crate::database::migrations::MAX_TOPIC_ID).contains(&category.id) {
            return Err(AppError::Validation(format!(
                "Topic id {} is outside 1..={}",
                category.id,
                crate::database::migrations::MAX_TOPIC_ID
            )));
        }
        if !seen_ids.insert(category.id) {
            return Err(AppError::Validation(format!(
                "Topic id {} was submitted twice",
                category.id
            )));
        }
        let label = category.label.trim();
        if label.is_empty() {
            return Err(AppError::Validation(format!(
                "Topic {} needs a name",
                category.id
            )));
        }
        if label.chars().count() > MAX_TOPIC_LABEL_CHARS {
            return Err(AppError::Validation(format!(
                "Topic {} name is longer than {} characters",
                category.id, MAX_TOPIC_LABEL_CHARS
            )));
        }
        if category.definition.chars().count() > MAX_TOPIC_DEFINITION_CHARS {
            return Err(AppError::Validation(format!(
                "Topic {} definition is longer than {} characters",
                category.id, MAX_TOPIC_DEFINITION_CHARS
            )));
        }
        if !seen_labels.insert(label.to_lowercase()) {
            return Err(AppError::Validation(format!(
                "Two topics share the name '{}'",
                label
            )));
        }
    }

    let known_ids: HashSet<i64> = categories.iter().map(|category| category.id).collect();
    let mut seen_names = HashSet::new();
    for assignment in assignments {
        let Some(normalized) = normalize_tag(&assignment.tag_name) else {
            return Err(AppError::Validation(format!(
                "'{}' is not a usable tag name",
                assignment.tag_name
            )));
        };
        if normalized != assignment.tag_name {
            return Err(AppError::Validation(format!(
                "'{}' must be submitted as '{normalized}'",
                assignment.tag_name
            )));
        }
        if !seen_names.insert(normalized) {
            return Err(AppError::Validation(format!(
                "'{0}' was submitted twice",
                assignment.tag_name
            )));
        }
        if !TOPIC_STATES.contains(&assignment.state.as_str()) {
            return Err(AppError::Validation(format!(
                "Unknown state '{}' for '{}'",
                assignment.state, assignment.tag_name
            )));
        }
        if !TOPIC_SOURCES.contains(&assignment.source.as_str()) {
            return Err(AppError::Validation(format!(
                "Unknown source '{}' for '{}'",
                assignment.source, assignment.tag_name
            )));
        }
        match (assignment.state.as_str(), assignment.category_id) {
            ("assigned", Some(id)) if known_ids.contains(&id) => {}
            ("assigned", Some(id)) => {
                return Err(AppError::Validation(format!(
                    "'{}' points at unknown topic {id}",
                    assignment.tag_name
                )))
            }
            ("assigned", None) => {
                return Err(AppError::Validation(format!(
                    "'{}' is assigned without a topic",
                    assignment.tag_name
                )))
            }
            (_, Some(_)) => {
                return Err(AppError::Validation(format!(
                    "'{}' is not assigned, so it must not carry a topic",
                    assignment.tag_name
                )))
            }
            (_, None) => {}
        }
    }
    Ok(())
}

/// Commit the catalog and the assignment set in one transaction.
///
/// `expected_hash` is the workspace hash the user was looking at. A mismatch
/// (another window, a background classification, a stale tab) rejects the save
/// and keeps the caller's draft, instead of overwriting decisions nobody saw.
#[tauri::command]
pub async fn apply_topic_changes(
    state: State<'_, AppState>,
    categories: Vec<crate::repositories::TopicCategory>,
    assignments: Vec<crate::repositories::TopicAssignment>,
    expected_hash: String,
) -> Result<TopicWorkspace> {
    validate_topic_changes(&categories, &assignments)?;

    let current = load_topic_workspace(&state).await?;
    if current.expected_hash != expected_hash {
        return Err(AppError::Validation(
            "The tag library changed since this edit was prepared; reload before saving".to_string(),
        ));
    }

    let mut sorted = assignments;
    sorted.sort_by(|left, right| left.tag_name.cmp(&right.tag_name));
    state
        .feed_repo
        .replace_topic_state(&categories, &sorted)
        .await?;
    load_topic_workspace(&state).await
}

/// Partition the subgraph induced by `members`, so a large territory has an
/// inside.
///
/// Returns nothing when the territory is small enough to read as one field, or
/// when its own edges cannot split it — a single region is an honest answer,
/// and forcing a split there would invent structure the articles do not show.
fn subdivide(
    members: &[String],
    edges: &[(String, String, i64)],
    degree: &HashMap<String, f64>,
    usage: &HashMap<String, i64>,
    article_ids_by_tag: &HashMap<String, HashSet<i64>>,
    depth: usize,
) -> Vec<TagOverviewCommunity> {
    if depth >= OVERVIEW_MAX_DEPTH || members.len() <= OVERVIEW_SUBDIVIDE_MIN {
        return Vec::new();
    }
    let inside: HashSet<&str> = members.iter().map(String::as_str).collect();
    let induced: Vec<(String, String, i64)> = edges
        .iter()
        .filter(|(left, right, _)| inside.contains(left.as_str()) && inside.contains(right.as_str()))
        .cloned()
        .collect();
    let mut names = members.to_vec();
    names.sort();
    let parts = map_equation_modules(&names, &induced);
    // No split, or a split that is still one blob: keep the plain field.
    if parts.len() < 2 || parts.len() == 1 && parts[0].len() == names.len() {
        return Vec::new();
    }

    let mut children = Vec::new();
    let mut parts = parts;
    parts.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    for part in parts {
        if part.len() < 2 {
            continue;
        }
        let grandchildren = subdivide(
            &part,
            &induced,
            degree,
            usage,
            article_ids_by_tag,
            depth + 1,
        );
        children.push(TagOverviewCommunity {
            id: crate::models::tag::state_hash(&part),
            summary_tags: rank_summary(&part, degree, usage),
            article_count: count_articles_for_members(article_ids_by_tag, &part),
            members: part,
            children: grandchildren,
        });
    }
    children
}

/// Progress of one topic-suggestion batch.
#[derive(Debug, Clone, Serialize)]
pub struct TopicSuggestionProgress {
    /// Proposals produced by this call, already validated.
    pub suggestions: Vec<crate::ai::TopicSuggestion>,
    /// Words still undecided after this batch.
    pub remaining: i64,
    /// Words the batch considered.
    pub considered: i64,
    /// Words whose category could not be proposed (no answer, or an invalid
    /// one). Reported instead of being silently retried forever.
    pub skipped: i64,
}

/// Ask the model to file the next batch of undecided words into the catalog.
///
/// Nothing is written to the assignment table: the proposals come back to the
/// workspace, which stages them like any other edit, so a suggestion is always
/// something a human saved rather than something the model decided. The batch
/// is bounded ([`crate::ai::TOPIC_SUGGEST_BATCH_SIZE`]) because one call has to
/// finish inside the HTTP timeout; the caller applies one page before asking
/// for the next.
#[tauri::command]
pub async fn suggest_topic_assignments(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<TopicSuggestionProgress> {
    let limit = limit
        .unwrap_or(crate::ai::TOPIC_SUGGEST_BATCH_SIZE)
        .clamp(1, crate::ai::TOPIC_SUGGEST_BATCH_SIZE);
    let workspace = load_topic_workspace(&state).await?;
    let repo = state.feed_repo.as_ref();

    let mut undecided: Vec<&TopicWord> = workspace
        .words
        .iter()
        .filter(|word| word.state == "undecided")
        .collect();
    // Most-used first: a wrong proposal on a hub tag is visible immediately,
    // and the tail is where a sparse tag costs least if it stays unplaced.
    undecided.sort_by(|left, right| {
        right
            .usage_count
            .cmp(&left.usage_count)
            .then_with(|| left.name.cmp(&right.name))
    });
    let remaining_before = undecided.len() as i64;
    if undecided.is_empty() {
        return Ok(TopicSuggestionProgress {
            suggestions: Vec::new(),
            remaining: 0,
            considered: 0,
            skipped: 0,
        });
    }

    let batch: Vec<String> = undecided
        .iter()
        .take(limit as usize)
        .map(|word| word.name.clone())
        .collect();
    let usage: HashMap<String, i64> = workspace
        .words
        .iter()
        .map(|word| (word.name.clone(), word.usage_count))
        .collect();
    let explanations: HashMap<String, String> = repo
        .find_tag_explanations()
        .await?
        .into_iter()
        .collect();

    let catalog: Vec<crate::ai::TopicChoice> = workspace
        .categories
        .iter()
        .map(|category| crate::ai::TopicChoice {
            id: category.id,
            label: category.label.clone(),
            definition: category.definition.clone(),
        })
        .collect();
    // A word's own definition is what makes placement possible; without one the
    // model is guessing from the name, so the missing ones are collected first.
    let mut inputs: Vec<crate::ai::TopicWordInput> = Vec::new();
    for name in &batch {
        let explanation = explanations.get(name).cloned().unwrap_or_default();
        inputs.push(crate::ai::TopicWordInput {
            name: name.clone(),
            explanation,
            usage_count: usage.get(name).copied().unwrap_or(0),
        });
    }

    // Proposals are stamped with the catalog and prompt they were made under,
    // so a reuse is only a reuse while both are unchanged.
    let allowed_ids: Vec<i64> = catalog.iter().map(|choice| choice.id).collect();
    let key = crate::models::tag::state_hash(&[
        format!("prompt:{}", crate::ai::TOPIC_SUGGEST_PROMPT_VERSION),
        catalog
            .iter()
            .map(|choice| format!("{}|{}|{}", choice.id, choice.label, choice.definition))
            .collect::<Vec<_>>()
            .join("\n"),
    ]);

    // A reload, a discard or a retry must not pay the provider again for the
    // same word under the same catalog, so cached proposals are used first.
    let cached = repo.find_topic_suggestions(&batch).await?;
    let mut suggestions: Vec<crate::ai::TopicSuggestion> = Vec::new();
    let mut pending: Vec<crate::ai::TopicWordInput> = Vec::new();
    for input in &inputs {
        let reused = match cached.get(&input.name) {
            Some((json, cached_key)) if cached_key == &key => serde_json::from_str(json)
                .ok()
                .and_then(|candidate| {
                    crate::ai::service::validate_topic_suggestion(candidate, &allowed_ids)
                }),
            _ => None,
        };
        match reused {
            Some(suggestion) => suggestions.push(suggestion),
            None => pending.push(input.clone()),
        }
    }

    if !pending.is_empty() {
        let ai = get_ai_service(&state).await?;
        let task = state
            .ai_activity
            .begin(AiTaskSpec::background_classification(pending.len()))
            .await;
        let result = with_ai_task(task.clone(), ai.suggest_topics(&catalog, &pending)).await;
        task.finish().await;
        let fresh = result?;
        let cache: Vec<(String, String, String)> = fresh
            .iter()
            .filter_map(|suggestion| {
                let json = serde_json::to_string(suggestion).ok()?;
                Some((suggestion.name.clone(), json, key.clone()))
            })
            .collect();
        repo.save_topic_suggestions(&cache).await?;
        suggestions.extend(fresh);
    }

    Ok(TopicSuggestionProgress {
        remaining: (remaining_before - suggestions.len() as i64).max(0),
        considered: batch.len() as i64,
        skipped: (batch.len() as i64 - suggestions.len() as i64).max(0),
        suggestions,
    })
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
    // Optional so a frontend built before these settings existed still saves.
    grouping_method: Option<String>,
    community_min_weight: Option<i64>,
) -> Result<TagMatchConfig> {
    let current = state.tag_matcher.config().await;
    let config = TagMatchConfig {
        enabled,
        similarity_threshold,
        grouping_method: grouping_method.unwrap_or(current.grouping_method),
        community_min_weight: community_min_weight.unwrap_or(current.community_min_weight),
    };
    state.tag_matcher.set_config(config.clone()).await?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        build_article_ids_by_tag, count_articles_for_members, map_equation_modules,
        semantic_partition, topic_state_hash, validate_topic_changes, weighted_degrees,
    };

    #[test]
    fn the_partition_keeps_weak_links_from_fusing_two_groups() {
        // The map asks this for its regions, so the behaviour that matters is
        // that a single shared article cannot join two otherwise separate
        // groups. A chain of weight-1 links must not collapse into one region.
        let nodes: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let edges = vec![
            ("a".to_string(), "b".to_string(), 9),
            ("c".to_string(), "d".to_string(), 9),
            ("b".to_string(), "c".to_string(), 1),
        ];
        let parts = map_equation_modules(&nodes, &edges);
        assert!(
            parts.len() >= 2,
            "a weight-1 bridge must not fuse two tight groups, got {parts:?}"
        );
        let joined: Vec<&String> = parts.iter().flatten().collect();
        assert_eq!(joined.len(), 4, "no tag may be dropped from the partition");
    }
    use crate::repositories::{TopicAssignment, TopicCategory};

    fn category(id: i64, label: &str) -> TopicCategory {
        TopicCategory {
            id,
            label: label.to_string(),
            definition: String::new(),
            sort_order: id,
        }
    }

    fn assignment(name: &str, category_id: Option<i64>, state: &str) -> TopicAssignment {
        TopicAssignment {
            tag_name: name.to_string(),
            category_id,
            state: state.to_string(),
            source: "manual".to_string(),
        }
    }

    #[test]
    fn a_topic_save_is_rejected_until_every_row_agrees_with_itself() {
        let catalog = vec![category(5, "Programming languages")];
        assert!(validate_topic_changes(&catalog, &[assignment("rust", Some(5), "assigned")]).is_ok());
        assert!(validate_topic_changes(&catalog, &[assignment("rust", None, "review")]).is_ok());
        assert!(
            validate_topic_changes(&catalog, &[assignment("opinion", None, "context_only")])
                .is_ok()
        );

        // The four ways a row can lie about itself.
        assert!(validate_topic_changes(&catalog, &[assignment("rust", None, "assigned")]).is_err());
        assert!(
            validate_topic_changes(&catalog, &[assignment("rust", Some(5), "context_only")])
                .is_err()
        );
        assert!(
            validate_topic_changes(&catalog, &[assignment("rust", Some(9), "assigned")]).is_err(),
            "a topic that was not submitted cannot be referenced"
        );
        assert!(validate_topic_changes(&catalog, &[assignment("Rust", None, "review")]).is_err());
        assert!(
            validate_topic_changes(&catalog, &[assignment("rust", None, "undecided")]).is_err(),
            "undecided is what the absence of a row means, never a stored state"
        );
    }

    #[test]
    fn a_topic_catalog_cannot_outgrow_the_navigation_ceiling_or_repeat_a_name() {
        assert!(validate_topic_changes(&[], &[]).is_err(), "a save carries the catalog");
        assert!(validate_topic_changes(&[category(50, "Extra")], &[]).is_err());
        assert!(validate_topic_changes(&[category(0, "Zero")], &[]).is_err());
        assert!(validate_topic_changes(&[category(1, "   ")], &[]).is_err());
        assert!(
            validate_topic_changes(&[category(1, "Databases"), category(2, "databases")], &[])
                .is_err(),
            "two topics named the same would make the navigation ambiguous"
        );
        assert!(validate_topic_changes(&[category(1, "Databases"), category(1, "Dup")], &[]).is_err());
    }

    #[test]
    fn the_save_hash_moves_when_any_read_state_moves() {
        let catalog = vec![category(1, "AI models & training")];
        let words = vec!["deep_learning".to_string()];
        let base = topic_state_hash(&catalog, &[], &words);

        assert_eq!(base, topic_state_hash(&catalog, &[], &words));
        assert_ne!(base, topic_state_hash(&[category(1, "Renamed")], &[], &words));
        assert_ne!(
            base,
            topic_state_hash(&catalog, &[assignment("deep_learning", Some(1), "assigned")], &words)
        );
        // A new word in the library changes the set the user was reviewing.
        assert_ne!(
            base,
            topic_state_hash(&catalog, &[], &["deep_learning".into(), "new_word".into()])
        );
    }

    #[test]
    fn article_count_decides_which_hub_claims_a_name() {
        // Two leaders sit at the same distance from the newcomer, so only the
        // usage term can break the tie: the tag that carries more articles wins.
        let names: Vec<String> = ["hub", "side", "newcomer"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let vectors = vec![
            vec![1.0, 0.0],
            vec![std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2],
            vec![0.9659, 0.2588],
        ];
        let usage: HashMap<String, i64> = [
            ("hub".to_string(), 90),
            ("side".to_string(), 2),
            ("newcomer".to_string(), 1),
        ]
        .into_iter()
        .collect();

        let groups = semantic_partition(&names, &vectors, &usage, 0.7);
        let owner = groups
            .iter()
            .find(|group| group.iter().any(|name| name == "newcomer"))
            .expect("the newcomer is placed somewhere");
        assert!(
            owner.iter().any(|name| name == "hub"),
            "the busier hub should win, got {groups:?}"
        );
    }

    #[test]
    fn usage_never_widens_admission_to_a_region() {
        // Same tie-break, but nothing clears the threshold: a hub must not pull
        // in a tag it is not similar to, or the partition collapses into one
        // region again.
        let names: Vec<String> = ["hub", "stranger"].iter().map(|n| n.to_string()).collect();
        let vectors = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let usage: HashMap<String, i64> =
            [("hub".to_string(), 500), ("stranger".to_string(), 1)].into_iter().collect();
        let groups = semantic_partition(&names, &vectors, &usage, 0.7);
        assert_eq!(groups.len(), 2, "two unrelated tags stay two regions: {groups:?}");
    }

    #[test]
    fn community_article_counts_union_ids_once() {
        let rows = vec![
            (1, "docker".to_string()),
            (1, "containerization".to_string()),
            (2, "docker".to_string()),
        ];
        let index = build_article_ids_by_tag(&rows);
        assert_eq!(count_articles_for_members(&index, &["docker".into()]), 2);
        assert_eq!(
            count_articles_for_members(&index, &["docker".into(), "containerization".into()]),
            2,
            "an article carrying two community tags must count once"
        );
    }

    #[test]
    fn territory_summaries_follow_edge_weight_not_just_article_counts() {
        let names: Vec<String> = ["hub", "spoke_a", "spoke_b"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let edges = vec![
            ("hub".to_string(), "spoke_a".to_string(), 5),
            ("hub".to_string(), "spoke_b".to_string(), 5),
            ("spoke_a".to_string(), "spoke_b".to_string(), 1),
        ];
        let degrees = weighted_degrees(&names, &edges);
        assert_eq!(degrees.get("hub").copied(), Some(10.0));
        assert_eq!(degrees.get("spoke_a").copied(), Some(6.0));
    }

}
