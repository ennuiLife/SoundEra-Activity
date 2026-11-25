# SoundEra Activity

A small service that runs next to a Navidrome (OpenSubsonic) server and adds what Navidrome cannot carry:
live presence now, and later follows, reactions, chat and listen-together. See [PLAN.md](PLAN.md).

Status: login, presence, follows, reactions, chat and blocking. Free software under the AGPL-3.0.

## Run

```bash
NAVIDROME_URL=https://music.example.com BIND=127.0.0.1:4540 DATA_DIR=./data cargo run -p activity-server --release
```

Put it behind HTTPS (a reverse proxy) when it is reachable from outside. Plain http to another machine for
`NAVIDROME_URL` is warned about, because logins would travel unprotected.

## API (v1)

| Call | What |
|---|---|
| `POST /activity/v1/login` `{user, token, salt}` or `{user, password}` | Navidrome confirms the login; returns `{session, expires_at}` (12 h). Nothing about the login is stored. |
| `GET /activity/v1/me`, `PUT /activity/v1/me/settings` `{share_presence}` | Your state; switching sharing off removes your presence at once. |
| `PUT /activity/v1/presence`, `DELETE ...`, `GET ...` | Send what you play (every 15 s; entries expire after 90 s), end it, read everyone else's. |
| `GET /activity/v1/ws?session=...` | Live events: `presence` and `presence_ended` from other people. |

| `PUT/DELETE /activity/v1/follows/{user}`, `GET .../follows`, `GET .../feed` | Follow people; the feed is what the people you follow play now. |
| `POST /activity/v1/reactions` `{to, song_id, kind}`, `GET .../reactions` | React (`like`, `fire`, `sad`, `laugh`) to what someone plays right now; the same reaction twice counts once. The inbox holds the last 7 days. |
| `POST /activity/v1/chat` `{body}`, `GET .../chat?after=ID&limit=N`, `DELETE .../chat/{id}` | The one server-wide room: 500 characters, 5 messages per 10 s, history kept 30 days, only the writer can delete. |
| `PUT/DELETE /activity/v1/blocks/{user}`, `GET .../blocks` | Block someone: their chat, reactions and presence are hidden both ways, and follows between you end. A blocked person is not told. |

Live events on the WebSocket now also include `reaction` (only to the person it is for), `chat` and `chat_deleted`.

Send the session as `Authorization: Bearer ...`.

## Test

```bash
cargo test
```

The tests run against a fake Navidrome with made-up users; no real data is used anywhere.

## Licence

Copyright (C) 2026 ennuiLife. SoundEra Activity is free software: you can use, host, study, change and share it under
the terms of the [GNU Affero General Public License](LICENSE), version 3 or (at your option) any later version. If you
run a modified version as a service for other people, the AGPL asks you to offer them its source too.

It is a separate program from the SoundEra player, which talks to it over HTTP.
