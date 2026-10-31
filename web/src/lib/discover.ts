// History discovery on the dashboard: how a discovery result, a suggestion
// and a refusal read on the Discover page. Pure functions over the
// generated types.

import type {
  ProjectSeen,
  ReadReport,
  ResultView,
  RootInfo,
  SourceSpan,
  Suggestion,
  SuggestionStatus,
} from './api.gen'
import { ApiError } from './api/http'

/** `cargo test > git commit > git push`: a suggestion's steps in order. */
export function stepsText(steps: string[]): string {
  return steps.join(' > ')
}

/** `app, site`: the projects a result names, by name. */
export function projectsText(projects: ProjectSeen[]): string {
  return projects.map((p) => p.name).join(', ')
}

/** `3 sessions on 2 days in app, site`. */
export function supportText(s: Suggestion): string {
  const { sessions, days, projects } = s.support
  const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`
  const where = projects.length ? ` in ${projectsText(projects)}` : ''
  return `${plural(sessions, 'session')} on ${plural(days, 'day')}${where}`
}

/** `claude_code: app/s1.jsonl, lines 5-10` (a single line once). */
export function sourceText(src: SourceSpan): string {
  const [from, to] = src.lines
  const lines = from === to ? `line ${from}` : `lines ${from}-${to}`
  return `${src.root}: ${src.file}, ${lines}`
}

/** What became of a suggestion, in a word or two. */
export function statusText(status: SuggestionStatus): string {
  switch (status.state) {
    case 'new':
      return 'new'
    case 'saved':
      return `saved as ${status.playbook} (${status.scope})`
    case 'dismissed':
      return 'declined'
  }
}

/** Whether a person can still save or decline `s`: a new suggestion of a
 * result this installation signed. */
export function canAct(view: Pick<ResultView, 'signed'>, s: Suggestion): boolean {
  return view.signed && s.status.state === 'new'
}

/** One line per root: `claude_code (claude_code): 12 of 14 files, 3.1 MB, skipped: too_old 2`. */
export function readLines(read: ReadReport): string[] {
  return read.roots.map((r) => {
    const skipped = Object.entries(r.skipped ?? {})
      .filter(([, n]) => (n ?? 0) > 0)
      .map(([why, n]) => `${why.replaceAll('_', ' ')} ${n}`)
    let line = `${r.name} (${r.agent}): ${r.files_read} of ${r.files_seen} files, ${bytesText(r.bytes_read)}`
    if (r.cut_files) line += `, ${r.cut_files} read in part`
    if (skipped.length) line += `, skipped: ${skipped.join(', ')}`
    return line
  })
}

/** `512 B`, `3.1 KB`, `2.4 MB`. */
export function bytesText(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / (1024 * 1024)).toFixed(1)} MB`
}

/** The roots a new discovery reads by default: every one that is there. */
export function defaultRoots(roots: RootInfo[]): string[] {
  return roots.filter((r) => r.available).map((r) => r.name)
}

/** A refusal as a person reads it, without the request URL (absolute, as
 * a browser reports it, or relative). */
export function refusalText(e: unknown): string {
  if (!(e instanceof ApiError)) return String(e)
  if (e.status === 403) return 'Session history is read only from a browser on this machine.'
  const why = e.message.replace(/^(?:[a-z][a-z0-9+.-]*:\/\/[^/\s]+)?\/api\/\S*: /i, '')
  if (e.status === 409 && why.startsWith('busy')) {
    return 'Another discovery is running. Try again when it is done.'
  }
  return why
}

/** The toast of a save that went through: the playbook it became. */
export function savedText(out: { id?: string }): string {
  return out.id ? `Saved as draft playbook ${out.id}` : 'Saved as a draft playbook'
}

/** What a save or decline answered with when the store refused it. */
export function rejectedText(rejected: string): string {
  switch (rejected) {
    case 'secret_like_value':
      return 'The draft still looks like it holds a secret, so it was not saved.'
    case 'possible_duplicate':
      return 'A playbook with the same trigger already exists.'
    case 'duplicate_id':
      return 'A playbook with this id already exists.'
    default:
      return `Refused: ${rejected}`
  }
}
