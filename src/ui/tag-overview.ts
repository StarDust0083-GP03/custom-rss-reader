/**
 * Community map: one picture of what the library currently talks about.
 *
 * Two independent encodings share the canvas:
 * - territory (filled area) = a community found by article co-occurrence, its
 *   size following the number of members
 * - dot colour = the saved topic a tag belongs to
 *
 * The map is read-only. Nothing here calls the topic writer, so browsing it
 * cannot change the library. Territories are laid out from the community list
 * alone, while dots use a deterministic weighted spring layout. Saving a topic
 * recolours dots without moving a single border or relationship.
 *
 * Layout is deterministic: same snapshot, same picture.
 */

import { tags as tagsApi } from "../api";
import { error as toastError } from "../toast";
import type {
  TagOverview,
  TagOverviewCommunity,
  TagOverviewEdge,
  TagOverviewNode,
} from "../types";

/** Virtual canvas the map is laid out in; the view scales it to the element. */
export const WORLD = { width: 1240, height: 860 };
/** Land occupies the top of the world; islands sit in the strip below it. */
const LAND = { x: 36, y: 36, width: WORLD.width - 72, height: 700 };
const NODE_MIN_RADIUS = 5;
const NODE_MAX_RADIUS = 12;
const DEFAULT_NODE_GAP = 8;

/**
 * Topic colours, indexed by `category_id - 1`.
 *
 * 49 slots because the navigation ceiling is 50 entries including the virtual
 * "Unsorted" one. Slots are hand-spread rather than generated from one hue
 * ramp: a generated ramp at this count produces neighbours that cannot be told
 * apart even by good eyes. With 49 possibilities no palette is fully
 * distinguishable, so the canvas never relies on colour alone: hovering,
 * searching and the territory labels all name what a dot is.
 */
export const TOPIC_COLORS: { light: string; dark: string }[] = [
  { light: "#5c6b84", dark: "#a9bad6" }, { light: "#8a6274", dark: "#d5adc2" },
  { light: "#6d7f5c", dark: "#b9cba4" }, { light: "#8b7050", dark: "#d6bb96" },
  { light: "#5d7b78", dark: "#a5cbc6" }, { light: "#7b6788", dark: "#c3b2d2" },
  { light: "#855f5c", dark: "#d3aaa5" }, { light: "#62776b", dark: "#abc7b7" },
  { light: "#7f6c55", dark: "#cdb99a" }, { light: "#5f6f8c", dark: "#aebcd9" },
  { light: "#8a6f6a", dark: "#d5b6b0" }, { light: "#6b7d62", dark: "#b7cba9" },
  { light: "#75648f", dark: "#bdb0d8" }, { light: "#87745c", dark: "#d2bd9e" },
  { light: "#5b7d7c", dark: "#a3ccca" }, { light: "#83687c", dark: "#ccb0c6" },
  { light: "#6f7f56", dark: "#c0cd9c" }, { light: "#7d6a86", dark: "#c5b4d1" },
  { light: "#8c665e", dark: "#d8b2a7" }, { light: "#5d767f", dark: "#a6c5cf" },
  { light: "#7a7263", dark: "#c6bdaa" }, { light: "#6a6d92", dark: "#b4b7dd" },
  { light: "#8b6a76", dark: "#d4b1be" }, { light: "#647b6f", dark: "#aec8ba" },
  { light: "#8a7a52", dark: "#d5c39b" }, { light: "#607b88", dark: "#aac7d3" },
  { light: "#80687f", dark: "#c9aec8" }, { light: "#6d8060", dark: "#b9cdaa" },
  { light: "#876c64", dark: "#d0b2a9" }, { light: "#62738a", dark: "#adbdd6" },
  { light: "#7c7a5a", dark: "#c9c69f" }, { light: "#7f6480", dark: "#c8abc9" },
  { light: "#5f7a6d", dark: "#a8c7b8" }, { light: "#8b7550", dark: "#d7bd92" },
  { light: "#6b6a80", dark: "#b6b5cd" }, { light: "#8a6a6c", dark: "#d3b0b2" },
  { light: "#68807a", dark: "#b0cbc4" }, { light: "#7e6d94", dark: "#c4b5dc" },
  { light: "#85735f", dark: "#d0bda4" }, { light: "#5f7c89", dark: "#a9c8d5" },
  { light: "#7b6c60", dark: "#c7b8aa" }, { light: "#6e7e7f", dark: "#b3c9c9" },
  { light: "#8a6d84", dark: "#d3b3cc" }, { light: "#66795f", dark: "#b1c7a8" },
  { light: "#8c7354", dark: "#d8ba95" }, { light: "#61718b", dark: "#acbbd5" },
  { light: "#826b6c", dark: "#cbb0b1" }, { light: "#6c7d68", dark: "#b5c8b0" },
  { light: "#776c8a", dark: "#bfb5d4" },
];

/** Territory fills. Soft on purpose: the dots carry the meaning. */
export const TERRITORY_FILLS: { light: string; dark: string }[] = [
  { light: "#e4e9ed", dark: "#242a30" }, { light: "#e3e9e1", dark: "#242d25" },
  { light: "#ede4e8", dark: "#30262d" }, { light: "#e8e5ee", dark: "#2a2733" },
  { light: "#ede8d9", dark: "#2f2b20" }, { light: "#e0e9e8", dark: "#212f2e" },
];

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface PlacedNode extends TagOverviewNode {
  x: number;
  y: number;
  radius: number;
  /** Label is shown at fit zoom; the rest appear on zoom, hover or search. */
  labeled: boolean;
}

/**
 * One community's patch of the map: a cloud of dots with a soft outline.
 *
 * The layout is a dot distribution, not a filled region: what a reader compares
 * is which dots sit near which — community membership decides the patch, topic
 * decides the colour, and the patch is only a backdrop so the two are readable
 * at once.
 */
export interface PlacedTerritory {
  id: string;
  summary: string;
  members: string[];
  articleCount: number;
  cx: number;
  cy: number;
  r: number;
  /** Closed, slightly irregular outline drawn behind the dots. */
  outline: { x: number; y: number }[];
  titleAnchor: { x: number; y: number };
  titleMaxWidth: number;
  fillIndex: number;
  /** 0 for a community, 1 for a region when a community was subdivided. */
  depth: number;
  children: PlacedTerritory[];
}

