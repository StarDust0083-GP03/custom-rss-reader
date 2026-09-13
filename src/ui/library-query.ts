/**
 * One generation counter for everything that replaces the article list.
 *
 * Search, tag/subscription filters, Today/Unread/Favorites, and Similar
 * all render into `#items-list`, so they must share a single notion of
 * "latest request". Two independent counters let a slow search resolve
 * after a newer filter load (the probe reproduced exactly that) and paint
 * results for a query the user already left.
 */

let libraryQuerySeq = 0;

/** Mark the start of a list-replacing request and return its generation. */
export function beginLibraryQuery(): number {
  return ++libraryQuerySeq;
}

/** The generation of the request that currently owns the list. */
export function libraryQueryGeneration(): number {
  return libraryQuerySeq;
}

/** May this generation still write to `state.currentItems` / the status bar? */
export function isCurrentLibraryQuery(seq: number): boolean {
  return seq === libraryQuerySeq;
}
