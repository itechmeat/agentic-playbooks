/** sessionStorage key for the YAML draft when creating/duplicating. */
export const DRAFT_STORAGE_KEY = 'apb:edit:draft'

export interface DraftPayload {
  yaml: string
  suggestedId?: string
}

export function storeDraftYaml(yaml: string, suggestedId?: string): void {
  const payload: DraftPayload = { yaml }
  if (suggestedId) payload.suggestedId = suggestedId
  sessionStorage.setItem(DRAFT_STORAGE_KEY, JSON.stringify(payload))
}

export function takeDraftYaml(): DraftPayload | null {
  try {
    const raw = sessionStorage.getItem(DRAFT_STORAGE_KEY)
    if (!raw) return null
    sessionStorage.removeItem(DRAFT_STORAGE_KEY)
    const parsed = JSON.parse(raw) as DraftPayload
    if (typeof parsed.yaml !== 'string') return null
    return parsed
  } catch {
    return null
  }
}

/** Suggested id for a playbook copy. */
export function suggestDuplicateId(sourceId: string): string {
  return `${sourceId}-copy`
}
