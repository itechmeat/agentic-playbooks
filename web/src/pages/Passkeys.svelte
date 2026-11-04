<script lang="ts">
  // The passkeys page: the machine's enrolled WebAuthn credentials, with the
  // enroll and remove ceremonies. The ceremony logic lives in
  // lib/passkeys.ts; this page only wires the real fetch calls and the
  // browser navigator API into it.
  import { getJson, requestJson, jsonHeaders, ApiError } from '../lib/api/http'
  import {
    PASSKEYS_MESSAGE,
    challengeBytes,
    enrollNewPurpose,
    removePurpose,
    runEnroll,
    secureContextOk,
    type EnrollEntry,
    type EnrollStart,
  } from '../lib/passkeys'
  import Topbar from '$lib/components/Topbar.svelte'
  import PageScroll from '$lib/components/PageScroll.svelte'
  import PageHead from '$lib/components/PageHead.svelte'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import * as Empty from '$lib/components/ui/empty'
  import { Skeleton } from '$lib/components/ui/skeleton'
  import { toast } from 'svelte-sonner'
  import KeyRound from '@lucide/svelte/icons/key-round'
  import Plus from '@lucide/svelte/icons/plus'
  import Trash2 from '@lucide/svelte/icons/trash-2'

  interface PasskeyEntry {
    label: string
    alg: number
    sign_count: number
    created_at: string
    id_prefix: string
  }

  let items = $state<PasskeyEntry[]>([])
  let count = $state(0)
  let loaded = $state(false)
  let secure = $state(secureContextOk())
  let adding = $state(false)
  let label = $state('')
  let code = $state('')
  let busy = $state(false)
  let removing = $state<string | null>(null)

  async function load() {
    try {
      const res = await getJson<{ passkeys: PasskeyEntry[]; count: number }>('/api/passkeys')
      items = res.passkeys
      count = res.count
    } catch (e) {
      toast.error('Failed to load passkeys', { description: String(e) })
    } finally {
      loaded = true
    }
  }

  $effect(() => {
    load()
  })

  // Base64url of an ArrayBuffer: the string shape the routes read verbatim
  // out of the browser API's binary fields.
  function b64u(buf: ArrayBuffer): string {
    const bytes = new Uint8Array(buf)
    let s = ''
    for (const b of bytes) s += String.fromCharCode(b)
    return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
  }

  async function post<T>(url: string, body: unknown): Promise<T> {
    return requestJson<T>(url, { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) })
  }

  function fail(e: unknown, what: string): string {
    const message = e instanceof ApiError ? e.message : String(e)
    toast.error(what, { description: message })
    return message
  }

  // The real assertion call behind the injected dep of the ceremonies.
  async function getAssertion(challenge: string, rpId: string) {
    const cred = await navigator.credentials.get({
      publicKey: {
        challenge: challengeBytes(challenge),
        rpId,
        userVerification: 'required',
      },
    })
    if (!cred) throw new Error('no credential returned')
    const r = (cred as PublicKeyCredential).response as AuthenticatorAssertionResponse
    return {
      id: cred.id,
      clientDataJSON: b64u(r.clientDataJSON),
      authenticatorData: b64u(r.authenticatorData),
      signature: b64u(r.signature),
    }
  }

  async function createCredential(start: EnrollStart) {
    const cred = await navigator.credentials.create({
      publicKey: {
        challenge: challengeBytes(start.challenge),
        rp: { id: start.rpId, name: 'apb' },
        user: {
          id: crypto.getRandomValues(new Uint8Array(16)),
          name: 'apb local',
          displayName: 'apb local',
        },
        pubKeyCredParams: start.algorithms.map((alg) => ({ type: 'public-key' as const, alg })),
        attestation: start.attestation as AttestationConveyancePreference,
        authenticatorSelection: {
          userVerification: start.userVerification as UserVerificationRequirement,
        },
      },
    })
    if (!cred) throw new Error('no credential returned')
    const r = (cred as PublicKeyCredential).response as AuthenticatorAttestationResponse
    return {
      id: cred.id,
      clientDataJSON: b64u(r.clientDataJSON),
      attestationObject: b64u(r.attestationObject),
    }
  }

  // A later passkey proves an existing one first; the verify hands back the
  // assertion token the enroll start route expects.
  async function enrollNewToken(): Promise<string> {
    const purpose = enrollNewPurpose()
    const { challenge } = await post<{ challenge: string }>('/api/passkeys/assert/challenge', {
      purpose,
    })
    const assertion = await getAssertion(challenge, location.hostname)
    const { token } = await post<{ token: string }>('/api/passkeys/assert/verify', {
      purpose,
      challenge,
      ...assertion,
    })
    return token
  }

  async function doEnroll() {
    if (!label.trim()) {
      toast.error('Give the passkey a label')
      return
    }
    busy = true
    try {
      const assertionToken = count > 0 ? await enrollNewToken() : undefined
      const got = await runEnroll(
        {
          startEnroll: (body) => post<EnrollStart>('/api/passkeys/enroll/start', body),
          finishEnroll: (body) => post<EnrollEntry>('/api/passkeys/enroll/finish', body),
          createCredential,
        },
        {
          label: label.trim(),
          code: count > 0 ? undefined : code.trim() || undefined,
          assertionToken,
        },
      )
      if (!got.ok) {
        if (got.message === PASSKEYS_MESSAGE) toast.error(PASSKEYS_MESSAGE)
        else toast.error('Enroll failed', { description: got.message })
        return
      }
      toast.success(`Enrolled "${got.entry.label}"`)
      adding = false
      label = ''
      code = ''
      await load()
    } catch (e) {
      fail(e, 'Enroll failed')
    } finally {
      busy = false
    }
  }

  async function removeKey(entry: PasskeyEntry) {
    removing = entry.label
    try {
      const purpose = removePurpose(entry.label)
      const { challenge } = await post<{ challenge: string }>('/api/passkeys/assert/challenge', {
        purpose,
      })
      const assertion = await getAssertion(challenge, location.hostname)
      const { token } = await post<{ token: string }>('/api/passkeys/assert/verify', {
        purpose,
        challenge,
        ...assertion,
      })
      await requestJson(`/api/passkeys/${encodeURIComponent(entry.label)}`, {
        method: 'DELETE',
        headers: jsonHeaders,
        body: JSON.stringify({ token }),
      })
      toast.success(`Removed "${entry.label}"`)
      await load()
    } catch (e) {
      fail(e, 'Remove failed')
    } finally {
      removing = null
    }
  }

  const algName = (alg: number) =>
    alg === -7 ? 'ES256' : alg === -257 ? 'RS256' : alg === -8 ? 'EdDSA' : `alg ${alg}`
