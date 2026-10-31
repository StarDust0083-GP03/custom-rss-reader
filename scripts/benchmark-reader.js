// Run against the frontend dev server, already open in agent-browser:
// agent-browser --session rss-perf eval --stdin < scripts/benchmark-reader.js
// Synthetic text only. These are detached-DOM stage timings in Chromium,
// not end-to-end native WebKit latency, layout/paint, or network timings.
(async () => {
  const { renderMarkdown } = await import("/src/markdown.ts");
  const { setSafeHtml } = await import("/src/sanitize.ts");
  const { normalizeMediaUrls } = await import("/src/media.ts");
  const { applyBionic } = await import("/src/bionic.ts");
  const paragraph = "Reading performance depends on keeping article rendering predictable. This paragraph contains ordinary English prose with enough words to exercise the reader pipeline.";
  const results = [];
  for (const paragraphs of [10, 100]) {
    const markdown = Array(paragraphs).fill(paragraph).join("\n\n");
    const samples = [];
    for (let run = 0; run < 6; run++) {
      const root = document.createElement("div");
      const t0 = performance.now();
      const html = renderMarkdown(markdown);
      const t1 = performance.now();
      setSafeHtml(root, html);
      const t2 = performance.now();
      normalizeMediaUrls(root, "https://reader-media.test/article");
      const t3 = performance.now();
      applyBionic(root);
      const t4 = performance.now();
      if (root.textContent?.trim() !== Array(paragraphs).fill(paragraph).join("\n")) {
        // Marked inserts blank-line whitespace between block paragraphs.
        if (root.textContent?.replace(/\s+/g, " ").trim() !== Array(paragraphs).fill(paragraph).join(" ")) {
          throw new Error("Rendering changed article text");
        }
      }
      if (!root.querySelector(".bionic-fix")) throw new Error("No fixation markers rendered");
      if (run > 0) samples.push({ markdown_ms: t1 - t0, sanitize_ms: t2 - t1,
        media_ms: t3 - t2, bionic_ms: t4 - t3, total_ms: t4 - t0 });
    }
    const median = key => samples.map(sample => sample[key]).sort((a, b) => a - b)[2];
    results.push({ paragraphs, words: paragraphs * paragraph.split(/\s+/).length, runs: 5,
      median_ms: Object.fromEntries(Object.keys(samples[0]).map(key => [key, Math.round(median(key) * 100) / 100])) });
  }
  window.__readerPerformance = { environment: "Chromium; synthetic detached DOM; one warm-up and five measured runs", results };
  return window.__readerPerformance;
})()
