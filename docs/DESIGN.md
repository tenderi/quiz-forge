# quiz-forge — design

Status: **draft v0.2** (2026-10-21). Nothing is implemented yet. This document is
the spec; code follows it, and changes to behaviour start as changes here.

## 1. What it is

A self-hosted question bank for people who run quiz nights. One deployment serves
one group. Members of a Telegram group:

1. **submit** questions with rich metadata (Telegram DM wizard or web form),
2. are **warned about near-duplicates** while submitting,
3. **vote** on whether a question goes into the bank,
4. **iterate** on it through edit proposals until there's one agreed current version,
5. optionally get an **AI review pass** that fixes typos, proposes fun facts and
   fact-checks,
6. **assemble quiz nights** from a template (X rounds × Y slots, with constraints per
   slot), filled deterministically from a seed, then tweaked by hand,
7. **print** the result: a host sheet with answers, player answer sheets and
   picture handouts. Players answer on paper; the app doesn't score them.

The question bank is private. The code is public, and anyone can deploy their own
instance with their own bot token, database and media.

### Non-goals for v1

- Playing quizzes in the browser or in Telegram, or live scoring.
- Several isolated groups on one instance (one bank per install).
- Semantic or embedding-based duplicate detection at submit time.
- A public sign-up. Membership comes from Telegram group membership only.

## 2. Principles

- **Deterministic core.** Every state change (submission, vote, closing a vote,
  accepting a proposal, filling a quiz) is a pure function of
  `(current state, input, now)`. The clock is injected and nothing reads wall
  time inside the domain layer. Randomness is a seed stored with the result.
  Replaying the same inputs gives the same state, and tests run without I/O.
- **AI is an input, not an authority.** The review agent is just another author.
  It only uses the same proposal mechanism humans use, with one narrow
  exception (typo fixes, §7.3), and **deterministic code** decides whether its
  edit really is a typo fix. The agent's own label doesn't count. Agent output
  is stored verbatim with model id and timestamp. The app is fully usable with
  the agent switched off, which is the default.
- **Nothing is overwritten.** Question versions are immutable. "Only one version"
  means exactly one *current* version, and the history stays for diffs, audit and
  undo.
- **Private by default.** The bank, its media and its exports are reachable only by
  authenticated members. The repository contains no data, no deployment-specific
  hosts and no secrets.

## 3. Roles

| Role | Who | Can |
|---|---|---|
| **Member** | Anyone currently in the configured Telegram group | Submit, vote, propose edits, browse the bank, build and print quizzes |
| **Author** | The member who submitted a question | Accept or decline proposals on it, revise and resubmit it if rejected |
| **Agent** | The AI review worker (system member, `kind = agent`) | Create review records and proposals, auto-apply typo fixes |
| **Operator** | Whoever deploys the instance | Config only (env vars, theme list, agent on/off); no in-app superpowers |

Per the brief, every member counts as an admin: there's no moderator tier. The
author is the only one with special rights, and only over their own question.

Membership is checked against Telegram with `getChatMember` on the configured group:
on every bot interaction (cached for 10 min), and again when the web session is
created and refreshed. Leaving the group removes access, but authorship and history
stay.

**Orphaned questions.** If an author leaves the group, any member can accept or
decline proposals on that question, provided they aren't the proposer.

## 4. Question model

### 4.1 Fields (one `question_version` row)

