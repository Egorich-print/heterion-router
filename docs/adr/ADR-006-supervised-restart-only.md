# ADR-006. Рестарт — только под супервизором

> Дата: 2026-09-18 · Статус: accepted

## Контекст

Часть изменений (активность подключений) применяется лишь при старте процесса,
поэтому дашборду нужна кнопка перезапуска. Наивный `POST /api/restart`,
убивающий процесс, опасен: под dev-запуском это убьёт чужую сессию, а
самозавершение без супервизора оставит шлюз мёртвым.

## Решение

Рестарт возможен только когда процесс **под launchd**. Эндпоинт проверяет
наличие job'а (`launchctl print gui/<uid>/<label>`), иначе отвечает `409` с
объяснением. При успехе запускается detached-скрипт с задержкой 2 с — этого
хватает, чтобы отдать `202` — после чего `launchctl kickstart -k` перезапускает
**тот же** бинарник. Новый код рестарт не подкладывает.

Чтобы UI отличал новый инстанс от старого, `/healthz` отдаёт `started_unix`.

## Следствия

- Кнопка «Restart now» безопасна: без супервизора она честно откажет.
- Во время перезапуска in-flight запросы рвутся — момент выбирает оператор.
- Лейбл настраивается `OMNIROUTE_SERVICE_LABEL`, порт — `OMNIROUTE_RUST_PORT`.

## Фактические имена и границы 2026-09-29

- Основное имя label — `HETERION_ROUTER_SERVICE_LABEL`, fallback —
  `OMNIROUTE_SERVICE_LABEL`; default label — `com.heterion.router`
  (`crates/heterion-router-gateway/src/restart.rs`).
- Основное имя порта — `HETERION_ROUTER_PORT`, legacy fallback —
  `OMNIROUTE_RUST_PORT`; dev/sidecar default — `20129`, service — `20128`.
- Tauri sidecar не является launchd job, поэтому его restart request честно
  получает `409`. Desktop UI сейчас всё ещё показывает кнопку restart, но
  при установленном launchd job она может перезапустить service на `20128`,
  а не sidecar на `20129`; это открытый operational gap, а не свойство ADR.
