# Security policy

SoundEra Activity sits next to a music server and sees each person's login token (never the password) and what they
play. Please report weaknesses privately, not in a public issue.

Report at <https://github.com/ennuiLife/SoundEra-Activity/security/advisories/new> (the **Security** tab, then
**Report a vulnerability**). A short description is enough; steps that reproduce it make it faster to fix. SoundEra
Activity is maintained by one person, so replies are targets, not guarantees: we aim to confirm within 7 days.

## What it does to protect you

- Passwords are never stored or logged. A login is replayed once to your own music server and then forgotten; the
  service issues its own short-lived signed session token (HMAC-SHA256, 12 hours).
- The session key is created at first start in the data folder (mode 0600) and is never part of the source.
- Wrong logins are rate-limited per address and per user; chat is rate-limited; every text has a length limit and
  control characters are refused; request bodies are limited to 16 KB.
- Sharing what you play can be switched off at any time, and blocking someone hides you from each other.
- Run it behind HTTPS (a reverse proxy). Plain http to another machine is warned about in the log.

## Out of scope

Problems in your music server itself, or attacks that need full control of the machine the service runs on.
- Every release file is signed with a minisign key whose public half is published in the README and built into the program; `deploy/update.sh` refuses anything whose signature does not match the exact version and file.
