// The effects box of a playbook: every effect the run policy counts, each
// marked assumed when only the pessimistic reading of a script or agent
// node that declared its own effects puts it there (the server's
// `assumed_effects`). An assumed effect is still counted by every run.

export interface EffectTag {
  effect: string
  assumed: boolean
}

export function effectTags(effects: string[], assumed: string[] = []): EffectTag[] {
  const marked = new Set(assumed)
  return [...new Set(effects)].map((effect) => ({ effect, assumed: marked.has(effect) }))
}