| Field | Type | Required | Notes |
|---|---|---|---|
| `kind` | enum | yes | `text`, `multiple_choice`, `multi_part`, `image`, `audio`, `tiebreaker` |
| `prompt` | text | yes | The question as read aloud / printed |
| `answer` | text | yes | Canonical answer. For `multi_part`, see `parts` |
| `accepted_answers` | list[text] | no | Alternatives the host should accept ("JFK", "Kennedy") |
| `choices` | list[text] | MC only | 3–6 options; `answer` must equal one of them |
| `parts` | list[{prompt?, answer, points}] | multi_part only | "Name the five…", per-part points, partial credit |
| `points` | int | yes (default 1) | For multi_part, the sum of the parts |
| `media` | list[media_ref] | image/audio only | 1+ images or 1 audio clip (`start`/`end` seconds optional) |
| `difficulty` | 1–5 | yes | 1 = everyone knows, 5 = specialist |
| `theme` | ref → `themes` | yes | Curated list (geography, history, music, sport, science, film & TV, literature, food & drink, nature, pop culture, language, art, tech, misc). Members can add themes |
| `tags` | list[text] | no | Free tags, normalized to lowercase |
| `language` | BCP-47 | yes | Defaults to the instance's `DEFAULT_LANGUAGE` |
| `background` | text (markdown) | no | Longer story: why it's interesting, context for the host |
| `fun_fact` | text | no | A "did you know" line the host can read after the answer. AI additions go here (as proposals) |
| `host_notes` | text | no | Pronunciation, what to emphasise, "accept either spelling" |
| `sources` | list[url] | no | Where the fact can be verified |

Only the web UI and exports render the UI language, which is English. Question
content can be in any language.

### 4.2 Question (the stable identity)

`question { id, author_id, status, current_version_id, vote_round, created_at,
last_reviewed_version_id }`

`status` ∈ `draft → voting → accepted | rejected`, plus `retired` (accepted but
taken out of use, e.g. the fact went stale; reversible).

### 4.3 House rules (hard validation, deterministic)

These run on every new version, at submit and when a proposal is applied. The bot
and the web give the same verdict because both call the same domain function.

1. **No yes/no questions.** A version is refused if its normalized answer (or, for
   multiple choice, its set of choices) is a yes/no pair from the per-language word
   list: `yes/no, true/false, kyllä/ei, totta/tarua, ja/nej, sant/falskt`, and so on.
   The list is a data file, and self-hosters can extend it. The bot posts a
   **shaming message** in the group (§4.4), picked deterministically from a list, by hash of the
   question id, so it stays the same on retry and varies between questions. The
   submission stays a `draft` so it can be fixed.
2. **Numeric answers only in tiebreakers, except years.**
   - The answer is *numeric* if, once normalized, it's a number. That covers digits
     with optional separators, decimals, units ("42 km") and number words from the
     per-language list.
   - It's a *year* if it's an integer from 1 to 2200 with an optional era marker
     (`BC/AD/eKr/jKr/eaa/jaa`), or the question's `kind` hints at one
     ("In which year…" patterns per language).
   - A non-year numeric answer is allowed **only** when `kind = tiebreaker`.
     Otherwise it's refused with a pointer to use the tiebreaker kind.
   - A year answer is allowed in any kind.
   - **Every numeric answer, years included, gets a "Boo!"** in the group (§4.4),
     whether accepted or refused. It's a reaction, not a block.
3. A tiebreaker **must** have a numeric answer (years count) so the closest guess
   wins.
4. Multiple choice: `answer ∈ choices`, choices are unique after normalization, and
   there are 3–6 of them. Choices are **text only**: a multiple-choice question
   can't carry media, so image and audio questions are always open-answer.
5. Image/audio kinds need media, and media has to be on the allow-listed MIME types
   and under the size caps.

The heuristics will miss things ("Is Helsinki the capital?" with the answer
"Correct"). The agent pass flags anything that slipped through (§7.2); a human then
decides.

### 4.4 Where house-rule reactions go

The shaming message and "Boo!" are **posted in the group only, never in a DM**,
whether the submission came from the DM wizard or the web. They mention the
submitter and quote the offending prompt. The wizard or web form itself only shows
the plain validation error ("yes/no questions aren't allowed"), so the
public reaction stays in the group. Both go through the outbox with a dedupe key
per `(version, rule)`, so a retry never posts twice.

## 5. Workflow

### 5.1 Lifecycle

