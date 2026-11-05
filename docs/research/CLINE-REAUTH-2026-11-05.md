# Cline: переавторизация. OAuth недоступен, рабочий путь — API-ключ

Дата: 2026-11-05 · Проект: `heterion-router` · Провайдер: `cline`
(в роутере — подключение «Егор Коростелев», `auth_type: oauth`)

## Коротко

- **OAuth у Cline сейчас сломан на их стороне.** Страница входа
  (`authkit.cline.bot`, WorkOS AuthKit) отдаёт Next.js-ошибку: HTTP 307 с
  телом `id="__next_error__"` и **пустым** `<body>`. Никакой браузер её не
  отрисует — проверил и во встроенном браузере сессии, и через curl.
- **API-хост Cline из России работает.** `GET https://api.cline.bot/api/v1/models`
  отвечает 200 и отдаёт **435 моделей**, включая `stealth/crouton-alpha`.
  То есть проблема не в гео-блокировке API, а только в странице входа.
- **Рабочий путь — API-ключ** (официально рекомендованный для программного
  доступа): `app.cline.bot` → Settings → API Keys. `app.cline.bot` из России
  отвечает 200, страница работает.
- Установщик подготовлен: одна команда ставит ключ в роутер, чистит
  истёкший OAuth-токен, перезапускает сервис и проверяет живым вызовом.
- Сам **OAuth-токен в роутере мёртв**: `access_token` истёк 2026-09-17, а
  refresh отдаёт `invalid_grant`.

## Что измерено

### Диагностика OAuth-флоу

Контракт изучен по исходникам Cline SDK
(`cline/cline`, `sdk/packages/core/src/auth/cline.ts`) и по реализации
OmniRoute (`src/lib/oauth/providers/cline.ts`).

| Шаг | Эндпоинт | Результат |
| --- | --- | --- |
| 1. Собрать URL | `GET /api/v1/auth/authorize?client_type=extension&callback_url=…&redirect_uri=…` | 302 на WorkOS; `state` (подписан) несёт наш `callback_url` |
| 2. Вход | `https://api.workos.com/user_management/authorize` → `authkit.cline.bot/bootstrap` → `authkit.cline.bot/?…&authorization_session_id=…` | **пустая страница** |
| 3. Обмен кода | `POST /api/v1/auth/token` | ждёт `grant_type` в **snake_case** |
| 4. Обновление | `POST /api/v1/auth/refresh` | ждёт `grantType` в **camelCase** |

Тонкость, из-за которой сначала не сходилось: `/token` и `/refresh` принимают
**разные** начертания одного поля. Проверено перебором:

| Поле | `/api/v1/auth/token` | `/api/v1/auth/refresh` |
| --- | --- | --- |
| `grant_type` | принимает («invalid or expired authorization code») | «granttype required» |
| `grantType` | «granttype required» | принимает («failed to refresh token: invalid_grant») |
| `granttype` | «granttype required» | принимает |

### AuthKit сломан глобально

Проверены три URL, в двух локалях (`Accept-Language: ru-RU` и `en-US`):

| URL | HTTP | `__next_error__` | `<title>` | `<body>` |
| --- | --- | --- | --- | --- |
| `https://authkit.cline.bot/` | 307 | да | Sign in | пустой |
| `https://authkit.cline.bot/device` | 307 | да | Confirm Device | пустой |
| `https://authkit.cline.bot/?client_id=…&state=…` | 307 | да | Вход / Sign in | пустой |
| то же с `Accept-Language: ru-RU` | 307 | да | Вход | пустой |

JS-чанги страницы отдаются нормально (200, 19–20 КБ), ошибок в консоли нет,
упавших запросов в сети нет — падает именно **server-side render**
(`__next_error__`), после чего клиентский React ничего не рисует: в
встроенном браузере DOM — `<body><div dir="ltr" lang="ru"></div></body>`.

То есть это поломка на стороне Cline (WorkOS AuthKit), а не проблема
российского egress и не наш браузер.

### Device-флоу (обход браузера) — тоже заблокирован

Cline SDK умеет RFC 8628 device-code: `POST
https://api.workos.com/user_management/authorize/device` отдаёт
`user_code` и `verification_uri`. Проверено — работает:

```
user_code: DSZH-ZSDG
verification_uri_complete: https://authkit.cline.bot/device?user_code=DSZH-ZSDG
```

Но страница подтверждения — тот же сломанный AuthKit (`/device` → 307,
`__next_error__`, пустой body). Поэтому device-флоу тоже не проходит.

### Что работает из России

| Хост | Результат |
| --- | --- |
| `api.cline.bot/api/v1/models` | **200**, 435 моделей (включая `stealth/crouton-alpha`) |
| `app.cline.bot` | **200**, дашборд работает |
| `docs.cline.bot` | 200 |
| `api.cline.bot/api/v1/api-keys` | 401 без ключа (ожидаемо) |
| `authkit.cline.bot` | 307 + сломанный SSR |

## Подготовленное состояние

### 1. Установщик ключа

`/tmp/cline_install_key.py` — одна команда:

```sh
python3 /tmp/cline_install_key.py <ваш-ключ>
```

Что делает:
1. проверяет ключ напрямую по `https://api.cline.bot/api/v1/chat/completions`;
2. шифрует его штатным `FieldCrypto` (`STATIC_SALT`, префикс `enc:v1:`) и
   кладёт в `provider_connections.api_key` для `provider='cline'`;
3. **чистит `access_token`/`refresh_token`** — в `token_source()` побеждает
   `access_token`, поэтому старый (истёкший) OAuth-токен иначе продолжил бы
   затенять новый ключ;
4. перезапускает launchd-job `com.heterion.router`;
5. проверяет живым вызовом через роутер.

### 2. Реестр

В `crates/heterion-router-providers/registry.json` в список моделей `cline`
добавлена `stealth/crouton-alpha` — она есть в каталоге Cline (проверено
живым `GET /v1/models`), но в реестре её не было, и она не попадала в
`/v1/models` роутера.

Правка `auth_type` в реестре с `"oauth"` на `"apikey"` **не** делалась:
Rust-роутер это поле в рантайме не читает (используется только
`registry.is_openai_format` и `registry.allows_keyless`), а `cline` как
провайдер в равной степени работает с обоими способами — при этом в
дашборде остаётся доступен OAuth-флоу, когда у Cline починят AuthKit.

### 3. Где взять ключ

`app.cline.bot` → войти в аккаунт → **Settings → API Keys** → создать ключ и
скопировать (второй раз не показывается). Формат запроса — обычный Bearer:

```sh
curl -X POST https://api.cline.bot/api/v1/chat/completions \
  -H "Authorization: Bearer $CLINE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"stealth/crouton-alpha","messages":[{"role":"user","content":"Hello"}]}'
```

## Ограничения

- Модель `stealth/crouton-alpha` в каталоге Cline есть, но её доступность
  по подписке не проверена — нет действующей сессии. После установки ключа
  это выяснится первым же вызовом.
- Ротация каталога Cline: список моделей меняется, поэтому запись в реестре
  может устареть.
- Если Cline починит AuthKit, OAuth-путь снова станет рабочим — установщик
  этого не ломает: он кладёт ключ в `api_key` и чистит OAuth-токен только у
  этой строки подключения.
