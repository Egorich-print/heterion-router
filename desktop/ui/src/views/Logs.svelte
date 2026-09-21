<script>
  import { getJson } from "../api.js";

  let { key } = $props();
  let limit = $state(50);
  let rows = $state(null);
  let appLines = $state([]);
  let appLoading = $state(false);
  let error = $state("");
  let tab = $state("calls");

  $effect(() => {
    if (!key) return;
    const count = limit;
    let cancelled = false;
    getJson(`/api/logs?limit=${count}`, key)
      .then((payload) => {
        if (!cancelled) {
          rows = payload.logs;
          error = payload.error ?? "";
        }
      })
      .catch((err) => {
        if (!cancelled) error = err.message;
      });
    return () => { cancelled = true; };
  });

  async function loadAppLogs() {
    if (!key) return;
    appLoading = true;
    try {
      const payload = await getJson(`/api/logs/app?limit=${limit}`, key);
      appLines = payload.lines ?? [];
    } catch (_err) {
      appLines = [];
    }
    appLoading = false;
  }

  const shortTime = (value) => (value ?? "").replace("T", " ").replace(/\.\d+Z?$/, "");

  const statusClass = (status) => {
    if (status === null || status === undefined) return "";
    if (status >= 500) return "bad";
    if (status >= 400) return "warn";
    return "ok";
  };
</script>

<h1>Logs</h1>

<div class="toolbar">
  <div class="tabs">
    <button class="{tab === 'calls' ? 'active' : ''}" onclick={() => { tab = 'calls' }}>API calls</button>
    <button class="{tab === 'app' ? 'active' : ''}" onclick={() => { tab = 'app'; loadAppLogs() }}>Application</button>
  </div>
  {#if tab === 'calls'}
    <label>
      rows
      <select bind:value={limit}>
        <option value={25}>25</option>
        <option value={50}>50</option>
        <option value={100}>100</option>
        <option value={250}>250</option>
      </select>
    </label>
  {/if}
</div>

{#if tab === 'calls'}
  {#if error}<div class="error">{error}</div>{/if}
  {#if rows}
    <table>
      <thead>
        <tr>
          <th>time</th><th>status</th><th>model</th><th>provider</th>
          <th class="num">ms</th><th class="num">in</th><th class="num">out</th><th>error</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row}
          <tr>
            <td class="mono muted">{shortTime(row.timestamp)}</td>
            <td><span class="tag {statusClass(row.status)}">{row.status ?? "—"}</span></td>
            <td class="mono">{row.model ?? row.requested_model ?? "—"}</td>
            <td class="mono muted">{row.provider ?? "—"}</td>
            <td class="num">{row.duration_ms ?? "—"}</td>
            <td class="num">{row.tokens_input ?? 0}</td>
            <td class="num">{row.tokens_output ?? 0}</td>
            <td class="muted">{row.error_summary ?? ""}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if rows.length === 0}
      <p class="muted">No calls recorded yet.</p>
    {/if}
  {:else if !error}
    <p class="muted">loading…</p>
  {/if}
{:else}
  {#if appLoading}<p class="muted">loading…</p>
  {:else if appLines.length === 0}
    <p class="muted">No application logs yet.</p>
  {:else}
    <div class="app-logs">
      {#each appLines as line}
        <div class="log-line mono">{line}</div>
      {/each}
    </div>
  {/if}
{/if}

<p class="sub">Application logs are written to <code>gateway.log</code> in the data dir (RUST_LOG level).</p>