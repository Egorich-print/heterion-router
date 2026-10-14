# Plan: Heterion-Router Combo / Provider / Reasoning Hardening

**Date:** 2026-10-14  
**Status:** Execution started  
**Based on:** User confirmation of all 4 clarifications + full plan

---

## 0. Confirmed Decisions (Locked)

| Item | Decision |
|------|----------|
| Scope | F (all layers): registry.json, DB seed, combos.rs, routing, pricing.rs, gateway/lib.rs, docs/research/, OpenCode (opencode.json) |
| Deletion policy | No data deletion without necessity. Dead combos → archive/inactive in OpenCode. Dead fields (C-13/C-14) refactor with ADR + regression tests. |
| Benchmark sources | Artificial Analysis + provider docs. Reasoning predefined in heterion-router. In OpenCode: remove reasoning control for heterion-router models. |
| exa-search | Add as search tool (not chat model). |
| agentrouter | Fix loop behavior (reproduce, identify cause, fix with red-first test). |

---

## Phase 0 — Read-Only Inventory + Smoke Tests (NOW)

**Goal:** Confirm current state of each provider/model across 6 categories.

### 0.1 Registry Audit
- [ ] Script: list all 278 providers, 2717 models; identify any missing format/executor
- [ ] Identify models with unmodelled formats (clova, kiro, cursor, etc.)

### 0.2 Provider Status Check (controlled smoke tests, minimal spend)
For each: agentrouter, exa-search, grok-cli, openrouter, pollinations, deepseek, mistral, qoder, commandcode, z-ai, google/gemini/ai-studio, heterion-local:
- [ ] [A] Registered in registry.json?
- [ ] [B] Auth/credential works?
- [ ] [C] Model accepts request, returns correct response?
- [ ] [D] Format, streaming, tools work?
- [ ] [E] Limitations: balance, quota, rate limit, block?
- [ ] [F] Result: SUCCESS / FAIL / SKIP (reason)

### 0.3 Real Response Verification
Test working models from code: grok-4.6 (grok-cli/openrouter/heterion-local), claude-opus-4, deepseek-v4-flash (ds)
- [ ] Save request, response (no secrets), status, latency, tokens

### 0.4 exa-search Check
- [ ] grep for exa-search in crates/
- [ ] Confirm missing integration

### 0.5 Agentrouter Loop Reproduction
- [ ] Find agentrouter usage in routing/gateway/combos
- [ ] Reproduce minimal cycle scenario
- [ ] Document: missing termination, timeout, max steps, retry limit

---

## Phase 1 — Real Model Inventory + Evidence Table
Output: `docs/research/mission-2026-10/model_inventory_2026-10-14.md`

Table columns: Provider | Model (API id) | Format | Context | Auth | Response Verified | Reasoning Supported | Price | Balance/Quota | Notes

Sections: Frontier (large ctx), Frontier (medium ~262K), Coding/Agentic, Economic/Stable, Search/Research, Google/Gemini/AI Studio (separate)

---

## Phase 2 — exa-search Integration + agentrouter Fix
- [ ] Add exa-search to registry.json if missing
- [ ] Integrate as search tool mechanism (not chat combo)
- [ ] Real search request test (content verification, not just HTTP 200)
- [ ] Fix agentrouter loop (termination, timeout, max steps, tool call cycle)
- [ ] Red-first test for loop fix

---

## Phase 3 — Combo Structure Fixes (DB + Code, No Data Deletion)
- [ ] Archive existing 8 combo rows (vivanta-auto/deepseek with empty models → inactive)
- [ ] New combo rows via seed/migration (preserve old)
- [ ] combos.rs: handle dead fields (max_set_retries, set_retry_delay_ms, weight, strategy:auto, combo_configs, load_combos double) — ADR decision
- [ ] routing.rs: update prefix rules, valid fallback chains

---

## Phase 4 — New Combo Design (Context Separation, Balance-Aware)
Groups:
- A. Frontier large context (~1M+) with same-class fallback
- B. Frontier confirmed ~262K with same-class fallback (no silent 1M transition)
- C. Coding/Agentic (tool calling, stability, agentrouter fix)
- D. Economic/Stable (mistral $10, qoder quota, z-ai post-reset, pollinations keyless)
- E. Search/Research (exa-search + others, search API not in chat list)
- F. Google/Gemini/AI Studio (separate, unstable, deprecated 2.5 Flash/Pro)

Balance rules: no DeepSeek/Mimo/Commandcode in default combos without balance confirmation; Z.AI after 11:30 14.10; Mistral cost-aware; Qoder only if unblocked.

---

## Phase 5 — Reasoning / Intelligence Mode Predefinition
- [ ] Per-model: check provider docs for reasoning params (OpenAI reasoning_effort, Anthropic thinking/budget, Gemini thinking)
- [ ] Artificial Analysis + fixed task comparison
- [ ] Predefine in heterion-router: model → {mode, budget, timeout, max_steps}
- [ ] Update opencode.json: remove `reasoning: true` for heterion-router models
- [ ] Document which models have predefined reasoning

---

## Phase 6 — Pricing + Usage + Tests
- [ ] Verify/extend pricing.rs for confirmed models
- [ ] Fix C-7 usage tracking: final provider, combo_name, connection_id, real HTTP status, requested_model ≠ model
- [ ] Red-first tests for all new fixes

---

## Phase 7 — Documentation + ADR
- [ ] ADR-013: combo dead fields refactor
- [ ] ADR-014: reasoning predefinition + OpenCode integration
- [ ] ADR-015: frontier combo design (context separation, balance awareness)
- [ ] ADR-016: exa-search integration
- [ ] ADR-017: agentrouter loop fix
- [ ] Update model_inventory, combo_design_notes, ROADMAP-2026-10.md, AGENTS.md

---

## Verification Gates (Per Phase)
1. `cargo fmt --all -- --check` → exit 0
2. `cargo clippy --workspace --all-targets -- -D warnings` → 0 warnings
3. `scripts/ci/check.sh` → `all green`
4. `cargo test --workspace` → 556 passed / 0 failed (or updated)
5. `git diff crates/heterion-router-db/src/migrate.rs` clean
6. No broken links (36/0 verified)
7. Independent verification subagent report
8. Active combos selectable in OpenCode
9. Agentrouter loop fixed (reproduced → fixed → green test)
10. Exa-search verified (search request + response content)

**No commits without separate user approval.**