```
            submit (rules pass)
  draft ──────────────────────────► voting ──(enough approvals)──► accepted
    ▲                                  │                              │
    │ rules fail                       └─(window runs out)─► rejected │ retire / unretire
    │ (stays draft)                                       │         ▼
    └────────── author revises & resubmits ◄──────────────┘      retired
                (new vote_round)
```

### 5.2 Submitting

- **Telegram:** `/add` in the group (or in DM) opens a DM wizard. It walks
  kind → prompt → answer → kind-specific fields (choices / parts / photo / audio) →
  difficulty (buttons) → theme (buttons) → language (default preselected) →
  optional fields (skip-able: alternatives, background, fun fact, host notes,
  sources, tags) → **review screen** → submit. Wizard state is persisted, so a bot
  restart doesn't lose a half-typed question, and `/cancel` drops it.
- **Web:** the same fields on one form, with live rule and duplicate feedback.
- On submit, the rules (§4.3) run, then the duplicate check (§6). The author sees
  the matches and **confirms anyway**: duplicates warn, they never block. Then the
  question enters `voting`, and the bot posts a **vote card** in the group.

### 5.3 Voting

Defaults, all configurable via env:

| Setting | Default |
|---|---|
| `VOTE_WINDOW` | 7 days |
| `VOTE_MIN_APPROVALS` | 1 (from someone other than the author) |
| Accept | **Immediately**, on the vote that makes `yes ≥ VOTE_MIN_APPROVALS` and `yes > no` |
| Reject | When the window runs out without the accept condition ever having been met |
| Author votes | Not allowed |
| Vote changes | Allowed while voting is open; the last vote counts |
| Visibility | Names shown on the card |

- The vote card shows the prompt, answer, kind, difficulty, theme and media, plus
  any duplicate warnings ("⚠ similar to #123"), with ✅/❌ buttons and a link to the
  web page.
- Acceptance is evaluated in the same transaction as each vote (`cast_vote(state,
  vote, now)`), so it happens the moment the deciding ✅ lands. A ❌ only delays
  acceptance: with one ❌ cast, two ✅ are needed.
- Rejection is a deterministic, idempotent `expire_votes(now)` run by a scheduler
  tick every minute. Either way the card is edited in place with the result, and
  the author gets a DM.
- **Material edits during voting** (prompt, answer, accepted answers, choices,
  parts, media, kind) start a new `vote_round`: votes reset, the window restarts and
  the card is reposted. Non-material edits (metadata, background, fun fact, notes,
  typo fixes) keep the votes.

### 5.4 Iterating: edit proposals

Anyone, including the agent, can propose an edit: a field-level diff against a
specific `base_version_id`, with a short rationale.

- **The author accepts or declines.** Accepting creates a new immutable version and
  moves `current_version_id`.
- If the proposal's base isn't the current version any more, it's **rebased** when
  it doesn't touch the fields that changed, and otherwise marked `stale` and left
  for the proposer to redo. Two concurrent proposals never silently clobber each
  other.
- **Material edits to an *accepted* question** need two people. An outside
  proposal the author accepts already has them (proposer + author). When the author
  proposes a material edit to their own accepted question, it needs one approval
  from another member, using the same ✅ on a proposal card. Non-material edits by
  the author apply directly.
- The author can also edit while the question is still `draft` or `voting`, without
  proposals. During voting a material edit resets the round (§5.3).
- **Revert** is just a proposal whose diff restores an older version.
- The author gets a DM for each new proposal. The web shows a side-by-side diff.

### 5.5 Rejected questions

They stay in the bank as `rejected`, with full history and the votes on record.
The author can revise and resubmit, which starts the next `vote_round`. Rejected
questions still show up in duplicate warnings, marked as rejected.

## 6. Duplicate detection (deterministic)

Run at submit and on every material edit; results are stored with the version as
a snapshot.

**Normalization:** Unicode NFKC, then casefold, then collapse whitespace, then strip
punctuation. Diacritics are **kept**: in Finnish `ä/ö` are distinct letters.
Stopwords for the question's language are dropped, from small per-language data
files.