export interface Island {
  name: string;
  x: number;
  y: number;
  rx: number;
  ry: number;
}

export interface TagMap {
  territories: PlacedTerritory[];
  islands: Island[];
  nodes: PlacedNode[];
}

/** A dot's semantic position in `[0, 1]`, when the backend computed one. */
export type SemanticPositions = Map<string, { x: number; y: number }>;

/**
 * Width of `text` in the map's label font.
 *
 * A canvas is the only thing that knows the real metrics, and guessing from a
 * character count is how a community's name ends up written over its neighbour.
 * jsdom has no 2-D context, so the estimate is the fallback.
 */
let measureContext: CanvasRenderingContext2D | null | undefined;
const LABEL_FONT = '12px "Public Sans", "Noto Sans CJK SC", sans-serif';

export function labelWidth(text: string): number {
  if (measureContext === undefined) {
    const canvas = typeof document === "undefined" ? null : document.createElement("canvas");
    measureContext = canvas?.getContext("2d") ?? null;
    if (measureContext) measureContext.font = LABEL_FONT;
  }
  if (measureContext) return measureContext.measureText(text).width;
  // No 2-D context: count CJK glyphs as full width, the rest at 6.6px.
  let estimate = 0;
  for (const character of text) estimate += character.charCodeAt(0) > 0x2e80 ? 12 : 6.6;
  return estimate;
}

/** Trim a community summary until it fits, dropping whole words. */
export function fitSummary(summaryTags: string[], maxWidth: number): string {
  const parts = summaryTags.length ? [...summaryTags] : [""];
  while (parts.length > 1 && labelWidth(parts.join(" · ")) > maxWidth) parts.pop();
  return parts.join(" · ");
}

/** A cheap deterministic 0..1 from a string, used for outline jitter only. */
function noise(text: string, salt: number): number {
  let hash = 2166136261 ^ salt;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return ((hash >>> 0) % 1000) / 1000;
}

/**
 * Place one disc per community, largest first, on a phyllotaxis spiral.
 *
 * Deterministic and collision-free by construction: each disc walks outward
 * until it clears every disc already placed, so a big community holds the
 * middle and small ones settle around it. Disc area follows the article count,
 * which is the honest reading of "this community is most of my reading".
 */
export function packClouds(
  weights: number[],
  area: Rect,
  gap = 18,
): { cx: number; cy: number; r: number }[] {
  if (weights.length === 0) return [];
  const total = weights.reduce((sum, weight) => sum + weight, 0);
  // Discs cover about half the canvas; the spiral then spreads them out.
  const usable = area.width * area.height * 0.5;
  const unit = Math.sqrt(usable / (Math.PI * Math.max(1, total)));
  const radii = weights.map(weight => Math.max(26, Math.sqrt(Math.max(1, weight)) * unit));

  // Placement happens in its own coordinate space with no clamping: clamping a
  // candidate into the canvas after the collision test is how two discs end up
  // on top of each other. The finished packing is fitted to the area instead,
  // which scales every distance and radius by the same factor and therefore
  // keeps the gaps it found.
  const placed: { cx: number; cy: number; r: number }[] = [];
  const golden = 2.399963229728653;
  for (const r of radii) {
    let candidate = { cx: 0, cy: 0 };
    let step = 1;
    for (;;) {
      const radius = 1.1 * r * Math.sqrt(step);
      const angle = step * golden;
      candidate = { cx: Math.cos(angle) * radius, cy: Math.sin(angle) * radius * 0.8 };
      const clear = placed.every(
        other => Math.hypot(other.cx - candidate.cx, other.cy - candidate.cy) > other.r + r + gap,
      );
      if (clear) break;
      step += 1;
      // Bounded so a pathological input cannot spin forever. Reaching the cap
      // means the canvas is genuinely too small for this many patches, and the
      // fit below is the honest answer rather than an infinite search.
      if (step > 20_000) break;
    }
    placed.push({ cx: candidate.cx, cy: candidate.cy, r });
  }

  const minX = Math.min(...placed.map(disc => disc.cx - disc.r));
  const maxX = Math.max(...placed.map(disc => disc.cx + disc.r));
  const minY = Math.min(...placed.map(disc => disc.cy - disc.r));
  const maxY = Math.max(...placed.map(disc => disc.cy + disc.r));
  const scale = Math.min(
    3,
    (area.width - 8) / Math.max(1, maxX - minX),
    (area.height - 8) / Math.max(1, maxY - minY),
  );
  const midX = (minX + maxX) / 2;
  const midY = (minY + maxY) / 2;
  return placed.map(disc => ({
    cx: area.x + area.width / 2 + (disc.cx - midX) * scale,
    cy: area.y + area.height / 2 + (disc.cy - midY) * scale,
    r: disc.r * scale,
  }));
}

/** Slightly irregular closed outline for one cloud. Deterministic per id. */
function cloudOutline(cx: number, cy: number, r: number, id: string): { x: number; y: number }[] {
  const points: { x: number; y: number }[] = [];
  const phase = noise(id, 1) * Math.PI * 2;
  const phase2 = noise(id, 2) * Math.PI * 2;
  const steps = 44;
  for (let index = 0; index < steps; index += 1) {
    const angle = (index / steps) * Math.PI * 2;
    const wobble =
      1 + 0.09 * Math.sin(3 * angle + phase) + 0.055 * Math.sin(5 * angle + phase2);
    points.push({ x: cx + Math.cos(angle) * r * wobble, y: cy + Math.sin(angle) * r * wobble });
  }
  return points;
}

/** Distance from a point to the cloud centre, against its wobbled radius. */
export function insideCloud(node: { x: number; y: number }, cloud: PlacedTerritory): boolean {
  const distance = Math.hypot(node.x - cloud.cx, node.y - cloud.cy);
  return distance <= cloud.r * 0.92;
}

