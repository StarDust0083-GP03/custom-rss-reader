/**
 * Shared `marked` configuration.
 *
 * Used by BOTH the host DOM text path and the iframe webview path — the
 * backend runs every article through the same html_to_markdown_pipeline, so
 * any renderer here is exercised for every article regardless of mode.
 * Configuring once here keeps the two paths from drifting apart.
 */

import { marked, Renderer } from "marked";

// Use tokens instead of raw text to properly handle nested elements
// (e.g., images inside links: [![alt](img.jpg)](url)).
const mdRenderer = new Renderer();
mdRenderer.link = function ({ href, title, tokens }) {
  const text = this.parser ? this.parser.parseInline(tokens) : "";
  return `<a target="_blank" rel="noopener noreferrer" href="${href}"${title ? ` title="${title}"` : ""}>${text}</a>`;
};

let configured = false;

/** Idempotently apply the shared renderer config; returns the marked instance. */
export function configureMarked(): typeof marked {
  if (!configured) {
    marked.use({ renderer: mdRenderer, gfm: true });
    configured = true;
  }
  return marked;
}
