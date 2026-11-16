# ADR-031 — Vision relay: the router borrows eyes

- **Status:** accepted
- **Date:** 2026-11-16
- **Relates to:** ADR-027 (unified Switchyard pool), ADR-029 (capability ladder
  and soft rotation), ADR-030 (trust and failure demotion)

## Context

The owner asked for a ladder that takes a model's *abilities* into account —
tool calling, vision — and gave the example: a model with vision can be called
and passed to another model, if that saves tokens.

Two facts made this more than a nice-to-have:

1. The router's plan for a request with images only contains models that
   accept images (`auto_plan_at` drops the blind ones). A client that asks a
   specific blind model for a picture gets the upstream's `400` — the picture
   never reaches a model that could look at it.
2. An image costs the reader about **1120 prompt tokens** against roughly 28
   for the same request in text (measured on Gemini, `docs/research/GEMINI-VISION-FIX-REPORT-2026-10-26.md`).
   On a frontier reasoning model that is the expensive part of the prompt, and
   a description of the same picture costs a few hundred tokens.

## Decision

The router borrows eyes. Before a request with images is dispatched, the
router may ask **one** vision-capable model to describe the picture and hands
the *description* to the model that would have answered.

The relay lives in `crates/heterion-router-gateway/src/vision_relay.rs` and is
wired into `RoutingBackend::complete`:

- **Mandatory relay** (on by default): the target cannot see images. The
  request is otherwise dead — the upstream refuses the picture. The relay
  turns a guaranteed failure into a working request.
- **Saving variant** (off by default, `HETERION_ROUTER_VISION_SAVING=1`): the
  target *can* see the picture, but a strictly cheaper class can. The picture
  is described there and the target receives the description. Cheaper in image
  tokens, but the target no longer sees the picture itself — a trade the
  operator makes, not the router.

### Rules

- **The description travels as text**, behind the marker
  `[Image description written by provider/model]`. The target must never
  believe it saw the picture itself.
- **A failed relay changes nothing.** The original request goes out unchanged,
  so the worst case is the upstream refusing the image exactly as before.
- **The describer is chosen by the ladder's own currency**: the lowest class
  first, the lowest priority inside the class. A model the router has never
  talked to is only used when no proven one is available — a description from a
  model that answers `404` is no description at all.
- **Up to three describers are tried** (`VISION_RELAY_DESCRIBERS`). A platform
  that refuses the picture shape (measured: `oneminai/gpt-4o-mini` answers
  `400 invalid_request_error` on `messages.0.content`) must not cost the
  relay: the next candidate gets the picture.
- **A describer must look like a model**: a window of at least
  `MIN_MODEL_WINDOW` (4096). The virtual aliases (`auto`, `auto:free`,
  `kilo-auto/*`) are routers that pick a model per request — the research
  marks them with a placeholder window of **one token**, and the platform
  refuses an image for some of them (`bazaarlink/auto:free`: `400
  auto_free_modality_unsupported`). Such a row claims nothing at all: no
  vision, no tools, no class.
- **The relay borrows a backend directly**, not the router: it cannot recurse
  into the ladder, and it costs one extra upstream call.
- **A successful description is evidence**: the describer is marked trusted
  (it really serves that model) and served.

### What the relay does not do

- It does not change the plan. `auto_plan_at` still drops blind models for a
  request with images — the contract "a vision request only reaches vision
  models" (`tests/auto_routing.rs::test_multimodal_vision_routing`) stands.
- It does not describe images for a model that can see them, unless the
  operator turns the saving variant on.
- It does not touch tool calls: the describer gets no tools, and the target
  keeps the client's tools.

## Consequences

- A client can ask a blind model about a picture and get an answer.
- The saving variant is off by default: an agent that must read a screenshot
  exactly keeps its own eyes, and the operator opts into the trade explicitly.
- One extra upstream call per relayed request; the borrowed call is not
  attributed to the request, so `call_logs` show the target only. The trust
  store, however, learns the describer.

## Measured (2026-11-16, live)

| what | result |
|---|---|
| `mistral/codestral-latest` + picture (blind model) | **served** — `red`, 47 prompt tokens (the description), the picture itself would have been ~1120 |
| `auto` + picture | served directly by a vision model, no relay (`red`) |
| first describer `oneminai/gpt-4o-mini` | `400 invalid_request_error` on `messages.0.content` — the platform refuses this picture shape |
| second describer `kilocode/openrouter/free` | described the picture, the blind target answered from it |
| virtual alias `bazaarlink/auto:free` as describer | `400 auto_free_modality_unsupported` — why aliases are excluded |

## Contracts

`crates/heterion-router-gateway/tests/vision_relay.rs`:

1. `a_blind_target_is_served_through_a_borrowed_description`
2. `without_the_relay_the_picture_stays_in_the_request`
3. `a_vision_target_looks_at_the_picture_itself`
4. `the_saving_variant_lends_the_picture_to_a_cheaper_model`
5. `a_failed_relay_leaves_the_request_unchanged`
