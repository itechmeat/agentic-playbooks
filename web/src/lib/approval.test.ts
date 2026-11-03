import { describe, expect, it } from 'vitest'
import { approvalRefusal, nodeApprovalRefusals } from './approval'
import type { RunDetail } from './types'

// A node refused by its approval check finishes failed with the output
// `<code>: <detail>` and no attempt (`ApprovalFailure`'s Display); a bound
// snapshot that no longer verifies when the gate consumes the decision ends
// the run with the same text as its failure reason.
describe('approval refusals on the node', () => {
  it('reads each refusal code with its detail, and nothing else', () => {
    expect(approvalRefusal('approval_stale: implement ran again after approval')).toEqual({
      code: 'approval_stale',
      text: 'A required node ran again, or an approved file changed, after the approval.',
      detail: 'implement ran again after approval',
    })
    expect(approvalRefusal('approval_mismatch: abc')?.text).toBe(
      'The commit to push is not the commit the approved result was taken at.',
    )
    // An agent output that merely mentions a code, or an unknown code.
    expect(approvalRefusal('done; approval_missing: none')).toBeNull()
    expect(approvalRefusal('approval_later: x')).toBeNull()
    expect(approvalRefusal('')).toBeNull()
  })

  it('takes the output of failed nodes only, and the run failure of a gate whose snapshot stopped verifying', () => {
    const detail = {
      nodes: { publish: 'failed', implement: 'succeeded', review: 'failed', other: 'failed' },
      outputs: {
        publish: 'approval_missing: gate review has no approving decision',
        implement: 'approval_missing: quoted by an agent',
        other: 'exit 1',
      },
      failure_reason: 'node `review`: approval_snapshot_unreadable: ' + 'a'.repeat(64),
    } as unknown as RunDetail
    const got = nodeApprovalRefusals(detail)
    expect([...got.keys()].sort()).toEqual(['publish', 'review'])
    expect(got.get('publish')?.code).toBe('approval_missing')
    expect(got.get('review')).toMatchObject({ code: 'approval_snapshot_unreadable', detail: 'a'.repeat(64) })
  })
})
