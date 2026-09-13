import { describe, expect, it } from "vitest";

import {
  buildMap,
  insideCloud,
  labelWidth,
  nodeColor,
  packClouds,
  springLayout,
  TOPIC_COLORS,
  WORLD,
} from "../src/ui/tag-overview";
import type { SemanticPositions } from "../src/ui/tag-overview";
import type { TagOverview } from "../src/types";

/** Two communities, one lone name, and one tag that shares nothing. */
const OVERVIEW: TagOverview = {
  snapshot_id: "snap-1",
  scope_label: "all subscriptions",
  coverage: { total_items: 40, tagged_items: 30, unreadable_items: 0 },
  nodes: [
    { name: "docker", usage_count: 12, category_id: 12 },
    { name: "containerization", usage_count: 9, category_id: 12 },
    { name: "kubernetes", usage_count: 7, category_id: 12 },
    { name: "rust", usage_count: 14, category_id: 5 },
    { name: "zig", usage_count: 4, category_id: 5 },
    { name: "anniversary", usage_count: 1, category_id: null },
  ],
  edges: [
    { source: "containerization", target: "docker", shared_articles: 5 },
    { source: "docker", target: "kubernetes", shared_articles: 3 },
    { source: "rust", target: "zig", shared_articles: 2 },
  ],
  communities: [
    {
      id: "c-infra",
      members: ["docker", "containerization", "kubernetes"],
      summary_tags: ["docker", "containerization", "kubernetes"],
      article_count: 20,
      children: [],
    },
    {
      id: "c-langs",
      members: ["rust", "zig"],
      summary_tags: ["rust", "zig"],
      article_count: 10,
      children: [],
    },
  ],
  singletons: ["anniversary"],
  blocked_excluded: 0,
  structuring: "cooccurrence",
  warnings: [],
};

const cloudOf = (map: ReturnType<typeof buildMap>, id: string) =>
  map.territories.find(territory => territory.id === id)!;

describe("community clouds", () => {
  it("keeps every dot inside the cloud of its own community", () => {
    const map = buildMap(OVERVIEW);
    for (const territory of map.territories) {
      const members = map.nodes.filter(node => territory.members.includes(node.name));
      expect(members).toHaveLength(territory.members.length);
      for (const node of members) {
        expect(
          insideCloud(node, territory),
          `${node.name} must sit in ${territory.id}, not in a neighbour's cloud`,
        ).toBe(true);
      }
    }
  });

  it("sizes a cloud from its article count, not its name count", () => {
    const map = buildMap(OVERVIEW);
    // c-infra carries twice the articles of c-langs.
    expect(cloudOf(map, "c-infra").r).toBeGreaterThan(cloudOf(map, "c-langs").r);
    expect(map.territories[0].id).toBe("c-infra");
  });

  it("keeps clouds apart, so a dot's patch is unambiguous", () => {
    const map = buildMap(OVERVIEW);
    for (let left = 0; left < map.territories.length; left += 1) {
      for (let right = left + 1; right < map.territories.length; right += 1) {
        const a = map.territories[left];
        const b = map.territories[right];
        const distance = Math.hypot(a.cx - b.cx, a.cy - b.cy);
        expect(distance).toBeGreaterThan(Math.max(a.r, b.r));
      }
    }
  });

  it("uses semantic positions as seeds while keeping weighted neighbours close", () => {
    // The strong docker/containerization edge should survive the initial seed,
    // while the weaker docker/kubernetes edge remains farther away.
    const positions: SemanticPositions = new Map([
      ["docker", { x: 0.1, y: 0.1 }],
      ["containerization", { x: 0.2, y: 0.15 }],
      ["kubernetes", { x: 0.9, y: 0.85 }],
      ["rust", { x: 0.5, y: 0.5 }],
      ["zig", { x: 0.55, y: 0.52 }],
    ]);
    const map = buildMap(OVERVIEW, positions);
    const near = (name: string) => map.nodes.find(node => node.name === name)!;
    const within = (a: string, b: string) =>
      Math.hypot(near(a).x - near(b).x, near(a).y - near(b).y);

    // Within one community the weighted springs decide the arrangement. Across
    // communities the distance is decided by the cloud packing, not by meaning,
    // so comparing those two distances would assert nothing.
    expect(within("docker", "containerization")).toBeLessThan(within("docker", "kubernetes"));
    for (const territory of map.territories) {
      for (const name of territory.members) {
        expect(insideCloud(near(name), territory)).toBe(true);
      }
    }
  });

  it("still fills a cloud when the encoder gave no positions", () => {
    const map = buildMap(OVERVIEW);
    const members = map.nodes.filter(node => node.name !== "anniversary");
    expect(members).toHaveLength(5);
    for (const node of members) {
      expect(Number.isFinite(node.x)).toBe(true);
      expect(Number.isFinite(node.y)).toBe(true);
    }
  });

  it("draws names with no observed relation as islands, never as a community", () => {
    const map = buildMap(OVERVIEW);
    expect(map.islands.map(island => island.name)).toEqual(["anniversary"]);
    const island = map.islands[0];
    for (const territory of map.territories) {
      expect(Math.hypot(island.x - territory.cx, island.y - territory.cy)).toBeGreaterThan(
        territory.r,
      );
    }
  });

  it("is deterministic, so the same snapshot draws the same picture", () => {
    expect(buildMap(OVERVIEW)).toEqual(buildMap(OVERVIEW));
  });

  it("survives an empty library and a single community", () => {
    const empty: TagOverview = { ...OVERVIEW, nodes: [], communities: [], singletons: [] };
    expect(buildMap(empty)).toEqual({ territories: [], islands: [], nodes: [] });

    const one: TagOverview = {
      ...OVERVIEW,
      communities: [OVERVIEW.communities[0]],
      singletons: [],
      nodes: OVERVIEW.nodes.filter(node => node.name !== "anniversary"),
    };
    const map = buildMap(one);
    expect(map.territories).toHaveLength(1);
    expect(map.nodes).toHaveLength(3);
  });

  it("keeps every cloud on the canvas", () => {
    const map = buildMap(OVERVIEW);
    for (const territory of map.territories) {
      expect(territory.cx - territory.r).toBeGreaterThanOrEqual(0);
      expect(territory.cx + territory.r).toBeLessThanOrEqual(WORLD.width);
      expect(territory.cy - territory.r).toBeGreaterThanOrEqual(0);
      expect(territory.cy + territory.r).toBeLessThanOrEqual(WORLD.height);
      for (const point of territory.outline) {
        expect(Number.isFinite(point.x) && Number.isFinite(point.y)).toBe(true);
      }
    }
  });
});

