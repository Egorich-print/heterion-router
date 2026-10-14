# ADR-009: Capability routing, credential rotation и security boundaries (2026-09-25)

- Статус: accepted
- Контекст: YOLO-аудит 2026-09-25 показал, что расширение registry без
  transport-aware dispatch отправляло Responses-модели в Chat Completions,
  refresh-token infrastructure теряла identity, а auth boundary зависела от
  наличия plaintext ключей и режима bind.

## Решение

1. **Transport выбирается по effective model capability.** Registry metadata
   (`target_format=openai-responses`) подключает Responses side-backend к
   любому совместимому provider, а не только к xAI. Это не меняет модель
   продукта: OpenAI-compatible gateway остаётся единой поверхностью.
2. **Grok refresh сохраняет credential identity.** Backend выбирает credential
   один раз, обрабатывает 401 именно у него, отправляет form-urlencoded
   request на xAI OAuth token endpoint, повторяет исходный request один раз и
   может сохранить rotation через callback. Endpoint/client id остаются
   runtime-конфигурацией, потому что private proxy не является публичным API.
3. **Security boundary fail closed.** Admin mutations требуют `manage`,
   model allow-list проверяется до dispatch, DB errors не превращаются в
   open proxy, non-loopback bind требует key, data-dir secrets получают
   private permissions.
4. **API keys hash-only для новых issuance.** SHA-256 hash используется для
   lookup; plaintext legacy rows остаются совместимыми до отдельной
   миграции, чтобы не сломать общий JS/Rust SQLite boundary. `no_log`
   отключает usage/call-log записи, а `last_used_at` обновляется при успешной
   аутентификации.
5. **Stream termination is explicit.** Disconnect отменяет upstream task;
   EOF без terminal event becomes an error/incomplete path, а не synthetic
   successful stop.
6. **Usage/call logs are additive.** Non-stream и terminal-stream accounting
   записывается в существующие `usage_history` и `call_logs`; provider/connection
   attribution и request-id/retry-after telemetry остаются отдельным follow-up.
7. **Stored Responses are owner-scoped.** Successful non-stream Responses
   objects and their normalized request envelope are stored in the existing
   SQLite key/value boundary and exposed through authenticated
   `GET/DELETE /v1/responses/{id}`. A stored `previous_response_id` replays the
   transcript locally; provider pinning and full opaque-reasoning semantics are
   deliberately left for the next milestone.

## Следствия

- Старые plaintext credentials и `OMNIROUTE_*` продолжают работать.
- Refresh без `GROK_OAUTH_CLIENT_ID` возвращает контролируемую ошибку;
  секреты не логируются.
- Для live xAI refresh нужна runtime-конфигурация OAuth client и действующие
  credentials; API key с нулевым балансом остаётся rejected upstream.
- Следующий milestone: provider-pinned opaque Responses continuation, полный
  stream usage/attribution и TLS termination.

## Audit addendum 2026-09-29

Проверка исходного текста показала, что несколько границ были сформулированы
шире фактической реализации:

- `0700/0600` applies to the data directory, DB and `.env` at the permission
  pass; log/signature files created afterwards can be `0644` until next boot.
- Non-stream upstream errors are sanitized, but SSE error frames still carry
  raw upstream text.
- Responses state is owner-scoped and test-covered, but default-on storage is
  unbounded and the storage id is upstream-controlled.
- Missing `STORAGE_ENCRYPTION_KEY` makes the Grok refresh write-back plaintext.
- NULL legacy policy columns intentionally preserve JS compatibility but are
  fail-open (`manage`/unrestricted models) until migrated.

The authoritative current matrix is
`docs/research/PROJECT-AUDIT-2026-09.md`; this addendum narrows the ADR's
claims without rewriting the original decision. The original «stateful
Responses storage/continuation» follow-up is now implemented for the
non-stream owner-scoped/replay path; provider pinning, retention and full
streaming semantics remain open.
