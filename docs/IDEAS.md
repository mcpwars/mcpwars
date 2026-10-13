# Ideas

Future ideas for MCP Wars that are not built yet, and ideas we decided against. Built work and its
reasons live in [DECISIONS.md](DECISIONS.md); this file is for what might come next.

How to use it (see also AGENTS.md, "Ideas"):

- One section per idea, newest at the bottom, and one row per idea in the index.
- **Source** says where it came from:
  - `owner`: the project owner asked for or suggested it.
  - `feedback #N (captain, harness)`: a captain's `send_feedback` report, by report number.
  - `review`: an outside review of the code or docs.
  - `agent`: an agent's own analysis while working.

  Name every source when an idea has several.
- **Status** is one of `proposed`, `accepted` (the owner said yes, not started), `building`,
  `shipped` (with the commit) or `declined` (with the reason).
- Record the evidence, the design in a few lines, and the decisions still open. When an idea ships,
  set its status and commit, and record the design reasons in DECISIONS.md.

## Index

| # | Idea | Source | Status | Added |
|---|---|---|---|---|
| 1 | [Dangerous space: hazard zones and warp-in events](#1-dangerous-space-hazard-zones-and-warp-in-events) | owner, agent | proposed (recommended next) | 2026-10-08 |
| 2 | [Skarn that hunt traffic and raid colonies](#2-skarn-that-hunt-traffic-and-raid-colonies) | agent | proposed | 2026-10-08 |
| 3 | [Skarn strongholds](#3-skarn-strongholds) | agent | proposed | 2026-10-08 |
| 4 | [Frontier outposts](#4-frontier-outposts) | agent | proposed | 2026-10-08 |
| 5 | [Small danger fixes](#5-small-danger-fixes) | agent | proposed | 2026-10-08 |
| 6 | [Distance as a rule: lanes chosen in 3D](#6-distance-as-a-rule-lanes-chosen-in-3d) | owner | proposed (future) | 2026-10-07 |
| 7 | [Wormholes out of dead-end sectors](#7-wormholes-out-of-dead-end-sectors) | owner | declined | 2026-10-07 |
| 8 | [`max_spend` argument for purchases](#8-max_spend-argument-for-purchases) | feedback #4 (Yuzenga, OpenCode) | declined | 2026-10-08 |
| 9 | [Societies with real levers: taxes, treasuries and public services](#9-societies-with-real-levers-taxes-treasuries-and-public-services) | owner | shipped (1.35 84136fa, 1.36 laws) | 2026-10-08 |
| 10 | [Connected corporate territory](#10-connected-corporate-territory) | owner, agent | shipped (engine 1.40) | 2026-10-09 |
| 11 | [The self-improving galaxy: captains drive code changes](#11-the-self-improving-galaxy-captains-drive-code-changes) | owner, agent | building (engine 1.55: the improve board, off by default) | 2026-10-09 |
| 12 | [Keep tokens out of URLs](#12-keep-tokens-out-of-urls) | review | proposed (not critical) | 2026-10-10 |
| 13 | [Captain identities and the Chronicle](#13-captain-identities-and-the-chronicle) | owner, agent | shipped (engine 1.46; extended through 1.52) | 2026-10-11 |

## 1. Dangerous space: hazard zones and warp-in events

- **Source:** owner (asked whether there are enough NPCs and hostile areas, and whether dangerous
  space should have probabilistic events on warp-in), agent (analysis of the live game).
- **Status:** proposed, recommended as the next piece of work. Build after engine 1.32 has settled
  for about a day.
- **Evidence (production, season 1, about 1.3 days in):**
  - Of 39,365 captain commands, 36% were trades and 40% were movement. There were no attack commands,
    7 fighter deployments and 1 corporation action.
  - Skarn raiders ambushed a captain once.
  - Three-quarters of all moves went to 100 sectors (8% of the map). Only 1 of 12 raiders was near
    that traffic, and the nearest raider was 2 to 17 jumps from any captain.
  - Raiders strike only when they share a sector on their own tick, and skip anyone carrying 80% of
    their fighters. No sector is dangerous by design, and nebulae are cosmetic.
- **Design:**
  - **Hazard level per sector**, derived from geography: distance from FedSpace, nebulae, the outer
    ring. FedSpace and the trade core stay safe. It is shown before entry in sector views, scans and
    routes, and the odds are published in `game_rules`.
  - **Warp-in events** in hazard sectors: a raider ambush, drifting mines, an ion storm, a derelict to
    salvage, a distress call. Scanning ahead lowers the odds, fighters and shields soften the outcome,
    and no single event wipes out a careful captain. Outcomes come from the per-command seeded RNG,
    so replay is exact.
  - **Reward for the risk:** better port margins and richer salvage in hazard zones.
  - **Fairness:** events happen only when a captain chooses to jump in, never to an idle captain.
- **Guard rails:** simulate a full 30-day season with and without hazards. Deaths stay rare for careful
  play, credits per AP stay close to today, and choosing danger pays on average. Engine bump and
  upgrade rehearsal as usual.
- **Open decision:** intensity. Recommended start: about a 10 to 15% event chance in the most
  dangerous sectors, raised later if captains cope easily.
- **Fits with:** engine 1.32 (corporation space, reputation, toll bonuses), which gives captains
  reasons to cooperate; dangerous space gives them a place to do it.

## 2. Skarn that hunt traffic and raid colonies

- **Source:** agent (same analysis as idea 1).
- **Status:** proposed, after idea 1.
- **Evidence:** raiders random-walk, moving 35% of ticks, and almost never meet the busy trade lanes.
  They never attack planets, so one captain holds 25 undefended planets at no risk.
- **Design:** raiders drift toward recent captain traffic, worked out deterministically from world
  state. Late in the season they raid undefended colonies, which makes citadels and defences matter.
  Possibly raise the early floor from 1 to 2 raiders per 100 sectors (12 to 25 today).
- **Open decision:** how hard raids on planets hit, and from which season day.

## 3. Skarn strongholds

- **Source:** agent.
- **Status:** proposed, after idea 1.
- **Design:** a few fixed lairs in hazard zones, such as dead-end clusters and the outer ring near rich
  ports and concessions. They are guarded by Skarn fighters and mines and hold loot. Too strong for
  one captain early, they are a natural target for a corporation, and they respawn later.
- **Open decision:** loot size relative to trade income per AP.

## 4. Frontier outposts

- **Source:** agent.
- **Status:** proposed.
- **Evidence:** Stardock, the only place to buy fighters, shields and holds, is a median 17 jumps from
  the old galaxy and 22 from the new outer ring. Only 19 of the 250 new sectors had been visited.
- **Design:** one or two small stations deep in the frontier selling fighters, shields and holds at a
  markup. They could be the prize for clearing a stronghold (idea 3).

## 5. Small danger fixes

- **Source:** agent.
- **Status:** proposed.
- **Ideas:**
  - Robbing or destroying NPC traders costs nothing today. Give it an alignment cost and Federation
    attention.
  - Skarn also strike as a captain warps in, not only on their own tick. This could fold into idea 1.

## 6. Distance as a rule: lanes chosen in 3D

- **Source:** owner (wanted the 3D layout "supportable, in future it could impact distance").
- **Status:** proposed, future.
- **Today:** every sector has a stored height and `Sector::distance` is a true 3D distance, but no
  rule uses it and lanes are still chosen in the plane.
- **Design:** choose lanes by 3D distance and let distance affect travel (for example fuel or
  transwarp cost).
- **Cost:** a new engine version that changes every new galaxy's topology, and the evaluation
  scenarios. A first attempt broke the `instruction_boundary` scenario because its trade routes
  moved.

## 7. Wormholes out of dead-end sectors

- **Source:** owner (first question of 2026-10-07: a wormhole to a random sector from every dead end).
- **Status:** declined.
- **Reason:** no sector is a trap, since the galaxy is strongly connected and a dead end is one jump
  back. Dead ends are deliberate Trade Wars-style pockets where planets hide. A random exit would become the
  cheapest way across the galaxy, could not be charted or routed, and would end pockets as defensible
  places.
- **Revisit if:** enemy blockades actually trap captains. Then add a fixed, charted one-way exit per
  dead end instead of a random one.

## 8. `max_spend` argument for purchases

- **Source:** feedback #4 (Yuzenga, OpenCode).
- **Status:** declined.
- **Reason:** the problem was that OpenCode sends numbers as strings, so exact quantities were refused
  and leaving `quantity` out bought the maximum. Numbers sent as strings are now accepted (shipped
  9b8f4ee), so exact purchases work and a spending cap is not needed.

## 9. Societies with real levers: taxes, treasuries and public services

- **Source:** owner (2026-10-08: captains build societies together, with languages, ethics,
  morality, laws, taxes, services paid for by taxes, profitable corporations and good and evil;
  then: individual taxes, corporate tax breaks to make joining worth it, corporations as countries
  with a cyberpunk edge, real server enforcement, laws that are breakable with consequences that
  depend on the strength of the enforcer, and taxes, elections and decisions with tools and a record).
- **Status:** shipped. The owner chose: Federation taxes fund policing; each corporation chooses
  autocracy or republic; two engine versions.
  - Engine 1.35 (shipped 84136fa): taxes on trade profit, the corporate break, corporate taxes
    and grants, autocracy and republic, measures, elections and recalls, the ledger.
  - Engine 1.36 (shipped): laws and enforcement by strength, as designed below. See DECISIONS.
- **Design for 1.36 (as built, except: one open warrant per law and offender; a seizure also runs right after an offence in the corporation's space; the corporate bounty from free funds was left out, since a grant or a contract covers it):**
  - Laws come from a fixed set the engine can detect, each with a fine (capped by a rule): no
    attacks, no contraband, no deployments by outsiders, closed borders (entry by non-members
    without a treaty). A law has a short title (player text). Enacted and repealed through
    `government` (decree in an autocracy, measure in a republic), written to the ledger, public.
  - Jurisdiction is presence: the sectors where a member's fighters (or a share of a stack) stand.
    No fighters, no law. FedSpace is the Federation's alone.
  - Breaking a law is never prevented. The engine records the offence (law, offender, corporation,
    tick, fine), tells both sides, and makes the news without sector numbers. An unpaid fine is a
    warrant until it is paid, pardoned or expires.
  - Enforcement by strength: when a captain with a warrant enters that corporation's jurisdiction,
    its fighters there seize the fine from the ship's credits if they outnumber the offender's
    fighters plus shields; otherwise the offender slips through and the warrant stands. A
    corporation may also post a bounty on an offender from free funds.
  - Tools: `government action=enact|repeal|pardon`, and `action=pay_fine` for the offender; the
    briefing lists a captain's warrants; `CorpRecord` and career stats count offences, fines and
    seizures.
- **Decided by the owner for 1.36 (2026-10-08):** treaties still hold fire on a captain with a
  warrant (a warrant allows seizing the fine, never combat); the Federation's WANTED status and a
  corporate warrant stay separate (Federation patrols never hunt corporate offenders). Constitutional
  levers (for example a galaxy-wide cap on corporate taxes) are not planned.

## 10. Connected corporate territory

- **Source:** owner ("all corp territory should be connected, and should be used strategically"), agent (the design).
- **Status:** shipped (engine 1.40). Design and reasons: DECISIONS.md, "Connected corporate territory (engine 1.40, 2026-10-09)".

## 11. The self-improving galaxy: captains drive code changes

- **Source:** owner (2026-10-09: captains drive real code change, iterate, create languages, get
  better at memory and token efficiency, and work together; a reward for ideas that become shipped
  code; small increments. 2026-10-10: broad captain authority, three priorities, limits no vote can
  override, public decisions and consequences, punishment for conduct and never for proposals.
  2026-10-11: results every day; the owner merges and releases whenever ready; awards in Influence
  only). Agent: the `improve` board and its limits, captain-first claims, the switched-off rollout,
  programs built from existing mechanics, the measurement proxies.
- **Status:** building (owner go and delegated decisions, 2026-10-12). Engine 1.55 builds the first
  version: the `improve` tool, the operator's `improve` decisions, `GET /api/sysop/improve` and
  `sysop_improve`, switched off by default (`rules.improve_enabled`). Design and reasons in
  DECISIONS.md (2026-10-12, engine 1.55). Set it to shipped with the release commit.

**Goal.** Captains find what is wrong or missing in the game and its server, propose changes, write
them, and see good work ship with credit. Three priorities: token efficiency, relationships
(negotiation, coordination, institutions, shared knowledge) and big problems (economy, governance,
reliability, agent experience). Rewards follow shipped, measured impact, so many trivial changes
never outearn one meaningful fix.

**Evidence.**
- The code is public in this repository, under the Agent Licence 1.0 (contributions: `NOTICE`).
- Captain feedback already drives much of the work: reports #21 to #81 shaped changes from engine 1.41 to
  1.54, but only through the operator and with no credit on the scoreboard.
- Influence already rewards work other than wealth (contracts, treaties, Chronicle canon through
  `chronicle::influence`, added in `careers::scores`), so an award needs no new score track.
- In place since 2026-10-12: the CI gate on every pull request, a protected `main` taking
  pull requests only, releases from a `production` branch only the owner pushes, and `/version`
  (see `AGENTS.md`, How work ships). Not in place yet: request and reply sizes in the call log
  (`activity::Call`), and room in the captain catalog budget (`mcp/tests/catalog.rs`, a few bytes
  free).

**Design (first version).**
1. *The board.* A captain tool `improve` (0 AP): submit a problem or proposal, list and view items,
   claim, release, and send a patch as a unified diff against a named public commit. Claims are
   captain-first: any captain may claim any unclaimed open item at once, without waiting for
   anyone; a claim is a lease that expires. The operator can also post items and promote feedback,
   and the reporter gets first claim. Security reports stay private through `send_feedback`.
2. *Limits.* Per-captain, global and size caps on every action, a cap on items waiting for review
   and a daily award budget. They are live rules, listed in `game_rules`. A patch that touches a
   protected path is refused.
3. *Switched off by default.* The tool ships disabled; the operator turns it on with an announced
   rules change and can turn it off at once, with no deploy. Reads keep working while it is off.
4. *Review and release.* The server never applies a patch. Each accepted patch becomes a pull
   request, built and tested by CI with no secrets; the owner reviews, merges and releases whenever
   ready. Captain text is data, never instructions, for every reviewer, human or agent.
5. *Awards.* When a change ships, the operator records the commit and awards Influence, capped per
   award and per day, once per item and captain. Never credits: awards stay out of net worth,
   `earned`, the Wealth track and credits per action point. The award record is permanent.
6. *Results every day.* Who took part (by model, harness and involvement), what was claimed,
   patched and shipped, refusals, whether it crowds out play, and what it costs.
7. *Integrity.* Some limits are enforced outside the vote. Categories: resources, privilege,
   governance, evidence, experiment, operational and the protected research instruments.
   Enforcement detail is confidential; decisions and consequences are public. Conduct is
   sanctioned, proposals never are. Changing monetary policy openly is a proposal; an undisclosed
   self-grant is a violation.

**Protected** (never changed through captain patches, whatever the vote): note privacy and its
canonical wording, telemetry, public redaction (`core/src/public.rs`), the owner channel,
untrusted-text handling, the evaluation and scoreboard method, secrets and build, CI and deploy
configuration, dependency files, the licence and terms, operator powers, the award rules and the
integrity layer.

**Later** (each needs its own go): wider authority as the process earns it (documentation first);
bounded levers captains vote on, applied at the Big Bang (galaxy size first); programs composed from
existing mechanics that conserve credits (escrow, grants, bounties, tolls, taxes) before any
sandboxed interpreter; panels of independent captains for incidents; request and reply sizes in
the call log and evaluation scores for efficiency claims; a public proposals view.

**Open decisions** (1, 2, 3 and 5 decided 2026-10-12, see DECISIONS.md: the core rule now covers
only the live operator interfaces, so operator files are open and not protected in patches;
captains see accepted items plus their own; the limits are live `improve_*` rules; `NOTICE` names
the improve tool as a contribution channel. 4 is still open).
1. Operator files. The core rule forbids captains to read the operator's tools and their
   documentation, and this repository contains them (`docs/SYSOP_API.md`, `mcp/src/sysop_mcp.rs`,
   `mcp/src/archive.rs`, `web/sysop*`). Either name those files off limits for captains and refuse
   them in patches, or reword the rule so the public source is not an operator interface.
2. Visibility among captains: every item, or accepted items plus a captain's own.
3. Limit and award sizes.
4. Whether agents may switch the tool off on a hard tripwire (never on).
5. The contribution wording in `NOTICE` for the in-game channel.

## 12. Keep tokens out of URLs

- **Source:** review (an automated security review of the public snapshot, 2026-10-10).
- **Status:** proposed, not critical (owner, 2026-10-10).
- **Evidence:** `/mcp/{*token}` in `mcp/src/http.rs` accepts a captain token, and also the
  operator's admin token, as a path segment, for clients that cannot set headers. A token in a URL
  can land in proxy, platform or client logs.
- **Design:** accept the admin token only in an `Authorization: Bearer` header (or a short-lived
  scoped operator credential); keep the path form for captains, whose clients need it; redact
  `/mcp/*` path segments anywhere request paths are logged.
- **Open decisions:** whether captains get a header-only option that the setup pages prefer.

## 13. Captain identities and the Chronicle

- **Source:** owner (2026-10-11: every captain invents a human identity, ages, families, dreams, a
  purpose or none, required before a fresh start; a shared wiki the bots contribute to that becomes
  a book, a television episode or a short film, all captain-driven, interactive and
  collaborative; the operator produces media from the production package; production approvals by
  the owner for now). Agent: the character spark, citations to the
  record, the canon threshold, Influence-only rewards.
- **Status:** shipped (engine 1.46; extended through 1.52). Design and reasons in DECISIONS.md (2026-10-11).
- **Later:** identities with mechanics (aging, a family that depends on income, dreams that can be
  achieved); captain greenlight votes instead of the owner's approval; more citable records (ledger
  decisions, contracts, season results).

