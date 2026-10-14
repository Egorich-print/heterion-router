# ADR-012 — Rust 1.99 / edition 2024 и hardening по результатам аудита 2026-10

Дата: 2026-10-02. Статус: принято (реализовано в рабочем дереве поверх `51dfe6d`).

## Контекст

Два независимых запроса к проекту:

1. **Обновление на Rust 1.99 / edition 2024.** Workspace уже был на `edition =
   "2024"` и `resolver = "3"`, но `rust-toolchain.toml` pinning'ил `1.98.0`, а
   `rust-version` в обоих манифестах стоял `1.98`. Edition-2024-специфичных
   нарушений аудит не нашёл (ни `static mut`, ни `gen`-идентификатора, ни
   `unsafe extern`, ни небезопасных атрибутов; `unsafe` встречается только в
   edition-2024-обязательном `unsafe { std::env::set_var(..) }` под `#[cfg(test)]`
   и в одном `libc::kill`). То есть реальная работа — это MSRV, пин и CI.
2. **Полный аудит проекта** со сравнением с OmniRoute и Future-AGI
   (`docs/research/audit-2026-10/`). Аудит дал 22 слабых места, матрицу паритета
   и ~11 high-confidence дубликатов/мёртвого кода.

Отдельно вскрылось, что часть «фич» безопасности существует только на бумаге:
аудит на live-БД показал, что **332 из 332** строк usage, записанных портом,
имели `provider = "routing"` и `connection_id = NULL`, а колонка `api_keys.key`
хранила SHA-256 дайджест, который сам по себе принимался как bearer-токен.

## Решение

1. **MSRV = 1.99.** `rust-toolchain.toml` → `channel = "1.99.0"`,
   `rust-version = "1.99"` в `Cargo.toml` и `desktop/src-tauri/Cargo.toml`.
   `1.99.0` установлен и проверен: workspace и исключённый крейт собираются,
   проходят clippy `-D warnings` и тесты без новых литов.
2. **CI проверяет пин, а не `@stable`.** `dtolnay/rust-toolchain@1.99.0` в
   `ci.yml` и `release.yml`; в `paths:` добавлены `desktop/src-tauri/**` и
   `docs/**`; появился отдельный job `tauri-fmt` (`cargo fmt` не компилирует,
   поэтому не нужны webkit2gtk/GTK). `release.yml` больше не зовёт `sha256sum`
   на macOS-раннере, где его нет (там `shasum -a 256`).
3. **Атрибуцияusage — реальная.** Бэкенд сообщает провайдера/connection id через
   `backend::Attribution` с per-request токеном, а не константной строкой;
   `requested_model` — модель клиента, `combo_name` заполняется. Прерванный
   клиентом поток тоже учитывается (`StreamAccounting` в `Drop`, call-log 499).
4. **Fail-closed идёт дальше.** Статус, доставленный внутри 200-тела,
   восстанавливается из payload (`upstream_status_or_recovered`), поэтому 429
   больше не превращается в 502 и не роняет breaker; SSE-декодер ограничен
   (`MAX_SSE_EVENT_BYTES`) и не растёт квадратично; `/api/logs/app` читает хвост
   файла, а не весь лог.
5. **Ничего в БД не является credential.** `api_keys.key` для ключей,
   выпускаемых портом, хранит маркер `hash-only:<id>`, а не дайджест; предикат
   аутентификации отвергает и маркер, и `enc:v1:`, и строки, где `key` совпал с
   `key_hash` (так выглядят строки, записанные сборкой до этого фикса).
   Наследие JS-приложения — plaintext в `key` — продолжает работать.
6. **Атрибуция не врёт и там, где провайдера не было.**
   `Attribution::provider` стал `Option<String>`: запрос, который не дошёл до
   провайдера (неизвестная модель, все шаги комбо упали), пишет `NULL`, а не
   имя роутера — иначе `WHERE provider = ?` никогда его не отфильтрует.
7. **Дубликаты удалены, а не задокументированы.** Канонизированы:
   `content_text`/`content_text_with` (core), `translate::chunk::*`,
   `pump_sse_stream` (responses), `drive_openai_sse`, `try_each_credential`,
   `backend::next_token`, `upstream_status_or_recovered` (дубликат
   префикс-парсера в `routing.rs` удалён). Удалены мёртвые `config::settings` и
   `credentials::active_providers`. Каждая склейка снабжена тестами-пинами, доказывающими
   неизменность байтов на проводе.

## Следствия

- **Воспроизводимость.** Пин в CI и в `rust-toolchain.toml` теперь один и тот
  же; MSRV можно проверить локально. Развилка со `@stable` закрыта.
- **Обратная совместимость.** Ни одна миграция БД не добавлена, формат
  `enc:v1:` и `STATIC_SALT` не тронуты, старые plaintext-строки
  `api_keys.key` аутентифицируются как раньше.
- **Латентная ошибка закрыта, а не задокументирована.** Прежний
  `% self.tokens.len()` в gemini-пуле паник бы на пустом пуле; канонический
  `next_token` возвращает `None` и не двигает курсор.
- **Открытые остатки** (осознанно вне этого ADR, отражены в `STATUS.md`):
  breaker/probe поколения для гонки «провал до поездки vs живой probe»,
  `/v1/messages` и `/v1/embeddings`, индекс `idx_ak_key_hash` (требует
  `migrate.rs`), offline eval-харнесс, квоты/бюджеты, persistency breaker.

## Проверка

`scripts/ci/check.sh` → `check.sh: all green` на 1.99.0; `cargo fmt --all
-- --check` и `cargo fmt --check` в `desktop/src-tauri` — чисто;
`cargo test --workspace` — 371 passed / 0 failed (215 в дереве до аудита;
коммит `51dfe6d` даёт 186).
