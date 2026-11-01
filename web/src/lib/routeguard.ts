// Whether an answer still belongs to the page that asked: a page reused
// for another route bumps its token, and a request started under the old
// token must then change nothing (no dialog, no toast, no redirect).

/** A check, taken when a request starts, that tells whether the page is
 * still on the route it started on. */
export function sameRoute(current: () => number): () => boolean {
  const at = current()
  return () => current() === at
}
