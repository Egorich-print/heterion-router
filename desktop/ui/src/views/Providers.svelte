<script>
  import { apiBase, getJson, patchJson, postJson } from "../api.js";

  let { key } = $props();
  let connections = $state(null);
  let error = $state("");
  let notice = $state("");
  let onlyProblems = $state(false);
  let savingId = $state(null);
  let restartPending = $state(false);
  let restarting = $state(false);

  async function load() {
    try {
      const payload = await getJson("/api/connections", key);
      connections = payload.connections;
      error = payload.connection_error ?? "";
    } catch (err) {
      error = err.message;
    }
  }

  $effect(() => {
    if (!key) return;
    load();
  });

  async function save(row, patch) {
    savingId = row.id;
    error = "";
    notice = "";
    try {
      const updated = await patchJson(`/api/connections/${row.id}`, key, patch);
      if (updated.restart_required) {
        restartPending = true;
        notice =
          "Saved. Connection activity changed — restart the gateway to apply it (backends are built at startup).";
      } else {
        notice = "Saved.";
      }
      await load();
    } catch (err) {
      error = err.message;
    } finally {
      savingId = null;
    }
  }

  async function restartNow() {
    restarting = true;
    error = "";
    try {
      const before = await fetch(`${apiBase()}/healthz`).then((res) => res.json());
      await postJson("/api/restart", key, {});
      notice = "Restart requested — waiting for the gateway to come back…";
      for (let i = 0; i < 30; i++) {
        await new Promise((resolve) => setTimeout(resolve, 2000));
        try {
          const health = await fetch(`${apiBase()}/healthz`).then((res) => res.json());
          if (health.status === "ok" && health.started_unix !== before.started_unix) {
            notice = "Gateway restarted and serving.";
            restartPending = false;
            await load();
            return;
          }
        } catch {
          /* still down — keep waiting */
        }
      }
      error = "Gateway did not come back within a minute — check the service.";
    } catch (err) {
      error = err.message;
    } finally {
      restarting = false;
    }
  }

  const shown = $derived(
    (connections ?? []).filter((row) => {
      if (!onlyProblems) return true;
      if (!row.is_active) return true;
      if (!row.has_credential) return true;
      if (!row.servable) return true;
      return Boolean(row.last_error);
    }),
  );

  /// The measured reason of a provider, in the operator's words.
  ///
  /// The reason comes from the platform's own answer (ADR-032), not from what
  /// the registry claims — so the wording says what the platform did.
  const REASON_TEXT = {
    served: "served — answers without a key",
    needs_key: "needs a key — the platform refused an anonymous call",
    credits_required: "needs credits — the platform wants a paid balance",
    not_served: "not served",
    no_connection_row: "no connection row — keyless, nothing to attach",
    no_web_adapter: "needs code — the web driver has no adapter for it",
    unsupported_format: "needs code — the gateway cannot speak this format",
    not_a_chat_api: "dead — the endpoint is not a chat API",
    dead_endpoint: "dead — the endpoint is gone",
    unknown: "unknown — the platform did not answer clearly",
  };

  /// The four groups the operator works with, plus the leftovers.
  const GROUPS = [
    { id: "live", label: "served now", reasons: ["served"] },
    { id: "code", label: "needs code", reasons: ["unsupported_format", "no_web_adapter"] },
    { id: "dead", label: "dead", reasons: ["dead_endpoint", "not_a_chat_api"] },
    { id: "key", label: "waiting for a key", reasons: ["needs_key", "no_connection_row"] },
    { id: "other", label: "other", reasons: ["credits_required", "not_served", "unknown"] },
  ];

  function groupOf(reason) {
    const found = GROUPS.find((group) => group.reasons.includes(reason));
    return found ? found.id : "other";
  }

  function reasonText(reason) {
    return REASON_TEXT[reason] ?? reason;
  }

  /// The matrix is fetched next to the connections: both come from the
  /// gateway, the matrix just answers a different question.
  let matrix = $state(null);
  let matrixError = $state("");
  let matrixGroup = $state("all");
  let matrixQuery = $state("");
  let probingId = $state(null);

  async function loadMatrix() {
    try {
      const payload = await getJson("/api/providers", key);
      matrix = payload.matrix ?? [];
      matrixError = "";
    } catch (err) {
      matrixError = err.message;
    }
  }

  $effect(() => {
    if (!key) return;
    loadMatrix();
  });

  /// Count the providers per group, ignoring the current filter.
  const groupCounts = $derived.by(() => {
    const counts = { all: (matrix ?? []).length };
    for (const group of GROUPS) counts[group.id] = 0;
    for (const row of matrix ?? []) counts[groupOf(row.reason)] += 1;
    return counts;
  });

  const matrixShown = $derived(
    (matrix ?? []).filter((row) => {
      if (matrixGroup !== "all" && groupOf(row.reason) !== matrixGroup) return false;
      const query = matrixQuery.trim().toLowerCase();
      if (!query) return true;
      return row.id.toLowerCase().includes(query) || (row.name ?? "").toLowerCase().includes(query);
    }),
  );

  /// Probe one provider again — the reason on screen is a measurement, and a
  /// measurement goes stale.
  async function probe(row) {
    probingId = row.id;
    matrixError = "";
    try {
      await postJson(`/api/providers/${row.id}/probe`, key, {});
      await loadMatrix();
    } catch (err) {
      matrixError = err.message;
    } finally {
      probingId = null;
    }
  }

  /// Secrets are never returned; only whether one exists.
  function credentialLabel(row) {
    if (row.has_credential) return row.auth_type === "oauth" ? "token" : "key";
    return "missing";
  }
