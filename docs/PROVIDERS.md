# Провайдеры Heterion Router

> Документ для пользователя: какие провайдеры поддерживаются, как добавить ключ
> провайдера и что делать, если запрос не проходит.
>
> Интерфейс приложения — английский, поэтому подписи кнопок приведены как в
> реальном приложении (**Add provider**, **Save**, **Cancel**, **Issue key**,
> **Create first key**, **Restart now**). Текст инструкции — по-русски.
>
> Все числа в разделе 2 посчитаны по
> `crates/heterion-router-providers/registry.json` (снимок реестра на дату
> документа: **280 записей провайдеров, 2741 запись моделей**) и по коду
> построения бэкендов в `crates/heterion-router-gateway/src/main.rs`.

---

## 1. Как добавить ключ провайдера

### 1.1. В приложении

1. Откройте раздел **Providers**.
2. Нажмите **Add provider** — откроется форма добавления подключения.
3. В поле **Search providers** найдите провайдера по названию или id — в
   каталоге 221 запись, и без поиска нужную строку не найти. Поиск идёт по
   имени и по id, регистр не важен.
4. В списке каталога выберите провайдера. Каталог — это `GET /api/providers`,
   то есть записи реестра, прошедшие gate (§2.2): у них есть HTTP(S)
   `base_url`, есть модели, и транспорт шлюза их обслуживает.
5. Заполните **Connection name** — это подпись подключения, её видно в списке
   (например, `work` или `личный`).
6. Вставьте свой ключ провайдера в поле **API key**. Поле маскируется; секрет
   очищается из полей и черновиков сразу после запроса и нигде не показывается
   повторно.
7. При желании задайте **Priority (optional)** — порядок перебора подключений
   одного провайдера (больше — раньше; см. §1.3).
8. Нажмите **Save**. Кнопка **Cancel** закрывает форму без сохранения.

Для провайдеров, работающих без ключа, поле **API key** не показывается вовсе
(в форме выводится пометка «This provider does not need a key.»). Для
OAuth-провайдеров вместо поля ключа показывается честное уведомление, что их
нельзя настроить вводом API-ключа (см. §2.5).

### 1.2. Что происходит на сервере

| Шаг | Что делает шлюз |
| --- | --- |
| Проверка | `POST /api/connections` сверяет `provider` с реестром и с gate каталога (§2.2): неизвестный или неподдерживаемый id отклоняется с `400 provider not supported` |
| Шифрование | Ключ шифруется полевым шифрованием: префикс `enc:v1:`, соль `omniroute-field-encryption-v1` (`crates/heterion-router-crypto`). Если `STORAGE_ENCRYPTION_KEY` не задан, шлюз **отказывается** писать ключ в открытом виде и возвращает ошибку |
| Запись | Строка кладётся в таблицу `provider_connections` (`provider`, `api_key`, `name`, `priority`, `is_active`) |
| Активация | Подключение создаётся активным (`is_active = 1`, если не указано иное) и сразу попадает в выборку при чтении |
| Приоритет | По умолчанию `priority = 10`; если у провайдера несколько подключений, они перебираются в порядке `priority DESC, id ASC` — сначала с наибольшим приоритетом (`crates/heterion-router-gateway/src/credentials.rs`) |
| Показ | В открытом виде ключ не показывается нигде — только префикс и признак «ключ есть» |

Ключ провайдера наружу не выходит: ни клиент, ни агент, ни MCP-сервер его не
видят. В открытом виде хранится только то, что лежит в БД шифром.

Форма **Add provider** ходит в админ-API шлюза: `GET /api/providers` — каталог
провайдеров, для которых можно завести ключ, `POST /api/connections` — создание
подключения (`provider`, `name`, `api_key`, `priority`, `is_active`).### 1.3. Важно: после добавления подключения шлюз нужно перезапустить

**Бэкенды строятся один раз при старте шлюза** (`main.rs::select_backend`):
при старте читаются активные подключения, и для каждого провайдера создаётся
свой backend. Новое подключение в этот список не попадает — до перезапуска
провайдер просто не будет обслуживать запросы, даже если ключ верный.