**Scores** against every non-draft question (a few thousand rows is fine in
Python; add a `pg_trgm` prefilter if the bank grows past ~50k):

- `q_sim` = character-trigram Jaccard on normalized prompts
- `a_sim` = exact match on normalized answers or any accepted answer → 1.0,
  otherwise trigram Jaccard
- `score = 0.7·q_sim + 0.3·a_sim`

| Band | Condition | Shown as |
|---|---|---|
| Likely duplicate | `score ≥ 0.80`, or `q_sim ≥ 0.90` | "⚠ Looks like #123" |
| Same answer, different question | `a_sim = 1.0` and `q_sim ≥ 0.35` | "Same answer as #123, might overlap" |
| Similar | `score ≥ 0.55` | Listed, low-key |

Thresholds are constants in one module with fixture-based tests, to be tuned on
real data. Ties are ordered by `(score desc, question_id asc)`, so the output is
stable.

"Same fact, different wording" (semantic duplicates) is left to the agent pass
(§7.2), which records it as a note on both questions.

## 7. AI review agent (optional, off by default)

### 7.1 Running

- Enabled by `AGENT_ENABLED=true` plus provider credentials. Provider interface:
  `review(question_version, context) → ReviewResult`. It ships with an Anthropic
  Claude implementation (model configurable, defaults to a current Sonnet-class
  model) that uses the web search tool for fact checks. Other providers can plug in.
- A scheduled job (`AGENT_SCHEDULE`, default nightly) picks the questions where
  `last_reviewed_version_id ≠ current_version_id` and the status is `voting` or
  `accepted`, oldest first. It stops at `AGENT_MAX_QUESTIONS_PER_RUN` (default 25)
  or `AGENT_MAX_COST_PER_RUN`, whichever comes first.
- Each run is one `review_run` row (start, end, model, counts, tokens, cost), and
  each question gets a `review` row with the raw structured output.

### 7.2 What it produces (structured output, validated against a schema)

| Output | Lands as |
|---|---|
| Spelling / grammar / punctuation fixes | **Auto-applied** if the typo guard passes (§7.3), otherwise a proposal |
| Fact-check verdict: `ok` · `doubtful` · `wrong` · `unverifiable` · `time_sensitive`, with explanation and sources | A `review` record shown on the question. `wrong`/`doubtful` DMs the author and badges the question in the bank |
| Correction of a wrong answer | Proposal (material, so it needs the author) |
| Fun fact / background enrichment | Proposal (non-material) |
| Metadata suggestions (difficulty, theme, tags) | Proposal (non-material) |
| House-rule suspicions (disguised yes/no, numeric) | Flag on the review |
| Semantic duplicates of other questions (it gets top-N lexical neighbours plus a list of same-theme prompts) | Note linking both questions |

### 7.3 The typo guard (deterministic)

The agent may mark changes as `typo`. **The app re-checks each one** and applies it
only if every one of these holds:

- Only `prompt`, `background`, `fun_fact` and `host_notes` change. **Never**
  `answer`, `accepted_answers`, `choices` or `parts`.
- Tokens line up 1:1, and each changed token is within edit distance ≤ 2, and
  ≤ 30 % of its length.
- No digits, proper-noun capitalization patterns or quoted strings change.
- At most 5 tokens change per field.

Anything else turns into an ordinary proposal. An auto-applied fix creates a version
authored by the agent, gives the author a DM listing the diff, and can be reverted
in one click. It doesn't reset votes (it's non-material).

### 7.4 Privacy note

When the agent is on, question content goes to the configured AI provider. That's
stated in the README and at the `AGENT_ENABLED` setting. Media isn't sent unless
`AGENT_SEND_MEDIA=true`.

## 8. Spoiler tracking

An `exposure(member_id, question_id, reason, at)` row is written whenever a member:

- authors the question,
- votes on it, or opens its vote card details on the web,
- proposes an edit to it or decides on a proposal,
- or is recorded as an attendee at an event that used it.

**Players** at a quiz night aren't necessarily members. `player` rows can be linked
to a member, or exist on their own (a name only). Exposure comes through the link.

The quiz builder takes an optional list of expected players and treats spoiled
questions as **excluded** (default) or **warn-only**. Browsing the bank isn't
recorded as exposure in v1. It's opt-in noise, and the UI keeps answers collapsed
by default.

## 9. Quiz building

### 9.1 Templates

A reusable shape:

```yaml
name: Standard pub night
rounds:
  - title: Warm-up
    slots: 10 × { difficulty: 1-2 }
  - title: Theme round
    slots: 10 × { theme: $round_theme }          # one theme for the whole round
  - title: Pictures
    slots:
      - 3 × { kind: image, group: A }            # "first three are image questions…"
      - 7 × { difficulty: 2-4 }
    groups: { A: { same: theme } }               # "…in the same theme"
  - title: Tiebreaker
    slots: 1 × { kind: tiebreaker }
defaults: { language: fi, exclude_used: true, spoilers: exclude }
```

A slot constraint can set: `kind`, `theme` (fixed, one of a list, or a
round/group variable), `difficulty` range, `tags_any`/`tags_none`, `language`,
and `exclude_ids`. A group constraint is `same: theme | tag`.

### 9.2 Seeded fill (deterministic)

`fill(template, bank_snapshot, seed, locks, players) → quiz`

1. **Candidates** for a slot: `accepted` questions that match its constraints,
   aren't used at a past event (unless allowed), aren't spoiled for the given
   players (if excluding), aren't already picked, and aren't a likely-duplicate
   (§6) of anything already picked.
2. **Ranking:** `sha256(seed ‖ slot_path ‖ question_id)`, ascending. That's
   stable across Python versions and platforms, unlike `random.shuffle`, and
   independent of row order.
3. **Variables and groups** (`$round_theme`, `same: theme`) are bound first. Themes
   with enough candidates for all their slots are ranked the same hash-based way,
   and the first that fits is taken.
4. Slots are filled in document order. If one can't be filled, it's left empty
   with the reason ("only 1 image question in theme 'music'"). Partial results
   still come back.
5. **Locks:** slots the host set by hand are kept, and **Refill** with the same or
   a new seed only touches unlocked slots.

The bank snapshot is "accepted questions as of time T", so a stored quiz can be
reproduced exactly: same template, same seed, same snapshot, same quiz.

### 9.3 Quiz → frozen

When the host clicks **Freeze**, the quiz pins each slot to a specific
`question_version_id`. Edits made later don't change a printed quiz, though the UI
shows "newer version available" with a diff and a button to take it.

### 9.4 Events

`event { date, title, quiz_id, players[] }`. Marking an event as **held** records
usage (`question_usage`) and attendee exposures. Questions used at an event are
excluded from future fills by default.

## 10. Exports (players answer on paper)

All generated server-side from the frozen quiz, as HTML print views with
`@media print` (save as PDF from the browser). No PDF library is needed in v1.

- **Host sheet:** per round, each question's prompt, answer, accepted answers,
  points, host notes and fun fact, plus media thumbnails and audio timestamps.
- **Player answer sheet:** per round, numbered blank lines (multi-part
  questions get several), a team name box and points boxes.
- **Picture handout:** the image questions, numbered, with no answers. One page per
  team.
- **Presenter view (web):** one question at a time, answers hidden until revealed,
  audio player inline. It's for projecting, not for players' devices.
- **Answer-key CSV** for whoever does the marking.

## 11. Architecture

```
            Telegram ─────────► bot (python-telegram-bot, long polling)
                                  │  HTTP + shared secret
 browser ─► reverse proxy ─► web (nginx, static SPA) ─► api (FastAPI) ─► Postgres
                                                          ▲      │
                              worker (scheduler: vote     │      └► media volume (private)
                              closing, outbox, agent) ────┘
```

