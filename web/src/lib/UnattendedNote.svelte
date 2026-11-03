<script lang="ts">
  import type { UnattendedOutcome } from './api.gen'
  import { unattendedLine, unattendedStops } from './nextactions'

  // What an unattended run (`apb run --unattended`) does at one wait: the
  // line the CLI and MCP show, marked when the run would stop there.
  let { outcome, testid }: { outcome: UnattendedOutcome; testid: string } = $props()
</script>

<p
  class="unattended"
  class:stops={unattendedStops(outcome)}
  title="What an unattended run (apb run --unattended) does at this wait"
  data-testid={testid}
>
  {unattendedLine(outcome)}
</p>

<style>
  .unattended {
    margin: 0;
    color: var(--muted-foreground);
    font-size: 11px;
  }
  .unattended.stops {
    color: var(--warning);
  }
</style>