Как перезапустить:

- В приложении (раздел **Providers**) после сохранения появляется кнопка
  **Restart now** — она перезапускает шлюз и ждёт, пока он снова начнёт
  отвечать. То же самое делает веб-дашборд (`desktop/ui`): после изменения
  активности подключения он показывает «restart the gateway to apply it» и
  кнопку **Restart now**.
- Перезапуск через кнопку работает, только если шлюз запущен как macOS-служба
  под launchd: при ручном запуске (например, как sidecar из приложения) шлюз
  отвечает «gateway is not supervised by launchd; restart it yourself» и
  перезапускать нужно вручную — `launchctl kickstart -k
  gui/$(id -u)/com.heterion.router` для службы, а для sidecar — закрыть и
  снова открыть приложение.

> Перезапуск — это перезапуск **того же бинаря**, а не обновление: in-flight
> запросы при этом обрываются, поэтому нажимайте **Restart now** в спокойный
> момент.

### 1.4. Как проверить, что всё работает

Выпустите ключ самого роутера (см. §1.5) и проверьте два эндпоинта:

```sh
# 1. Список моделей, доступных роутингу
curl -s http://127.0.0.1:20128/v1/models \
  -H "Authorization: Bearer sk-ваш-ключ"

# 2. Реальный запрос
curl -s http://127.0.0.1:20128/v1/chat/completions \
  -H "Authorization: Bearer sk-ваш-ключ" \
  -H "Content-Type: application/json" \
  -d '{"model":"провайдер/модель","messages":[{"role":"user","content":"Привет"}]}'
```

`sk-ваш-ключ` — плейсхолдер; подставьте ключ, выпущенный в самом роутере.
Порт: **20128** — если шлюз стоит как macOS-служба; **20129** — если он
поднят приложением как sidecar.

Модель адресуется как `провайдер/модель` (например `openai/gpt-4.1`,
`agentrouter/gpt-5.6-sol`) либо именем комбо.

Где смотреть, что произошло:

| Что | Где |
| --- | --- |
| Журнал вызовов | Раздел **Logs** в приложении (вкладка «API calls») |
| Таблица в БД | `call_logs` (в той же SQLite, что и остальные данные роутера) |
| Логи самого приложения | Раздел **Logs**, вкладка «Application»; файл `gateway.log` в каталоге данных |
| Статус подключения | Раздел **Providers**: колонка **servable** (`yes`/`no`) — построен ли backend для этого провайдера |

### 1.5. Как выпустить ключ самого роутера

| Ситуация | Где |
| --- | --- |
| Свежая установка (в БД нет ни одного ключа) | На экране входа — кнопка **Create first key**: ключ выпускается, сохраняется в `<каталог данных>/desktop/operator-key.json` (файл `0600`) и вход происходит сразу |
| Обычный случай | Раздел **Keys** → **Issue key** → задать имя ключа |

Секрет показывается **ровно один раз** — сразу после выпуска. В базе лежит
только хеш, восстановить ключ из базы нельзя: если потеряли — выпустите
новый. Ключ оператора годится и для `/v1/*`, и для `/api/*`.

---

## 2. Каталог поддерживаемых провайдеров

### 2.1. Что в реестре

Полный каталог лежит в
[`crates/heterion-router-providers/registry.json`](../crates/heterion-router-providers/registry.json)
и вкомпилируется в шлюз при сборке. Реестр — это **каталог**, а не обещание,
что каждая запись рабочая: одна и та же запись может иметь формат, который
транспорт шлюза не умеет, или адрес, по которому шлюз ходить не может.

Сводка по реестру (280 записей, 2741 модель):

