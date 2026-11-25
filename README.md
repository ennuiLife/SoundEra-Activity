# SoundEra Activity

**Live presence, follows, reactions, chat and "listen together" for people who share a Navidrome server.**

SoundEra Activity is a small, self-hosted service that runs *next to* your [Navidrome](https://www.navidrome.org/)
(or any OpenSubsonic) server. Navidrome can only say "this person is playing this song". Activity adds what a music
server does not carry: a live "now playing" for everyone, people you follow, reactions to what someone plays, a chat
room, blocking, and joining someone's listening. Your library stays in Navidrome; nobody has to move anything.

The [SoundEra](https://github.com/ennuiLife/SoundEra-releases) desktop player talks to it. Anyone can write another
client: the protocol is plain JSON over HTTPS plus one WebSocket (see the [API](#api)).

It is free software ([AGPL-3.0](LICENSE)). It has no accounts of its own: **people sign in with the Navidrome login
they already have**, and the service never stores a password.

```
SoundEra  ──HTTPS──►  your reverse proxy  ──►  SoundEra Activity  ──►  Navidrome
 (many people)         (Caddy / nginx)           (this program)        (your music)
```

## Contents

- [What you get](#what-you-get) · [How logging in works](#how-logging-in-works)
- [Install](#install): [binary](#option-a-the-ready-made-program) · [Docker](#option-b-docker) · [from source](#option-c-build-it-yourself)
- [Run it as a service](#run-it-as-a-service) · [HTTPS](#put-https-in-front-of-it) · [Configuration](#configuration)
- [Connect SoundEra](#connect-soundera) · [Using it](#using-it)
- [Privacy and security](#privacy-and-security) · [Updates](#updates) · [Backups](#backups) · [Troubleshooting](#troubleshooting)
- [API](#api) · [Developing](#developing) · [Licence](#licence)

## What you get

| Feature | What it does |
|---|---|
| **Presence** | Everyone sees what the others on the server play right now: song, artist, how long ago, on which device. Updates arrive live. You can switch sharing off at any time, and you disappear at once. |
| **Follows** | Follow people; show only the people you follow. |
| **Reactions** | React to what someone is playing right now with like, fire, sad or laugh. They only reach the person, and only while they play that very song. |
| **Chat** | One chat room for the whole server. 500 characters a message, a few messages per ten seconds, history kept 30 days. You can reply to a message (the reply carries a short look at the one it answers) and delete your own messages. |
| **Pictures** | Each person can set a profile picture (Navidrome cannot store one). Send any PNG or JPEG up to 4 MB; the service keeps a 256 px square PNG made from the middle of it. Everyone using the service sees it. |
| **Profiles** | Each person can share a small summary (plays, liked songs and playlists counts, top five artists and genres) so others can open their profile. Nothing else about the library is sent. Switching sharing off removes it. |
| **Blocking** | Block someone: you stop seeing their presence, reactions and chat, they stop seeing yours, follows end, and listening along ends. They are not told. |
| **Listen together** | Join someone who is playing: you get their song and position to start in step, they are told you joined, and you are let go when they stop. |

What it does **not** do: stream or store music, scan libraries, or show a web page. It is only the social layer.

## How logging in works

There is no separate sign-up. When SoundEra starts, it sends the service your user name and the same one-time
*token and salt* it already uses to talk to Navidrome (never the password). The service replays them **once** to your
Navidrome's `ping` call. If Navidrome says yes, the service issues its own short-lived session token (12 hours) and
forgets the login. Because Navidrome is the one deciding, only people with a Navidrome account can use your service,
and their accounts are exactly the ones you already manage there.

## Install

You need a Linux machine that can reach your Navidrome (often the same one). Pick one option.

### Option A: the ready-made program

Each [release](https://github.com/ennuiLife/SoundEra-Activity/releases) has a static Linux x86_64 program (it runs on
any distribution) and a `SHA256SUMS` file.

```bash
VERSION=v0.2.0   # use the newest version on the Releases page
BASE=https://github.com/ennuiLife/SoundEra-Activity/releases/download/$VERSION
curl -LO $BASE/soundera-activity-$VERSION-linux-x86_64.tar.gz
curl -LO $BASE/soundera-activity-$VERSION-linux-x86_64.tar.gz.minisig
curl -LO $BASE/SHA256SUMS
sha256sum -c SHA256SUMS            # must say: OK
tar xzf soundera-activity-$VERSION-linux-x86_64.tar.gz
sudo install -m 755 soundera-activity-$VERSION-linux-x86_64/soundera-activity /usr/local/bin/
soundera-activity --version
soundera-activity --verify soundera-activity-$VERSION-linux-x86_64.tar.gz soundera-activity-$VERSION-linux-x86_64.tar.gz.minisig
# must print: version:v0.2.0 file:soundera-activity-v0.2.0-linux-x86_64.tar.gz   (the signature, see Updates)
```

The tarball also holds the `deploy/` folder with the service file and proxy examples used below.

### Option B: Docker

An image is published with every release: `ghcr.io/ennuilife/soundera-activity`. With Docker Compose, save
`deploy/docker-compose.yml`, set `NAVIDROME_URL` to where Navidrome is reachable from the container, and:

```bash
docker compose up -d
```

It listens on `127.0.0.1:4540` only (put a proxy in front, below), keeps its data in a volume, runs as an
unprivileged user with a read-only file system, and has a health check. To build the image yourself instead:
`docker build -t soundera-activity .`

### Option C: build it yourself

You need [Rust](https://rustup.rs/) (stable).

```bash
git clone https://github.com/ennuiLife/SoundEra-Activity.git
cd SoundEra-Activity
cargo build --release -p activity-server
sudo install -m 755 target/release/soundera-activity /usr/local/bin/
```

## Run it as a service

With the program installed (options A or C), use the systemd unit from `deploy/`:

```bash
sudo cp deploy/soundera-activity.service /etc/systemd/system/
sudoedit /etc/systemd/system/soundera-activity.service     # set NAVIDROME_URL
sudo systemctl daemon-reload
sudo systemctl enable --now soundera-activity
systemctl status soundera-activity
curl http://127.0.0.1:4540/activity/v1/health              # {"status":"ok","version":"..."}
```

The unit runs it as a private, throw-away user with its own data folder (`/var/lib/soundera-activity`) and with most
of the system closed off. Logs: `journalctl -u soundera-activity -f`. The first line of every run says which version it is, for example
`SoundEra Activity 0.3.0 (linux x86_64) starting`, and `soundera-activity --version` and `GET /activity/v1/health` say it too.

## Put HTTPS in front of it

The service listens on `127.0.0.1:4540` and speaks plain HTTP, so that it is never exposed by accident. **Put a reverse
proxy with HTTPS in front of it**: SoundEra refuses a plain `http://` address for anything outside your own network,
because the login token would travel unprotected.

You need a domain name pointing at your server (say `activity.example.com`) and ports 80 and 443 open. Do **not** open
port 4540.

**Caddy** (easiest: it gets and renews the certificate itself). Put this in `/etc/caddy/Caddyfile` and reload Caddy:

```
activity.example.com {
	reverse_proxy 127.0.0.1:4540
}
```

**nginx**: use `deploy/nginx.conf` (it includes the headers the WebSocket needs) with a certificate from Let's Encrypt.

Check it from outside: `curl https://activity.example.com/activity/v1/health`.

## Configuration

Everything is set with environment variables.

| Variable | Default | What it is |
|---|---|---|
| `NAVIDROME_URL` | *(required)* | Where your Navidrome (or OpenSubsonic server) listens, as this machine sees it, e.g. `http://127.0.0.1:4533` or `https://music.example.com`. Plain `http://` to another machine is warned about. |
| `BIND` | `127.0.0.1:4540` | Address and port to listen on. Leave it on the local address when a proxy is in front; the Docker image uses `0.0.0.0:4540` inside the container. |
| `DATA_DIR` | `data` | Where the database (`activity.db`: follows, blocks, chat) and the session key (`session.key`) live. Created with owner-only permissions. |
| `RUST_LOG` | `info` | Log level, for example `debug` or `soundera_activity=debug`. Logs never contain passwords, tokens or full addresses. |
| `UPDATE_CHECK` | on | Set to `off` to stop the daily look for a newer version (the only time the service contacts GitHub). |

Command line: `soundera-activity --version`, `--help`, `--health` (asks the running service whether it is well; exit
code 0 or 1; used by the Docker health check), `--check-update` and `--verify FILE SIGNATURE` (see [Updates](#updates)).

## Connect SoundEra

1. Install SoundEra and connect to your Navidrome as usual.
2. Open **Settings > Online > Activity service**, type the address (for example `https://activity.example.com`) and
   press **APPLY**. The line under the field says *Connected to the activity service.* when it works, or why it does
   not.
3. Everyone who should see each other does the same with the same address.

Leave the field empty to go back to SoundEra's normal behaviour, which uses only the server.

## Using it

- **See who is listening.** Add the **Online activity** widget (Edit Mode > New widget, or open the Profile page).
  Each row is someone playing right now. Click it to play that song.
- **Follow people.** The person icon on a row follows (or stops following). Press **FOLLOWING** in the widget header to
  show only the people you follow.
- **React.** The thumbs-up icon on a row opens four reactions: like, fire, sad, laugh. The person gets a notice the
  next time their SoundEra looks (within about 15 seconds).
- **Chat.** Add the **Chat** widget (Edit Mode > New widget). Type and press Enter. Right-click a message to block its
  writer, or to delete your own.
- **Block.** Right-click a person's row or chat message > **Block**. Settings > Online lists the people you blocked
  with an **UNBLOCK** button.
- **Stop sharing.** Switch **Online activity** off (Settings > Online, or the globe icon on the Profile page). Nothing
  is sent and nothing is shown, and you vanish from everyone's list at once.

## Privacy and security

What the service **sees and keeps**:

| | |
|---|---|
| Sees, never keeps | Your Navidrome token and salt, once, at sign-in. |
| Keeps in memory only | What each person plays now (forgotten after 90 seconds without an update, and on restart), who listens along with whom. |
| Keeps on disk | User names (from Navidrome), who follows or blocked whom, chat messages and replies (30 days), reactions (as received), each person's profile summary (numbers and the top five artist and genre names) and the picture they chose (a small PNG). |
| Never has | Passwords, your music library, file names, IP addresses in the database. |

How it protects you:

- **No passwords, ever.** Logins are replayed to your own Navidrome and forgotten. Sessions are signed tokens
  (HMAC-SHA256) that expire after 12 hours. The signing key is created on first start in the data folder (owner-only)
  and is not part of this source code.
- **Profile summaries** are limited (five entries, short names, no control characters) and removed when sharing is switched off.
- **Rate limits** on wrong logins (per address and per user) and on chat; length limits and control-character checks on
  every text; request bodies are capped at 16 KB; the WebSocket accepts nothing from clients.
- **Opt-out and blocking** are immediate and two-way, and a blocked person is not told.
- It runs as an unprivileged user, ideally behind HTTPS and a firewall that exposes only ports 80 and 443.
- Reporting a weakness: see [SECURITY.md](SECURITY.md) (please use GitHub's private reporting, not a public issue).

Good to know: anyone with a Navidrome account on your server can use the service, and the other people on it can see
what they play (unless they switch sharing off). Chat is one room for all of them. It is meant for people who already
share a server and trust each other, such as family and friends.

## Updates

SoundEra Activity never replaces itself: that is left to you, systemd and Docker, because a network service that
overwrites its own program from the internet would undo the protection the service file and the container give it. What
it does for you:

- **It tells you when there is a newer version.** At start and once a day it asks GitHub for the newest release (one
  request, nothing is downloaded) and writes one line to the log when there is something newer, for example
  `a newer version is available: v0.3.0 (this is 0.2.0)`. Look with `journalctl -u soundera-activity | grep newer`, or ask
  any time with `soundera-activity --check-update`. To stop it contacting GitHub, set `UPDATE_CHECK=off`.
- **Every release file is signed** with the project's [minisign](https://jedisct1.github.io/minisign/) key, and the
  signed text names the exact version and file, so an old genuine file cannot be passed off as a new one. The public
  key (also built into the program) is:

  ```
  RWSxvwYiiiQcQQMP9+Pl6QpJi5DVfg8ibrI1Ms4Y2eOx6qD1/xQ8ta4/
  ```

- **`deploy/update.sh` updates a program installation safely.** It needs only `curl`, `tar` and `sha256sum`:

  ```bash
  deploy/update.sh --check      # just say whether a newer version exists
  sudo deploy/update.sh         # download, verify the signature, install, restart
  ```

  It finds the newest version, downloads the tarball and its signature, has the program you already have installed
  check the signature (`soundera-activity --verify FILE SIGNATURE`), refuses anything that does not match, then installs
  the new program next to the old one and renames it into place (so a failure never leaves half a program) and
  restarts the systemd service.
- **Docker:** `docker compose pull && docker compose up -d`. To update automatically, run a tool such as Watchtower
  yourself; the image is rebuilt for every release.
- **By hand:** download the tarball and its `.minisig`, run `soundera-activity --verify FILE FILE.minisig`, then replace
  the program and `systemctl restart soundera-activity`. If you have minisign: `minisign -Vm FILE -P <key above>`.

Updating is safe at any time: the database upgrades itself, a restart only clears who is playing right now (everyone's
SoundEra sends it again within 15 seconds and signs in again by itself), and the protocol is versioned under
`/activity/v1`, so a SoundEra and a service that are one version apart keep working while people update at different
times.

## Backups

- **Back up** the data folder (`/var/lib/soundera-activity`, or the Docker volume). Losing it only loses follows,
  blocks and chat history; keep `session.key` if you want people to stay signed in across a restore (otherwise they
  just sign in again automatically).
- **Move to another machine** by copying the data folder.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| SoundEra says *The activity service could not be used: the activity service could not be reached* | Wrong address, the proxy is down, or port 443 is closed. Try `curl https://your-address/activity/v1/health`. |
| ...*did not accept the login* | Navidrome refused it: wrong `NAVIDROME_URL` in the service (check it from the same machine with `curl`), or the user does not exist. Five wrong tries lock that user and address for a minute. |
| ...*the music server could not be reached* (HTTP 502 in the log) | The service cannot reach Navidrome at `NAVIDROME_URL`. Inside Docker, `127.0.0.1` is the container itself: use the container name or the host address. |
| SoundEra says *Use https for the activity service unless it is on your own network* | You typed an `http://` address that is not on your own network. Set up HTTPS (above). |
| Nobody shows up in Online activity | Only people who are playing a song from the server show, and only if they switched sharing on and their SoundEra has the service address set. |
| Chat or follows disappeared after a restart | The data folder was not kept (Docker without a volume). Presence is memory-only by design; follows, blocks and chat are on disk. |
| WebSocket drops behind nginx | Use the `Upgrade` headers in `deploy/nginx.conf`. |

## API

Everything is under `/activity/v1`. Bodies are JSON. After signing in, send `Authorization: Bearer <session>`. Errors
are `{"error": "..."}` with a fitting status code.

| Call | What |
|---|---|
| `GET /health` | No login. `{"status":"ok","version":"..."}`. |
| `POST /login` `{user, token, salt}` or `{user, password}` | Navidrome confirms it; returns `{session, expires_at}`. 401 if refused, 429 after too many wrong tries, 502 if Navidrome is unreachable. |
| `GET /me`, `PUT /me/settings` `{share_presence}` | Your state. Switching sharing off removes your presence at once. |
| `PUT /presence`, `DELETE /presence`, `GET /presence` | Send what you play (every 15 s; an entry expires after 90 s), end it, read everyone else's. |
| `GET /follows`, `PUT/DELETE /follows/{user}`, `GET /feed` | Who you follow and who follows you; follow or stop; what the people you follow play now. |
| `POST /reactions` `{to, song_id, kind}`, `GET /reactions` | `kind` is `like`, `fire`, `sad` or `laugh`. The person must be playing `song_id` now (409 otherwise). The inbox lists the last 7 days. |
| `GET /chat?after=ID&limit=N`, `POST /chat` `{body, reply_to?}`, `DELETE /chat/{id}` | The room. `reply_to` is the id of the message answered (400 if it is gone); a reply carries `reply: {id, from, body}` with the first 120 characters (`from` and `body` are `null` once the original was deleted). Only the writer can delete (404 otherwise). |
| `PUT /avatar` (the image as the body), `DELETE /avatar`, `GET /avatar/{user}` | Set, remove and read profile pictures. Only PNG and JPEG are accepted (400 otherwise, 413 over 4 MB); the image is decoded with limits, cropped to the middle square and stored as a 256 px PNG. A person's picture is hidden (404) from someone who blocked them or whom they blocked. |
| `PUT /profile` `{plays, liked, playlists, top_artists[], top_genres[]}`, `GET /profile/{user}` | Share your own summary (at most five `{name, plays}` entries each; 403 while sharing is off) and read someone else's: `{user, summary, updated_at_ms, followers, following, you_follow, playing_now}`. 404 when they share nothing, or when either of you blocked the other. |
| `PUT/DELETE /blocks/{user}`, `GET /blocks` | Block, unblock, list. |
| `PUT /listen/{host}`, `DELETE /listen`, `GET /listen` | Listen along with someone playing now (returns their presence); leave; who you follow along and who follows along with you. |
| `GET /ws?session=TOKEN` | WebSocket with live events (below). |

Example, with a made-up user:

```bash
A=https://activity.example.com/activity/v1
SESSION=$(curl -s -X POST $A/login -H 'content-type: application/json' \
  -d '{"user":"alice","password":"..."}' | jq -r .session)
curl -s -X PUT $A/presence -H "authorization: Bearer $SESSION" -H 'content-type: application/json' \
  -d '{"song_id":"s1","title":"Example","artist":"Made Up","position_ms":1000,"duration_ms":200000,"state":"playing","device":"Desktop"}'
curl -s $A/presence -H "authorization: Bearer $SESSION"
```

**WebSocket events** (JSON, one per message; you never get an echo of your own actions): `presence`
`{entry}` · `presence_ended` `{user}` · `reaction` `{from,to,song_id,kind}` (only to the person it is for) · `chat`
`{message}` (with its `reply`) · `chat_deleted` `{id}` · `listener_joined` / `listener_left` `{host,listener}` (to the host) · `listen_ended`
`{host,listeners}` (to the people listening along). Blocked people's events are never delivered.

## Developing

```bash
cargo test --workspace          # unit tests and integration tests against a fake Navidrome (made-up users only)
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace has two crates: `activity-core` (protocol types, session tokens, presence and chat rules; no network) and
`activity-server` (the HTTP and WebSocket service). [PLAN.md](PLAN.md) holds the design notes. Releases are made by
pushing a tag such as `v0.2.0`: the workflow builds the static program, a checksum file and the Docker image.

Contributions are welcome under the same licence.

## Licence

Copyright (C) 2026 ennuiLife. SoundEra Activity is free software: you can use, host, study, change and share it under
the terms of the [GNU Affero General Public License](LICENSE), version 3 or (at your option) any later version. If you
run a modified version as a service for other people, the AGPL asks you to offer them its source too.

It is a separate program from the SoundEra player, which talks to it over HTTP.