| Component | Tech | Notes |
|---|---|---|
| `api` | Python 3.14, FastAPI, SQLAlchemy 2, Alembic (hand-written migrations) | The only writer to the DB. Owns the domain layer |
| `domain` | Pure Python package, no I/O | Rules, dedupe, voting, proposals, typo guard, quiz fill. Most tests live here |
| `bot` | python-telegram-bot 22 | Thin: wizard UI and vote cards; calls the API. **Sets the `httpx` logger to WARNING**, because at INFO it logs URLs containing the bot token |
| `worker` | Same image as `api`, different entrypoint | 1-min tick: `expire_votes(now)`, flush the **outbox**, run the agent on its schedule |
| `web` | React + TypeScript + Vite + Tailwind, served by nginx | Bank browser, question page (history/diff/proposals), submit form, templates, quiz builder, print views |
| `db` | PostgreSQL | Single database |
| Media | Content-addressed files on a volume (`sha256` path) | Served **only** through the authenticated API, never as static files |

**Outbox.** Every Telegram message the system needs to send (vote cards, DMs,
"Boo!") goes into an `outbox` table inside the same transaction as the state
change. The worker delivers it, retries with backoff, and marks it sent. A crash
can't lose a notification or double-apply a state change.

**Authentication.**
- Bot → API: shared secret header, from `.env`.
- Web: **login link from the bot**. `/web` in a DM sends a one-time link that
  expires in 10 minutes. Opening it creates a session cookie (HttpOnly, Secure,
  SameSite=Lax) holding a signed session id. Sessions are stored server-side and
  re-check group membership every 24 h. No passwords, no email.

**Deployment (generic).** A single `docker-compose.yml` and a `.env.example`.
Every published port binds to `127.0.0.1` by default (`WEB_BIND`/`WEB_PORT`), and
TLS is the deployer's reverse proxy (an example Caddy snippet goes in
`docs/DEPLOYMENT.md`). Images are buildable off-box and shipped. The stack must
survive host reboots (`restart: unless-stopped`). `backup.sh` takes a `pg_dump`
plus a media tarball, and the destination is up to the deployer.

**Configuration** (env): `TELEGRAM_BOT_TOKEN`, `TELEGRAM_GROUP_ID`,
`PUBLIC_BASE_URL`, `DEFAULT_LANGUAGE`, `VOTE_WINDOW`, `VOTE_MIN_APPROVALS`,
`MEDIA_MAX_IMAGE_MB`, `MEDIA_MAX_AUDIO_MB`, `AGENT_*`, plus generated secrets
(`SESSION_SECRET`, `BOT_API_SECRET`, `POSTGRES_PASSWORD`) created on the host by an
`ensure_env.sh`.

## 12. Data model (tables)