| Параметр | Значение |
| --- | --- |
| Всего записей | **280** |
| Всего записей моделей | **2741** |
| Проходят gate построения бэкенда | **271** провайдер / 2295 моделей |
| …из них с HTTP(S) `base_url` | **264** провайдера / 1993 моделей |
| …из них с не-HTTP схемой (`wss://`, `://cli/stdio`, `://acp/stdio`, `://app-server/stdio`, `://cli/websocket`) | **7** провайдеров / 302 моделей |
| Не проходят gate построения | **9** провайдеров / 446 моделей |
| Попадают в каталог `GET /api/providers` | **221** провайдеров / 1993 модели |
| Keyless (`auth_type: none` или `optional`) | **20** провайдеров в реестре, **16** с HTTP(S) `base_url`, **15** — в каталоге |
| `auth_type: oauth` | **21** провайдер в реестре, **15** — в каталоге |
| С нестандартным executor | **76** провайдеров в реестре, **62** — среди построимых с HTTP(S) адресом, **61** — в каталоге |

### 2.2. Два gate: построение бэкенда и каталог

В коде их два, и они не совпадают.

**Gate построения бэкенда** (`main.rs::select_backend`) — по нему шлюз решает,
какого провайдера он вообще способен обслуживать. Запись подходит, если:

1. `base_url` в реестре непустой (`provider_base_url_from`);
2. формат записи — `openai`, `openai-responses`, `claude` или `gemini`
   (либо executor `exa` — отдельная ветка поиска).

Схему адреса код не проверяет: backend для `wss://` или `://cli/stdio`
строится, но `reqwest` по такому адресу не поедет, и провайдер падает с
ошибкой транспорта в момент запроса. Поэтому «не-HTTP» записи отделены от
«прошедших gate» — проходят они формально, работать не будут.

Не проходят gate построения (9 записей): `agy`, `antigravity`, `bedrock`,
`clova-studio`, `cursor`, `cursor-api`, `deepai`, `kiro`, `muse-code` — у них
либо пустой `base_url`, либо формат, для которого в шлюзе нет транспорта.
С не-HTTP схемой (7 записей): `auggie`, `codex-app-server`, `copilot-m365-web`,
`copilot-web`, `devin-cli`, `devin-cli-agentic`, `zcode`.

**Gate каталога** (`admin::is_key_provider`) — по нему `GET /api/providers`
решает, для каких провайдеров вообще можно завести ключ через форму
**Add provider**. Условия строже:

1. формат записи моделится в `parse_wire_format`
   (`crates/heterion-router-providers/src/registry.rs`): `openai`,
   `openai-responses`, `claude`, `gemini`, `antigravity` — **либо** у записи
   есть свой исполнитель, который шлюз реализует сам (сейчас это `exa`,
   §2.6);
2. `base_url` начинается с `http://` или `https://`;
3. в реестре есть хотя бы одна модель.

В каталог попадают **221 провайдеров / 1993 модели**. Из 59 исключённых
записей: 5 — формат не моделится и своего исполнителя нет (`clova-studio`,
`cursor`, `cursor-api`, `deepai`, `kiro`), 11 — нет HTTP(S) `base_url`, 43 — нет
моделей в реестре.

> Форматы `clova`, `kiro`, `cursor` в первом релизе **не поддержаны**: для них
> нет ни ветки в `parse_wire_format`, ни транспорта в шлюзе. Формат `custom`
> моделируется только через свой исполнитель: запись `exa-search` обслуживается
> рантаймом отдельной веткой (§2.6) и **доступна в форме Add provider**.
> Всё, что не проходит gate, шлюз помечает как «не обслуживается», и в логах
> это видно как `backend … skipped`.

### 2.3. Основные провайдеры

Только то, что реально есть в реестре и видно в каталоге формы. Формат —
поле `format` записи; «нужен ли ключ» — поле `auth_type` (`apikey` — нужен;
`none` / `optional` — не нужен; `oauth` — см. §2.5).

