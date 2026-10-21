# Deploying quiz-forge

One binary, one SQLite file, one media directory. No database server and no
Node. It runs comfortably on a small VPS.

## 1. Telegram

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`). Keep the token.
2. In BotFather, `/setprivacy` → *Disable*, so the bot sees `/add` in the group.
   (It ignores ordinary group chat.)
3. Add the bot to your quiz group.
4. Find the group's chat id: send any message in the group, then open
   `https://api.telegram.org/bot<TOKEN>/getUpdates` and look for
   `"chat":{"id":-100…`. That negative number is `TELEGRAM_GROUP_ID`.
   (Stop quiz-forge while you do this; only one process can poll for updates.)

Everyone in the group is a member of the bank. Leaving the group ends their
access; their questions stay.

## 2. The binary

Either download the `quiz-forge-x86_64-linux-musl` artifact from the
*Build release binary* workflow (static, runs on any x86-64 Linux), or build it:

```bash
cargo build --release                                          # for this machine
cargo build --release --target x86_64-unknown-linux-musl       # static
```

## 3. Configure and run

```bash
mkdir -p /opt/quiz-forge && cd /opt/quiz-forge
cp /path/to/quiz-forge .  && cp /path/to/repo/.env.example .env
$EDITOR .env               # token, group id, PUBLIC_BASE_URL, LISTEN, DATA_DIR
./quiz-forge check-config
./quiz-forge serve
```

The database is created and migrated on start. For a service, use
`deploy/quiz-forge.service` (systemd, restarts on failure and after reboots)
or the image from `deploy/Dockerfile`.

**Keep it off the open internet except through the proxy.** The default
`LISTEN=127.0.0.1:8090` is reachable only locally. If you run the container
image, publish the port as `127.0.0.1:8090:8090`, never a bare `8090:8090`
(Docker's port publishing bypasses host firewalls).

## 4. HTTPS

Put a reverse proxy with TLS in front; `deploy/Caddyfile.example` has two
variants: proxying to a local port, or to a Unix socket
(`LISTEN=unix:/path/quiz.sock`) for a proxy running in a container that shares
only that socket's directory. Set `PUBLIC_BASE_URL` to the https address: login
links use it, and with an `https://` base the session cookie is marked Secure
and vote cards get an "Open" button.

## 5. Sign in

Send `/web` to the bot in a private chat. It replies with a one-time link,
valid for ten minutes.

## 6. Backups

```bash
./quiz-forge backup /var/backups/quiz-forge
```

writes a consistent copy of the database (`quiz-<timestamp>.db`, via
`VACUUM INTO`, safe while running) and copies any new media files. Run it from
cron and ship the directory off the machine with whatever you use (rclone,
rsync, restic). To restore: stop the service, put a backup `.db` in place as
`DATA_DIR/quiz.db`, copy `media/` back, start.

## 7. The AI review pass (optional)

Set `AGENT_ENABLED=true` and `ANTHROPIC_API_KEY`. Every `AGENT_INTERVAL_HOURS`
it reviews up to `AGENT_MAX_QUESTIONS_PER_RUN` questions whose current version
hasn't been reviewed, stopping early at `AGENT_MAX_COST_USD_PER_RUN`. Question
text goes to Anthropic's API; images only with `AGENT_SEND_MEDIA=true`. What it
does with the answers is in `docs/DESIGN.md` §7: only typo fixes that pass a
deterministic check are applied directly; everything else becomes a proposal
the author decides on.

## Upgrading

Stop, replace the binary, start. Migrations run automatically; take a backup
first.