</script>

<h1>Providers</h1>
<p class="sub">
  Connections from the shared database. <strong>servable</strong> means a backend is built for that
  provider, which requires an active row with a credential and an executor for its format.
  Toggling activity needs a gateway restart; priority and name apply on the next read.
</p>

{#if error}<div class="error">{error}</div>{/if}
{#if notice}
  <div class="notice">
    {notice}
    {#if restartPending}
      <div class="toolbar" style="margin:10px 0 0">
        <button class="primary" disabled={restarting} onclick={restartNow}>
          {restarting ? "restarting…" : "Restart now"}
        </button>
      </div>
    {/if}
  </div>
{/if}

<div class="toolbar">
  <label><input type="checkbox" bind:checked={onlyProblems} /> only problems</label>
</div>

{#if connections}
  <table>
    <thead>
      <tr>
        <th>provider</th><th>name</th><th>auth</th><th>credential</th>
        <th>active</th><th>priority</th><th>servable</th><th>expires</th><th>last error</th>
      </tr>
    </thead>
    <tbody>
      {#each shown as row}
        <tr>
          <td class="mono">{row.provider}</td>
          <td>{row.name ?? "—"}</td>
          <td class="muted">{row.auth_type ?? "—"}</td>
          <td>
            {#if row.has_credential}
              <span class="tag ok">{credentialLabel(row)}</span>
            {:else}
              <span class="tag bad">missing</span>
            {/if}
          </td>
          <td>
            <button
              disabled={savingId === row.id}
              title={row.is_active ? "Deactivate (needs restart)" : "Activate (needs restart)"}
              onclick={() => save(row, { is_active: !row.is_active })}
            >
              {#if row.is_active}on{:else}off{/if}
            </button>
          </td>
          <td class="mono">
            <button
              disabled={savingId === row.id}
              title="Lower priority"
              onclick={() => save(row, { priority: row.priority - 1 })}
            >−</button>
            {row.priority}
            <button
              disabled={savingId === row.id}
              title="Raise priority"
              onclick={() => save(row, { priority: row.priority + 1 })}
            >+</button>
          </td>
          <td>
            {#if row.servable}<span class="tag ok">yes</span>
            {:else}<span class="tag bad">no</span>{/if}
          </td>
          <td class="mono muted">{row.expires_at ?? "—"}</td>
          <td class="muted">{row.last_error ?? "—"}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if shown.length === 0}
    <p class="muted">All connections look healthy.</p>
  {/if}
{:else if !error}
  <p class="muted">loading…</p>
{/if}

<h2>Provider matrix</h2>
<p class="sub">
  The whole registry with the <strong>measured</strong> reason for each provider — the platform's own
  answer, not what the registry claims (ADR-032). Probe a provider to refresh its reason.
</p>

{#if matrixError}<div class="error">{matrixError}</div>{/if}

{#if matrix}
  <div class="toolbar">
    <button class:primary={matrixGroup === "all"} onclick={() => (matrixGroup = "all")}>
      all {groupCounts.all}
    </button>
    {#each GROUPS as group}
      <button class:primary={matrixGroup === group.id} onclick={() => (matrixGroup = group.id)}>
        {group.label} {groupCounts[group.id]}
      </button>
    {/each}
    <input type="search" placeholder="filter by id or name" bind:value={matrixQuery} />
  </div>

  <table>
    <thead>
      <tr>
        <th>provider</th><th>name</th><th>format</th><th>models</th><th>reason</th>
        <th>last probe</th><th></th>
      </tr>
    </thead>
    <tbody>
      {#each matrixShown as row}
        <tr>
          <td class="mono">{row.id}</td>
          <td>{row.name ?? "—"}</td>
          <td class="mono muted">{row.format || "—"}</td>
          <td class="mono">{row.models}</td>
          <td>
            {#if row.served}
              <span class="tag ok">{reasonText(row.reason)}</span>
            {:else}
              <span class="tag bad">{reasonText(row.reason)}</span>
            {/if}
          </td>
          <td class="mono muted">
            {#if row.last_probe}
              {row.last_probe.status ?? "—"} · {row.last_probe.at ?? "—"}
            {:else}
              never
            {/if}
          </td>
          <td>
            <button disabled={probingId === row.id} onclick={() => probe(row)}>
              {probingId === row.id ? "probing…" : "probe"}
            </button>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if matrixShown.length === 0}
    <p class="muted">No provider matches this filter.</p>
  {/if}
{:else if !matrixError}
  <p class="muted">loading…</p>
{/if}
