# Switchyard Dynamic Routing Integration — Status & Continuation Guide

> **Document Type:** Project Handover & Architecture Continuity Guide  
> **Target:** Any LLM model / engineer / subagent picking up work on `heterion-router`.  
> **Last Updated:** 2026-11-15 18:10 UTC+3  
> **Working Directory:** `/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router`  

---

## 1. Project Goal & Overview

Replace static manual combos (`main-1m`, `main-262k`, etc.) in `heterion-router` with an **NVIDIA Switchyard-inspired in-process intelligent dynamic router** exposed to clients as a single unified model:
- **Primary Model ID:** `heterion-router/auto` (with alias `auto`).

### Key Architectural Pillars
1. **Zero-Quota Algorithmic Context Pruning:**
   - Instead of calling LLMs to summarize context (which wastes quotas, adds 2-3s latency, and creates failure points), pruning runs in **pure deterministic Rust in <50 µs with 0 tokens consumed**.
   - Preserves: `system prompt`, initial user goal, active trailing 4 turns (`verbatim`), and 1-to-1 `tool_call_id` parity.
   - Folds bulky intermediate tool outputs older than 4 turns into structured digests with UTF-8 safe boundary slicing:
     `[Tool Output Digest: ... lines, status, head, tail]`.
   - When 1M models are exhausted or rate-limited (429/403), this prunes 250k prompts down to ~35k, allowing immediate handoff to 262k/128k tiers (`codestral`, `gemma-26b`, `deepseek`).
2. **Dynamic Priority Ladder:**
   - $\text{Effective Priority} = \text{Base Priority} \times \text{Quality Score} \times \text{Availability State}$
   - **Tier 1 (Flagship / 1M / Deep Reasoning):** `gemini/gemini-3-flash-preview`, `agentrouter/gpt-5.6-sol`, `agentrouter/gpt-6.1-sol`.
   - **Tier 2 (Workhorses / 262k / Code Specialists):** `mistral/codestral-latest`, `gemini/gemma-4-26b-a4b-it`.
   - **Tier 3 (High-Speed / Quota Protectors):** `gemini/gemini-3.5-flash-lite`, `gemini-3.1-flash-lite`.
3. **Resilience & Fault Isolation:**
   - **HTTP 429:** Exponential backoff (30s $\rightarrow$ 60s $\rightarrow$ 120s) or `Retry-After`; the cooldown is **model-scoped** — only the rate-limited model leaves the ladder, the sibling models of the same provider stay usable; the request cascades immediately to the next tier; the model is restored to the ladder when the cooldown expires.
   - **HTTP 403 / 402:** **Provider-scoped** balance freeze for 1 hour (`DEFAULT_BALANCE_FREEZE`) — the balance belongs to the account, not to one model; no futile roundtrips.
   - **HTTP 502 / 504 / Transport Drops:** Managed by the existing `BreakerSet`; a repeated 5xx additionally puts the *model* into a 15-minute probation, so a single dead model never takes its whole provider down.
   - **Defective Responses (Pseudo-XML, empty output, repetition loops):** Penalized `QualityScore -= 0.3`, demoted 1-2 tiers; 15-minute probation on repeated failures; gradual recovery back to 1.0 on clean streaks.
4. **Strict Tool-Calling Isolation:**
   - Models emitting raw XML `<tool_call>` (`morph`, `syn-pro`) are classified as `ToolSupport::PseudoXml` and **strictly excluded** from any request containing `request.tools`.

---

## 2. Milestone Implementation Status

The project is structured into 5 milestones tracked under `.agents/teamwork/`:

| Milestone | Description | Status | Key Files |
|---|---|---|---|
| **M1** | Request Inspector & Model Capability Catalog | **DONE & AUDITED (CLEAN)** | `crates/heterion-router-gateway/src/inspector.rs`, `model_spec.rs`, `tests/adversarial_inspector.rs`, `tests/agent_stage_stress.rs`, `tests/empirical_challenge_m1_it3.rs` |
| **M2** | Zero-Quota Algorithmic Context Pruning | **DONE & GATED** | `crates/heterion-router-gateway/src/context_pruner.rs`, `tests/context_pruner.rs`, `tests/empirical_challenge_m2_*.rs` |
| **M3** | Resilience, Dynamic Priority Ladder & Quality Guard | **DONE & GATED** | `crates/heterion-router-gateway/src/resilience.rs`, `crates/heterion-router-routing/src/breaker.rs`, `crates/heterion-router-http/src/client.rs` |
| **M4** | Switchyard Dynamic Router (`heterion-router/auto`) | **DONE & GATED** | `crates/heterion-router-gateway/src/routing.rs` (candidate plan, cooldowns, failover), `src/lib.rs` (`/v1/models` listing) |
| **M5** | End-to-End Verification, CI Gates & Deployment | **IN PROGRESS** | `scripts/ci/check.sh`, `crates/heterion-router-gateway/tests/empirical_challenge_m5_*.rs` |

> **Note on M4:** the milestone was *not* defect-free when it was first gated. A 429 on one model
> put the whole **provider** into cooldown, which made the Tier-3 "high-quota protectors"
> unreachable and turned a rate limit into a hard failure. The fix (model-scoped 429 cooldown)
> and the tests that pin it are part of the M5 change set — see
> `.agents/teamwork/challenger_m4_1/handoff.md:59-85`.

---

## 3. How to Resume Work (If Session / Quota Changes)

If this session disconnects or another agent takes over, follow these exact steps:

1. **Check Git Status & Active Changes:**
   ```bash
   cd /Volumes/Netac_SSD/ai-workstation/Projects/heterion-router
   git status
   ```
2. **Review Multi-Agent Progress State:**
   - Read `.agents/teamwork/ORIGINAL_REQUEST.md` (authoritative task specifications).
   - Read `.agents/teamwork/PROJECT.md` (milestone execution breakdown).
   - Check the latest handoffs in `.agents/teamwork/worker_m2_it2/` or current active worker.
3. **Run CI Gates to verify codebase state:**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
4. **Where the work actually stands:**
   - **M1–M4 are implemented and gated.** The dynamic ladder, the model-scoped cooldowns, the
     quality tracker and the `auto` candidate plan live in
     `crates/heterion-router-gateway/src/{routing,resilience,inspector,model_spec,context_pruner}.rs`.
   - **M5 is the verification milestone** — adversarial suites, the CI gate and the deployment.
   - **For Milestone 5:** verify with the repository gate, **not** against the live service:
     ```bash
     cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
     # or, the same thing CI runs:
     ./scripts/ci/check.sh
     ```
     Per the project rule (`AGENTS.md`), mission changes are **not** verified against the live
     launchd instance on port `20128` — the service keeps serving the previously deployed build
     until a deliberate deploy, and a deploy is a separate, explicit step
     (`scripts/deploy-service.sh`, then `launchctl kickstart -k gui/$(id -u)/com.heterion.router`).

---

## 4. MiniMax Code (MCode) Configuration

The model `heterion-router/auto` is registered in `/Users/egorich/.minimax/config.yaml` under `custom_provider: heterion:`:
```yaml
      "heterion-router/auto":
        name: "⚡ Heterion Auto (Switchyard Dynamic Routing)"
        enabled: true
        configuration_source: manual
        reasoning: true
        tool_call: true
        temperature: true
        attachment: true
        modalities:
          input: [ text, image ]
          output: [ text ]
        limit:
          context: 1048576
          output: 65536
        thinking:
          effortOptions: [ low, medium, high ]
          defaultEffort: medium
```

All requests to port `20128` are automatically logged to SQLite database at `/Users/egorich/.heterion-router/storage.sqlite` in tables `call_logs`, `usage_history`, and `daily_usage_summary`.
