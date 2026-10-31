import { afterEach, describe, expect, it, vi } from "vitest";
import { applyBionic } from "../src/bionic";

afterEach(() => vi.restoreAllMocks());

describe("bionic plain-text fast path", () => {
  it("emphasizes plain text without allocating a live Range for every word", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>Reading performance stays predictable.</p>";
    const original = root.textContent;
    const ranges = vi.spyOn(document, "createRange");
    applyBionic(root);
    expect(root.textContent).toBe(original);
    expect(root.querySelectorAll(".bionic-fix")).toHaveLength(4);
    expect(root.querySelector(".bionic-fix")?.textContent).toBe("Readi");
    expect(ranges).not.toHaveBeenCalled();
    const html = root.innerHTML;
    applyBionic(root);
    expect(root.innerHTML).toBe(html);
  });

  it("keeps the Range fallback for words spanning inline markup", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>rea<em>ding</em> works</p>";
    const ranges = vi.spyOn(document, "createRange");
    applyBionic(root);
    expect(ranges).toHaveBeenCalled();
    expect(root.textContent).toBe("reading works");
    expect(root.querySelector(".bionic-fix em")).not.toBeNull();
    expect(root.querySelectorAll(".bionic-fix")).toHaveLength(2);
  });

  it("keeps plain text inert and preserves skipped code and author emphasis", () => {
    const root = document.createElement("div");
    root.textContent = '<video src="example.mp4"> ordinary prose & more';
    const original = root.textContent;
    applyBionic(root);
    expect(root.textContent).toBe(original);
    expect(root.querySelector("video")).toBeNull();

    root.innerHTML = "<p><code>untouched code</code> <strong>author emphasis</strong> ordinary prose</p>";
    applyBionic(root);
    expect(root.querySelector("code .bionic-fix, strong .bionic-fix")).toBeNull();
    expect(root.querySelectorAll(".bionic-fix")).toHaveLength(2);
  });
});