export function nodeGap(left: number, right: number): number {
  return left + right + DEFAULT_NODE_GAP;
}

/** Keep a dot inside its cloud without collapsing it onto the centre. */
export function pullIntoCloud(node: PlacedNode, cloud: { cx: number; cy: number; r: number }): void {
  const limit = cloud.r * 0.86;
  const dx = node.x - cloud.cx;
  const dy = node.y - cloud.cy;
  const distance = Math.hypot(dx, dy);
  if (distance <= limit) return;
  const scale = limit / (distance || 1);
  node.x = cloud.cx + dx * scale;
  node.y = cloud.cy + dy * scale;
}

/**
 * Deterministic force layout for one community.
 *
 * Shared-article edges are springs: a larger weight shortens the target
 * distance and increases the pull. Pairwise repulsion keeps labels and dots
 * from collapsing into one point. The bounded iteration count is deliberate,
 * because this is a read-only canvas layout, not a continuously animated graph.
 */
export function springLayout(
  names: string[],
  edges: TagOverviewEdge[],
  bounds: { cx: number; cy: number; r: number },
  initial: SemanticPositions = new Map(),
): Map<string, { x: number; y: number }> {
  if (names.length === 0) return new Map();
  if (names.length === 1) return new Map([[names[0], { x: bounds.cx, y: bounds.cy }]]);

  const index = new Map(names.map((name, position) => [name, position]));
  const spacing = Math.max(14, Math.min(34, (bounds.r / Math.sqrt(names.length)) * 1.8));
  const points = names.map((name, position) => {
    const seed = initial.get(name);
    if (seed) {
      return {
        x: bounds.cx + (seed.x - 0.5) * bounds.r * 1.2,
        y: bounds.cy + (seed.y - 0.5) * bounds.r * 1.2,
      };
    }
    const angle = position * 2.399963229728653;
    const ratio = Math.sqrt((position + 0.55) / names.length);
    return {
      x: bounds.cx + Math.cos(angle) * ratio * bounds.r * 0.62,
      y: bounds.cy + Math.sin(angle) * ratio * bounds.r * 0.62,
    };
  });
  const springs = edges
    .map(edge => ({
      left: index.get(edge.source),
      right: index.get(edge.target),
      weight: Math.max(1, edge.shared_articles),
    }))
    .filter(
      (edge): edge is { left: number; right: number; weight: number } =>
        edge.left !== undefined && edge.right !== undefined && edge.left !== edge.right,
    );
  const maxWeight = Math.max(1, ...springs.map(edge => edge.weight));
  const limit = bounds.r * 0.78;

  for (let iteration = 0; iteration < 48; iteration += 1) {
    const forces = points.map(() => ({ x: 0, y: 0 }));
    const repel = (left: number, right: number) => {
      let dx = points[right].x - points[left].x;
      let dy = points[right].y - points[left].y;
      let distance = Math.hypot(dx, dy);
      if (distance < 0.001) {
        const angle = noise(names[left] + names[right], iteration + 17) * Math.PI * 2;
        dx = Math.cos(angle) * 0.001;
        dy = Math.sin(angle) * 0.001;
        distance = 0.001;
      }
      const repulsion = (spacing * spacing * 2.2) / (distance * distance);
      const fx = (dx / distance) * repulsion;
      const fy = (dy / distance) * repulsion;
      forces[left].x -= fx;
      forces[left].y -= fy;
      forces[right].x += fx;
      forces[right].y += fy;
    };
    if (points.length <= 256) {
      for (let left = 0; left < points.length; left += 1) {
        for (let right = left + 1; right < points.length; right += 1) repel(left, right);
      }
    } else {
      // Full pairwise repulsion freezes the WebView on real libraries. A
      // deterministic rolling neighbourhood bounds each iteration while the
      // complete weighted edge list still supplies the graph structure.
      const neighbours = Math.min(32, points.length - 1);
      for (let left = 0; left < points.length; left += 1) {
        for (let offset = 1; offset <= neighbours; offset += 1) {
          repel(left, (left + offset) % points.length);
        }
      }
    }

    for (const spring of springs) {
      const from = points[spring.left];
      const to = points[spring.right];
      const dx = to.x - from.x;
      const dy = to.y - from.y;
      const distance = Math.max(0.001, Math.hypot(dx, dy));
      const strength = 0.022 * (0.55 + Math.sqrt(spring.weight / maxWeight));
      const ideal = spacing + 18 + (1 - Math.sqrt(spring.weight / maxWeight)) * Math.min(72, bounds.r * 0.42);
      const pull = (distance - ideal) * strength;
      const fx = (dx / distance) * pull;
      const fy = (dy / distance) * pull;
      forces[spring.left].x += fx;
      forces[spring.left].y += fy;
      forces[spring.right].x -= fx;
      forces[spring.right].y -= fy;
    }

    for (const [position, point] of points.entries()) {
      const step = Math.min(10, 0.72 + iteration * 0.02);
      point.x = Math.max(bounds.cx - limit, Math.min(bounds.cx + limit, point.x + forces[position].x * step));
      point.y = Math.max(bounds.cy - limit, Math.min(bounds.cy + limit, point.y + forces[position].y * step));
    }
  }

  return new Map(names.map((name, position) => [name, points[position]]));
}

/**
 * Build the map: communities become clouds, dots carry the topics.
 *
 * `positions` are optional seeds for the weighted spring layout. Shared-article
 * edges decide the final neighbourhoods, so the map still has an honest
 * structure when the semantic encoder is unavailable.
 */
