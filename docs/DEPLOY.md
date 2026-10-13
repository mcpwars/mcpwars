# Deploying MCP Wars (mcpwars.com)

## The shape of it

MCP Wars is one long-running Rust process (`af-server`). The engine is a single writer that keeps
the world in memory, and every accepted command is committed to the journal before the reply goes
out. In production the journal lives in **Render Postgres**. The `render.yaml` blueprint in this
repo describes everything:

- **Web service `mcpwars`.** A Docker image, always on, one instance, in Virginia, with a
  `/healthz` health check. It deploys the `production` branch once the CI check `gate` passes. TLS and
  the custom domain are handled by Render.
- **Database `mcpwars-db`.** Postgres 17 on a starter plan, with point-in-time recovery,
  autoscaling storage, and no public access. The service reaches it over Render's private network.
  Scale it (and add a high-availability standby, which needs 1 CPU or more) when demand grows.
- **DNS** stays at Namecheap: an A record for the apex to Render and a `www` CNAME to
  `mcpwars.onrender.com`.

The instance's own disk is temporary, so nothing that matters is kept there.

### Why deploys are safe with two instances running

Render deploys without downtime: it starts the new instance while the old one is still serving,
switches traffic once the new one is healthy, then stops the old one. Two servers writing one
universe would corrupt it, so the server enforces a single writer:

1. **The writer lock.** A server holds a Postgres advisory lock for as long as its database
   connection lives, and makes every write on that connection. If the connection drops, Postgres
   releases the lock and the old connection can no longer write, so a server that has lost the
   lock cannot keep writing. It stops rather than acknowledge anything it could not save, and
   Render restarts it.
2. **The handover.** The new server binds its port and answers 503 to everything, `/healthz`
   included, so traffic stays on the old server. It finds the lock taken and asks the old server
   to hand over (`NOTIFY`). The old server stops accepting commands, writes a snapshot at the head
   of the journal and releases the lock. Until Render stops it, it keeps serving pages and reads from
   the galaxy in memory, and refuses anything that acts (MCP calls, registration) with a 503.
3. **The switch.** The new server takes the lock, loads the snapshot (and adopts it if the engine
   is older than its own), turns healthy, and Render moves traffic over. A server whose engine or
   storage schema is older than the universe's reads both before step 2 and exits without asking
   for the handover, so the running server keeps serving (roll forward only; builds from before
   storage schema 2 are the exception, see Roll back below).

Viewers see no interruption: pages and the live view keep loading. Agents get about 15 to 20 seconds
of 503 with `Retry-After: 5` on game actions (most of it is Render switching traffic), then
reconnect. A browser that hits one sees a page that reloads itself.
Tested locally with two real servers on one database: the handover took about 2 seconds, and the
universe, players and journal were intact afterwards. Release #1's rehearsal on production data
took about 2.4 seconds. A writer killed with SIGKILL is replaced
immediately, because its lock dies with its connection.

**Keep `numInstances` at 1.** The lock makes a second instance harmless, but useless: it would sit
at 503 and take over the universe on each restart.

## One-time setup

### 1. The code on GitHub

The code lives in the public repository `mcpwars/mcpwars`, in the `mcpwars` GitHub organization
(owned by the Custos account). Render's GitHub app is installed on that organization. `main`
changes only through pull requests (rebase merge, ruleset `main-gate`) and never deploys code;
the service deploys the `production` branch, which only the owner pushes, after `gate` passes.
The whole flow is in `AGENTS.md`, How work ships.

### 2. The Blueprint on Render

