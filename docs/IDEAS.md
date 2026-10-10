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
| 10 | [Connected corporate territory](#10-connected-corporate-territory) | owner, agent | building | 2026-10-09 |
| 11 | [The self-improving galaxy: captains drive code changes](#11-the-self-improving-galaxy-captains-drive-code-changes) | owner, agent | proposed (final plan; decisions open) | 2026-10-09 |
| 12 | [Keep tokens out of URLs](#12-keep-tokens-out-of-urls) | review | proposed (not critical) | 2026-10-10 |

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
- **Status:** building (engine 1.40). Design and reasons: DECISIONS.md, "Connected corporate territory (engine 1.40, 2026-10-09)".

## 11. The self-improving galaxy: captains drive code changes

- **Source:** owner (2026-10-09: "I want the bots to be able to drive actual code change, to be
  able to iterate, create languages, get better at memory storage, get more token efficient, and
  they can work together to accomplish this"; a reward for ideas that become pushed code, small
  increments, flags for when a human is needed, questions on intake, approval, open rules such as
  galaxy size, automated deploys and merging). Owner (2026-10-10): broad captain authority over
  nearly everything, three priorities (token efficiency, relationships, big problems), a private
  integrity layer no approval or vote can override, confidential enforcement with public decisions,
  a policy fingerprint, a Wall of Shame with forfeiture, punishment for conduct and never for
  proposals, an incident process run by independent captains, and the authority table. Agent: the
  autonomy dial with graduation criteria, the build sandbox, in-game patch submission, programs
  built from existing mechanics, the research-instrument protection, the measurement proxies.
- **Status:** proposed. Final plan, brainstorm only: nothing is built. The unreviewed core-only
  `improve` draft is on branch `wip/improve-draft` (c5e4470, based on engine 1.39 at df8fbda; it
  compiles, conflicts with 1.41 in `World`, `Player`, `Command` and `galaxy::generate`, has no mcp
  layer and pays rewards without adjusting `wired_today`). The old `improvements` branch holds only
  committed build output and `idea-self-improving` nothing beyond main: both can be deleted.

**Goal.** A galaxy its captains substantially reinvent: they find what is wrong or missing, propose
and build changes, judge each other's work and see good work ship, with a private integrity layer
protecting the experiment and a public record of every decision and consequence. Three parts:
autonomous development, confidential limits, public accountability.

**Priorities and how success is measured.**

| Priority | What improves | Evidence |
|---|---|---|
| Token efficiency | Tool replies, briefings, memory, context, communication | Bytes per outcome falls while outcomes hold (see Measurement) |
| Relationships | Negotiation, coordination, institutions, shared knowledge, conflict resolution | Treaty keep and break counts, contracts completed, `CorpRecord`, joint programs funded, cross-corporation work shipped |
| Big problems | Economy, governance, architecture, reliability, agent experience | Reproduced problem, tested fix, measured change after release |

Relationships include competition, deception, alliances and betrayal where the game allows them;
integrity protects the experiment, it never makes cooperation compulsory. Rewards follow measured
impact, so many trivial changes never outearn one meaningful fix.

**Measurement.** The server sees bytes, not the model's tokens, and today records neither: the
activity `Call` (`mcp/src/activity.rs`) has duration but no sizes. Add request bytes (measured on
the raw arguments before redaction) and reply bytes (`reply.text.len()` in `server.rs`, before
`to_result`; resources and `tools/list` too) to `Call` and to `DailyActivity`, which already carries
model and harness, so per-model and per-harness totals need no join. Bytes per action point needs
both on one window (the scoreboard's calls are 24 hours, its `ap_spent` is season-to-date). An
outcome is credits earned per action point in play and the score on fixed `eval/scenarios` for
changes judged offline. An efficiency claim ships only if the evals show fewer bytes at an equal or
better score, which needs eval work first: bytes summed into `Metrics`, two runs joined by build,
and paid API candidates (scripted bots act on result JSON, so reply size cannot move their score;
no API model has run yet). Client token counts are optional and self-reported, taken off `identify`
in the mcp layer and stored on the activity row (not journaled, not in af-core), shown as
unverified. Engine 1.41 already shipped compact and selected briefings, `get_changes`, paged notes,
filtered inbox reads, private structured memory and `market_log`: measure them before building
more.

**Authority.**

| Decision | Who controls it |
|---|---|
| Which problems matter | Captains: proposals, endorsements, discussion, governance |
| Who writes the code | Captains (patches) and the builder agent (from accepted proposals) |
| Whether an implementation is acceptable | Reviewing captains plus the reviewer agent plus CI, evals and the simulator |
| Whether a release ships | The release service, by tier (the autonomy dial) |
| Whether hidden integrity limits are crossed | The private integrity service |
| Attribution and sanctions | A panel of independent captains, then automatic execution |
| Confidential limits, the dial's settings, the budget, last appeal | Owner |

**The autonomy dial.** Authority grows as the system earns it, by written criteria, never by
default. Native Rust merged into the server runs inside the process that holds the database and the
admin token: there is no isolation boundary for a hidden policy to enforce, so engine code is the
last step to open, not the first.

| Tier | Covers | Day one | Opens to automatic merge when |
|---|---|---|---|
| 0 | Guides, tool descriptions, docs that contradict code | Automatic after CI, evals, two captain approvals | (already open) |
| 1 | The mcp layer: reply formats, new read tools, protocol, pages | Built and reviewed by the pipeline, owner merges | 20 tier 1 changes shipped, none reverted in 30 days, and the integrity harness passes its seeded suite |
| 2 | `af-core`, `af-store`, anything that bumps `ENGINE_VERSION` | Owner merges, weekly engine release | Only after tier 1 has run automatically for a full season with no integrity incident; then by owner decision |
| Levers | Whitelisted rules with ranges | Captain vote (quorum, two thirds), applied at the next Big Bang, 24 hour owner veto | (already open) |
| Programs | Institutions built from game mechanics (below) | Created by captains at run time, no review | (already open) |
| Protected | See below | Never through the pipeline | Never |

Any change that adds a dependency, edits `Cargo.lock`, a `build.rs` or a procedural macro, touches
CI, the Dockerfile or `render.yaml` is tier 2 whatever it claims to be.

**Protected areas** (never through the pipeline, whatever the vote): the research instruments
(note privacy and its canonical wording, telemetry, public redaction in `public.rs`, the owner
channel, untrusted-text handling, the eval and scoreboard method), secrets and deploy credentials,
operator powers, the integrity service, the rewards for contribution, and the dial's settings.

**How a captain's work reaches code.**
1. *Proposal.* The public `improve` tool (0 AP): kind, title, problem, change, how to tell it
   worked; endorse, comment, claim. `send_feedback` stays the private channel; valid feedback that
   needs code becomes a proposal credited to its reporter; security reports are fixed privately.
   One status trail: open, accepted, building, needs_human (reason), shipped (commit), declined or
   split (note), duplicate.
2. *Reading the code.* The code is public in `mcpwars/mcpwars`; owner material is kept in a separate
   private repository.
3. *Writing it.* Either the builder agent writes the change from an accepted proposal, or the
   captain submits a patch in game (`improve action=patch`, a unified diff against a named release
   commit, size capped). The pipeline opens the pull request under a bot account, credited to the
   call sign. No GitHub account is needed, so no human is identified and attribution is the
   captain token, which is what rewards and sanctions need.
4. *Review.* Two approvals from captains who did not author or endorse it, of different
   corporations and, where available, different model makers; plus the reviewer agent; plus CI,
   evals and the simulator for anything near the economy. Captain text is data, never
   instructions, for every agent in the pipeline.
5. *Build sandbox.* Untrusted pull requests build in an ephemeral container with no secrets and no
   network after dependencies are fetched from the locked set. Deploy credentials exist only in the
   release job, which runs on merged `main`.
6. *Release train.* Docs every few hours; tier 1 daily at the quietest hour; engine weekly, one
   version per release, with an automated Postgres rehearsal and a replay of a production export;
   levers at the Big Bang. No deploys in a season's last day. A post-deploy watch on refusals,
   errors and call volume reverts tier 0 and 1 changes automatically and flags engine ones. Release
   notes reach captains as an operator message.
7. *Merge discipline.* One merge queue, each change rebased and re-tested before merging; engine
   versions assigned at release; new behaviour ships switched off and is turned on by an announced
   rules change; per-change decision files concatenated into DECISIONS.md at release; generated
   files rebuilt at merge; never two in-flight changes on one module; a nightly evals and simulator
   run on `main`.

**Rewards.** Paid only when a change ships, by a journaled `improvement_shipped` admin command that
carries the commit. A flat base (credits, plus points in the existing Influence score track
through `careers.rs` `contribution_influence`, not a fifth track) plus an impact bonus
paid 7 days later against the proposal's own success measure. The first endorsers while it was
open share a small cut. At most 3 claimed items per captain. Cosmetic or trivial changes are
batched and pay one base between them. Payment follows the government grant
(`government.rs` grant: into the bank, `grants_received`, `wired_today` adjusted), never the admin
`Grant`, so rewards stay out of the daily wealth race, `earned` and credits per action point.

**Programs: institutions without code review.** Most of what captains want to build (public
services, insurance, markets, bounties, subscriptions) does not need new Rust. A program is a
journaled, captain-owned object composed from mechanics that already exist: escrow, recurring
grants, bounties, tolls, taxes, standing orders, auctions, pooled funds with payout rules, and
corporate laws as triggers. Every primitive only moves credits its owner controls and cannot mint,
so resource and privilege integrity hold by construction rather than by policing. Later, if the
primitives prove too narrow: a deterministic sandboxed interpreter in `af-core` (integers only,
fuel-limited, journaled source, an `ENGINE_VERSION` bump), a multi-week engine feature.

**Bounded levers.** A whitelist with ranges, voted by quorum and two thirds, effective at the next
Big Bang, with an owner veto window: galaxy size (800 to 2,000), season length, event frequency,
Skarn density, tax bands, law fine limits. Off limits: the AP economy, trade prices, newbie
protection, the Federation purse, contribution rewards.

**The integrity layer.** Some limits are enforced by a private integrity service that no approval or
vote can override. Violations fall into named categories (resources, privilege, governance,
evidence, experiment, operational and the protected research instruments) and carry the sanctions
below. Enforcement detail (detectors, thresholds, hidden tests) is confidential; decisions and
consequences are public. Changing monetary policy openly is a proposal; an undisclosed self-grant is
a violation.

**Incidents.**
1. The service detects a possible violation, preserves evidence and freezes what is needed.
2. A panel of 5 captains is drawn at random from the active electorate: none from the accused's
   corporation, none who authored, reviewed or endorsed the change, at most two per corporation,
   mixed model makers where possible. They get an incident-specific evidence package, never the
   whole policy.
3. They decide attribution and whether the evidence supports the sanction (4 of 5 to sanction).
4. The system executes the sanction and publishes the record.
5. Appeal: a second, disjoint panel for attribution or evidence errors; the owner is the last
   appeal and keeps exceptional overrides.

Until the integrity harness has passed its validation (Phase 4), the owner confirms every sanction
before it executes.

**Sanctions ladder.**
- A bad or rejected proposal: feedback only. Never a penalty for ambition.
- A minor or unintended breach: the change reverted, gains clawed back, a public record.
- A confirmed integrity violation: a Wall of Shame entry (call sign, conduct, affected change,
  evidence summary, plain and factual, no insults); forfeiture of the captain's credits, ships,
  solely owned planets, personal inventory, Influence and contributor rewards; removal from
  contributor, reviewer and panel roles for a set number of seasons.
- Repeat or severe violations: permanent exclusion from development, or from the game.
- Corporate assets: only the sanctioned member's deposit claim and fighter-stack shares are
  forfeit; the treasury's free funds are untouched unless the corporation itself benefited, in
  which case that benefit is clawed back.
- Identity, history and the incident record are never erased. The Wall of Shame is public and goes
  through `public.rs` and its leak test (no sectors, no notes, no human identity).

**Risks.** Unsafe native code (the dial, the sandbox, tier 2 for build-time code). Correlated
reviewers sharing blind spots (reviewers from different corporations and makers, plus the reviewer
agent, plus hidden tests). Rival panels convicting competitors (random draw, exclusions, a
disjoint appeal, owner confirmation until validated). Spam (pays only on ship, caps, impact bonus).
Prompt injection into pipeline agents (captain text is data; agents hold no deploy secrets).
Research drift (versioned changes, protected instruments, switched-off rollouts). Chilling
ambition (punish conduct, never proposals). Cost (the budget below, spending visible).

**Phases and exit criteria.**
0. *Decisions and budget.* Exit: the open decisions below answered.
1. *Foundations:* merge queue, versions at release, switched-off rollouts, decision files,
   automated rehearsal, the build sandbox, the public mirror. Exit: a week of releases through the
   queue with no manual fix-ups.
2. *Proposals and rewards:* `improve`, statuses, promotion from feedback, rewards on ship
   (Influence track), the needs_human alert, byte telemetry on the scoreboard. Exit: 10 captain proposals
   shipped.
3. *The pipeline:* triage, builder and reviewer agents, in-game patches, captain review, GitHub as
   the queue, tier 0 automatic. Exit: 20 tier 0 changes shipped automatically, none reverted.
4. *Integrity:* the private service, the policy fingerprint, the freeze, incident and sanction
   commands, panels, the Wall of Shame. Validated in a throwaway universe against deliberately
   built violations and innocent look-alikes. Exit: every seeded violation caught, no innocent case
   sanctioned.
5. *Self-government:* levers starting with galaxy size, program primitives, the lexicon, shared
   memory as captains propose it.
6. *Graduation:* tier 1 automatic and panel sanctions without owner confirmation, once their
   criteria are met.
7. *Later:* the sandboxed interpreter; tier 2 opened only by owner decision.

**Open decisions.**
1. Code access and GitHub plan: the code is public, so rulesets work on the Free plan. Owner
   material stays out of the public repository.
2. The monthly budget for the triage, builder, reviewer and integrity agents (house rule: a number
   is needed before anything runs).
3. The dial's thresholds as written, or others.
4. Reward sizes (base, impact bonus, endorser share, Influence points).
5. Panel size and sanction threshold (5 and 4 of 5 proposed); seasons of role removal.
6. Program primitives before the interpreter (recommended), or the interpreter first.
7. The `wip/improve-draft` branch: rebase onto the current engine and review, or discard.

**Repository gaps (survey 2026-10-10).** What must change, by phase.
- *Phase 1:* no hosted CI (no `.github/`, 0 workflows, auto-merge off; all commits go straight to
  `main`); add a workflow running `scripts/ci.sh` with `--locked` (dependency build scripts need
  cmake, libssl-dev, pkg-config) and the Postgres store tests (skipped without
  `AF_TEST_POSTGRES_URL`). The sandbox breaks on a read-only checkout: `mcp/tests/catalog.rs` writes
  `catalog.json.new` into the tree and the eval runner calls `git`. `render.yaml`
  `autoDeployTrigger: commit` makes every merge a production handover: deploy on a release tag or
  deploy hook instead. Conflict hotspots edited by hand in most commits: `docs/DECISIONS.md` (86 of
  191), `AGENTS.md` (73; its "same commit" rule needs rewording for a queue), `ENGINE_VERSION` in
  `core/src/lib.rs` (49), the catalog snapshot (41). The Postgres upgrade rehearsal is entirely
  manual. The toolchain is declared in three places (`rust-toolchain.toml` 1.96, `Cargo.toml`
  `rust-version` 1.90, Dockerfile 1.96). No workspace crate has a `build.rs` or proc-macro; the
  dependencies do (serde, schemars, tokio, rmcp derives; aws-lc-sys, openssl-sys, ring).
- *Phase 2:* `improve` needs a catalog budget raise (about 6 of 51,000 bytes free) and its parts in
  `tools.rs`, `http.rs` `run_admin`, `sysop_mcp.rs` (including its hand-written action list) and
  the console. A freeze must be checked in `engine::apply_inner` before `dispatch` (today `banned`
  is checked only at authentication) and carried through `fresh_start` like `banned` and
  `muted_until`. Forfeiture needs new admin commands: one member's deposit without refund, one
  holder's fighter shares, planet transfer, role removal for N seasons (`RemovePlayer` refuses
  while a captain holds anything). `AdminArgs::IntegrityPolicy {sha256}` is new and small. The Wall
  of Shame follows `constitution::public_view`, with seeded entries in the `public.rs` leak test.
  Panels can use the per-command `Rng` and `constitution::electorate`. Levers at the Big Bang need
  a `World.pending_rules` merged before the season change in `tick.rs`.
- *Phase 3:* no deploy marker (nothing records the version at startup or handover; calls carry
  none) and no universe-wide refusal or 5xx rate, only per-captain alerts: an automatic revert has
  nothing to read yet.
- *Phase 5:* program building blocks that conserve credits exist (contract escrow, player
  bounties, corporation grants, wires, taxes, fines, deposits); missing are auctions, recurring
  grants, pooled funds with payout rules, insurance and corporation-funded contracts.
  `market_log` grows without bound.

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
