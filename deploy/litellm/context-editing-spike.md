# Spike: does LiteLLM forward Anthropic context editing?

Status: unverified. Context editing stays off until this spike passes against a local proxy.

## Question

Claude Code reaches the gateway through the Anthropic Messages API (`POST /v1/messages`). Anthropic context editing needs two things to reach the native `anthropic/` deployment unchanged:

- the `anthropic-beta` request header, with the context-management beta value;
- the `context_management` field in the request body.

## What the source shows (BerriAI/litellm main @ `60fc121`, read on 2026-11-01)

- `litellm/llms/anthropic/chat/transformation.py` (the **chat completions** path, OpenAI format in, Anthropic out) knows `context_management`: it maps OpenAI-style entries to Anthropic's `{"edits": [...]}` (`map_openai_context_management_to_anthropic`) and adds the beta header itself (`_ensure_context_management_beta_header`: `context-management-2025-06-27`, or the compaction beta for `compact_20260112` edits). It merges existing `anthropic-beta` values instead of replacing them.
- Not checked: the `/v1/messages` pass-through path (`litellm/llms/anthropic/experimental_pass_through/`), which is the one Claude Code uses. The file fetched at the expected path was empty, so nothing is claimed about it.
- Not checked: whether the proxy forwards a client `anthropic-beta` header on `/v1/messages`, or filters unknown betas.

## Observed from the client side (2026-11-01, no provider)

Running the installed `claude` 2.1.316 with the `claude-code` adapter's argv against a local fake Messages server showed that **Claude Code already sends context management on its own**: the request carried a `context_management` body field and `anthropic-beta: ...,context-management-2025-06-27,...`. So "context editing stays off" is not true for Claude Code by default. What the gateway does with these is the open question: if LiteLLM forwards them on `/v1/messages`, Claude Code's own context editing is active; if it strips them, it is off. The spike must settle which, and the contract (tokens.md) must say so.

## How to verify (no provider needed)

1. Run the proxy with this `config.yaml`, but point one `anthropic/` deployment's `api_base` at a local HTTP echo server that records requests.
2. Send `POST /v1/messages` with `anthropic-beta: context-management-2025-06-27` and a body carrying `context_management: {"edits": [{"type": "clear_tool_uses_20250919"}]}`, using a virtual key.
3. Pass if the echo server receives both the header value and the body field unchanged. Repeat with the chat completions path to confirm the mapping above.

Record the LiteLLM version tested here and update `docs/contracts/tokens.md` (the **spike** item) in the same change.