</script>

<Topbar active="passkeys" />

<PageScroll>
  <div class="wrap">
    <PageHead title="Passkeys" class="mb-2">
      {#snippet meta()}
        {#if loaded}
          <span class="text-muted-foreground">{count} enrolled</span>
        {/if}
      {/snippet}
    </PageHead>
    <p class="intro">
      Passkeys on this machine. A review that needs a person takes one of these instead of a session
      key, so approving a run needs the key it is bound to.
    </p>

    {#if !secure}
      <div class="notice" role="note">{PASSKEYS_MESSAGE}</div>
    {:else}
      {#if adding}
        <section class="box add">
          <h2 class="box-cap">Add a passkey</h2>
          <label class="field">
            <span>Label</span>
            <Input bind:value={label} placeholder="laptop" disabled={busy} />
          </label>
          {#if count === 0}
            <label class="field">
              <span>One-time code</span>
              <Input bind:value={code} placeholder="asked once, for the first passkey" disabled={busy} />
            </label>
          {:else}
            <p class="hint">Prove an existing passkey to enroll the next one.</p>
          {/if}
          <div class="row">
            <Button size="xs" onclick={doEnroll} disabled={busy}>
              {busy ? 'Working' : 'Enroll'}
            </Button>
            <Button variant="outline" size="xs" onclick={() => (adding = false)} disabled={busy}>
              Cancel
            </Button>
          </div>
        </section>
      {:else}
        <div class="row">
          <Button size="sm" onclick={() => (adding = true)}>
            <Plus data-icon="inline-start" />
            Add
          </Button>
        </div>
      {/if}

      {#if !loaded}
        <div class="list">
          {#each Array(2) as _, i (i)}<Skeleton class="h-14 w-full" />{/each}
        </div>
      {:else if items.length === 0}
        <Empty.Root class="empty-box">
          <Empty.Header>
            <Empty.Media variant="icon" class="empty-icon">
              <KeyRound />
            </Empty.Media>
            <Empty.Title class="empty-title">No passkeys yet</Empty.Title>
            <Empty.Description>
              Add one to approve person-only gates from this browser without a session key.
            </Empty.Description>
          </Empty.Header>
        </Empty.Root>
      {:else}
        <section class="box list">
          <h2 class="box-cap">Enrolled · {items.length}</h2>
          <ul>
            {#each items as e (e.id_prefix)}
              <li>
                <span class="name" title={e.label}>{e.label}</span>
                <span class="tag">{algName(e.alg)}</span>
                <span class="meta tabular" title="Created">{new Date(e.created_at).toLocaleString()}</span>
                <span class="meta tabular" title="Credential id prefix">{e.id_prefix}</span>
                <span class="meta tabular" title="Signatures seen">{e.sign_count}</span>
                <Button
                  variant="outline"
                  size="xs"
                  class="remove"
                  onclick={() => removeKey(e)}
                  disabled={removing === e.label}
                >
                  <Trash2 data-icon="inline-start" />
                  <span class="remove-label">Remove</span>
                </Button>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {/if}
  </div>
</PageScroll>

<style>
  /* Plain CSS over the app's variables; no utility framework. */
  .wrap {
    margin: 0 auto;
    width: 100%;
    max-width: 60rem;
    padding: 1.25rem 1rem 2.5rem;
  }
  .intro {
    color: var(--c-muted);
    font-size: 0.75rem;
    margin: 0 0 1.75rem;
  }
  .notice {
    border: 1px solid var(--c-line);
    background: var(--c-panel);
    padding: 1rem;
    font-size: 0.875rem;
  }
  .box {
    border: 1px solid var(--c-line);
    background: var(--c-panel);
  }
  .box-cap {
    font-size: 0.75rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--c-muted);
    margin: 0 0 0.5rem;
  }
  .row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1.25rem;
  }
  .add {
    padding: 1rem;
    margin-bottom: 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.75rem;
    color: var(--c-muted);
    max-width: 20rem;
  }
  .hint {
    font-size: 0.75rem;
    color: var(--c-muted);
    margin: 0;
  }
  .list {
    padding: 1rem;
  }
  .list ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .list li {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.25rem 1rem;
    padding: 0.5rem 0;
    border-top: 1px solid var(--c-line);
  }
  .list li:first-child {
    border-top: none;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .tag {
    font-size: 0.7rem;
    border: 1px solid var(--c-line-2);
    padding: 0.05rem 0.4rem;
    color: var(--c-muted);
  }
  .meta {
    font-size: 0.7rem;
    color: var(--c-muted);
  }
  .tabular {
    font-variant-numeric: tabular-nums;
  }
  .empty-box {
    border: 1px dashed var(--c-line-2);
    background: var(--c-panel);
  }
  .empty-icon {
    border: 1px solid var(--c-line-2);
    background: transparent;
    color: var(--c-muted);
  }
  .empty-title {
    font-size: 0.875rem;
    font-weight: 600;
  }
  @media (max-width: 640px) {
    /* The remove label collapses on a narrow window; the icon stays. */
    .remove .remove-label {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
    }
  }
</style>
