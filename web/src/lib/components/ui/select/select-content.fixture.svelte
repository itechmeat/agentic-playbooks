<script lang="ts">
  // Test fixture for select-content.test.ts: one picker, the way the pages
  // use it: a trigger that shows the picked value, and two options.
  import * as Select from './index'

  let { open = $bindable(false) }: { open?: boolean } = $props()
  let value = $state('a')
</script>

<Select.Root type="single" bind:open bind:value>
  <Select.Trigger>{value}</Select.Trigger>
  <Select.Content>
    <Select.Item value="a" label="a">a</Select.Item>
    <Select.Item value="b" label="b">b</Select.Item>
  </Select.Content>
</Select.Root>
