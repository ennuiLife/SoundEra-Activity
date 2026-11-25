# SoundEra Activity: plan

A small Rust service that runs **next to** a Navidrome (OpenSubsonic) server and carries the social features
Navidrome cannot: richer presence, follows, reactions, chat and "listen together". People keep their Navidrome and
their library. SoundEra gets one setting (the service address) and falls back to plain `getNowPlaying` without it.

Status: weeks 1 to 4 built: login, presence, follows, reactions, chat, blocking and listen together on the service; SoundEra has screens for all but listen together; packaging (Docker, systemd, proxy examples, CI and release workflows) is in `deploy/` and `.github/` (login, presence, follows and feed on the service; SoundEra setting for the address, sending and reading presence with fallback). Nothing published. Fictional data only in tests and docs.

## 1. Features, in priority order

| # | Feature | What it does |
|---|---|---|
| M1 | Login + presence | Verify the user against Navidrome; clients send now-playing (song, position, state, device); others read it live. Per-user sharing switch. |
| M2 | Follows | Follow / unfollow users; "friends listening now" feed; follow requests optional (off by default: everyone on the server is visible to everyone, as today). |
| M3 | Reactions | React to a song someone is playing (a small fixed set, e.g. like / fire / sad / laugh); shown to that user live. |
| M4 | Chat | Per-server room and direct messages; text only, length-limited, history kept N days; mute and block. |
| M5 | Listen together | A host's playback state (song, position, play/pause) is broadcast; followers can join and stay in sync. |

Out of scope for now: media streaming, library scanning, files, a web UI, federation between servers.

## 2. Auth (no passwords stored)

- The service is configured with one Navidrome base URL (`NAVIDROME_URL`).
- Login: the client sends the user name plus the OpenSubsonic credentials it already has (`t` token + `s` salt, or
  the password over HTTPS). The service replays them to Navidrome's `ping` (and `getUser` for the user's id / admin flag).
  A good answer means "this user is who they say". The credentials are **not stored or logged**.
- The service then issues a short-lived signed session token (HMAC, 12 h, user + issued-at), used as a Bearer token
  for HTTP and for the WebSocket. Signing key: random at first start, kept in the data folder (mode 0600).
- Rate-limit logins per IP and per user; constant-time token compare; HTTPS expected (reverse proxy), plain http only
  for localhost / LAN with a warning in the log.

## 3. Protocol

JSON over HTTPS plus one WebSocket for live updates. Versioned under `/activity/v1`.

- `POST /activity/v1/login` body `{user, token, salt}` or `{user, password}` -> `{session, expires_at}`
- `GET  /activity/v1/me`, `PUT /activity/v1/me/settings` (`share_presence`, later `share_with`)
- `PUT  /activity/v1/presence` `{song_id, title, artist, album, position_ms, duration_ms, state, device}`
- `GET  /activity/v1/presence` -> everyone sharing: `[{user, song..., state, updated_at}]` (own entry excluded)
- `GET  /activity/v1/ws?session=...` -> events: `presence`, `follow`, `reaction`, `chat`, `room` (M5)
- Later: `/follows`, `/reactions`, `/chat/rooms`, `/rooms` (listen together).
- Presence expires after 90 s without an update (client heart-beats every 15 s, as `getNowPlaying` is polled now).
- Everything is optional for a client: no service, no features, SoundEra keeps working.

## 4. Data model (SQLite, `rusqlite` + migrations)

`users(id, name, created_at, share_presence)`, `presence(user_id, song_json, position_ms, state, device,
updated_at)` (in memory with periodic snapshot is fine), `follows(follower, followee, created_at)`,
`reactions(id, from_user, to_user, song_id, kind, created_at)`, `messages(id, room, from_user, to_user NULL, body,
created_at)`, `rooms(...)` for M5. User ids come from Navidrome user names (the stable key). No passwords, tokens or
emails are stored.

## 5. Security and privacy

- Presence is opt-out per user and off the moment the switch is off (the row is deleted).
- Input limits everywhere (body size, text length, rate per user); all text is only ever shown, never run.
- Chat: block / mute lists, retention limit, delete own messages. No media or links previews (no server-side fetching).
- Logs never contain credentials or full tokens; web addresses are cut before `?`.
- One process, one SQLite file, runs as an unprivileged user; ships with a systemd unit and a Docker example later.
- A `SECURITY.md` like SoundEra's before anything is published.

## 6. Stack

Rust, `axum` + `tokio`, `rusqlite`, `serde`, `hmac`/`sha2`, `tracing`. Workspace: `crates/activity-core` (types,
protocol, tests), `crates/activity-server` (binary). Integration tests run against a **fake Navidrome** (a few
lines of axum answering `ping` / `getUser`), with fictional users only.

## 7. Milestones (focused work)

| Week | Result |
|---|---|
| 1 | Scaffold, config, fake Navidrome, login + session tokens, presence over HTTP and WebSocket, tests. |
| 2 | SoundEra side: setting "activity service address", uses presence from the service, falls back to `getNowPlaying`. Follows (M2). |
| 3 | Reactions (M3), chat (M4), moderation basics. |
| 4 | Listen together (M5), hardening, packaging (systemd, Docker), docs, security policy. |

## 8. Open questions for the user

1. Project name and repo: `SoundEra-Activity` (private first, like SoundEra) fine?
2. Everyone on the server can see everyone's presence today; keep that default, or opt-in per user?
3. Chat: server-wide room only at first, or direct messages from the start?
4. Licence for this service (all rights reserved like SoundEra, or open source so others can self-host?).
