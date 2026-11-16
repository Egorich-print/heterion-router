# Vision relay — живой замер (2026-11-16)

Задача: роутер занимает глазами для модели, которая картинок не видит
(ADR-031). Проверка — на живом шлюзе (`127.0.0.1:20128`), картинка — PNG
64×64, сплошной красный (`/tmp/red64.png`).

## Что спрашивали

| # | запрос | цель | результат |
|---|---|---|---|
| 1 | `mistral/codestral-latest` + картинка | слепая модель | **обслужен**: ответ `red`, 47 prompt-токенов (описание), вместо ~1120 токенов картинки |
| 2 | `auto` + картинка | любая модель пула | обслужен напрямую vision-моделью, ответ `Red`, эстафета не включалась |
| 3 | `oneminai/gpt-4o-mini` + картинка | vision по capability-таблице | `400 invalid_request_error` на `messages.0.content` — площадка отвергает эту форму картинки |

## Как отработала эстафета (запрос №1)

```
WARN vision relay: the describer failed, trying the next one
     provider=oneminai model=gpt-4o-mini
     error=upstream status 400: Invalid input (param messages.0.content)
INFO vision relay: the picture was described by a borrowed model
     provider=kilocode model=openrouter/free target=codestral-latest
```

Первый кандидат-описатель отказал по форме запроса — эстафета взяла
следующего. Без перебора кандидатов запрос №1 упал бы.

## Что вскрылось по дороге

**Виртуальные алиасы попадали в роль описателя.** Research помечает
`auto`, `auto:free`, `kilo-auto/*` плейсхолдером «окно 1 токен»
(«context 1 is a placeholder» в `docs/research/ladder/C-openai-anthropic-xai.jsonl`),
но capability-таблица принимала это как настоящее окно. Итог: `bazaarlink/auto:free`
считался vision-моделью и уходил в описатели первым (класс `fallback` — самый
дешёвый), а площадка отвечала:

```
400 {"message":"auto:free does not support image/video input or output;
     please choose a vision/image-capable model.","code":"auto_free_modality_unsupported"}
```

Исправлено в двух местах:

1. `CapabilityStore::from_rows` — строка с окном ≤ 1 токена **не даёт
   ничего**: это роутер-алиас, а не модель (нет ни vision, ни tools, ни
   класса). Такие алиасы больше не лезут в голову лестницы.
2. `vision_describers` — описателем может быть только модель с окном от
   `MIN_MODEL_WINDOW` (4096), и пробуются до трёх кандидатов
   (`VISION_RELAY_DESCRIBERS`).

## Итог

- Слепая модель обслуживает запрос с картинкой: да, по описанию.
- Путь `auto` не изменён: картинку видит сама vision-модель.
- Эстафету выключают двумя способами: `with_vision_relay(false)` —
  целиком; `HETERION_ROUTER_VISION_SAVING=1` — включает эконом-вариант
  (цель, которая видит картинку сама, отдаёт её более дешёвому классу).
  По умолчанию эконом-вариант **выключен**: цель теряет саму картинку.