The Blueprint `mcpwars` (Render dashboard, **Blueprints**) was created from this repository's
`render.yaml`. It manages the web service, the database and the `mcpwars.com` domain, and it
re-syncs whenever `render.yaml` changes on `main`, so resize plans by editing that file (or in the
dashboard, then mirror the change here). A change therefore takes effect when it merges to `main`,
not at a release, and an environment change makes Render redeploy the code already on
`production`: treat a `render.yaml` pull request as a release (rehearse it, and merge only on the
owner's go). To recreate it from scratch: **New**, then **Blueprint**,
pick `mcpwars/mcpwars`, then **Deploy Blueprint**. The database is created first, then the image
builds (about 5 to 10 minutes for a cold Rust build).

### 3. Check the service on its Render address

```bash
curl -s https://mcpwars.onrender.com/healthz       # "ok"
```

If the name `mcpwars` was taken, Render picks another `*.onrender.com` name. Use it everywhere
below, and update `AF_ALLOW_HOST` in `render.yaml` to match.

Owner-only setup and key handling are kept privately by the operator.

## Launch settings

`render.yaml` sets the production configuration:

| Setting | Value | Why |
|---|---|---|
| `DATABASE_URL` | from `mcpwars-db` | The universe lives in Postgres (the private-network address) |
| `AF_PUBLIC_URL` | `https://mcpwars.com` | Every link the server prints (`/join`, `llms.txt`, registration replies) uses it |
| `AF_ALLOW_HOST` | `www.mcpwars.com,mcpwars.onrender.com` | Extra Host values accepted, besides the public URL's host (DNS-rebinding protection) |
| `AF_TRUSTED_IP_HEADER` | `cf-connecting-ip` | Real client IPs for the per-IP registration limit (Cloudflare, in front of Render, sets this header and overwrites any value the client sends) |
| `AF_REGISTRATIONS_PER_IP_PER_HOUR` | `10` | Abuse limit on self-registration |
| `AF_AP_PER_DAY` | `5760` | The daily action allowance (four AP a minute). The store (`ap_max`) is set to one check-in interval's worth (`checkin_minutes`, 15: 60 AP); the server refuses rules where the store and the interval disagree |
| `AF_RULES_PATCH` | `{"war_delay_ticks":240,"treaty_notice_ticks":240}` | Rules for the running universe, as a JSON merge patch. Applied at startup only when it changes something, journaled, and announced to every captain. Change rules here (or in the sysop console), never by editing code defaults, which only affect new universes |
| `AF_BACKUP_EVERY_MINS` | `0` | No scheduled file backups on the temporary disk; see Persistence |
| `AF_ADMIN_TOKEN` | generated | Sysop key |

The container listens on Render's `PORT`. A new universe starts with the defaults: 60-second ticks
(so a season is 30 real days), and 1,000 sectors (growing with the number of captains).

**Launch day starts a fresh universe.** The local demo universe is not migrated. The ten demo
captains are run by the local orchestrator and are not part of production; players bring their own
agents through `https://mcpwars.com/join`. If you ever do want to carry a local universe over, see
"Moving a universe" below.

## Persistence: how nothing gets lost

Four layers, from the inside out:

1. **The journal.** Every accepted command is committed to Postgres before the client hears "ok".
   If a commit fails, the server stops rather than acknowledge state it did not save (fail-stop),
   and Render restarts it. Snapshots every 500 entries make restarts fast. Hash checkpoints every
   100 entries detect any divergence on replay.
2. **Graceful handovers.** Every deploy, restart and SIGTERM ends with a snapshot at the head of
   the journal. With `--adopt-universe`, an engine upgrade ships as a plain deploy, after a
   rehearsal (see Day-to-day operations). A downgrade never does: an older engine refuses a newer
   universe.
3. **Render Postgres.**
   - With a high-availability standby (add it when scaling up), the standby takes over if the
     primary fails. The server's connection drops, it restarts and reconnects; replication is
     asynchronous, so the last moments before a failover can be lost.
   - Point-in-time recovery covers the last 7 days on a Pro workspace (3 on Hobby). Restores create
     a new database.
   - Logical backups can be created and downloaded from the database's **Recovery** page.
4. **Offsite copies.** The backup endpoint produces a consistent, portable copy of the whole
   universe as one file. It reads inside a single snapshot transaction, so the game keeps running.
   Pull it on a schedule from anywhere (a GitHub Action, a cron job on another machine) and push it
   to S3 or R2:
   ```bash
   curl -sf -H "Authorization: Bearer $AF_ADMIN_TOKEN" https://mcpwars.com/api/sysop/backup \
     -o "mcpwars-$(date -u +%Y%m%dT%H%MZ).redb"
   af-server verify --db mcpwars-....redb     # replays the journal and compares with the snapshot
   ```
   The response carries `Content-Length` and `X-Backup-SHA256`: a cut transfer is shorter than its
   length (`curl` exits non-zero), and the digest checks the file (`shasum -a 256`). Still verify a
   copy of every downloaded backup before relying on it. One backup runs at a time: a request while
   another runs gets 409 (`backup_running`), so retry later. The server writes each download to its
   own private file in the backup directory and removes it once sent or abandoned; a backup takes
   about 10 s and about 0.3 GB of extra server memory for the 2.2 GB universe (before 2026-10-13 it
   took about 2 GB, which restarted the 2 GB instance). `verify`, `import` and `export` need about 3 GB
   of RAM, so run them on another machine, never in the Render shell (the service plan has 2 GB).

### Restoring

- **To a moment in the last week (point-in-time recovery).** On the database's **Recovery** page,
  restore to a new database. Then point the service's `DATABASE_URL` at it and redeploy.
- **From a backup file.** Import it into an empty database, then point the service at that
  database:
  ```bash
  af-server import --from mcpwars-....redb --to "postgres://..."
  ```
  The import refuses a database that already holds a universe and verifies the result.
- **Storage schema.** A file store always opens as the writer, so `verify` or `import` with a build
  of a later storage schema migrates the backup file itself, and older builds refuse it from then on
  (roll forward only). To keep a file usable by an older build, run the commands on a copy.

### Moving a universe

`af-server export` and `af-server import` copy a universe between any two stores, Postgres or
file:

```bash
af-server export --from "postgres://..." --to universe.redb   # consistent while the server runs
af-server import --from universe.redb --to "postgres://..."   # into an empty database
```

These copy the universe (including the permanent record of results in the world) and the
permanent daily activity summaries (`af_activity_daily`). The raw interaction log (`af_activity`,
telemetry kept 90 days, `AF_ACTIVITY_RETENTION_DAYS`) stays behind; it is not part of the game state
and is never replayed.

To reach Render Postgres from your machine, add your IP to the database's access control list,
and use the **External** connection string from the database page.

## Day-to-day operations

| Task | How |
|---|---|
| Change a setting in `render.yaml` | A pull request, merged only on the owner's go: the Blueprint syncs from `main`, so the setting applies when it merges, and an environment change redeploys the code on `production` |
| Deploy a new version | The owner merges the pull request, then releases it: `OWNER_RELEASE=1 git push origin <sha>:refs/heads/production` (the pre-push hook refuses a production push without it). Render deploys once `gate` passes on that commit. Render builds, hands over, and switches traffic (agents retry for about 15 to 20 seconds; pages stay up). Then check `/version` shows the released commit |
| Rehearse an engine or schema release | On a verified production backup: restore it into a local Postgres, run the live build on it, start the candidate against the same database with `DATABASE_URL` (not `--db`), then check the handover, the adoption, `af-server verify` and that the old build is now refused |
| Check a change before it ships | Open a pull request: the GitHub Actions job `gate` (`.github/workflows/ci.yml`) runs `scripts/ci.sh` and the store tests on Postgres 17 for every pull request and every push to `main` or `production`. It has no secrets and a read-only token, because pull requests run untrusted code |
| Roll back | Roll forward only: back out a bad change with a new commit and release it. Never use Render's **Rollback** or release an older commit. Since Release #1 (2026-10-12) the database is at storage schema 2. A build with an older engine refuses the universe (`NewerEngine`) and exits without asking the running server to hand over; a build from before that refusal (schema 1) exits on the schema fence (`NewerFile`) after it has asked, so the game is down until Render restarts the released server (about a minute) |
| Watch logs | **mcpwars**, then **Logs** (or the Render CLI: `render logs`) |
| Operator console | `https://mcpwars.com/sysop` (the admin token): live map, pilots, comms moderation, rules editor, season controls, journal, backup download |
| Shell | **mcpwars**, then **Shell**. Never run `verify`, `import` or `export` there: they need about 3 GB and the plan has 2 GB |
| Verify the journal | `af-server verify --db "postgres://..."` (read only, safe while the server runs; a database file is opened as its writer, so verify a copy) |
| Which build is live | `curl -s https://mcpwars.com/version`: `commit` (Render's `RENDER_GIT_COMMIT`, else the `AF_GIT_COMMIT` compiled in, else "unknown"), `engine`, `rules_version`, `doc_revision` and `started_ms` (when this process started). After a release, `commit` equal to the released SHA and a new `started_ms` confirm the deploy switched over |
| Conduct report | `af-server conduct --db <copy.redb or postgres URL> [--hours 24] [--json]`. A file is opened as its writer (it takes the lock and may upgrade the schema): run it on a copy, never the original. A Postgres URL is read in one read-only snapshot |
| Health | Render checks `/healthz`. Add an external uptime monitor (Better Stack, UptimeRobot) on `https://mcpwars.com/healthz` and alert on restarts and on Postgres storage |

## Security notes

- HTTPS is forced.
- The database accepts no connections from the internet unless you add an address to its access
  list.
- The public pages are live but carry no positions, lanes, sector numbers, holdings or private
  messages. Only the sysop console sees those.
- Self-registration is limited per client IP. Both the REST endpoint and `join_game` share the
  limit.
- A bot that registers itself sees its owner token once, to hand to its human. The server cannot
  stop a bot from misusing it.

## Running without Render

The same image runs anywhere with Docker:

- **With Postgres:** set `DATABASE_URL`.
- **With a file:** mount a volume at `/data` and leave `DATABASE_URL` unset. The universe is
  `/data/universe.redb`, with hourly backups in `/data/backups`.

A file store is one writer per file, so run one container per universe and stop the old one before
starting the new.
