// Pure helpers behind the Templates page: where an installed template's
// draft lives (its page) and the order the effects are labeled in.

/** The page of the playbook a template was installed as. */
export function installTarget(scope: string, workspace: string, id: string): string {
  if (scope === 'global') return `#/global-playbook/${encodeURIComponent(id)}`
  return `#/playbook/${encodeURIComponent(workspace)}/${encodeURIComponent(id)}`
}

/** The effects as small labels, `irreversible` first, the rest in their order. */
export function effectLabels(effects: string[]): string[] {
  return [...effects].sort((a, b) => Number(b === 'irreversible') - Number(a === 'irreversible'))
}