| id | название | формат API | нужен ли ключ | base_url | где взять ключ |
| --- | --- | --- | --- | --- | --- |
| `openai` | OpenAI | openai | да | `https://api.openai.com/v1/chat/completions` | platform.openai.com → API keys |
| `anthropic` | Anthropic (Claude) | claude | да | `https://api.anthropic.com/v1/messages` | console.anthropic.com → Settings → API Keys |
| `gemini` | Google Gemini | gemini | да | `https://generativelanguage.googleapis.com/v1beta/models` | aistudio.google.com → Get API key |
| `openrouter` | OpenRouter | openai | да | `https://openrouter.ai/api/v1/chat/completions` | openrouter.ai → Keys |
| `deepseek` | DeepSeek | openai-responses | да | `https://api.deepseek.com/responses` | platform.deepseek.com → API keys |
| `groq` | Groq | openai | да | `https://api.groq.com/openai/v1/chat/completions` | console.groq.com → API Keys |
| `mistral` | Mistral | openai | да | `https://api.mistral.ai/v1/chat/completions` | console.mistral.ai → API Keys |
| `together` | Together AI | openai | да | `https://api.together.xyz/v1/chat/completions` | api.together.xyz → API Keys |
| `xai` | xAI (Grok) | openai | да | `https://api.x.ai/v1/chat/completions` | console.x.ai → API Keys |
| `moonshot` | Moonshot (Kimi) | openai | да | `https://api.moonshot.ai/v1/chat/completions` | platform.moonshot.ai → API Keys |
| `zai` | Z.ai (GLM) | claude | да | `https://api.z.ai/api/anthropic/v1/messages` | z.ai → Manage API Key |
| `ollama-cloud` | Ollama Cloud | openai | да | `https://ollama.com/v1/chat/completions` | ollama.com → Settings → Keys |
| `siliconflow` | SiliconFlow | openai | да | `https://api.siliconflow.com/v1/chat/completions` | cloud.siliconflow.cn → Account → API Key |
| `novita` | Novita AI | openai | да | `https://api.novita.ai/openai/v1/chat/completions` | novita.ai → Settings → Key Management |
| `hyperbolic` | Hyperbolic | openai | да | `https://api.hyperbolic.xyz/v1/chat/completions` | app.hyperbolic.xyz → Settings → API Key |
| `cerebras` | Cerebras | openai | да | `https://api.cerebras.ai/v1/chat/completions` | cloud.cerebras.ai → API Keys |
| `fireworks` | Fireworks AI | openai | да | `https://api.fireworks.ai/inference/v1/chat/completions` | fireworks.ai → API Keys |
| `nvidia` | NVIDIA NIM | openai | да | `https://integrate.api.nvidia.com/v1/chat/completions` | build.nvidia.com → Get API Key |
| `huggingface` | Hugging Face Inference | openai | да | `https://router.huggingface.co/v1/chat/completions` | huggingface.co → Settings → Access Tokens |
| `ai21` | AI21 | openai | да | `https://api.ai21.com/studio/v1/chat/completions` | studio.ai21.com → API Keys |
| `agentrouter` | AgentRouter | openai-responses | да | `https://agentrouter.org/v1/responses` | agentrouter.org → Dashboard → Tokens → Add Token |

Замечания по таблице:

- `deepseek` в реестре имеет формат `openai-responses` (есть эндпоинт
  `/responses`), а не `openai`. Псевдоним реестра — `ds`.
- `agentrouter` — Responses-протокол, и площадка **меряет квоту по типу
  клиента**: без распознанного клиента отдаёт `401 unauthorized_client_error`
  даже с верным ключом. Это политика площадки, а не ошибка ключа.
- В реестре **нет** записей `zhipu` / `zhipuai` (есть только `zai`), `jina`,
  `ollama` в виде «локального Ollama» (есть `ollama-cloud` и прокси
  `g4f-ollama`) и `gemini-api` (есть `gemini`). Такие id в каталоге не
  появятся — писать их вручную нельзя.
- Идентификаторы моделей у провайдера — его собственные; актуальный список
  моделей площадки стоит смотреть у провайдера, а не в реестре.

### 2.4. Keyless-провайдеры (ключ не нужен)

У этих записей `auth_type` — `none` или `optional`: шлюз не шлёт им
`Authorization` вообще (проверяется тестом
`keyless_provider_sends_no_bearer`). Поле **API key** в форме не
показывается, поле можно оставить пустым.

