# 03 — Future-AGI parity audit

> **Audit date:** 2026-10-02 · **Subject:** `heterion-router` vs **Future AGI** (futureagi.com)
> **Baseline:** `main` @ `51dfe6d` + working tree (42 dirty paths at time of audit)
> **Method:** Part 1 from official web sources only (futureagi.com, docs.futureagi.com,
> github.com/future-agi, producthunt.com). Part 2 from code only — every claim carries
> `file:line`. No file other than this report was created or modified.
>
> **Evidence labels:** `[V]` verified from an official source · `[C]` verified from code ·
> `[P]` project judgement · `[unverified]` could not be confirmed from an official source.
>
> **Local prior art:** `heterion/HETERION_RESEARCH.md:5` already recorded an intent to
> "integrate FutureAGI SDK only for sampled field traces" — that is the only prior
> Future-AGI reference in the workstation, and it is *not* implemented in
> `heterion-router` (see Part 2.8).

---

## Part 1 — Future-AGI capability inventory

56 capability groups, all from official sources unless marked otherwise.

### 1.1 Observability & tracing

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 1 | **LLM observability / tracing** | Every request becomes a navigable trace with spans, sessions and scores | [docs/observe](https://docs.futureagi.com/docs/observe) · [docs/tracing/concepts](https://docs.futureagi.com/docs/tracing/concepts) | "Every request becomes a trace: the step-by-step record of the model calls, tool calls, and retrievals behind one response." |
| 2 | **OpenTelemetry-native backend** | Ingest is OTLP (HTTP or gRPC), not a proprietary protocol | [docs/tracing/concepts](https://docs.futureagi.com/docs/tracing/concepts) | "Future AGI's observability stack is built on OpenTelemetry." / "Your App → traceAI / OpenTelemetry SDK → OTLP (HTTP or gRPC) → Future AGI Backend" |
| 3 | **Trace search & filters** | 11 span types, 70+ filters, full-text search, session + agent-graph views | [futureagi.com/platform/monitor/tracing](https://futureagi.com/platform/monitor/tracing) · [pricing](https://futureagi.com/pricing) | "11 span types, 70+ filters · full-text search" |
| 4 | **traceAI instrumentation SDK** | OTel instrumentation with LLM-specific spans; Py/TS/Java; one line per framework | [github.com/future-agi/traceAI](https://github.com/future-agi/traceAI) · [docs/integrations](https://docs.futureagi.com/docs/integrations) | "Open-source observability for AI applications - trace every LLM call, prompt, token, retrieval step, and agent decision." |
| 5 | **50+ framework integrations** | LangChain, LangGraph, LlamaIndex, CrewAI, AutoGen, DSPy, Haystack, MCP, Vercel AI SDK, Mastra, Instructor, PromptFlow, Guardrails AI, smolagents, Langfuse SDK | [docs/integrations](https://docs.futureagi.com/docs/integrations) | "TraceAI provides pre-built auto-instrumentation for the following frameworks and LLM providers." |
| 6 | **Dashboards & alerting** | Unlimited custom dashboards; 3 alert monitors (Free) / 15 (Boost) / unlimited (Scale), Slack + email | [pricing](https://futureagi.com/pricing) | "Custom dashboards - unlimited, drag-and-drop" / "3 alert monitors with Slack + email" |
| 7 | **Falcon AI (AI error analysis)** | Auto error clustering, insight generation, auto-tagging, AI-summarised reports, human-readable eval explanations | [pricing](https://futureagi.com/pricing) · [docs/falcon-ai](https://docs.futureagi.com/docs/falcon-ai) | "Error analysis - 4 scoring dimensions, clustering … Auto-tagging - AI-generated tags" |
| 8 | **Error Feed** | Auto-scans traces, decides what went wrong, groups into ticket-like issues with severity/status/assignee and a fix-layer pointer; optional Linear ticket, optional RCA | [docs/error-feed](https://docs.futureagi.com/docs/error-feed) | "reads a sample of the traces in an Observe project, decides for itself what went wrong in each one, and groups the traces that went wrong the same way into a single issue you work like a ticket." |

### 1.2 Evaluation

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 9 | **Evaluation framework (132 built-ins)** | Named, versioned templates scored across dataset rows, live traces, prompts and simulations | [docs/evaluation](https://docs.futureagi.com/docs/evaluation) | "132 ready evaluators across quality, safety, RAG, format, and more" |
| 10 | **Built-in eval catalogue** | 7 RAG/retrieval, 16 safety/compliance, ~27 conversation/agent, ~30 output-quality/format, 8 similarity, 7 audio/voice, 7 image/document, plus statistical/NLP | [docs/evaluation/builtin](https://docs.futureagi.com/docs/evaluation/builtin) | "Context Adherence \| Measures how well responses stay within the provided context. \| output, context \| … \| LLM as Judge" |
| 11 | **76+ local heuristic metrics (SDK)** | Run <1 ms, no API key, no network — string/JSON/similarity/hallucination/RAG/agent/guardrail categories | [docs/sdk/evals/metrics](https://docs.futureagi.com/docs/sdk/evals/metrics) | "76+ local metrics, all run in under 1ms with no API key" |
| 12 | **LLM-as-judge (BYOK)** | Custom rubric in plain English, `{field}` substitution, auto-rubric generation, any LiteLLM model | [docs/sdk/evals/llm-judge](https://docs.futureagi.com/docs/sdk/evals/llm-judge) | "Define custom grading criteria and run them with any LLM: GPT-4o, Gemini, Claude, Ollama, or any LiteLLM-supported model." |
| 13 | **Code evals & composite evals** | User-supplied Python as an eval; composite evals combine metrics into one score | [docs/evaluation](https://docs.futureagi.com/docs/evaluation) (nav: Concepts) | "A **template** defines what to measure … a **config** points it at your data … a **run** executes … a **score** comes out" |
| 14 | **RAG evaluation** | Recall@K, Precision@K, NDCG@K, MRR, Hit Rate, Chunk Attribution, Chunk Utilization, Context Relevance, Groundedness | [docs/evaluation/builtin](https://docs.futureagi.com/docs/evaluation/builtin) | "Recall@K \| Evaluates recall at K for retrieval-based systems. \| output, context \| RAG & Retrieval \| Statistical Metric" |
| 15 | **Agentic / trajectory evaluation** | Trajectory Match, Step Count, Tool Call Accuracy, Task Completion, Conversation Coherence | [docs/evaluation/builtin](https://docs.futureagi.com/docs/evaluation/builtin) | "Trajectory Match \| … \| Conversation & agents" |
| 16 | **Online (live-trace) + offline + continuous evaluation** | Same eval attaches to datasets, prompt runs, simulations *and* live Observe traces; "inline, continuous + historical" modes | [docs/evaluation/builtin](https://docs.futureagi.com/docs/evaluation/builtin) · [pricing](https://futureagi.com/pricing) | "Built-in evals are pre-configured evaluation templates you can attach to dataset runs, prompt runs, simulations, and live traces in Observe." |
| 17 | **Ground truth & feedback** | Ground-truth labels, human feedback collection, threshold calibration | [docs/evaluation](https://docs.futureagi.com/docs/evaluation) (nav: Concepts → Ground truth / Feedback) | "[Ground truth]" / "[Feedback]" |
| 18 | **CI/CD eval gate** | SDK call from your own CI, version-tagged runs, side-by-side version comparison, PR comment | [docs/evaluation/guides/cicd](https://docs.futureagi.com/docs/evaluation/guides/cicd) | "Turn an eval threshold into a merge gate that blocks regressions" / "Future AGI never triggers or runs your pipeline." |
| 19 | **Eval usage & analytics** | Per-eval run volume, latency and outcome analytics in the playground | [docs/evaluation/guides/explore-playground/usage-analytics](https://docs.futureagi.com/docs/evaluation/guides/explore-playground/usage-analytics) | "[Usage & analytics]" |
| 20 | **Agent Compass (research line)** | Purpose-built framework for diagnosing agentic workflows in production | [futureagi.com/research](https://futureagi.com/research) | "AgentCompass: first evaluation framework purpose-built for monitoring and debugging agentic workflows in production, SOTA on the TRAIL benchmark." |

### 1.3 Safety, guardrails, red-teaming

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 21 | **Protect (inline guardrails)** | Named checks run on gateway traffic with 4 actions: block / warn / mask / log; pre / post / both stages | [docs/protect](https://docs.futureagi.com/docs/protect) | "It runs named guardrail checks, like PII detection or prompt injection, on each request and returns one of four actions: block, warn, mask, or log." |
| 22 | **10 first-party guardrail checks** | `pii-detector`, `injection-detector`, `secrets-detector`, `content-moderation`, `keyword-blocklist`, `topic-restriction`, `language-detection`, `system-prompt-protection`, `hallucination-detection`, `data-leakage-prevention` | [docs/protect/reference/guardrail-checks](https://docs.futureagi.com/docs/protect/reference/guardrail-checks) | "These ten checks run inside Protect without an external provider." |
| 23 | **18 provider-backed guardrail checks** | `futureagi-eval`, `llama-guard`, `azure-content-safety`, `presidio-pii`, `lakera-guard`, `bedrock-guardrails`, `hiddenlayer`, `aporia`, `pangea`, `dynamoai`, `enkrypt`, `ibm-ai-detector`, `grayswan`, `lasso`, `crowdstrike-aidr`, `zscaler`, `tool-permissions`, `mcp-security` | [docs/protect/reference/guardrail-checks](https://docs.futureagi.com/docs/protect/reference/guardrail-checks) | "Each of these 18 checks has a **Provider Settings** section in its Rules dialog" |
| 24 | **14 PII entity types** | SSN, CREDIT_CARD, EMAIL, PHONE, ADDRESS, NAME, DOB, PASSPORT, DRIVER_LICENSE, IP_ADDRESS, BANK_ACCOUNT, MEDICAL_RECORD, AWS_KEY, API_KEY | [docs/protect/reference/guardrail-checks](https://docs.futureagi.com/docs/protect/reference/guardrail-checks) | "A `pii-detector` check … can look for these 14 entity ids" |
| 25 | **Guardrail pipeline controls** | Parallel/Sequential mode, Fail-Open toggle, 5000 ms timeout, 0.0–1.0 confidence threshold | [docs/protect/reference/guardrail-checks](https://docs.futureagi.com/docs/protect/reference/guardrail-checks) | "Fail Open \| Toggle \| On \| What happens when a check doesn't return a verdict before Timeout runs out." |
| 26 | **ML guardrail models** | Protect Flash / Full built on Gemma 3n; Turing purpose-trained hallucination detectors as eval judges | [pricing](https://futureagi.com/pricing) · [futureagi.com/research](https://futureagi.com/research) | "ML Protect (Gemma 3n) - uses AI Credits per check" / "purpose-trained evaluation models that detect hallucinations with higher accuracy and lower cost than generic LLM judges" |
| 27 | **Guardrail monitoring & drift** | Block-rate analytics by rule/agent/endpoint, drift detection, deep trace integration per block event | [futureagi.com/platform/monitor/guardrails](https://futureagi.com/platform/monitor/guardrails) | "Detect when your guardrails become less effective." / "Every guardrail event links directly to the full agent trace." |
| 28 | **Red-teaming** | Adversarial dataset suites (prompt injections, jailbreaks, encoding tricks) + open-source library with "built-in guardrails, red teaming capabilities" | [futureagi.com/platform/agents/datasets](https://futureagi.com/platform/agents/datasets) · [futureagi.com/blogs/june-update-future-agi-2025](https://futureagi.com/blogs/june-update-future-agi-2025) | "**Red-team with adversarial suites** — Build datasets of prompt injections, jailbreaks, and encoding tricks. Test your guardrails before attackers find the gaps." |
| 29 | **Sub-100 ms inline enforcement** | Guardrails execute inside the gateway hop, no extra network call | [futureagi.com/platform/monitor/command-center](https://futureagi.com/platform/monitor/command-center) | "Our guardrails are built into the gateway itself … all executing inline at sub-100ms. No external API calls. No extra hop." |

### 1.4 Datasets, simulation, replay

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 30 | **Versioned datasets** | Unlimited, stored forever, 14 column types, 6 creation methods (manual/CSV/PDF/HuggingFace/SDK), pin experiments to a version | [futureagi.com/platform/agents/datasets](https://futureagi.com/platform/agents/datasets) · [pricing](https://futureagi.com/pricing) | "Unlimited datasets - stored forever, 14 column types … 6 creation methods (manual, CSV, PDF, HuggingFace, SDK)" |
| 31 | **Dynamic columns** | Columns that auto-compute: run code, call APIs, query vector DBs, classify, extract entities | [futureagi.com/platform/agents/datasets](https://futureagi.com/platform/agents/datasets) | "Add columns that compute themselves - execute code, call APIs, query vector databases, run classification, or extract entities." |
| 32 | **Synthetic data generation** | Realistic edge cases, adversarial inputs, multi-turn conversations anchored to an uploaded Knowledge Base | [docs/quickstart/generate-synthetic-data](https://docs.futureagi.com/docs/quickstart/generate-synthetic-data) · [futureagi.com/blog/synthetic-data-generator](https://futureagi.com/blog/synthetic-data-generator) | "lets you create realistic, structured datasets from scratch without collecting or exposing real user data" |
| 33 | **Simulation (persona + scenario)** | 28+ built-in personas + custom, 3 scenario types (script / dataset / graph), transcripts + session comparison | [docs/simulation](https://docs.futureagi.com/docs/simulation) · [pricing](https://futureagi.com/pricing) | "28+ pre-built personas + custom personas … 3 scenario types (script, dataset, graph)" |
| 34 | **Voice simulation** | LiveKit / VAPI / Retell / Pipecat voice agents, 60 free min/mo, then $0.08/min | [pricing](https://futureagi.com/pricing) | "Voice Simulation 60 min minutes" / "Voice 60+ $0.08/minute" |
| 35 | **Trace / conversation replay** | Replay a real production trace or voice call back into the next rehearsal as a test case | [docs/simulation/concepts/replay](https://docs.futureagi.com/docs/simulation/concepts/replay) | "a production trace you [replay] becomes a new test case" |
| 36 | **Annotations / human review** | 3→10→unlimited queues, 5 label types (numeric/text/categorical/star/thumbs), auto-assign, round-robin, load-balanced, inter-annotator agreement on Scale | [pricing](https://futureagi.com/pricing) | "5 label types - numeric, text, categorical, star, thumbs … Auto-assign, round-robin, load-balanced" |
| 37 | **Knowledge Base** | Document management + retrieval for RAG, 5 KBs on Boost / unlimited on Scale | [docs/knowledge-base](https://docs.futureagi.com/docs/knowledge-base) · [pricing](https://futureagi.com/pricing) | "5 knowledge bases" (Boost add-on) |

### 1.5 Prompt management, playground, optimization

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 38 | **Prompt management & versioning** | Named versioned prompt objects with labels, folders, draft→version lifecycle, A/B, deployment labels | [docs/prompt](https://docs.futureagi.com/docs/prompt) | "a named, versioned object on the platform rather than a string hardcoded in your application, so you can change it without shipping a deploy, compare versions side by side" |
| 39 | **Prompt playground** | BYOK, 100+ models, full parameter control, tool/function calling, format validation | [pricing](https://futureagi.com/pricing) | "Prompt playground - BYOK, $0 platform cost · 100+ models, full parameter control" |
| 40 | **Agent Playground (visual builder)** | Canvas graph of LLM-Prompt nodes + Agent nodes, versions, per-step output inspection | [docs/agent-playground](https://docs.futureagi.com/docs/agent-playground) | "assemble a multi-step agent from nodes on a canvas, run it, and read what each step produced" |
| 41 | **Model comparison** | Multi-model comparison in experiments; `analytics.model_comparison()` in the gateway SDK | [pricing](https://futureagi.com/pricing) · [docs/command-center/features/cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking) | "Unlimited experiments - multi-model comparison" / `client.analytics.model_comparison(models=[...])` |
| 42 | **Prompt optimization (6 algorithms)** | GEPA, PromptWizard, ProTeGi, Bayesian Search, Meta-Prompt, Random Search — evals act as the judge | [docs/optimization](https://docs.futureagi.com/docs/optimization) · [docs/optimization/reference/optimizers](https://docs.futureagi.com/docs/optimization/reference/optimizers) | "Any of six optimizer algorithms can run against the same prompt column and eval set." |

### 1.6 Gateway (Agent Command Center)

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 43 | **OpenAI-compatible gateway, 108 endpoints** | 20+ endpoint categories under `/v1/`, admin under `/-/` | [docs/command-center/api/endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) | "Agent Command Center exposes 108 endpoints across 20+ categories." |
| 44 | **100+ providers + native-format translation** | Anthropic / Gemini / Bedrock / Cohere auto-translated to OpenAI format | [docs/command-center](https://docs.futureagi.com/docs/command-center) | "Providers with different native APIs (Anthropic, Gemini, Bedrock, Cohere) are automatically translated to the standard OpenAI format" |
| 45 | **Native format passthrough** | `POST /v1/messages` (Anthropic), `/v1beta/models/{model}:generateContent` (Gemini), `:streamGenerateContent` | [docs/command-center/api/endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) | "Anthropic Messages API (native format)" / "Google GenAI generate content" |
| 46 | **Extended OpenAI surface** | embeddings, rerank, search, OCR, TTS/STT/translations, images, video jobs, files, vector stores, assistants/threads/messages/runs, responses, async, batch, scheduled completions, realtime WebSocket | [docs/command-center/api/endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) | "Full proxy for the OpenAI Assistants API. Create assistants, manage threads, send messages, and execute runs." |
| 47 | **Routing strategies** | round-robin, weighted, least-latency, cost-optimized, adaptive, fastest/race (pricing page claims **9** total) | [docs/command-center/features/routing](https://docs.futureagi.com/docs/command-center/features/routing) · [pricing](https://futureagi.com/pricing) | "9 load balancing strategies, auto-failover, retry" / table lists `round-robin`, `weighted`, `least-latency`, `cost-optimized`, `adaptive`, `fastest` |
| 48 | **Adaptive weights** | 100-request learning phase, 30 s recompute, 0.3 smoothing, 5 % floor, latency 0.5 / error-rate 0.4 signal weights | [docs/command-center/features/routing](https://docs.futureagi.com/docs/command-center/features/routing) | "Learning phase: For the first N requests (default: 100), uses round-robin to gather baseline latency and error data" |
| 49 | **Conditional routing DSL** | Priority-ordered rules on `model/user/stream/provider/session_id/request_id/metadata.<key>` with `$eq $ne $in $nin $regex $gt $lt $gte $lte $exists`, `$and/$or/$not` | [docs/command-center/features/routing](https://docs.futureagi.com/docs/command-center/features/routing) | "Rules are evaluated in priority order (lower number = higher priority). First match wins." |
| 50 | **Complexity-based routing** | 8 weighted signals (token_count, message_count, system_prompt_length, tool_count, multimodal, keyword_heuristics, structured_output, max_tokens) → simple/moderate/complex tiers; per-request override header | [docs/command-center/features/routing](https://docs.futureagi.com/docs/command-center/features/routing) | "Agent Command Center scores each request on 8 signals and maps it to a tier." |
| 51 | **Model fallbacks + provider lock** | Per-model fallback chains; `x-agentcc-provider-lock` sticky routing with allow/deny lists | [docs/command-center/features/routing](https://docs.futureagi.com/docs/command-center/features/routing) | "Force a request to a specific provider, bypassing the routing strategy." |
| 52 | **Failover / retry / circuit breaker / timeouts** | `failover_on` status list, exponential backoff (100 ms→10 s, ×2, 2 retries), 3-state breaker with % error threshold + half-open probes, request & provider timeouts | [docs/command-center/features/routing](https://docs.futureagi.com/docs/command-center/features/routing) | "Circuit breaking … Closed / Open / Half-Open" / "initial_backoff_ms 100 … backoff_multiplier 2" |
| 53 | **Exact + semantic response cache** | `strategy: exact \| semantic`, TTL, LRU `max_entries`, namespaces, force-refresh / no-store / TTL / namespace headers, 6 backends (memory, Redis, Qdrant, Pinecone, …) | [docs/command-center/features/caching](https://docs.futureagi.com/docs/command-center/features/caching) | "Cache LLM responses at the gateway with exact match and semantic caching." / "Streaming requests bypass cache entirely" |
| 54 | **Rate limiting, budgets, key credits** | global / per-org / per-key RPM+TPM, `X-Ratelimit-*` headers, 429 body, daily/weekly/monthly/total budgets with block/warn, managed-key USD credit balances + auto top-up endpoints | [docs/command-center/features/rate-limiting](https://docs.futureagi.com/docs/command-center/features/rate-limiting) | "Give each customer key a credit balance that auto-deducts per request." |
| 55 | **Cost & latency analytics** | Per-request `x-agentcc-cost`, `X-AgentCC-Cost: 0` on cache hit, `x-agentcc-metadata` attribution dimensions, dashboard breakdowns by model/provider/key/dimension, 5 alert types incl. `latency_spike` (P95) and `error_spike` | [docs/command-center/features/cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking) | "The cost appears in the `x-agentcc-cost` response header" / "`latency_spike` P95 latency exceeds configured threshold" |
| 56 | **Shadow experiments (traffic mirroring)** | `sample_rate` fraction of traffic mirrored to a shadow model in background; captures both responses, latencies, tokens, statuses; `/-/shadow/stats` | [docs/command-center/features/shadow-experiments](https://docs.futureagi.com/docs/command-center/features/shadow-experiments) | "Silently copy a percentage of production LLM requests to a second model without affecting the user-facing response." |
| 57 | **Virtual keys** | Per-key model allow-lists, providers list, `internal` vs `byok` key types, admin-token management | [docs/command-center/concepts/virtual-keys](https://docs.futureagi.com/docs/command-center/concepts/virtual-keys) · [self-hosted](https://docs.futureagi.com/docs/command-center/deployment/self-hosted) | "Only keys with `key_type: \"internal\"` can use the providers in `config.yaml`." |
| 58 | **MCP + A2A endpoints** | `POST /mcp` (tool aggregation), `GET /mcp` SSE, `/-/mcp/*` management, `/.well-known/agent.json`, `POST /a2a` | [docs/command-center/api/endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) | "Agent Command Center acts as an MCP server, aggregating tools from upstream MCP tool servers." |
| 59 | **Observability ops endpoints** | `/-/metrics` (Prometheus), `/healthz`, `/livez`, `/readyz`, `/-/config`, `/-/reload`, `/-/health/providers[/{org}]`, `/-/cluster/nodes` | [docs/command-center/api/endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) | "GET `/-/metrics` Prometheus metrics" |
| 60 | **Webhooks & custom properties** | Webhook delivery + user-defined metadata schema for cost-attribution dimensions | [docs/command-center/features/webhooks](https://docs.futureagi.com/docs/command-center/features/webhooks) · [custom-properties](https://docs.futureagi.com/docs/command-center/features/custom-properties) | "[Webhooks]" / "[Custom Properties] Define structured metadata schemas for cost attribution dimensions" |
| 61 | **Gateway OTLP export** | Gateway can emit its own traces via `otel.exporter: otlp` | [self-hosted](https://docs.futureagi.com/docs/command-center/deployment/self-hosted) | "With OTLP export (`otel.exporter: otlp`), add up to 15 seconds for the exporter's last flush." |

### 1.7 Platform, deployment, enterprise, pricing

| # | Capability | Description | Source | Proof quote |
|---|---|---|---|---|
| 62 | **Open source, Apache 2.0 core** | `future-agi/future-agi` (~2.1k★, 658 forks), 6 pillars, `docker compose up`, `./bin/install` | [github.com/future-agi/future-agi](https://github.com/future-agi/future-agi) | "Open-source, end-to-end platform for evaluating, observing, and improving LLM and AI agent applications. Tracing · Evals · Simulations · Datasets · Gateway · Guardrails. Self-hostable. Apache 2.0." |
| 63 | **Self-hosted gateway as Go binary + Docker** | Standalone Go 1.25+ binary or `futureagi/agentcc-gateway` image; `/healthz` + `/readyz`; Standalone / Distributed / Helm topologies | [self-hosted](https://docs.futureagi.com/docs/command-center/deployment/self-hosted) | "Agent Command Center is distributed as a Go binary and Docker image." |
| 64 | **SDK ecosystem** | `futureagi` (Py), `@future-agi/sdk` (TS), `ai-evaluation` (Py+TS), `traceAI` (+ per-framework packages, Maven Central), `@futureagi/agentcc` (Py+TS), `agent-command-center-sdk` | [github.com/future-agi/future-agi](https://github.com/future-agi/future-agi) · [docs/sdk/evals](https://docs.futureagi.com/docs/sdk/evals) | "each SDK is independently usable, independently packaged, Apache/MIT-licensed" |
| 65 | **Docs MCP server + in-app AI assistant** | Docs MCP server installable into Cursor/Claude Code/VS Code; Falcon AI answers across all docs | [docs](https://docs.futureagi.com/docs/falcon-ai/guides/use-the-mcp-server) · [futureagi.com/research](https://futureagi.com/research) | "Install Docs MCP server / Add to Cursor, Claude Code, VS Code, etc" / "Ask me anything about the FutureAGI platform — I can search across all docs instantly." |
| 66 | **Export & import integrations** | Export to Datadog, PostHog, Mixpanel, PagerDuty, S3/Azure Blob/GCS, SQS/Pub/Sub; import traces from Langfuse | [docs/integrations](https://docs.futureagi.com/docs/integrations) | "Already using another observability platform? Pull your existing traces into Future AGI without re-instrumenting your code." |
| 67 | **RBAC (4 roles × settings sections)** | Owner / Admin / Member / Viewer with a per-section access matrix | [docs/admin-settings](https://docs.futureagi.com/docs/admin-settings) · [docs/roles-and-permissions](https://docs.futureagi.com/docs/roles-and-permissions) | "Access to different settings depends on your role." / table: API Keys → Owner only; Usage Summary → all four |
| 68 | **SSO, audit, compliance** | OAuth SSO (Boost), SAML SSO + SCIM + HIPAA BAA (Scale), audit logs + ABAC + data masking + custom retention (Enterprise) | [pricing](https://futureagi.com/pricing) | "SOC 2 Type II · OAuth SSO · audit logs" (Boost) / "HIPAA BAA · SAML SSO + SCIM" (Scale) / "Custom retention · ABAC · data masking" (Enterprise) |
| 69 | **Org / user / workspace management** | Invitations, role assignment, workspace-level settings, org config API | [docs/admin-settings](https://docs.futureagi.com/docs/admin-settings) · [command-center/admin/organizations](https://docs.futureagi.com/docs/command-center/admin/organizations) | "Invite users, assign roles, and manage team members." |
| 70 | **Pricing: usage-based, 6 dimensions, no per-seat** | Storage ($2/GB), AI Credits ($10/1K), Gateway requests ($5/100K), Cache hits ($1/100K), Text sim ($2/1M tokens), Voice sim ($0.08/min). Add-ons Boost $250 / Scale $750 / Enterprise $2,000 | [pricing](https://futureagi.com/pricing) | "There are 6 billing dimensions: Storage, AI Credits, Gateway Requests, Cache Hits, Text Simulation, and Voice Simulation." / "No per-seat pricing - ever." |
| 71 | **Pricing: BYOK vs managed credits** | Heuristic evals always free; BYOK LLM-as-judge $0 platform cost; managed Turing judges billed per run in credits | [pricing](https://futureagi.com/pricing) | "BYOK (Bring Your Own Key) means you provide your own OpenAI/Anthropic/etc. API key for evaluations - $0 platform cost." |
| 72 | **Third-party launch record** | Product Hunt: 5 launches, 4.9/9 reviews, ~1.7k followers; #2 Product of the Day 2024-09-20 (Future AGI), #3 2026-04-01 (traceAI) | [producthunt.com/products/future-agi](https://www.producthunt.com/products/future-agi?launch=future-agi) · [producthunt.com/posts/traceai](https://www.producthunt.com/posts/traceai) | "Future AGI was ranked #2 of the day for September 20th, 2024 traceAI was ranked #3 of the day for April 1st, 2026" |

**Inventory size: 72 numbered capability rows (rows 1–72) grouped into 56 capability
groups**, organised as 7 areas: 8 observability/tracing, 12 evaluation, 9
guardrails/safety/red-teaming, 8 datasets/simulation/replay, 5 prompt/playground/optimization,
19 gateway, 7 platform/enterprise/pricing, plus a multi-tenant-adjacent billing group.

### 1.8 Explicitly **unverified** from official sources

- Exact **"9 load-balancing strategies"** list — pricing page says 9, routing page tables only 6.
  The other 3 names are **unverified**.
- The claim **"108 endpoints"** is stated on the docs page but the page body enumerates
  fewer; the exact count is **unverified** against the running gateway.
- **Apache 2.0 vs GPL v3** licensing split across repos: `future-agi/future-agi` and
  `futureagi-sdk` are Apache-2.0; `ai-evaluation` is reported GPL v3 in search snippets
  but I did not fetch its LICENSE page. **unverified**.
- **Voice-agent RL fine-tuning** (marketing page) vs the doc surface (only simulation) —
  no official doc page for RL fine-tuning was reachable. **unverified**.
- Guardrail **latency claims** ("sub-100ms", "<47ms p99", "~45ms") are marketing figures
  with no benchmark methodology published. Verified as *claims*, not as measurements.

---

## Part 2 — heterion-router capability inventory (code only)

Baseline `main` @ `51dfe6d`, working tree dirty (42 paths). Every row below is `[C]`.

### 2.1 Provider registry — `crates/heterion-router-providers/registry.json`

Counted with `python3` over the JSON array.

| Metric | Value |
|---|---|
| Providers | **278** (STATUS.md:2717 models in tree; `AGENTS.md` agrees: "Registry содержит 278 providers") |
| Distinct `auth_type` | **5** — `apikey` 236, `oauth` 21, `optional` 10, `none` 10, `cookie` 1 |
| Distinct `format` | **9** — `openai` 253, `claude` 10, `openai-responses` 5, `gemini` 3, `antigravity` 2, `cursor` 2, `clova` 1, `cursor`…, `custom` 1, `kiro` 1 |
| Distinct `executor` | **68** — `default` 203, plus 67 bespoke executors (`bedrock`, `vertex` ×2, `glm` ×3, `opencode` ×3, `antigravity` ×2, `moonshot` ×2, and 60 single-instance web/CLI executors: `grok-web`, `chatgpt-web-codex`, `perplexity-web`, `cursor`, `trae`, `zed-hosted`, …) |
| Entry shape | `id`, `alias`, `format`, `executor`, `auth_type`, `base_url`, `models[{id, target_format}]` |

**Interpretation:** breadth beats Future-AGI's 100+ on paper (278 vs 100+), but
`AGENTS.md` + `STATUS.md` are explicit that the registry is a *catalogue, not a
capability*: 259/278 pass the structural builder gate, 253 have HTTP(S) base URLs, and
**61 HTTP-capable providers / 959 models ignore their bespoke `executor`** and are served
by the generic path (`STATUS.md`: "Registry — каталог, не гарантия доступности").

### 2.2 Gateway HTTP surface — `crates/heterion-router-gateway/src/lib.rs:145-171`

**19 `.route()` calls total** (18 in `lib.rs`, 1 in `routing.rs:187` which is
`Router::route`, not an axum route).

Inference (4 handlers, `lib.rs`):

| Method | Path | Handler | Line |
|---|---|---|---|
| GET | `/healthz` | `healthz` (public — **no** auth extractor) | `lib.rs:147`, fn at `lib.rs:173` |
| GET | `/v1/models` | `list_models` | `lib.rs:162`, fn at `lib.rs:181` |
| POST | `/v1/chat/completions` | `chat_completions` | `lib.rs:163`, fn at `lib.rs:225` |
| POST | `/v1/responses` | `responses` | `lib.rs:164`, fn at `lib.rs:858` |
| GET/DELETE | `/v1/responses/{id}` | `get_response` / `delete_response` | `lib.rs:165-169`, fns at `lib.rs:809`, `lib.rs:821` |

Admin API (11 handlers, all behind `ManageAuthenticated`):

| Method | Path | Handler | Line |
|---|---|---|---|
| GET | `/api/overview` | `admin::overview` | `lib.rs:148`, fn `admin.rs:53` |
| GET | `/api/combos` | `admin::combos` | `lib.rs:149`, fn `admin.rs:92` |
| PATCH | `/api/combos/{id}` | `admin::update_combo` | `lib.rs:150`, fn `admin.rs:606` |
| GET | `/api/connections` | `admin::connections` | `lib.rs:151`, fn `admin.rs:140` |
| PATCH | `/api/connections/{id}` | `admin::update_connection` | `lib.rs:152`, fn `admin.rs:716` |
| GET | `/api/usage` | `admin::usage` | `lib.rs:153`, fn `admin.rs:406` |
| GET | `/api/logs` | `admin::logs` | `lib.rs:154`, fn `admin.rs:449` |
| GET | `/api/logs/app` | `admin::logs_app` (tails `application_log_path(data_dir)`) | `lib.rs:155`, fn `admin.rs:511` |
| GET/POST | `/api/keys` | `admin::keys` / `create_key` | `lib.rs:156`, fns `admin.rs:198`, `admin.rs:240` |
| POST | `/api/keys/{id}/revoke` | `admin::revoke_key` | `lib.rs:157`, fn `admin.rs:318` |
| POST | `/api/keys/{id}/restore` | `admin::restore_key` | `lib.rs:158`, fn `admin.rs:327` |
| POST | `/api/restart` | `admin::restart_service` (launchd-supervised only, 409 otherwise) | `lib.rs:159`, fn `admin.rs:379` |

UI serving (2): `/` and `/{*path}` → `ui::serve` (`lib.rs:160-161`).

**Absent from the route table:** `/v1/embeddings`, `/v1/completions`, `/v1/files`,
`/v1/assistants`, `/v1/threads`, `/v1/vector_stores`, `/v1/audio/*`, `/v1/images/*`,
`/v1/batches`, `/v1/rerank`, `/mcp`, `/-/metrics`, `/-/health/providers`, `/-/reload`.

**Middleware:** exactly one layer — `cors::cors` (`lib.rs:170`). No rate limiter, no
metrics exporter, no OTel layer.

### 2.3 Auth — `crates/heterion-router-gateway/src/auth.rs`

- Bearer extractor, not middleware, so each handler opts in and `/healthz` stays public
  by construction (`auth.rs:8-10`, `lib.rs:143-144`).
- `AuthKey { id, name, scopes, allowed_models, allowed_combos, no_log }` — `auth.rs:24-33`.
- `Authenticated::allows_model()` glob matcher over models **and** combos — `auth.rs:44-59`.
- `ManageAuthenticated` gates all `/api/*` mutations (`admin.rs:53` etc.).
- `ManageAuthenticated` is a **single flat scope**, not RBAC: no roles, no per-section matrix.
- Keys: SHA-256 hashed (`repos.rs:177 api_key_hash`), lifecycle columns
  `revoked_at / expires_at / last_used_at / is_active / is_banned / key_hash / scopes /
  allowed_combos` (`migrate.rs:63-85 API_KEYS_COMPAT_COLUMNS`).
- Field encryption at rest: `aes-gcm`, `STATIC_SALT = "omniroute-field-encryption-v1"`,
  prefix `enc:v1:` (`crypto/src/lib.rs:18`, `:23`, `encrypt` `:82`, `decrypt` `:107`).
- Non-loopback bind without a key refuses to start (`STATUS.md`, ADR-009).

### 2.4 Routing, combos, failover — `crates/heterion-router-routing/`, `gateway/src/routing.rs`, `gateway/src/combos.rs`

| Capability | Status | Evidence |
|---|---|---|
| Model-prefix routing | **present** — case-insensitive prefix, first match wins, else default chain | `routing/src/router.rs:39-47` |
| Weighted / least-latency / cost-optimized / adaptive / race strategies | **absent** — `Router` returns a *fixed ordered `Vec<String>`*; no strategy enum exists | `routing/src/router.rs:39-47` |
| Conditional routing on request attributes | **absent** — no field/op DSL, no metadata inspection | `routing/src/router.rs:39-47` |
| Circuit breaker | **present, in-memory only** — `BreakerState`, `CircuitBreaker{threshold, cooldown}`, `BreakerSet` keyed by backend | `routing/src/breaker.rs:13`, `:22`, `:32`, `:77`, `:100` |
| Breaker persistence | **absent** — `domain_circuit_breakers` table exists in schema but **zero Rust references** | `db/schema_base.sql:242`; grep for `domain_circuit_breakers` in `crates/**/*.rs` → 0 hits |
| Combo planning | **present** — ordered entries, alias resolution, `openai-compatible` fallback, missing-backend skip | `gateway/src/combos.rs:214-252 plan_steps` / `:234 dispatch_backend` |
| Transient retry + failover-before-retry | **present** — `failover_before_retry` honoured in both non-stream and stream paths | `gateway/src/routing.rs:236`, `:309` |
| `max_set_retries` / `set_retry_delay_ms` | **parsed but dead** — read at `combos.rs:173`, `:177`; **never read** in `routing.rs` | `STATUS.md` confirms: "парсятся, но не влияют на routing" |
| `weight`, `strategy: auto` | **parsed but dead** — order is honoured for both strategies | `combos.rs:59-60`, `STATUS.md` |
| Per-model fallback chains | **schema only** — `domain_fallback_chains` table, 0 Rust references | `db/schema_base.sql:196` |
| Model fallbacks | **present** via combos (ordered entries act as the chain) | `combos.rs:214-232` |
| Provider lock / sticky routing | **absent** | route table `lib.rs:145-171` |
| 4xx must not trip the breaker | **present, contract-locked** | `routing.rs:44 is_backend_outage`; test `routing.rs:428` |
| Upstream status parsing from client message | **present** | `routing.rs:58 upstream_status`; test `routing.rs:462` |

### 2.5 Usage & cost accounting — `heterion-router-usage/`, `heterion-router-db/`

**`usage_history`** (`db/schema_base.sql:97-122`) — 21 columns:
`provider, model, connection_id, account_key, account_label, account_label_priority,
api_key_id, api_key_name, tokens_input, tokens_output, tokens_cache_read,
tokens_cache_creation, tokens_reasoning, service_tier, status, success, latency_ms,
ttft_ms, error_code, timestamp`. 3 indexes (`:119-121`).

**`call_logs`** (`schema_base.sql:124-170`) — 38 columns including
`tokens_cache_read/creation/reasoning/compressed`, `cache_source`, `request_type`,
`source_format`, `target_format`, `combo_name`, `combo_step_id`, `combo_execution_key`,
`error_summary`, `detail_state`, `artifact_relpath`, `artifact_size_bytes`,
`artifact_sha256`, `has_request_body`, `has_response_body`, `has_pipeline_details`,
`request_summary`, `correlation_id`.

> **Critical finding:** `UsageRecord` (`usage/src/records.rs:9-25`, 15 fields) and
> `CallLogRecord` (`records.rs:27-42`, 17 fields) **do not populate** `has_request_body`,
> `has_response_body`, `artifact_relpath`, `artifact_sha256`, `artifact_size_bytes`,
> `correlation_id`, `source_format`, `target_format`, `detail_state`, `ttft_ms`.
> Verified by grep: `has_request_body|artifact_relpath|correlation_id|source_format|detail_state`
> → **0 hits** in `crates/**/*.rs`. The schema is ready; the writer is not.

**`proxy_logs`** (`schema_base.sql:172-194`) — 18 columns (proxy type/host/port, level,
public_ip, target_url, tls_fingerprint). **0 Rust references** → schema-only.

**Domain/cost tables — all schema-only, 0 Rust references:**
`domain_fallback_chains:196`, `domain_budgets:201` (daily/weekly/monthly USD limits +
`warning_threshold`), `domain_budget_reset_logs:215`, `domain_cost_history:227`,
`domain_lockout_state:236`, `domain_circuit_breakers:242`,
**`semantic_cache:250`** (signature, prompt_hash, response, tokens_saved, hit_count,
expires_at), `quota_snapshots:264` (provider quota windows, `remaining_percentage`).

**Pricing** (`usage/src/pricing.rs`): `ModelPrice{input, output, cached, reasoning,
cache_creation}` per 1M tokens (`:14-25`); `PriceTable` with built-in seed +
user overrides from the `pricing` `key_value` namespace (`:27-33`, `:97 set_override`);
`cost_usd()` (`:109`). Doc comment is explicit: *"unknown cost is reported as unknown,
never fabricated"* (`pricing.rs:5-6`).

**Aggregation** — `repos.rs:849 usage_summary()` returns requests / successes /
tokens_input / tokens_output / tokens_cache_read + `by_provider` + `by_model` buckets
(`repos.rs:876-877`). Surfaced by `admin.rs:406-447` (window clamped 1..90 days, top-N ≤ 50).
**No cost is exposed in the admin API** — only token counts. No latency percentile query.
No budget enforcement reads `domain_budgets`.

### 2.6 Translation pipeline — `heterion-router-translate/`

Hub-and-spoke with **OpenAI as the pivot** (`translate/src/lib.rs:1-2`).

| Direction | Function | File:line |
|---|---|---|
| openai → openai-responses | `openai_to_responses` | `requests.rs:218` |
| openai → claude | `openai_to_claude(model, body, stream)` | `claude_request.rs:107` |
| openai → gemini | `openai_to_gemini(body)` | `gemini_request.rs:14` |
| responses → chat (non-stream) | `response_to_completion` | `gateway/src/responses.rs:216` |
| responses SSE → chat SSE | `convert_event(ResponsesState, …)` + `ToolCallEntry` + `TranslatedUsage` + `Converted` | `responses.rs:104`, `:19`, `:81`, `:91` |
| claude SSE → openai SSE | `convert_event(ClaudeState, …)` + `ClaudeUsage` + `Converted` | `claude.rs:54`, `:17`, `:32`, `:42` |
| gemini chunk → openai SSE | `convert_event(GeminiState, chunk, now) -> Vec<Value>` | `gemini.rs:61`, `:18`, `:48` |

**Response state store & replay:** `persist_response(state, auth, request, response)`
(`gateway/src/lib.rs:606`); owner-scoped lookup; cross-model replay rejected with
"previous_response_id belongs to a different model" (`lib.rs:771`); the incoming
`previous_response_id` is removed before re-dispatch (`lib.rs:789`, `:874`, `:1076`);
restart-survival test `lib.rs:2168 previous_response_id_replays_stored_input_after_restart_state`.
**This is the one real "replay" capability heterion-router has** — but it replays *its own*
Responses conversation state, not arbitrary captured traces.

### 2.7 Dashboard / desktop UI — `desktop/ui/`

**7 views**, tab-driven (no router library): `App.svelte:3-9`, tab table `App.svelte:12-20`,
nav buttons `App.svelte:77`.

`Overview.svelte` · `Combos.svelte` · `Providers.svelte` · `Keys.svelte` ·
**`Usage.svelte`** (7-day window switcher, success-rate derived) ·
**`Logs.svelte`** · **`Playground.svelte`**.

`Playground.svelte` sends a **real** `POST /v1/chat/completions` through the gateway and
shows `model · finish_reason · wall-ms · total_tokens` (`Playground.svelte:36-46`). That is
a genuine model smoke-test — closer to Future-AGI's "prompt playground (BYOK)" than to a
provider-agnostic multi-model comparison surface. It is **single-turn, single-model, no
history, no side-by-side.**

Tauri v2 shell in `desktop/src-tauri` (excluded from the workspace — `AGENTS.md`).

### 2.8 What is definitively **not** in heterion-router

Grep-verified absent across `crates/**/*.rs` for
`opentelemetry|otlp|opentracing|eval|guardrail|redteam|red_team|semantic_cache|rate_limit|ratelimit`:

- No OpenTelemetry / OTLP client or exporter.
- No eval engine, no scorer, no metric, no judge — the string `eval` appears only in
  unrelated identifiers.
- No guardrail, PII, jailbreak, injection, or moderation code path.
- No prompt storage/versioning table, no prompt table at all.
- No dataset, annotation, simulation, replay-from-capture, or synthetic-data code.
- No rate limiter, no budget enforcement, no semantic cache reader/writer.
- No metrics endpoint, no health-per-provider endpoint, no webhooks.
- No MCP, no embeddings/files/assistants/vector-store routes.
- No OpenAPI/Swagger export, no Rust SDK crate for heterion-router itself.

> The one Prior-art intent, `heterion/HETERION_RESEARCH.md:5`
> ("integrate FutureAGI SDK only for sampled field traces"), was never carried into
> heterion-router: no OTel dependency exists in `Cargo.toml:32-68`
> (`axum, tokio, tokio-stream, serde, serde_json, rusqlite, thiserror, tempfile,
> tracing, tracing-appender, tracing-subscriber, async-trait, futures, aes-gcm, scrypt,
> sha2, hex, getrandom, insta, wiremock, reqwest`).

### 2.9 Test & CI coverage

| Gate | Evidence |
|---|---|
| Local single script | `scripts/ci/check.sh` — exec-bit check → `cargo fmt --all -- --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test --workspace` |
| CI (Rust) | `.github/workflows/ci.yml` → `scripts/ci/check.sh` on `ubuntu-latest`, `dtolnay/rust-toolchain@stable` + `Swatinem/rust-cache@v2` |
| CI (UI) | `.github/workflows/ci.yml` job `ui` → `npm ci` + `npm run build` in `desktop/ui` (Node 24) |
| Release | `.github/workflows/release.yml` — `workflow_dispatch` only; ubuntu-latest + macos-14 matrix; tar.gz + `.sha256` via `gh release` |
| Test count | `docs/MISSION-2026-10.md` §2.1 records **`cargo test --workspace` → 213 passed, 0 failed** (re-verified there, not inherited). My own `#[test]`/`#[tokio::test]` grep gives 195 across 10 crates (gateway 107, translate 23, db 17, providers 14, core 8, http 8, config 6, routing 6, usage 6, crypto 0) — the delta is test fns not matched by my pattern. Treat **213** as authoritative. |
| Known CI holes | `MISSION-2026-10.md`: CI uses `@stable`, **not** the pinned `rust-toolchain.toml` `1.98.0`; `desktop/src-tauri` is outside the workspace so its 2 fmt diffs escape the gate; `release.yml:24,44` uses `macos-14` + `sha256sum` (non-portable). |

### 2.10 Summary of what heterion-router is *better* at

- **Provider breadth**: 278 catalogue entries / 68 distinct executors vs Future-AGI's
  "100+ providers" (`registry.json`; `AGENTS.md`). Caveat: 61 providers/959 models fall
  back to the generic executor (`STATUS.md`).
- **Translation depth**: bidirectional OpenAI↔Claude↔Gemini↔Responses with 23 translate
  tests (`translate/`), plus bespoke Gemini thought-signature persistence and a
  Grok-CLI 401→form-refresh→retry backend — surfaces Future-AGI does not document.
- **Credential hygiene**: AES-GCM field encryption with a versioned `enc:v1:` prefix, a
  documented legacy contract, SHA-256 key hashing, and fail-closed non-loopback binding.
- **Deployment shape**: single Rust binary + Tauri shell serving its own dashboard —
  no Docker, no Postgres, no control plane, no org model.
- **Config portability**: SQLite schema inherited from the JS server verbatim
  (`migrate.rs:15`, "17 tables + indexes"), so a future cache/budget feature can be built
  on tables that already exist.

---

## Part 3 — Gap analysis

Severity scale: **P0** = breaks correctness or blocks a documented promise ·
**P1** = a core Future-AGI pillar is entirely missing · **P2** = meaningful but not
load-bearing · **P3** = nice-to-have or out-of-scope-by-design.

"In scope?" is judged against a **local-first, single-binary, no-tenant, single-user Rust
gateway** served from `~/.heterion-router`.

| Capability | Future-AGI (source URL) | heterion-router (file:line) | Gap | In scope? | Recommendation |
|---|---|---|---|---|---|
| Request logging w/ bodies, artifacts, correlation | [cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking), [endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) — full input/output logged | Schema ready `db/schema_base.sql:124-170` (`has_request_body`, `artifact_relpath`, `artifact_sha256`, `correlation_id`); **writer omits them** `usage/src/records.rs:27-42` | **P0** | yes | **Wire the existing columns.** The migration already landed; this is a field-mapping change in one struct + one INSERT. Cheapest P0 on this list. Add a `retention_days` sweep — MISSION-2026-10 §O-3 already demands honest attribution. |
| Semantic + exact response cache | [caching](https://docs.futureagi.com/docs/command-center/features/caching) — `exact`/`semantic`, TTL, LRU, namespaces, 6 backends | Table exists `db/schema_base.sql:250 semantic_cache`; **0 Rust references** (grep-verified) | **P0** | yes | **Implement against the existing table.** Exact mode = SHA-256 of (model, messages, params) → SQLite row; semantic mode = embedding + cosine over the same table (skip Qdrant/Redis/Pinecone — wrong shape for a laptop). Add `x-router-cache: hit\|miss` + `x-router-cost: 0` on hit, mirroring Future-AGI's `X-AgentCC-Cache` / `X-AgentCC-Cost: 0`. Streaming bypass on both read and write (documented Future-AGI behaviour worth copying). |
| Per-key spend budgets + rate limits | [rate-limiting](https://docs.futureagi.com/docs/command-center/features/rate-limiting) — RPM/TPM, daily/weekly/monthly budgets, block/warn, `X-Ratelimit-*` | Tables exist `db/schema_base.sql:201 domain_budgets`, `:227 domain_cost_history`; **0 Rust references**. No limiter middleware — only `cors` at `lib.rs:170` | **P0** | yes | **Implement daily/monthly USD budget with `warn_threshold` from the existing table**, plus a per-key RPM window. Skip per-org and managed-key credit balances (no tenants). A local user still wants "stop burning my OpenAI credits at 3am" — that is the actual value. |
| Inline guardrails (PII / secrets / injection) | [protect](https://docs.futureagi.com/docs/protect), [guardrail-checks](https://docs.futureagi.com/docs/protect/reference/guardrail-checks) — 10 first-party + 18 provider checks, block/warn/mask/log, pre/post/both | **Nothing.** 0 hits for `guardrail` across `crates/**/*.rs` | **P0** | yes | **Ship 4 rule-based checks first (S effort, no deps):** `secrets-detector` (regex + Shannon entropy for `sk-*`, `AKIA*`, JWT), `pii-detector` (email/phone/credit-card/AWS-key — Future-AGI's 14 entities are the spec), `keyword-blocklist`, `language-detection`. Actions `block \| warn \| log` + `x-router-guardrail-triggered: true` on warn/log, mirroring the documented status contract. Defer ML guardrails (`futureagi-eval`, Protect Gemma 3n) and all 18 vendor adapters — they need a network hop or a model the binary shouldn't carry. |
| Circuit breaker persistence / half-open probes | [routing](https://docs.futureagi.com/docs/command-center/features/routing) — `error_threshold_percent`, `min_requests`, `half_open_max_requests`, `open_duration_seconds` | In-memory only `routing/src/breaker.rs:22`, `:77`; held in a `Mutex` `gateway/src/routing.rs:119`; table `db/schema_base.sql:242 domain_circuit_breakers` unused | **P1** | yes | Persist state to the existing table so a gateway restart doesn't re-trip a known-dead provider. Add half-open probe support (current `BreakerState` has no probe allowance). |
| Offline evaluation harness | [evaluation](https://docs.futureagi.com/docs/evaluation) — 132 built-ins, custom code evals, composite evals, ground truth | **Nothing** | **P1** | yes | **Do not chase 132 metrics.** Build a *gateway-shaped* eval set instead: ~8 local scorers (`contains`, `regex`, `json_schema`, `levenshtein_similarity`, `is_refusal`, `tool_call_accuracy`, `json_valid`, `schema_compliance`) + one BYOK LLM-as-judge path that reuses the gateway's **own** combo routing. Fixture files in-repo, run offline with `cargo test`. Effort **M**. |
| CI/CD eval gate | [evaluation/guides/cicd](https://docs.futureagi.com/docs/evaluation/guides/cicd) — version-tagged runs, PR comment, version comparison | CI = fmt/clippy/test + UI build only: `.github/workflows/ci.yml`, `scripts/ci/check.sh` | **P1** | yes | Add a `harness/evals/` fixture suite to `scripts/ci/check.sh` as a 4th gate. Also fix the two holes MISSION-2026-10 already names: pin `rust-toolchain.toml` in CI (currently `@stable`), and add a separate fmt check for the excluded `desktop/src-tauri`. |
| OpenTelemetry / OTLP export | [tracing/concepts](https://docs.futureagi.com/docs/tracing/concepts) — "built on OpenTelemetry"; gateway `otel.exporter: otlp` | **No OTel dep** `Cargo.toml:32-68`; `tracing` + `tracing-subscriber` only | **P1** | **partial** | Two-tier: (a) **S effort, do now** — `/metrics` in Prometheus text format (counters from `usage_history`, breaker states, in-flight, TTFT histogram). Pure `prometheus`-style hand-rolled exposition, ~150 lines, no new deps. (b) OTLP export = **M**, only if a real external collector appears — otherwise it is speculative. This also retro-fits `heterion/HETERION_RESEARCH.md:5`. |
| Prompt management & versioning | [prompt](https://docs.futureagi.com/docs/prompt) — versioned objects, labels, folders, A/B, deployment labels | **No prompt table** in `db/schema_base.sql:1-264` | **P1** | **partial** | Adopt the *versioning primitive* only: a `prompt_templates(id, name, version, body, labels, active)` table + `/api/prompts` CRUD. Skip folders, deployment labels, and the full workbench. Rationale: a local user edits prompts in their editor, not in a dashboard — the win is rollback, not collaboration. |
| Routing strategies beyond ordered priority | [routing](https://docs.futureagi.com/docs/command-center/features/routing) — weighted, least-latency, cost-optimized, adaptive, race | Only prefix-first-match returning a fixed `Vec<String>`: `routing/src/router.rs:39-47`. `weight` and `strategy:auto` **parsed but dead**: `combos.rs:59-60` | **P1** | yes | **Highest leverage per line of code on this table.** Make `ComboConfig.weight` live (honour it inside `plan_steps` `combos.rs:214-232`) and add `least-latency` + `cost-optimized` on top of data you already collect (`latency_ms` `usage_history`, `PriceTable` `usage/pricing.rs:109`). Skip `race` (bills every loser — Future-AGI's own docs warn "You are billed by every provider that receives the request"). Adaptive weights = **L**, defer. |
| Model comparison / shadow traffic | [shadow-experiments](https://docs.futureagi.com/docs/command-center/features/shadow-experiments), [cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking) `model_comparison()` | **Nothing.** `Playground.svelte:36-46` is single-model, single-shot | **P1** | **partial** | **Do shadow mirroring, skip the platform.** A `mirror_to` + `sample_rate` on a combo, results into a `shadow_runs` table, plus an A/B panel in `Playground.svelte` that fires N models against one prompt. This is the single most useful thing for a local user migrating between providers — and it composes with the eval harness. Effort **M**. |
| Exact/semantic cache headers & cost-per-request | [cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking) — `x-agentcc-cost`, `x-agentcc-provider` | `PriceTable::cost_usd()` exists `usage/src/pricing.rs:109`, but `/api/usage` returns **tokens only** `admin.rs:437-446`; no response headers | **P1** | yes | Expose `cost_usd` in `admin::usage` and emit `x-router-cost` / `x-router-provider` / `x-router-model-used` on every proxied response. Small diff, high operator value, and it is a prerequisite for the budget feature to make sense. |
| Latency analytics (P50/P95) | [cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking) — `latency_spike` on P95 | `latency_ms`, `ttft_ms` columns exist `schema_base.sql:97-117`; aggregation is COUNT/SUM only `db/src/repos.rs:849-877` | **P2** | yes | Add percentile aggregation in `usage_summary`. Needed for both `least-latency` routing and any alerting. **S**. |
| Condition/conditional routing DSL | [routing](https://docs.futureagi.com/docs/command-center/features/routing) — 10 fields × 10 operators, `$and/$or/$not` | **Absent** | **P2** | **partial** | Adopt only: `metadata.<key>`, `stream`, `model` regex — enough for "route cheap models for simple traffic" and "send streaming to the fast provider". Full 10-operator DSL is a config language to maintain for one user. **S**. |
| Complexity-based routing | [routing](https://docs.futureagi.com/docs/command-center/features/routing) — 8 weighted signals → tiers | **Absent** | **P2** | **partial** | 3 of the 8 signals are already in the request body (`tools.len()`, `response_format`, `max_tokens`). `token_count` needs a tokenizer dep — use a rough `bytes/4`. Skip `multimodal` and `keyword_heuristics`. **S-M**. |
| Model fallback chains | [routing](https://docs.futureagi.com/docs/command-center/features/routing) — `model_fallbacks` map | Combos already *are* an ordered chain `combos.rs:214-232`; plus orphan table `schema_base.sql:196` | **P2** | yes | Effectively covered by combos — no new capability needed. Note the dead table in the audit trail rather than building a second mechanism. |
| Full OpenAI surface (embeddings, files, assistants, vector stores, batch, audio, images) | [endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) — 108 endpoints | **Only** `/v1/models`, `/v1/chat/completions`, `/v1/responses`, `/v1/responses/{id}`: `lib.rs:162-169` | **P2** | **partial** | **`/v1/embeddings` yes** (**S**) — most common missing route, trivially proxied, and it unlocks the semantic cache. **`/v1/completions` + `/v1/files` yes** (cheap). **Assistants / threads / vector stores / audio / images / video / batch — no**: each implies a stateful store and a lifecycle the single-binary design deliberately avoids. |
| Native format passthrough | [endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) — `/v1/messages`, `/v1beta/models/{m}:generateContent` | Internal-only translation `translate/src/lib.rs:8-13`; no inbound native routes | **P2** | **partial** | The translators already exist (`claude_request.rs:107`, `gemini_request.rs:14`). Exposing `/v1/messages` as an inbound route is **S** and makes Anthropic-SDK clients work unmodified. |
| Provider health endpoints | [endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) — `/-/health/providers` | Breaker state in-process only `routing/src/breaker.rs:100`; surfaced nowhere over HTTP | **P2** | yes | Expose breaker state per backend from a `/api/health/providers`. One read of the existing `BreakerSet`; also the natural home for the `/metrics` counters. **S**. |
| Config reload without restart | [endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) — `/-/reload` | `kv_set` `repos.rs:758` + live combo reload via `with_combo_source` `routing.rs:105`; restart only for the service `admin.rs:379` | **P3** | yes | Effectively covered by combo/connection PATCH endpoints. No action. |
| Alerts & webhooks | [cost-tracking](https://docs.futureagi.com/docs/command-center/features/cost-tracking) — 5 alert types, Slack + email, cooldown | **Absent** (no Slack, no email, no webhook table) | **P3** | **partial** | **Local log + dashboard badge only** (reuse `logs_app` `admin.rs:511` and `tracing`). A webhook is **S** if the operator wants ntfy/Slack; Slack-specific delivery is out of scope. |
| Annotations / human review queues | [pricing](https://futureagi.com/pricing) — queues, 5 label types, inter-annotator agreement | **Absent** | **P3** | **partial** | Reduce to the one useful primitive: a `ratings` table + a column in `Logs.svelte` to thumbs-up/down a `call_logs` row. Queues, assignment algorithms and agreement metrics are org machinery. **S**. |
| Synthetic data generation | [generate-synthetic-data](https://docs.futureagi.com/docs/quickstart/generate-synthetic-data) | **Absent** | **P3** | **no** | LLM-driven synthetic corpora need a corpus to anchor to; Future-AGI anchors to an uploaded Knowledge Base. Wrong shape for a gateway. Covered indirectly by the eval fixture set. |
| Simulation (personas, voice agents) | [simulation](https://docs.futureagi.com/docs/simulation) | **Absent** | **P3** | **no** | Voice (LiveKit/VAPI/Retell/Pipecat) is a different product category. Text simulation is subsumed by the eval fixture set + shadow A/B. |
| Prompt optimization (6 algorithms) | [optimization](https://docs.futureagi.com/docs/optimization) — GEPA, PromptWizard, ProTeGi, Bayesian, Meta-Prompt, Random | **Absent** | **P3** | **no** | 6 optimizer algorithms to port for one operator who can edit a prompt by hand is a bad trade. Requires a working eval harness first; revisit only *after* the P1 eval work lands. |
| Falcon AI (auto error analysis, clustering) | [pricing](https://futureagi.com/pricing) | **Absent** | **P3** | **no** | Depends on AI Credits / managed Turing models. The local analogue is `call_logs.error_summary` (`schema_base.sql:148`) already surfaced by `/api/logs` (`admin.rs:449`). Enough. |
| Knowledge Base (RAG doc mgmt + retrieval) | [knowledge-base](https://docs.futureagi.com/docs/knowledge-base) | **Absent** | **P3** | **no** | A gateway routes and accounts; it does not own documents or a retrieval index. Out of product scope. |
| MCP / A2A endpoints | [endpoints](https://docs.futureagi.com/docs/command-center/api/endpoints) — `/mcp`, `/a2a`, `/-/mcp/*` | **Absent** | **P3** | **no** | Adds an agent-tool-server runtime and a capability-card contract. Wrong boundary for a routing proxy. Revisit if Heterion becomes a tool host. |
| RBAC (4 roles × settings matrix) | [admin-settings](https://docs.futureagi.com/docs/admin-settings), [roles-and-permissions](https://docs.futureagi.com/docs/roles-and-permissions) | Single flat `ManageAuthenticated` scope `auth.rs` + `admin.rs:53` | **P3** | **no** | One operator, one machine. The existing `scopes` + `allowed_models` + `allowed_combos` fields (`auth.rs:26-31`) are already the right granularity for a laptop. |
| SSO (OAuth / SAML / SCIM), audit logs, HIPAA/SOC2 | [pricing](https://futureagi.com/pricing) — Boost/Scale/Enterprise tiers | Field encryption `crypto/src/lib.rs:18-23`; no SSO, no audit log | **P3** | **no** | Compliance surface is meaningless for a process that only listens on `127.0.0.1` by default and holds one operator's keys. |
| Multi-tenant orgs, workspaces, managed-key credit balances, usage-based billing | [rate-limiting](https://docs.futureagi.com/docs/command-center/features/rate-limiting), [admin-settings](https://docs.futureagi.com/docs/admin-settings) | `provider_connections."group"` column exists `schema_base.sql:36`; no org model, no balances, no invoices | **P3** | **no** | This is Future-AGI's revenue model. A single-binary local gateway has exactly one payer. |
| Trace replay from capture | [simulation/concepts/replay](https://docs.futureagi.com/docs/simulation/concepts/replay) | **Own** Responses replay only: `lib.rs:606 persist_response`, `:771` owner/model check, test `lib.rs:2168` | **P2** | **partial** | Once P0 body-capture lands, "re-run this captured `call_logs` row against combo X" is a thin CLI over existing rows. Do not build a replay *engine*; add a `harness replay <call_log_id>` script. **S**. |
| Streaming guardrail on token deltas | [ai-evaluation README](https://github.com/future-agi/ai-evaluation) — "Streaming Assessment — monitor token-by-token, early-stop on safety violations" | **Absent** (no guardrails at all) | **P3** | **partial** | Only meaningful once guardrails exist, and only for the `responses`/`claude` SSE translators. Defer behind the rule-based guardrail work. |

**Gap table: 42 capability rows**, of which 11 are P0/P1. Severity distribution:
**4× P0**, **7× P1**, **15× P2**, **16× P3**.

---

## Part 4 — Strategy

### 4.1 Worth adopting

Ordered by value-per-effort for a **local-first, no-tenant, single-user Rust gateway**.

| Rank | Capability | Effort | Why it fits |
|---|---|---|---|
| 1 | **Response cache (exact, then semantic)** | **S** | The `semantic_cache` table is *already in the shipped schema* (`schema_base.sql:250`) — zero migration risk, and it is the only feature on this list that pays for itself in the first hour of use. Future-AGI's own docs set the exact contract worth copying (`strategy`, `default_ttl`, `max_entries` LRU, namespaces, streaming bypass, `X-AgentCC-Cache` / `X-AgentCC-Cost: 0`). Reject their Redis/Qdrant/Pinecone backends: SQLite + FTS is the right store for a laptop. |
| 2 | **Per-key spend budgets + RPM limits** | **S** | `domain_budgets` (with `warning_threshold`, daily/weekly/monthly) and `domain_cost_history` are already in the schema (`schema_base.sql:201`, `:227`). Same argument as #1: build against inherited tables. Real value is "don't wake up to a surprise OpenAI bill" — that is a *single-user* problem, which is exactly this product. |
| 3 | **Cost + latency percentiles surfaced** | **S** | `cost_usd()` already exists (`usage/pricing.rs:109`) and `latency_ms`/`ttft_ms` are already persisted. `/api/usage` currently returns tokens only (`admin.rs:437-446`) — a one-function diff that also makes #2's numbers legible and `least-latency` routing (#5) possible. |
| 4 | **Rule-based guardrails: secrets, PII, keyword blocklist** | **S-M** | The highest-trust, lowest-dependency guardrail tier. `secrets-detector` (regex + entropy) and `pii-detector` are pure Rust, no network, no model — a genuinely sub-millisecond inline check that fits the single-binary design. Replicate Future-AGI's status contract (`block` → 403, `warn`/`log` → 200 + header) so client behaviour is familiar. |
| 5 | **Live `weight` + `least-latency` / `cost-optimized` routing** | **S-M** | `ComboConfig.weight` is parsed and then discarded (`combos.rs:24` vs `routing.rs`). Making a dead config field work is the cheapest routing win available; the signals for latency/cost strategies are already recorded. Also fixes two of MISSION-2026-10's honesty complaints about routing. |
| 6 | **`/metrics` (Prometheus text) + `/api/health/providers`** | **S** | Reads data already in memory/DB. Makes the gateway scriptable and lets the breaker state be observable instead of implicit. ~150 lines, no new dependencies. |
| 7 | **Body capture into the existing `call_logs` columns** | **M** | Highest *correctness* value (MISSION-2026-10 §O-3 is literally about attribution lying). Priced as M only because it forces a retention/redaction decision — write bodies to `artifact_relpath` with a configurable TTL and a size cap. |
| 8 | **Offline eval harness + CI gate** | **M** | The Future-AGI lesson to copy is the *loop* (eval as a merge gate), not the 132 metrics. ~8 deterministic scorers plus a BYOK judge that reuses this gateway's own combo routing. Fixtures live in-repo; runs offline; slots into `scripts/ci/check.sh` as a fourth gate. Also the prerequisite for any future prompt optimization. |
| 9 | **Shadow mirroring + A/B playground panel** | **M** | The single most valuable thing for a local user deciding whether to switch providers — and it is exactly what the existing `Playground.svelte` half-does today (`Playground.svelte:36-46`). Writes to a new `shadow_runs` table. |
| 10 | **`/v1/embeddings` + `/v1/messages` inbound routes** | **S** | Two small proxy routes that unlock real client compatibility — and `/v1/embeddings` is a dependency-free prerequisite for the semantic half of #1. |
| 11 | **Prompt template versioning (minimal)** | **M** | Adopt the rollback primitive, not the workbench. `prompt_templates` + `/api/prompts` CRUD. Deprioritise until the eval harness can score a prompt. |
| 12 | **Circuit-breaker persistence + half-open probes** | **S-M** | The unused `domain_circuit_breakers` table is free; surviving a restart without re-tripping known-dead providers is real operational value. |

### 4.2 Explicitly out of scope

| Capability | Reasoning |
|---|---|
| Multi-tenant orgs, workspaces, per-org config | The product identity is a local process bound to `127.0.0.1:20128` in a launchd job with a SQLite file under `~/.heterion-router`. An org model is a control-plane concept with no local analogue. |
| Usage-based billing / AI Credits / managed-key credit balances | Future-AGI's revenue model. There is exactly one payer, and it is the person holding the machine. `domain_cost_history` is enough to answer "what did I spend". |
| SSO (OAuth/SAML/SCIM), audit logs, HIPAA/SOC 2/ISO 27001, ABAC, data masking | Compliance surface is meaningless for a process that holds one operator's keys on one disk. Field encryption (`crypto/src/lib.rs:18-23`) plus loopback-by-default binding is the honest security story; don't imitate a badge. |
| Full RBAC matrix | `scopes` + `allowed_models` + `allowed_combos` (`auth.rs:26-31`) is the right granularity. A 4×8 role matrix serves an org chart. |
| ML guardrails (Protect Gemma 3n, Turing) and the 18 vendor adapters (Lakera, Presidio, Llama Guard, Bedrock, …) | Two failure modes, both disqualifying: the adapters add a network hop (Future-AGI's own selling point against them is "no extra hop"), and a bundled GGUF guardrail model breaks the "one binary, no runtime deps" contract. Rule-based checks cover the high-value cases (leaked secrets, PII) without either cost. |
| Voice simulation (LiveKit/VAPI/Retell/Pipecat) | Different product category. A gateway has no business hosting telephony. |
| Agent Playground (visual canvas graph builder) | UI for building agents. heterion-router routes *to* agents; it doesn't author them. |
| Knowledge Base / RAG document management | A gateway owns no documents and no vector index. `/v1/embeddings` (proxying) is the right amount of RAG surface. |
| Prompt optimization algorithms (GEPA, PromptWizard, ProTeGi, Bayesian, Meta-Prompt, Random) | Porting 6 search algorithms for one operator who can edit a prompt directly is a poor trade. Blocked behind a working eval harness anyway — revisit only after the P1 eval work, and then as *one* algorithm (Bayesian is the honest default), not six. |
| Falcon AI auto error-analysis / clustering | Depends on managed Turing models and AI Credits. The local analogue — `call_logs.error_summary` (`schema_base.sql:148`) via `/api/logs` (`admin.rs:449`) — already covers the operator's actual need. |
| Assistants / threads / runs / vector stores / batch / async / scheduled / realtime WS | Each adds a stateful object lifecycle and a store. That is the architecture heterion-router deliberately rejected (see the rationale in `docs/adr/ADR-001-shared-sqlite-boundary.md`). Proxy `/v1/embeddings` and `/v1/files`; do not become an agent host. |
| MCP / A2A endpoints | An agent-tool-server runtime plus a capability-card contract. Wrong boundary for a routing proxy. Revisit only if Heterion grows a tool host. |
| Synthetic data generation | Anchored to an uploaded corpus; a gateway has no corpus. Subsumed by in-repo eval fixtures. |
| Redis / Qdrant / Pinecone cache backends | Wrong deployment shape. Multi-instance horizontal scale is not a requirement of a laptop-resident binary. |
| 70+ integration instrumentation packages (traceAI per framework) | Heterion's own note (`heterion/HETERION_RESEARCH.md:5`) — sample traces via the SDK rather than instrument everything. That remains the right call. |

### 4.3 Where heterion-router is already ahead — do not regress

Three places where copying Future-AGI would be a **downgrade**:

1. **Provider catalogue depth**: 278 entries / 68 executors (`registry.json`) vs "100+".
   Even discounting the 61 providers that fall back to the generic executor
   (`STATUS.md`), this is a differentiator. But it needs an honesty fix — the registry
   should advertise *effective* coverage, not catalogue size.
2. **Credential + key hygiene**: AES-GCM `enc:v1:` field encryption with a documented
   legacy contract (`crypto/src/lib.rs:18-23`, `AGENTS.md`), SHA-256 key hashing,
   fail-closed non-loopback binding, breaker-immune 4xx (`routing.rs:44`). None of this is
   in Future-AGI's public docs.
3. **Translation surface**: bidirectional OpenAI↔Claude↔Gemini↔Responses
   (`translate/src/lib.rs:8-13`, 23 tests) plus Gemini thought-signature persistence and
   the Grok-CLI refresh backend. Future-AGI translates *inbound* provider formats but
   documents nothing at this depth.

### 4.4 One-line verdict

> **Future-AGI is a 6-pillar platform (simulate → evaluate → protect → monitor → optimize →
> gateway) sold as one subscription. heterion-router is pillar 6 (the gateway) done
> unusually well for a local binary.** The strategically correct move is *not* parity — it
> is to keep the gateway excellent and adopt the three gateway-adjacent layers that pay
> for themselves offline (**cache**, **budgets**, **rule-based guardrails**), plus one
> forward-compatible primitive that costs little now and unblocks everything later
> (**`/metrics` + body-capture into the columns that already exist**).

---

## Sources

All URLs accessed **2026-10-02**. Every row in Parts 1 and 3 links to one of these.

| # | URL | Used for |
|---|---|---|
| 1 | https://futureagi.com/ | Landing page; positioning; "six pillars"; FAQ; Apache-2.0 self-host claim |
| 2 | https://futureagi.com/pricing | **Pricing model** (6 usage dimensions, per-dimension rates, Boost $250 / Scale $750 / Enterprise $2000), feature matrix incl. "9 load balancing strategies", "11 span types, 70+ filters", annotation label types, compliance tiers, comparison table |
| 3 | https://futureagi.com/platform/monitor/command-center | Gateway + native guardrails, sub-100 ms claim, cost tracking / rate limits / caching claims |
| 4 | https://futureagi.com/platform/monitor/tracing | Trace search UI, filter facets, traceAI callout |
| 5 | https://futureagi.com/platform/monitor/guardrails | Guardrail monitoring, block-rate analytics, drift detection, deep trace integration |
| 6 | https://futureagi.com/platform/agents/datasets | Dataset versioning, dynamic columns, synthetic generation, **red-teaming use case** |
| 7 | https://futureagi.com/research | Publications (AgentCompass, Protect, eval framework, synthetic data); Falcon AI assistant |
| 8 | https://futureagi.com/blog/synthetic-data-generator | Synthetic generation anchored to Knowledge Base |
| 9 | https://futureagi.com/integrations/guardrails | Guardrails-AI integration, 70+ evaluators per span |
| 10 | https://futureagi.com/blogs/june-update-future-agi-2025 | "built-in guardrails, **red teaming capabilities**" (official blog) |
| 11 | https://docs.futureagi.com/docs/observe | Observe overview, traces = requests |
| 12 | https://docs.futureagi.com/docs/tracing/concepts | **OTel-native**; OTLP HTTP/gRPC; traces/spans/sessions model |
| 13 | https://docs.futureagi.com/docs/evaluation | Eval model (template/config/run/score); **132 built-ins**; quality loop; CI gate; RAG/agentic eval families |
| 14 | https://docs.futureagi.com/docs/evaluation/builtin | Built-in eval catalogue table (Context Adherence, Recall@K, PII, Toxicity, Trajectory Match, …) |
| 15 | https://docs.futureagi.com/docs/evaluation/guides/cicd | **CI/CD** eval gate, version tags, PR comment, cross-version comparison |
| 16 | https://docs.futureagi.com/docs/sdk/evals | Three eval engines (local <1 ms / Turing / LLM-as-judge), `evaluate()` routing |
| 17 | https://docs.futureagi.com/docs/sdk/evals/metrics | **76+ local metrics** by category |
| 18 | https://docs.futureagi.com/docs/sdk/evals/llm-judge | **LLM-as-judge** custom criteria, LiteLLM models, rubric generation |
| 19 | https://docs.futureagi.com/docs/sdk/evals/evaluate | `evaluate()` parameter reference, `EvalResult` shape |
| 20 | https://docs.futureagi.com/docs/protect | Protect overview; **block/warn/mask/log** actions; Cloud+Enterprise gating |
| 21 | https://docs.futureagi.com/docs/protect/reference/guardrail-checks | **10 first-party + 18 provider-backed checks**, 14 PII entities, 8 topic categories, pipeline settings, 403/200 status contract |
| 22 | https://docs.futureagi.com/docs/prompt | **Prompt management & versioning** |
| 23 | https://docs.futureagi.com/docs/optimization | **6 optimizer algorithms** |
| 24 | https://docs.futureagi.com/docs/optimization/reference/optimizers | Optimizer reference index (Random/Bayesian/ProTeGi/Meta-Prompt/PromptWizard/GEPA) |
| 25 | https://docs.futureagi.com/docs/agent-playground | Agent Playground (visual canvas) |
| 26 | https://docs.futureagi.com/docs/simulation | Simulation overview; persona/scenario/agent-definition triad; replay |
| 27 | https://docs.futureagi.com/docs/error-feed | **Error Feed** — auto-detect, grouping, severity/status, Linear, RCA |
| 28 | https://docs.futureagi.com/docs/admin-settings | **RBAC** access matrix; org/user/workspace management |
| 29 | https://docs.futureagi.com/docs/roles-and-permissions | Roles reference (linked from #28) |
| 30 | https://docs.futureagi.com/docs/quickstart/generate-synthetic-data | **Synthetic data generation** |
| 31 | https://docs.futureagi.com/docs/integrations | **50+ traceAI integrations**; Langfuse import; export targets (Datadog/PostHog/Mixpanel/PagerDuty/S3/SQS) |
| 32 | https://docs.futureagi.com/docs/command-center | Gateway overview; 100+ providers; native-format translation; `base_url` swap |
| 33 | https://docs.futureagi.com/docs/command-center/features/routing | **Routing strategies**, failover, retries, circuit breaker, timeouts, **conditional routing**, **complexity routing**, model fallbacks, provider lock, adaptive, race |
| 34 | https://docs.futureagi.com/docs/command-center/features/caching | **Exact + semantic cache**, TTL/LRU/namespaces/headers, 6 backends, streaming bypass |
| 35 | https://docs.futureagi.com/docs/command-center/features/rate-limiting | **Rate limits**, RPM/TPM, `X-Ratelimit-*`, 429 body, **budgets**, **managed key credits** |
| 36 | https://docs.futureagi.com/docs/command-center/features/cost-tracking | **Cost tracking**, `x-agentcc-cost`, metadata attribution, dashboard views, **5 alert types** incl. `latency_spike` |
| 37 | https://docs.futureagi.com/docs/command-center/features/shadow-experiments | **Shadow experiments**, `sample_rate`, collected fields, limitations |
| 38 | https://docs.futureagi.com/docs/command-center/api/endpoints | **108 endpoints / 20+ categories**, extended OpenAI surface, MCP, A2A, `/-/metrics`, native passthrough |
| 39 | https://docs.futureagi.com/docs/command-center/deployment/self-hosted | **Self-host** (Go binary + Docker), `config.yaml`, health checks, **OTLP export**, key types, graceful shutdown |
| 40 | https://docs.futureagi.com/docs/command-center/concepts/virtual-keys | Virtual keys (linked from nav; content not separately fetched) |
| 41 | https://docs.futureagi.com/docs/command-center/features/webhooks | Webhooks (linked from nav) |
| 42 | https://docs.futureagi.com/docs/command-center/features/custom-properties | Custom metadata properties (linked from nav) |
| 43 | https://docs.futureagi.com/docs/knowledge-base | Knowledge Base (linked from nav) |
| 44 | https://docs.futureagi.com/docs/falcon-ai/guides/use-the-mcp-server | **Docs MCP server** + in-app assistant |
| 45 | https://github.com/future-agi/future-agi | Apache-2.0 core repo; 6 pillars; SDK table; quickstart; nightly-release caveat |
| 46 | https://github.com/future-agi/futureagi-sdk | Platform SDK (datasets, prompt workbench, KB, analytics, simulate) |
| 47 | https://github.com/future-agi/ai-evaluation | 50+ local metrics list; **streaming assessment**; guardrail scanners <10 ms |
| 48 | https://github.com/future-agi/traceAI | traceAI OTel instrumentation library |
| 49 | https://github.com/orgs/future-agi/repositories | Org repo index |
| 50 | https://www.producthunt.com/products/future-agi?launch=future-agi | Launch record (2024-09-16, 4.9/9, awards) |
| 51 | https://www.producthunt.com/posts/traceai | Launch timeline: Future AGI #2 (2024-09-20), Agent Compass, Simulate, FMA, traceAI #3 (2026-04-01) |
| 52 | https://www.npmjs.com/package/@future-agi/sdk | TypeScript SDK distribution |

**Local sources (read-only, no URL):**
`heterion-router/AGENTS.md`, `STATUS.md`, `CHANGELOG.md`, `docs/MISSION-2026-10.md`,
`docs/adr/*`, `crates/*/src/*`, `crates/heterion-router-providers/registry.json`,
`crates/heterion-router-db/schema_base.sql`, `desktop/ui/src/*`,
`.github/workflows/{ci,release}.yml`, `scripts/ci/check.sh`,
and `heterion/HETERION_RESEARCH.md` (sibling project, Prior-art reference only).

**Web availability:** all Part 1 pages were reachable. No fallback to model-internal
knowledge was required for any *verified* row. Items that could not be confirmed from an
official source are listed in §1.8.

---

## Commands run

Local research commands (all read-only; the report file in
`docs/research/audit-2026-10/03-futureagi-parity.md` is the only artifact created):

```sh
# 1. Prior-art check across the workstation
rg -i "future-?agi" /Users/egorich/ai-workstation/Projects \
   "/Users/egorich/Documents/Obsidian Vault" 2>/dev/null | head -20

# 2. Registry census (provider / auth_type / format / executor counts)
python3 -c "
import json; from collections import Counter
d=json.load(open('crates/heterion-router-providers/registry.json'))
print('count:', len(d))
for k in ['auth_type','format','executor','base_url_scheme','api_style']:
    c=Counter(p.get(k) for p in d if isinstance(p,dict))
    if any(c): print(k, dict(c))
"
python3 -c "
import json; d=json.load(open('crates/heterion-router-providers/registry.json'))
print(json.dumps(d[0], indent=1))"

# 3. HTTP route enumeration (18 axum routes + 1 Router::route false positive)
rg -n "\.route\(" crates/heterion-router-gateway/src/
rg -c "\.route\(" crates/heterion-router-gateway/src/*.rs

# 4. Schema / table + column census
find . -name "schema_base.sql" -not -path "./target/*"
rg -n "CREATE TABLE" crates/heterion-router-db/schema_base.sql
sed -n '97,200p'   crates/heterion-router-db/schema_base.sql   # usage_history, call_logs, proxy_logs
sed -n '196,275p'  crates/heterion-router-db/schema_base.sql   # domain_*, semantic_cache, quota_snapshots
sed -n '1,64p'     crates/heterion-router-db/schema_base.sql   # provider_connections, provider_nodes
rg -n "CREATE TABLE|CREATE INDEX" crates/heterion-router-db/src/

# 5. Schema-vs-code drift check (the P0 finding)
rg -n "semantic_cache|domain_budgets|domain_cost_history|quota_snapshots|domain_lockout_state|domain_circuit_breakers|proxy_logs|domain_fallback_chains" crates/ --type rust   # -> 0 hits
rg -n "has_request_body|artifact_relpath|correlation_id|source_format|combo_name|ttft_ms|detail_state" crates/ --type rust
rg -n "max_set_retries|set_retry_delay_ms|failover_before_retry" crates/heterion-router-gateway/src/{routing,combos}.rs

# 6. Absent-capability sweep
rg -in "opentelemetry|otlp|opentracing|\btrace\b|eval|guardrail|redteam|red_team|semantic_cache|rate_limit|ratelimit" crates/ --type rust -l

# 7. Routing / breaker / combos / translate surface
rg -n "pub fn|pub struct|pub enum" crates/heterion-router-routing/src/breaker.rs
cat  crates/heterion-router-routing/src/router.rs
rg -n "fn |enum |struct " crates/heterion-router-gateway/src/routing.rs crates/heterion-router-gateway/src/combos.rs
sed -n '18,70p;214,256p' crates/heterion-router-gateway/src/combos.rs
rg -n "^pub fn|^pub struct|^pub enum" crates/heterion-router-translate/src/*.rs
sed -n '1,80p' crates/heterion-router-translate/src/lib.rs

# 8. Usage / pricing / admin surface
rg -n "^pub fn" crates/heterion-router-db/src/repos.rs
sed -n '406,460p;511,530p' crates/heterion-router-gateway/src/admin.rs
sed -n '1,75p'  crates/heterion-router-usage/src/records.rs
sed -n '1,60p'  crates/heterion-router-usage/src/pricing.rs
sed -n '849,880p' crates/heterion-router-db/src/repos.rs

# 9. Auth / crypto / config
sed -n '1,60p'  crates/heterion-router-gateway/src/auth.rs
sed -n '140,200p' crates/heterion-router-gateway/src/lib.rs
rg -n "pub fn|STATIC_SALT|enc:v1" crates/heterion-router-crypto/src/*.rs
rg -n "HETERION_ROUTER_" crates/heterion-router-config/src/config.rs

# 10. Responses store / replay
rg -n "response_store|persist_response|previous_response_id|correlation_id" crates/heterion-router-gateway/src/lib.rs

# 11. UI + CI
ls desktop/ui/src/views/; rg -n "view|tab|route|import" desktop/ui/src/App.svelte
cat desktop/ui/src/views/Playground.svelte; sed -n '1,40p' desktop/ui/src/views/Usage.svelte
ls .github/workflows/; cat .github/workflows/{ci,release}.yml; cat scripts/ci/check.sh

# 12. Test counts + dependency surface
for c in crates/*/; do n=$(rg -c "#\[test\]|#\[tokio::test\]" $c/src/*.rs 2>/dev/null | awk -F: '{s+=$2} END {print s+0}'); echo "$c $n"; done
sed -n '32,70p' Cargo.toml

# 13. Baseline
git rev-parse --short HEAD; git status --porcelain | wc -l
sed -n '1,70p' docs/MISSION-2026-10.md; sed -n '1,60p' STATUS.md; cat AGENTS.md
```

Web research tool calls (2026-10-02), all read-only:

- **websearch** (4, parallel): "Future-AGI AI observability platform features LLM tracing evaluation";
  "futureagi docs built-in evals metrics LLM-as-judge"; "FutureAGI github repository open source SDK";
  "Future-AGI red teaming guardrails datasets synthetic data generation"
- **websearch** (1): `"Future AGI" Product Hunt launch agent observability 2025`
- **webfetch** (18): `/`, `/pricing`, `/platform/monitor/command-center`, `/platform/monitor/tracing`,
  `/platform/monitor/guardrails`, `/platform/agents/datasets`, `/research`, `/docs/observe`,
  `/docs/tracing/concepts`, `/docs/evaluation`, `/docs/evaluation/builtin`,
  `/docs/evaluation/guides/cicd`, `/docs/evaluation/guides/explore-playground/usage-analytics`
  *(nav only)*, `/docs/sdk/evals/metrics`, `/docs/sdk/evals/llm-judge`, `/docs/protect`,
  `/docs/protect/reference/guardrail-checks`, `/docs/prompt`, `/docs/optimization`,
  `/docs/agent-playground`, `/docs/simulation`, `/docs/error-feed`, `/docs/admin-settings`,
  `/docs/integrations`, `/docs/quickstart/generate-synthetic-data`,
  `/docs/command-center`, `/docs/command-center/features/{routing,caching,rate-limiting,cost-tracking,shadow-experiments}`,
  `/docs/command-center/api/endpoints`, `/docs/command-center/deployment/self-hosted`

No `cargo` build, no `cargo test`, no `git commit`, no database access, and no writes
outside this report were performed.