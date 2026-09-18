# Changelog

## Unreleased

### Added
- Провайдер `heterion-local`: keyless OpenAI-совместимый бэкенд на Heterion-сервер
  (`http://127.0.0.1:8080/v1`, override `HETERION_LOCAL_BASE_URL`); model id — транзит.
- `HETERION_ROUTER_*` env; `HETERION_LOCAL_BASE_URL`; data dir `~/.heterion-router`;
  сервис `com.heterion.router`.

### Changed
- Проект переименован: OmniRoute-Rust → Heterion Router (ADR-008). Крейты
  `omniroute-*` → `heterion-router-*`, бинарь `heterion-router-gateway`,
  Tauri `com.heterion.router`, `Heterion Router` в UI.
- Новый репозиторий `Egorich-print/heterion-router` (история Rust-поверхности
  перенесена filter-repo; JS-дерево осталось в замороженном референсе).

### Compatibility (один цикл)
- `OMNIROUTE_*` env honoured как fallback; `~/.omniroute` резолвится как legacy
  data dir; порты `20128/20129` без изменений.

### Frozen
- `enc:v1:` + соль `omniroute-field-encryption-v1` — навсегда (живые БД).