В каталоге их **15** (из 20 keyless-записей реестра):

| id | auth_type | base_url |
| --- | --- | --- |
| `heterion-local` | none | `http://127.0.0.1:8080/v1` (переопределяется `HETERION_LOCAL_BASE_URL`) |
| `pollinations` | optional | `https://text.pollinations.ai/v1/chat/completions` |
| `kilo-gateway` | optional | `https://api.kilo.ai/api/gateway/chat/completions` |
| `ovhcloud` | optional | `https://oai.endpoints.kepler.ai.cloud.ovh.net/v1/chat/completions` |
| `uncloseai` | optional | `https://hermes.ai.unturf.com/v1/chat/completions` |
| `cloudflare-playground` | none | `https://playground.ai.cloudflare.com` |
| `duckduckgo-web` | none | `https://duck.ai/duckchat/v1/chat` |
| `veoaifree-web` | none | `https://veoaifree.com/wp-admin/admin-ajax.php` |
| `uc` | none | `https://internal-6.pubyar.com` |
| `chipotle` | none | `https://amelia.chipotle.com` |
| `g4f-gemini` | optional | `https://g4f.space/api/gemini/v1/chat/completions` |
| `g4f-groq` | optional | `https://g4f.space/api/groq/v1/chat/completions` |
| `g4f-nvidia` | optional | `https://g4f.space/api/nvidia/v1/chat/completions` |
| `g4f-ollama` | optional | `https://g4f.space/api/ollama/v1/chat/completions` |
| `g4f-pollinations` | optional | `https://g4f.space/api/pollinations/v1/chat/completions` |

Ещё 5 keyless-записей в реестре в каталог не попадают: `auggie`,
`codex-app-server`, `devin-cli-agentic`, `zcode` — не-HTTP схемы, и
`naga-ac` — в реестре нет моделей. В первом релизе они не обслуживаются.

### 2.5. OAuth-провайдеры (надо авторизоваться вручную)

21 запись реестра имеет `auth_type: oauth`: `agy`, `antigravity`, `claude`,
`cline`, `clinepass`, `codebuddy-cn`, `codex`, `cursor`, `devin-cli`,
`devin-desktop`, `ghe-copilot`, `github`, `gitlab-duo`, `grok-cli`,
`kilocode`, `kimi-coding`, `kiro`, `openference`, `trae`, `xai-oauth`,
`zed-hosted`. Из них **15** видны в каталоге формы.

Для них **ввод API-ключа не подходит**: их access-token выдаёт сам провайдер
после ручной авторизации (логин, OAuth, cookie), и такой токен живёт в
`access_token`/`refresh_token`, а не в `api_key`. Поэтому форма **Add
provider** для OAuth-записей не показывает поле ключа и выводит уведомление
«OAuth connections must be configured manually. Adding a key through this form
is not available.» Такие подключения настраиваются вручную (или через
специализированные сценарии входа), и в первом релизе их нельзя завести
через эту форму.

Ещё одна запись вне этих двух классов: `suno` (`auth_type: cookie`) — она
тоже требует ручной настройки (кука вместо ключа), но в реестре числится как
`cookie`, а не `oauth`.

### 2.6. Провайдеры с executor: `exa`

Отдельный класс — провайдеры с нестандартным executor'ом: им не нужен
«ключ как у чат-провайдера, идущий в `/chat/completions`», они работают как
**модель роутера** на чат-поверхности.

Единственный такой провайдер в реестре сейчас — `exa-search`
(`executor: "exa"`, `base_url: https://api.exa.ai`, формат `custom`):

- адресуется как `exa/exa-search` (псевдоним `exa` в реестре);
- последнее сообщение пользователя — это поисковый запрос, ответ приходит
  цитированным блоком;
- это инструмент поиска, а не участник чат-комбо: в комбо он не входит;
- ключ Exa хранится **только в БД роутера**; потребитель (например, плагин
  поиска) берёт ключ у роутера и ходит на `http://127.0.0.1:20128`, а не на
  `api.exa.ai` напрямую.

