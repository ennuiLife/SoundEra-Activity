# SoundEra Activity

A small service that runs next to a Navidrome (OpenSubsonic) server and adds what Navidrome cannot carry:
live presence now, and later follows, reactions, chat and listen-together. See [PLAN.md](PLAN.md).

Status: milestone 1 (login and presence). Private, unpublished.

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

Send the session as `Authorization: Bearer ...`.

## Test

```bash
cargo test
```

The tests run against a fake Navidrome with made-up users; no real data is used anywhere.