export function buildMap(overview: TagOverview, positions: SemanticPositions = new Map()): TagMap {
  // An IPC response is a trust boundary: a missing or renamed field must
  // degrade a patch, not blank the whole map.
  const byName = new Map((overview.nodes ?? []).map(node => [node.name, node]));
  const normalize = (community: TagOverviewCommunity): NormalizedCommunity => ({
    id: community.id,
    members: community.members ?? [],
    summary_tags: community.summary_tags?.length
      ? community.summary_tags
      : (community.members ?? []).slice(0, 3),
    article_count: Math.max(1, community.article_count ?? 0),
    children: (community.children ?? []).map(normalize),
  });
  const communities = (overview.communities ?? []).map(normalize);
  communities.sort(
    (left, right) =>
      right.members.length - left.members.length ||
      left.summary_tags.join("|").localeCompare(right.summary_tags.join("|")),
  );

  const cells = packClouds(
    communities.map(community => Math.max(1, community.article_count)),
    LAND,
  );

  const nodes: PlacedNode[] = [];
  const territories: PlacedTerritory[] = [];
  communities.forEach((community, index) => {
    const cell = cells[index];
    if (!cell) return;
    territories.push(
      placeCloud(
        community,
        cell,
        index % TERRITORY_FILLS.length,
        0,
        byName,
        positions,
        overview.edges ?? [],
        nodes,
      ),
    );
  });

  // Islands: names that ended up alone. They are deliberately not collected
  // into a community, because "nothing sits near it" is not a finding about the
  // subject matter — but they stay on the map, in a compact band, so a long
  // tail of unrelated tags is still visible.
  const lone = [...(overview.singletons ?? [])].sort();
  const stepX = 46;
  const stepY = 40;
  const columns = Math.max(1, Math.floor((WORLD.width - 60) / stepX));
  const islands: Island[] = lone.map((name, index) => ({
    name,
    x: 34 + (index % columns) * stepX + 12,
    y: LAND.y + LAND.height + 26 + Math.floor(index / columns) * stepY,
    rx: 15,
    ry: 10,
  }));
  for (const island of islands) {
    const node = byName.get(island.name);
    if (!node) continue;
    nodes.push({
      ...node,
      x: island.x,
      y: island.y - 2,
      radius: NODE_MIN_RADIUS,
      labeled: islands.length <= 24,
    });
  }

  return { territories, islands, nodes };
}

interface NormalizedCommunity {
  id: string;
  members: string[];
  summary_tags: string[];
  article_count: number;
  children: NormalizedCommunity[];
}

/** Fill one cloud with its dots, subdividing it first when it has regions. */
function placeCloud(
  community: NormalizedCommunity,
  cell: { cx: number; cy: number; r: number },
  fillIndex: number,
  depth: number,
  byName: Map<string, TagOverviewNode>,
  positions: SemanticPositions,
  edges: TagOverviewEdge[],
  nodes: PlacedNode[],
): PlacedTerritory {
  const titleMaxWidth = Math.max(40, cell.r * 1.7);
  const summaryText = fitSummary(community.summary_tags, titleMaxWidth);
  const cloud: PlacedTerritory = {
    id: community.id,
    summary: summaryText,
    members: community.members,
    articleCount: community.article_count,
    cx: cell.cx,
    cy: cell.cy,
    r: cell.r,
    outline: cloudOutline(cell.cx, cell.cy, cell.r, community.id),
    titleAnchor: { x: cell.cx, y: cell.cy - cell.r * 0.74 },
    titleMaxWidth,
    fillIndex,
    depth,
    children: [],
  };

  const ordered = [...community.members].sort((left, right) => {
    const leftUsage = byName.get(left)?.usage_count ?? 0;
    const rightUsage = byName.get(right)?.usage_count ?? 0;
    return rightUsage - leftUsage || left.localeCompare(right);
  });

  // Seed the spring system from the optional semantic projection, then let
  // shared-article weights decide the final neighbourhoods.
  const layout = springLayout(ordered, edges, cell, positions);
  ordered.forEach((name, position) => {
    const node = byName.get(name);
    if (!node) return;
    const point = layout.get(name) ?? { x: cell.cx, y: cell.cy };
    const placed: PlacedNode = {
      ...node,
      x: point.x,
      y: point.y,
      radius: Math.min(
        NODE_MAX_RADIUS,
        NODE_MIN_RADIUS + Math.sqrt(Math.max(1, node.usage_count)) * 0.44,
      ),
      labeled: position < 3,
    };
    pullIntoCloud(placed, cell);
    nodes.push(placed);
  });

  const regions = community.children.filter(child => child.members.length > 1);
  if (regions.length > 1 && depth === 0) {
    // Subdivision keeps the same dots but draws the inside structure: the
    // regions partition the cloud rather than moving it.
    regions.forEach((child, index) => {
      const angle = (index / regions.length) * Math.PI * 2;
      const childRadius = Math.max(30, cell.r * (0.42 + 0.14 * (index % 2)));
      const childCell = {
        cx: cell.cx + Math.cos(angle) * (cell.r - childRadius) * 0.55,
        cy: cell.cy + Math.sin(angle) * (cell.r - childRadius) * 0.42,
        r: childRadius,
      };
      cloud.children.push(
        placeCloud(child, childCell, fillIndex, depth + 1, byName, positions, edges, nodes),
      );
    });
  }

  return cloud;
}

export function nodeColor(
  node: TagOverviewNode,
  dark: boolean,
  unassigned: string,
): string {
  if (!node.category_id) return unassigned;
  const slot = TOPIC_COLORS[(node.category_id - 1) % TOPIC_COLORS.length];
  return dark ? slot.dark : slot.light;
}

export function territoryFill(index: number, dark: boolean): string {
  const slot = TERRITORY_FILLS[index % TERRITORY_FILLS.length];
  return dark ? slot.dark : slot.light;
}

// ---------------------------------------------------------------------------
// Rendering and interaction
// ---------------------------------------------------------------------------

const MIN_ZOOM = 0.5;
const MAX_ZOOM = 5;

interface View {
  scale: number;
  x: number;
  y: number;
}

export interface OverviewElements {
  canvas: HTMLCanvasElement;
  search: HTMLInputElement;
  results: HTMLElement;
  status: HTMLElement;
  legend: HTMLElement;
}