Ключ добавляется через ту же форму **Add provider**: `exa-search` есть в
каталоге (у него свой executor `exa`, а формат `custom` — шлюз обслуживает его
отдельной веткой по executor'у в `main.rs`), поэтому гейт каталога его
пропускает. Ключ пишется в `provider_connections` и после рестарта шлюза
подхватывается тем же путём, что и у обычных провайдеров.

### 2.7. Прочие executor'ы

В реестре 76 записей с executor'ом, отличным от `default` (`glm`, `opencode`,
`moonshot`, `vertex`, `github`, `grok-cli`, `xai`, `kilo-gateway` и другие).
Это **метаданные**, а не отдельные реализации: рантайм ветвится по формату
записи, а не по executor'у. Поэтому поведение таких провайдеров — «обычный
OpenAI-совместимый вызов по Bearer + Chat Completions» (или по своему
формату, если формат не `openai`). Специальный код есть только у `exa`.

---

## 3. Типовые проблемы

| Симптом | Причина | Что делать |
| --- | --- | --- |
| `401` от провайдера | Неверный ключ (или ключ с другого провайдера) | Проверить ключ в кабинете провайдера, перевыпустить и перезаменить в форме **Add provider**; после замены — **Restart now**. Если 401 приходит именно от `agentrouter` — проверить, что клиент идентифицирован площадкой |
| `403` от провайдера | Ключ есть, но нет прав на модель/группу/лимит | В кабинете провайдера открыть доступ к нужной модели или группе; проверить лимиты тарифа |
| `404` от провайдера | Неверный `base_url` для этого id либо модели нет в реестре | Сверить `base_url` с §2.3; модель должна быть в списке `GET /v1/models`; у провайдера может быть своя политика имён моделей |
| `404` от роутера (`no_candidate`) | Модель не пробросилась: нет комбо с таким именем и нет пары `провайдер/модель` | Взять точное имя из `GET /v1/models`; проверить, что для провайдера построен backend |
| Пустой ответ без текста | Модель не пробросилась, либо у reasoning-модели выбран слишком малый `max_tokens` (бюджет ушёл в размышление) | Проверить маршрут по логам (**Logs**); для `agentrouter` и других reasoning-моделей давать `max_tokens` 4000+ |
| «no backend» / ничего не работает | Нет активных подключений, либо шлюз не перезапущен после добавления | Проверить колонку **servable** в разделе **Providers** (`no` — backend не построен); после добавления нажать **Restart now** (или перезапустить шлюз вручную, если он не под launchd) |
| Провайдер есть в реестре, но его нет в каталоге | Запись не проходит gate (нет HTTP(S) `base_url`, нет моделей, формат не поддержан) | Взять другого провайдера из §2.3; запись без транспорта в первом релизе не заработает |
| Поле ключа не появляется | Провайдер keyless (§2.4) или OAuth (§2.5) | Для keyless оставьте поле пустым; для OAuth настраивайте вручную |
| `401` от самого роутера | Неверный или отозванный ключ роутера | Выпустить новый в разделе **Keys** → **Issue key**; отозванные ключи перестают работать сразу |

---

## 4. Где смотреть первоисточники

| Что | Файл |
| --- | --- |
| Полный каталог провайдеров и моделей | `crates/heterion-router-providers/registry.json` |
| Правила чтения реестра, `parse_wire_format`, keyless-предикат | `crates/heterion-router-providers/src/registry.rs` |
| Построение бэкендов (gate, транспорт по формату) | `crates/heterion-router-gateway/src/main.rs` |
| Чтение ключей, порядок по priority | `crates/heterion-router-gateway/src/credentials.rs` |
| Полевым шифрованием (`enc:v1:`, соль) | `crates/heterion-router-crypto/src/lib.rs` |
| Форматы API шлюза | `crates/heterion-router-core/src/format.rs` |
| Установка и первый запуск | [docs/INSTALL.md](INSTALL.md) |
| Шпаргалка для агента | [docs/AGENT-ROUTER-GUIDE.md](AGENT-ROUTER-GUIDE.md) |