```
member(id, telegram_id UNIQUE, display_name, kind[human|agent], in_group, joined_at, left_at)
theme(id, name UNIQUE, created_by)
question(id, author_id, status, vote_round, current_version_id, last_reviewed_version_id, created_at)
question_version(id, question_id, n, created_by, created_at, reason, parent_id,
                 kind, prompt, answer, accepted_answers[], choices[], parts jsonb, points,
                 difficulty, theme_id, tags[], language, background, fun_fact, host_notes,
                 sources[], rule_result jsonb, dup_snapshot jsonb)        -- immutable
question_media(version_id, media_id, position, start_s, end_s)
media(id, sha256 UNIQUE, mime, bytes, width, height, duration_s, uploaded_by, created_at)
vote(question_id, vote_round, member_id, value[yes|no], cast_at, PRIMARY KEY(question_id, vote_round, member_id))
vote_result(question_id, vote_round, closes_at, closed_at, yes, no, outcome)
proposal(id, question_id, base_version_id, proposer_id, diff jsonb, rationale,
         material bool, status[open|accepted|declined|stale|withdrawn], decided_by, decided_at,
         needs_second_approval bool, second_approver_id)
review_run(id, started_at, finished_at, provider, model, questions, tokens_in, tokens_out, cost)
review(id, run_id, question_id, version_id, verdict, explanation, sources[], raw jsonb,
       flags[], auto_applied_version_id, created_at)
note(id, question_id, author_id, body, links_question_id, created_at)   -- agent dup notes, comments
exposure(member_id, question_id, reason, at)
player(id, name, member_id NULL)
template(id, name, body jsonb, created_by, updated_at)
quiz(id, template_id, template_snapshot jsonb, seed, bank_as_of, created_by, frozen_at)
quiz_slot(quiz_id, round_ix, slot_ix, question_id, version_id NULL, locked bool, empty_reason)
event(id, quiz_id, date, title, held_at)
event_player(event_id, player_id)
question_usage(question_id, version_id, event_id)
outbox(id, kind, chat_id, payload jsonb, dedupe_key UNIQUE, attempts, next_attempt_at, sent_at, tg_message_id)
wizard_state(member_id, step, draft jsonb, updated_at)
login_token(token_hash, member_id, expires_at, used_at)
session(id_hash, member_id, created_at, last_seen_at, expires_at)
audit(id, at, actor_id, action, subject, data jsonb)
```

## 13. Telegram surface

| Where | Command / element | Does |
|---|---|---|
| Group | vote card | ✅/❌ inline buttons, live tally, link to web; edited in place on close |
| Group | `/add` | Starts the DM wizard (the bot replies "check your DMs") |
| Group | `/stats` | Counts per status/theme, top contributors |
| DM | `/add`, `/cancel` | Submission wizard |
| DM | `/mine` | Your questions with status and open proposals |
| DM | `/inbox` | Proposals awaiting your decision, with accept/decline buttons and the diff inline |
| DM | `/web` | One-time login link |
| DM | `/random [theme]` | A random accepted question to read; records exposure |
| Bot replies | shaming message, "Boo!" | House rules (§4.3) |

## 14. Repository layout (planned)

```
src/quizforge/
  domain/        rules.py dedupe.py voting.py proposals.py typo_guard.py quizfill.py  (pure)
  data/          yes_no.toml numbers.toml stopwords/*.txt shaming.toml themes.toml
  persistence/   models.py repo.py
  api/           app.py routes/*.py auth.py media.py
  bot/           app.py wizard.py cards.py
  worker/        main.py outbox.py scheduler.py agent/{base.py,anthropic.py,prompts.py}
alembic/
frontend/
docker/ docker-compose.yml .env.example ensure_env.sh backup.sh
tests/  domain/ api/ fixtures/
docs/   DESIGN.md DEPLOYMENT.md
```

`.gitignore` excludes `.env`, `data/`, `media/`, `backups/` and `*.dump` from day
one. The repo must never contain a real question.

## 15. Milestones

1. **Domain core**: rules, dedupe, voting, proposals, typo guard, quiz fill, with
   tests. No I/O.
2. **API + DB**: models, migrations, routes, media upload, outbox, worker vote
   closing.
3. **Bot**: wizard, vote cards, inbox, `/web` login.
4. **Web**: bank browser, question page with diff, submit form, quiz builder,
   print views.
5. **Agent**: provider interface, Claude implementation, typo guard wired in,
   budget caps.
6. **Ops**: compose, `ensure_env.sh`, backup, `DEPLOYMENT.md`, CI (tests, lint,
   build images).

## 16. Decisions log

| Date | Decision |
|---|---|
| 2026-10-21 | Accept as soon as the approval threshold is met; reject when the window runs out without it |
| 2026-10-21 | Multiple choice is text only; image/audio questions are always open-answer |
| 2026-10-21 | Shaming message and "Boo!" go to the group only |
| 2026-10-21 | No read-aloud time estimate |
| 2026-10-21 | Licence: MIT |