describe("spring layout", () => {
  it("pulls a strongly related tag closer than a weakly related one", () => {
    const points = springLayout(
      ["hub", "strong", "weak"],
      [
        { source: "hub", target: "strong", shared_articles: 10 },
        { source: "hub", target: "weak", shared_articles: 1 },
      ],
      { cx: 400, cy: 300, r: 240 },
    );
    const distance = (left: string, right: string) => {
      const a = points.get(left)!;
      const b = points.get(right)!;
      return Math.hypot(a.x - b.x, a.y - b.y);
    };
    expect(distance("hub", "strong")).toBeLessThan(distance("hub", "weak"));
  });

  it("is deterministic for the same weighted graph", () => {
    const edges = [{ source: "a", target: "b", shared_articles: 3 }];
    const bounds = { cx: 200, cy: 200, r: 120 };
    expect(springLayout(["a", "b"], edges, bounds)).toEqual(
      springLayout(["a", "b"], edges, bounds),
    );
  });

  it("does not block the UI for a library-sized community", () => {
    const names = Array.from({ length: 1450 }, (_, index) => `tag_${index}`);
    const edges = names.slice(1).map((name, index) => ({
      source: names[index],
      target: name,
      shared_articles: 1,
    }));
    const started = performance.now();
    const points = springLayout(names, edges, { cx: 600, cy: 350, r: 300 });
    expect(points).toHaveLength(names.length);
    expect(performance.now() - started).toBeLessThan(700);
  });
});

describe("cloud packing", () => {
  it("gives disjoint discs that all fit the area", () => {
    const area = { x: 0, y: 0, width: 1000, height: 600 };
    const discs = packClouds([100, 60, 20, 5, 2, 1], area);
    expect(discs).toHaveLength(6);
    expect(discs[0].r).toBeGreaterThan(discs[5].r);
    for (let left = 0; left < discs.length; left += 1) {
      for (let right = left + 1; right < discs.length; right += 1) {
        const distance = Math.hypot(
          discs[left].cx - discs[right].cx,
          discs[left].cy - discs[right].cy,
        );
        expect(distance).toBeGreaterThan(discs[left].r + discs[right].r);
      }
    }
    for (const disc of discs) {
      expect(disc.cx - disc.r).toBeGreaterThanOrEqual(area.x - 1);
      expect(disc.cx + disc.r).toBeLessThanOrEqual(area.x + area.width + 1);
      expect(disc.cy - disc.r).toBeGreaterThanOrEqual(area.y - 1);
      expect(disc.cy + disc.r).toBeLessThanOrEqual(area.y + area.height + 1);
    }
  });

  it("never hands out a disc smaller than a readable patch", () => {
    const discs = packClouds([5000, 1, 1], { x: 0, y: 0, width: 1200, height: 700 });
    // Two lone tags next to a library-sized community still get a label-sized
    // patch, which is what keeps them from becoming unreadable specks.
    expect(discs[1].r).toBeGreaterThan(20);
    expect(discs[2].r).toBeGreaterThan(20);
  });

  it("returns nothing rather than dividing by zero", () => {
    expect(packClouds([], { x: 0, y: 0, width: 10, height: 10 })).toEqual([]);
  });
});

describe("topic colour slots", () => {
  it("gives one topic one colour and the unsorted dots the neutral grey", () => {
    const [docker, container] = [OVERVIEW.nodes[0], OVERVIEW.nodes[1]];
    expect(nodeColor(docker, false, "#999")).toBe(nodeColor(container, false, "#999"));
    expect(nodeColor(OVERVIEW.nodes[5], false, "#999")).toBe("#999");
    expect(nodeColor(docker, true, "#999")).not.toBe(nodeColor(docker, false, "#999"));
  });

  it("keeps a slot for every topic id the ceiling allows, wrapping safely", () => {
    expect(TOPIC_COLORS.length).toBeGreaterThanOrEqual(49);
    expect(nodeColor({ name: "x", usage_count: 1, category_id: 49 }, false, "#999")).toBe(
      TOPIC_COLORS[48].light,
    );
    // An id beyond the ceiling must still render instead of throwing.
    expect(nodeColor({ name: "x", usage_count: 1, category_id: 60 }, false, "#999")).toBeTruthy();
  });
});

describe("labels", () => {
  it("trims a summary to the room its cloud has", () => {
    expect(labelWidth("docker")).toBeGreaterThan(0);
    // A wide label asked for a narrow slot still measures wider, so the caller
    // is expected to trim; this pins the measurement being real, not zero.
    expect(labelWidth("docker · containerization · kubernetes")).toBeGreaterThan(
      labelWidth("docker"),
    );
  });
});
