# Evidence per change kind

## Bug fix

- Follow the apb-diagnosis skill; this list is the evidence it must leave.
- Keep symptom, hypothesis, cause and mitigation apart.
- Reproduce at the boundary that owns the failure when you can.
- Show the failure happens for the reason you claim before the fix, and that
  it passes after the fix.
- Use small, bounded experiments to tell causes apart.
- For a failure you cannot reach or that comes and goes, credible incident
  observations with stated uncertainty beat a contrived test.
- Fix the responsible boundary. Verify the original scenario and the
  guarantees around it.
- Write the regression test before the fix and see it fail for the named
  cause; gate it with the apb-test-audit skill. When a test is not
  practical, record why and what evidence stands in for it.
- A containment without a known cause is a mitigation, not a verified fix.
  Label it so.

## Refactoring

- Name a realistic later change or use that becomes simpler: fewer owners of
  one rule, clearer dependencies, a safer interface, less ripple.
- A smaller line count or a preferred pattern is not a benefit by itself.
- Before you restructure, list what must not change: public interfaces, error
  meaning, formats, ordering, concurrency, effects, performance.
- Keep distinct domain rules apart even when the code looks alike.
- Change in small steps with feedback at the real interfaces.
- Support preservation with existing tests, characterization tests,
  side-by-side comparisons or targeted measurements. A sample covers only
  what it observed.
- Any intended behavior change gets the feature or fix obligations.

## Feature

- One design pass (apb-brainstorm), or reuse of a current design record
  (advised, or fully specified).
- Acceptance comes from the original request, not from the code you wrote.
- Cover the main scenario, the edge cases the design names and the behavior
  that must keep working.