export class CommunityMap {
  private map: TagMap | null = null;
  private overview: TagOverview | null = null;
  private view: View = { scale: 1, x: 0, y: 0 };
  private fitScale = 1;
  private width = 1;
  private height = 1;
  private dpr = 1;
  private dark = false;
  private focus: number | null = null;
  private hover: number | null = null;
  private query = "";
  private matches: PlacedNode[] = [];
  private resultIndex = -1;
  private resultsOpen = false;
  private pointers = new Map<number, { x: number; y: number }>();
  private gesture: { kind: "pan"; last: { x: number; y: number }; origin: { x: number; y: number } } | { kind: "pinch"; distance: number; center: { x: number; y: number } } | null = null;
  private dragged = false;
  private tip: HTMLElement | null = null;
  private topicLabels = new Map<number, string>();

  constructor(private readonly elements: OverviewElements) {}

  setTopicLabels(labels: Map<number, string>): void {
    this.topicLabels = labels;
    this.draw();
  }

  async load(subscriptionId?: number | null): Promise<void> {
    try {
      const overview = await tagsApi.overview(subscriptionId);
      if (!overview || !Array.isArray(overview.nodes) || !Array.isArray(overview.communities)) {
        // A backend that cannot answer yet must not white-screen the modal.
        this.elements.status.textContent = "The community map is unavailable right now.";
        return;
      }
      this.setData(overview);
    } catch (error) {
      this.elements.status.textContent = `Could not load the map: ${error}`;
      toastError(`Could not load the community map: ${error}`);
    }
  }

  setData(overview: TagOverview): void {
    this.overview = overview;
    this.map = buildMap(overview);
    this.focus = null;
    this.hover = null;
    this.query = "";
    this.elements.search.value = "";
    this.matches = [];
    this.elements.status.textContent = this.describe(overview);
    this.elements.legend.textContent = overview.warnings.join(" · ");
    this.fit();
  }

  private describe(overview: TagOverview): string {
    const { coverage, communities, singletons, nodes } = overview;
    const territories = communities.length;
    const share = coverage.total_items
      ? Math.round((coverage.tagged_items / coverage.total_items) * 100)
      : 0;
    const blocked = overview.blocked_excluded
      ? ` · ${overview.blocked_excluded} blocked name(s) hidden`
      : "";
    const grouping = overview.structuring === "semantic" ? "grouped by tag meaning" : "grouped by shared articles";
    return [
      overview.scope_label,
      `${nodes.length} tags`,
      `${territories} communities`,
      singletons.length ? `${singletons.length} islands` : null,
      grouping,
      `${coverage.tagged_items}/${coverage.total_items} articles tagged (${share}%)`,
    ]
      .filter(Boolean)
      .join(" · ") + blocked;
  }

  attach(): void {
    const { canvas } = this.elements;
    canvas.addEventListener("pointerdown", event => this.onPointerDown(event));
    canvas.addEventListener("pointermove", event => this.onPointerMove(event));
    canvas.addEventListener("pointerup", event => this.onPointerUp(event));
    canvas.addEventListener("pointercancel", event => this.onPointerUp(event, true));
    canvas.addEventListener("pointerleave", () => {
      this.hover = null;
      this.draw();
    });
    canvas.addEventListener(
      "wheel",
      event => {
        event.preventDefault();
        const point = this.coords(event);
        const delta = Math.max(-120, Math.min(120, event.deltaY));
        this.zoom(Math.exp(-delta * 0.002), point.x, point.y);
      },
      { passive: false },
    );
    canvas.addEventListener("keydown", event => this.onKeyDown(event));

    this.elements.search.addEventListener("input", () => {
      this.query = this.elements.search.value.trim().toLowerCase();
      this.matches = this.query ? this.search(this.query) : [];
      this.resultIndex = -1;
      this.resultsOpen = true;
      this.renderResults();
      this.draw();
    });
    this.elements.search.addEventListener("focus", () => {
      this.resultsOpen = true;
      this.renderResults();
    });
    this.elements.search.addEventListener("blur", () => {
      this.resultsOpen = false;
      this.renderResults();
    });
    this.elements.search.addEventListener("keydown", event => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        this.resultsOpen = true;
        if (this.matches.length) {
          const step = event.key === "ArrowDown" ? 1 : -1;
          this.resultIndex = (this.resultIndex + step + this.matches.length) % this.matches.length;
        }
        this.renderResults();
      } else if (event.key === "Enter" && this.matches.length) {
        event.preventDefault();
        this.choose(Math.max(0, this.resultIndex));
      } else if (event.key === "Escape") {
        this.resultsOpen = false;
        this.renderResults();
      }
    });

    if (typeof ResizeObserver !== "undefined") {
      new ResizeObserver(() => this.resize()).observe(canvas.parentElement ?? canvas);
    } else {
      window.addEventListener("resize", () => this.resize());
    }
    this.resize();
  }

  /** Follow the app's light/dark choice without re-reading the backend. */
  setTheme(dark: boolean): void {
    this.dark = dark;
    this.draw();
  }

  fit(): void {
    this.fitScale = Math.max(
      MIN_ZOOM,
      Math.min((this.width - 20) / WORLD.width, (this.height - 60) / WORLD.height),
    );
    this.view = {
      scale: this.fitScale,
      x: (this.width - WORLD.width * this.fitScale) / 2,
      y: 28 + (this.height - 60 - WORLD.height * this.fitScale) / 2,
    };
    this.draw();
  }

  zoom(factor: number, cx?: number, cy?: number): void {
    const centerX = cx ?? this.width / 2;
    const centerY = cy ?? this.height / 2;
    const next = Math.max(
      this.fitScale * MIN_ZOOM,
      Math.min(this.fitScale * MAX_ZOOM, this.view.scale * factor),
    );
    const ratio = next / this.view.scale;
    this.view = {
      scale: next,
      x: centerX - (centerX - this.view.x) * ratio,
      y: centerY - (centerY - this.view.y) * ratio,
    };
    this.draw();
  }

  private resize(): void {
    const parent = this.elements.canvas.parentElement;
    if (!parent) return;
    const previous = { width: this.width, height: this.height };
    const rect = parent.getBoundingClientRect();
    this.width = Math.max(240, Math.round(rect.width));
    this.height = Math.max(240, Math.round(rect.height));
    this.dpr = window.devicePixelRatio || 1;
    this.elements.canvas.width = Math.round(this.width * this.dpr);
    this.elements.canvas.height = Math.round(this.height * this.dpr);
    this.elements.canvas.style.width = `${this.width}px`;
    this.elements.canvas.style.height = `${this.height}px`;
    const atFit = Math.abs(this.view.scale - this.fitScale) < 0.001;
    if (previous.width <= 1 || previous.height <= 1 || atFit) {
      this.fit();
      return;
    }
    this.view.x += (this.width - previous.width) / 2;
    this.view.y += (this.height - previous.height) / 2;
    this.draw();
  }

  private toScreen(point: { x: number; y: number }): { x: number; y: number } {
    return {
      x: point.x * this.view.scale + this.view.x,
      y: point.y * this.view.scale + this.view.y,
    };
  }

  private coords(event: MouseEvent | PointerEvent): { x: number; y: number } {
    const rect = this.elements.canvas.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  private css(name: string, fallback: string): string {
    const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return value || fallback;
  }

  private draw(): void {
    const canvas = this.elements.canvas;
    const context = canvas.getContext("2d");
    if (!context || !this.map) return;
    const { width, height } = this;
    context.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    context.clearRect(0, 0, width, height);

    const paper = this.css("--bg-primary", "#f7f2e9");
    const ink = this.css("--text-primary", "#221c14");
    const secondary = this.css("--text-secondary", "#6e6353");
    const border = this.css("--border-color", "#d9cfbd");
    const unassigned = this.css("--muted-color", "#9a8f7d");
    const focusColor = this.css("--accent-color", "#bc3f2c");

    context.fillStyle = paper;
    context.fillRect(0, 0, width, height);

    context.save();
    context.translate(this.view.x, this.view.y);
    context.scale(this.view.scale, this.view.scale);

    for (const island of this.map.islands) {
      context.beginPath();
      context.ellipse(island.x, island.y, island.rx, island.ry, -0.12, 0, Math.PI * 2);
      context.fillStyle = this.dark ? "#24211c" : "#ece5d8";
      context.fill();
      context.lineWidth = 1 / this.view.scale;
      context.strokeStyle = border;
      context.stroke();
    }

    // A cloud is a soft patch behind its dots: the dots are the data, the
    // patch only says which of them belong together. Regions inside a
    // subdivided community are drawn as a dashed outline instead of a second
    // fill, so the nesting stays readable.
    const drawCloud = (territory: PlacedTerritory) => {
      context.beginPath();
      territory.outline.forEach((point, index) =>
        index ? context.lineTo(point.x, point.y) : context.moveTo(point.x, point.y),
      );
      context.closePath();
      if (territory.depth === 0) {
        context.fillStyle = territoryFill(territory.fillIndex, this.dark);
        context.globalAlpha = 0.85;
        context.fill();
        context.globalAlpha = 1;
        context.lineWidth = 1 / this.view.scale;
        context.strokeStyle = this.dark ? "#4a443c" : "#d3ccbf";
      } else {
        context.setLineDash([2.5 / this.view.scale, 3.5 / this.view.scale]);
        context.lineWidth = 1 / this.view.scale;
        context.strokeStyle = this.dark ? "#4a443c" : "#c8bfb0";
      }
      context.lineJoin = "round";
      context.stroke();
      context.setLineDash([]);
    };
    for (const territory of this.map.territories) {
      context.save();
      drawCloud(territory);
      for (const child of territory.children) drawCloud(child);
      context.restore();
    }

    // Keep the article relationships visible, not only on hover. A heavier
    // shared-article edge is darker and wider, so the spring layout has a
    // matching visual explanation.
    const edgeMax = Math.max(1, ...(this.overview?.edges ?? []).map(edge => edge.shared_articles));
    const nodesByName = new Map(this.map.nodes.map(node => [node.name, node]));
    context.lineCap = "round";
    for (const edge of this.overview?.edges ?? []) {
      const from = nodesByName.get(edge.source);
      const to = nodesByName.get(edge.target);
      if (!from || !to) continue;
      const weight = Math.log1p(Math.max(1, edge.shared_articles)) / Math.log1p(edgeMax);
      context.globalAlpha = 0.12 + weight * 0.28;
      context.lineWidth = (0.65 + weight * 1.8) / this.view.scale;
      context.strokeStyle = this.dark ? "#b9aa92" : "#766b5b";
      context.beginPath();
      context.moveTo(from.x, from.y);
      context.lineTo(to.x, to.y);
      context.stroke();
    }
    context.globalAlpha = 1;
    context.restore();

    const focused = this.hover ?? this.focus;
    if (focused !== null) {
      const node = this.map.nodes[focused];
      context.strokeStyle = this.css("--text-secondary", "#6e6353");
      context.globalAlpha = 0.5;
      context.lineWidth = 1;
      for (const edge of this.overview?.edges ?? []) {
        if (edge.source !== node?.name && edge.target !== node?.name) continue;
        const from = this.map.nodes.find(candidate => candidate.name === edge.source);
        const to = this.map.nodes.find(candidate => candidate.name === edge.target);
        if (!from || !to) continue;
        const start = this.toScreen(from);
        const end = this.toScreen(to);
        context.beginPath();
        context.moveTo(start.x, start.y);
        context.lineTo(end.x, end.y);
        context.stroke();
      }
      context.globalAlpha = 1;
    }

    const labelBoxes: { x: number; y: number; w: number; h: number }[] = [];
    context.fillStyle = secondary;
    context.font = '12px "Public Sans", "Noto Sans CJK SC", sans-serif';
    if (this.view.scale >= this.fitScale * 0.8) {
      const regions: PlacedTerritory[] = [];
      for (const territory of this.map.territories) {
        regions.push(territory, ...territory.children);
      }
      for (const region of regions) {
        const leftEdge = region.cx - region.r * 0.9;
        const rightEdge = region.cx + region.r * 0.9;
        // A region narrower than its own name cannot label itself; the search
        // list and the tooltip still name its tags, and drawing the text would
        // write it over a neighbour.
        const available = rightEdge - leftEdge;
        const width = Math.min(region.titleMaxWidth, labelWidth(region.summary));
        if (available < Math.min(width, 54)) continue;
        const text = fitSummary(region.summary.split(" · "), available);
        const drawn = Math.min(width, labelWidth(text));
        const anchor = this.toScreen(region.titleAnchor);
        // Centred on the reserved band: the nodes already avoided that band at
        // build time, so clamping the text elsewhere would drop it onto them.
        const x = anchor.x - drawn / 2;
        // Never write outside the visible canvas: a label for a region the view
        // has scrolled past reads as belonging to whatever is next to it.
        if (x < 0 || x + drawn > width || anchor.y < 24 || anchor.y > height - 4) continue;
        context.fillStyle = secondary;
        context.fillText(text, x, Math.min(anchor.y, height - 6), available * this.view.scale + 4);
        labelBoxes.push({ x, y: anchor.y - 13, w: drawn, h: 18 });
      }
    }

    const hasMatches = this.matches.length > 0;
    for (const node of this.map.nodes) {
      const point = this.toScreen(node);
      if (point.x < -40 || point.x > width + 40 || point.y < -40 || point.y > height + 40) continue;
      const index = this.map.nodes.indexOf(node);
      const isMatch = hasMatches && this.matches.includes(node);
      const isFocused = focused === index;
      const radius = Math.max(3.6, node.radius * this.view.scale);
      context.globalAlpha = hasMatches && !isMatch && !isFocused ? 0.35 : 1;
      context.beginPath();
      context.arc(point.x, point.y, radius, 0, Math.PI * 2);
      const color = nodeColor(node, this.dark, unassigned);
      context.fillStyle = color;
      context.strokeStyle = color;
      context.lineWidth = 1.5;
      if (node.category_id) {
        context.fill();
      } else {
        // Filled, not hollow: a library where nothing is assigned yet would
        // otherwise render as a mesh of empty rings, which reads as an empty
        // map even though it carries every tag.
        context.fillStyle = this.dark ? "rgba(200,190,170,0.35)" : "rgba(130,118,102,0.28)";
        context.fill();
        context.stroke();
      }
      if (isFocused || isMatch) {
        context.beginPath();
        context.arc(point.x, point.y, radius + 4, 0, Math.PI * 2);
        context.strokeStyle = isFocused ? focusColor : color;
        context.lineWidth = isFocused ? 1.7 : 1;
        context.stroke();
      }
      context.globalAlpha = 1;
    }

    context.font = '11px "JetBrains Mono", ui-monospace, "Noto Sans CJK SC", monospace';
    const ordered = [...this.map.nodes].sort((left, right) => {
      const leftIndex = this.map!.nodes.indexOf(left);
      const rightIndex = this.map!.nodes.indexOf(right);
      return (
        Number(rightIndex === focused) - Number(leftIndex === focused) ||
        right.usage_count - left.usage_count
      );
    });
    for (const node of ordered) {
      const index = this.map.nodes.indexOf(node);
      const isFocused = focused === index;
      const isMatch = hasMatches && this.matches.includes(node);
      if (!node.labeled && !isFocused && !isMatch && this.view.scale < this.fitScale * 1.75) continue;
      const point = this.toScreen(node);
      const textWidth = context.measureText(node.name).width;
      const radius = Math.max(3.6, node.radius * this.view.scale);
      const box = {
        x: point.x - textWidth / 2 - 3,
        y: point.y + radius + 5,
        w: textWidth + 6,
        h: 17,
      };
      if (box.x < 4 || box.x + box.w > width - 4 || box.y < 26 || box.y + box.h > height - 4) {
        continue;
      }
      const collides = labelBoxes.some(
        other =>
          box.x < other.x + other.w &&
          box.x + box.w > other.x &&
          box.y < other.y + other.h &&
          box.y + box.h > other.y,
      );
      if (collides && !isFocused) continue;
      labelBoxes.push(box);
      context.globalAlpha = hasMatches && !isMatch && !isFocused ? 0.4 : 1;
      context.fillStyle = ink;
      context.fillText(node.name, box.x + 3, box.y + 12);
      context.globalAlpha = 1;
    }

    this.updateTip(focused);
  }

  private updateTip(index: number | null): void {
    const tip = this.tip ?? this.elements.status.parentElement?.querySelector<HTMLElement>("#tag-map-tip");
    this.tip = tip ?? null;
    if (!tip || index === null || !this.map) {
      if (tip) tip.hidden = true;
      return;
    }
    const node = this.map.nodes[index];
    const point = this.toScreen(node);
    const label = node.category_id
      ? this.topicLabels.get(node.category_id) ?? `Topic ${node.category_id}`
      : "Unsorted";
    tip.hidden = false;
    tip.innerHTML = "";
    const name = document.createElement("p");
    name.className = "tag-map-tip-name";
    name.textContent = node.name;
    const meta = document.createElement("p");
    meta.className = "tag-map-tip-meta";
    meta.textContent = `${label} · ${node.usage_count} article(s)`;
    tip.append(name, meta);
    const width = tip.offsetWidth;
    const height = tip.offsetHeight;
    tip.style.left = `${Math.max(8, Math.min(this.width - width - 8, point.x + 16))}px`;
    tip.style.top = `${Math.max(32, Math.min(this.height - height - 8, point.y - height - 16))}px`;
  }

  private search(query: string): PlacedNode[] {
    if (!this.map) return [];
    return this.map.nodes.filter(
      node =>
        node.name.includes(query) ||
        (node.category_id
          ? (this.topicLabels.get(node.category_id) ?? "").toLowerCase().includes(query)
          : "unsorted".includes(query)),
    );
  }

  private renderResults(): void {
    const { results, search } = this.elements;
    results.replaceChildren();
    search.setAttribute("aria-expanded", String(this.resultsOpen && !!this.query));
    search.removeAttribute("aria-activedescendant");
    if (!this.resultsOpen || !this.query) return;
    if (!this.matches.length) {
      const empty = document.createElement("div");
      empty.className = "tag-map-result-empty";
      empty.textContent = "No tag matches. The map is unchanged.";
      results.append(empty);
      return;
    }
    const unassigned = this.css("--muted-color", "#9a8f7d");
    this.matches.forEach((node, index) => {
      const row = document.createElement("div");
      row.className = "tag-map-result";
      row.id = `tag-map-result-${index}`;
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(index === this.resultIndex));
      const swatch = document.createElement("i");
      swatch.className = "tag-map-swatch";
      swatch.style.background = nodeColor(node, this.dark, unassigned);
      const text = document.createElement("span");
      text.textContent = node.name;
      const meta = document.createElement("small");
      meta.textContent = node.category_id
        ? this.topicLabels.get(node.category_id) ?? `Topic ${node.category_id}`
        : "Unsorted";
      text.append(meta);
      row.append(swatch, text);
      row.addEventListener("pointerdown", event => event.preventDefault());
      row.addEventListener("click", () => this.choose(index));
      results.append(row);
    });
    if (this.resultIndex >= 0) {
      const id = `tag-map-result-${this.resultIndex}`;
      search.setAttribute("aria-activedescendant", id);
      results.querySelector(`#${id}`)?.scrollIntoView({ block: "nearest" });
    }
  }

  private choose(index: number): void {
    const node = this.matches[index];
    if (!node || !this.map) return;
    this.focus = this.map.nodes.indexOf(node);
    this.resultsOpen = false;
    this.renderResults();
    this.centerOn(node);
    this.elements.canvas.focus({ preventScroll: true });
  }

  private centerOn(node: PlacedNode): void {
    this.view.scale = Math.max(this.view.scale, this.fitScale * 1.7);
    this.view.x = this.width / 2 - node.x * this.view.scale;
    this.view.y = this.height / 2 - node.y * this.view.scale;
    this.draw();
  }

  private onPointerDown(event: PointerEvent): void {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    this.elements.canvas.focus({ preventScroll: true });
    this.elements.canvas.setPointerCapture(event.pointerId);
    this.pointers.set(event.pointerId, this.coords(event));
    this.dragged = this.pointers.size > 1;
    this.beginGesture();
  }

  private beginGesture(): void {
    const points = [...this.pointers.values()];
    if (points.length >= 2) {
      this.gesture = {
        kind: "pinch",
        distance: Math.hypot(points[1].x - points[0].x, points[1].y - points[0].y),
        center: {
          x: (points[0].x + points[1].x) / 2,
          y: (points[0].y + points[1].y) / 2,
        },
      };
    } else if (points.length === 1) {
      this.gesture = { kind: "pan", last: points[0], origin: points[0] };
    }
  }

  private onPointerMove(event: PointerEvent): void {
    const point = this.coords(event);
    if (this.pointers.has(event.pointerId)) {
      this.pointers.set(event.pointerId, point);
      const points = [...this.pointers.values()];
      if (this.gesture?.kind === "pinch" && points.length >= 2) {
        const distance = Math.hypot(points[1].x - points[0].x, points[1].y - points[0].y);
        const center = {
          x: (points[0].x + points[1].x) / 2,
          y: (points[0].y + points[1].y) / 2,
        };
        this.zoom(distance / Math.max(1, this.gesture.distance), this.gesture.center.x, this.gesture.center.y);
        this.view.x += center.x - this.gesture.center.x;
        this.view.y += center.y - this.gesture.center.y;
        this.gesture = { kind: "pinch", distance, center };
        this.dragged = true;
      } else if (this.gesture?.kind === "pan") {
        if (Math.hypot(point.x - this.gesture.origin.x, point.y - this.gesture.origin.y) > 4) {
          this.dragged = true;
        }
        this.view.x += point.x - this.gesture.last.x;
        this.view.y += point.y - this.gesture.last.y;
        this.gesture.last = point;
      }
      this.hover = null;
      this.draw();
      return;
    }
    const hit = this.hitTest(point.x, point.y);
    if (hit !== this.hover) {
      this.hover = hit;
      this.elements.canvas.style.cursor = hit !== null ? "pointer" : "grab";
      this.draw();
    }
  }

  private onPointerUp(event: PointerEvent, cancel = false): void {
    if (!this.pointers.has(event.pointerId)) return;
    const point = this.coords(event);
    this.pointers.delete(event.pointerId);
    if (!cancel && !this.dragged && this.pointers.size === 0) {
      const hit = this.hitTest(point.x, point.y);
      this.focus = hit;
      this.draw();
    }
    if (this.pointers.size) this.beginGesture();
    else this.gesture = null;
  }

  private hitTest(x: number, y: number): number | null {
    if (!this.map) return null;
    let nearest: number | null = null;
    let distance = Number.POSITIVE_INFINITY;
    this.map.nodes.forEach((node, index) => {
      const point = this.toScreen(node);
      const current = Math.hypot(x - point.x, y - point.y);
      if (current <= Math.max(11, node.radius * this.view.scale + 5) && current < distance) {
        nearest = index;
        distance = current;
      }
    });
    return nearest;
  }

  private onKeyDown(event: KeyboardEvent): void {
    if (!this.map) return;
    const forward = ["ArrowRight", "ArrowDown"].includes(event.key);
    const backward = ["ArrowLeft", "ArrowUp"].includes(event.key);
    if (forward || backward) {
      event.preventDefault();
      const total = this.map.nodes.length;
      const step = forward ? 1 : -1;
      const next = this.focus === null ? (forward ? 0 : total - 1) : (this.focus + step + total) % total;
      this.focus = next;
      this.centerOn(this.map.nodes[next]);
      this.elements.status.textContent = this.describeFocused(next);
    } else if (event.key === "+" || event.key === "=") {
      event.preventDefault();
      this.zoom(1.25);
    } else if (event.key === "-") {
      event.preventDefault();
      this.zoom(0.8);
    } else if (event.key === "0") {
      event.preventDefault();
      this.fit();
    } else if (event.key === "Escape") {
      this.focus = null;
      this.draw();
    }
  }

  private describeFocused(index: number): string {
    const node = this.map?.nodes[index];
    if (!node) return "";
    const label = node.category_id
      ? this.topicLabels.get(node.category_id) ?? `Topic ${node.category_id}`
      : "Unsorted";
    return `${node.name} · ${label} · ${node.usage_count} article(s)`;
  }
}
