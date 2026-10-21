-- quiz-forge initial schema (DESIGN.md §12). Timestamps are RFC 3339 UTC text;
-- JSON columns are TEXT holding JSON.

CREATE TABLE member (
    id              INTEGER PRIMARY KEY,
    telegram_id     INTEGER UNIQUE,              -- NULL for the agent
    display_name    TEXT NOT NULL,
    username        TEXT,
    kind            TEXT NOT NULL DEFAULT 'human' CHECK (kind IN ('human', 'agent')),
    in_group        INTEGER NOT NULL DEFAULT 1,
    checked_at      TEXT,                        -- last getChatMember check
    created_at      TEXT NOT NULL
);

CREATE TABLE theme (
    name            TEXT PRIMARY KEY,
    created_by      INTEGER REFERENCES member(id),
    created_at      TEXT NOT NULL
);

CREATE TABLE media (
    id              INTEGER PRIMARY KEY,
    sha256          TEXT NOT NULL UNIQUE,
    mime            TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('image', 'audio')),
    bytes           INTEGER NOT NULL,
    uploaded_by     INTEGER REFERENCES member(id),
    created_at      TEXT NOT NULL
);

CREATE TABLE question (
    id                          INTEGER PRIMARY KEY,
    author_id                   INTEGER NOT NULL REFERENCES member(id),
    status                      TEXT NOT NULL CHECK (status IN ('voting', 'accepted', 'rejected', 'retired')),
    vote_round                  INTEGER NOT NULL DEFAULT 1,
    current_version_id          INTEGER,             -- set right after the first version
    last_reviewed_version_id    INTEGER,
    review_flag                 TEXT,                -- latest non-ok AI verdict, for badges
    created_at                  TEXT NOT NULL,
    updated_at                  TEXT NOT NULL
);
CREATE INDEX question_status ON question(status);

-- Immutable. Search columns are copies of fields inside content.
CREATE TABLE question_version (
    id              INTEGER PRIMARY KEY,
    question_id     INTEGER NOT NULL REFERENCES question(id),
    n               INTEGER NOT NULL,
    parent_id       INTEGER REFERENCES question_version(id),
    created_by      INTEGER NOT NULL REFERENCES member(id),
    created_at      TEXT NOT NULL,
    reason          TEXT NOT NULL,
    content         TEXT NOT NULL,
    kind            TEXT NOT NULL,
    theme           TEXT NOT NULL,
    difficulty      INTEGER NOT NULL,
    language        TEXT NOT NULL,
    prompt          TEXT NOT NULL,
    answer          TEXT NOT NULL,
    rule_report     TEXT NOT NULL,
    dup_snapshot    TEXT NOT NULL DEFAULT '[]',
    UNIQUE (question_id, n)
);

CREATE TABLE vote_round (
    question_id     INTEGER NOT NULL REFERENCES question(id),
    round           INTEGER NOT NULL,
    opened_at       TEXT NOT NULL,
    closes_at       TEXT NOT NULL,
    closed_at       TEXT,
    outcome         TEXT CHECK (outcome IN ('accepted', 'rejected', 'superseded')),
    superseded_version_id INTEGER REFERENCES question_version(id),  -- what the card showed
    card_chat_id    INTEGER,
    card_message_id INTEGER,
    PRIMARY KEY (question_id, round)
);
CREATE INDEX vote_round_open ON vote_round(closes_at) WHERE closed_at IS NULL;

CREATE TABLE vote (
    question_id     INTEGER NOT NULL,
    round           INTEGER NOT NULL,
    member_id       INTEGER NOT NULL REFERENCES member(id),
    value           TEXT NOT NULL CHECK (value IN ('yes', 'no')),
    cast_at         TEXT NOT NULL,
    PRIMARY KEY (question_id, round, member_id),
    FOREIGN KEY (question_id, round) REFERENCES vote_round(question_id, round)
);

CREATE TABLE proposal (
    id                      INTEGER PRIMARY KEY,
    question_id             INTEGER NOT NULL REFERENCES question(id),
    base_version_id         INTEGER NOT NULL REFERENCES question_version(id),
    proposer_id             INTEGER NOT NULL REFERENCES member(id),
    patch                   TEXT NOT NULL,
    rationale               TEXT NOT NULL DEFAULT '',
    material                INTEGER NOT NULL,
    needs_second_approval   INTEGER NOT NULL DEFAULT 0,
    status                  TEXT NOT NULL DEFAULT 'open'
                            CHECK (status IN ('open', 'accepted', 'declined', 'stale', 'withdrawn')),
    decided_by              INTEGER REFERENCES member(id),
    decided_at              TEXT,
    applied_version_id      INTEGER REFERENCES question_version(id),
    created_at              TEXT NOT NULL
);
CREATE INDEX proposal_open ON proposal(question_id) WHERE status = 'open';

CREATE TABLE review_run (
    id              INTEGER PRIMARY KEY,
    started_at      TEXT NOT NULL,
    finished_at     TEXT,
    provider        TEXT NOT NULL,
    model           TEXT NOT NULL,
    questions       INTEGER NOT NULL DEFAULT 0,
    tokens_in       INTEGER NOT NULL DEFAULT 0,
    tokens_out      INTEGER NOT NULL DEFAULT 0,
    cost_usd        REAL NOT NULL DEFAULT 0,
    error           TEXT
);

CREATE TABLE review (
    id                      INTEGER PRIMARY KEY,
    run_id                  INTEGER NOT NULL REFERENCES review_run(id),
    question_id             INTEGER NOT NULL REFERENCES question(id),
    version_id              INTEGER NOT NULL REFERENCES question_version(id),
    verdict                 TEXT NOT NULL,
    explanation             TEXT NOT NULL,
    sources                 TEXT NOT NULL DEFAULT '[]',
    flags                   TEXT NOT NULL DEFAULT '[]',
    raw                     TEXT NOT NULL,
    auto_applied_version_id INTEGER REFERENCES question_version(id),
    typo_rejected           TEXT,
    created_at              TEXT NOT NULL
);

CREATE TABLE note (
    id                  INTEGER PRIMARY KEY,
    question_id         INTEGER NOT NULL REFERENCES question(id),
    author_id           INTEGER NOT NULL REFERENCES member(id),
    body                TEXT NOT NULL,
    links_question_id   INTEGER REFERENCES question(id),
    created_at          TEXT NOT NULL
);

CREATE TABLE exposure (
    member_id       INTEGER NOT NULL REFERENCES member(id),
    question_id     INTEGER NOT NULL REFERENCES question(id),
    reason          TEXT NOT NULL,
    at              TEXT NOT NULL,
    PRIMARY KEY (member_id, question_id, reason)
);

CREATE TABLE player (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    member_id       INTEGER UNIQUE REFERENCES member(id),
    created_at      TEXT NOT NULL
);

CREATE TABLE template (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    body            TEXT NOT NULL,
    created_by      INTEGER NOT NULL REFERENCES member(id),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE quiz (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    template_id     INTEGER REFERENCES template(id),
    template_body   TEXT NOT NULL,                -- snapshot: later template edits don't move a quiz
    seed            TEXT NOT NULL,
    bank_as_of      TEXT NOT NULL,
    bindings        TEXT NOT NULL DEFAULT '{}',
    created_by      INTEGER NOT NULL REFERENCES member(id),
    created_at      TEXT NOT NULL,
    frozen_at       TEXT
);

CREATE TABLE quiz_slot (
    quiz_id         INTEGER NOT NULL REFERENCES quiz(id) ON DELETE CASCADE,
    round_ix        INTEGER NOT NULL,
    slot_ix         INTEGER NOT NULL,
    question_id     INTEGER REFERENCES question(id),
    version_id      INTEGER REFERENCES question_version(id),   -- set on freeze
    locked          INTEGER NOT NULL DEFAULT 0,
    empty_reason    TEXT,
    warnings        TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (quiz_id, round_ix, slot_ix)
);

CREATE TABLE quiz_player (
    quiz_id         INTEGER NOT NULL REFERENCES quiz(id) ON DELETE CASCADE,
    player_id       INTEGER NOT NULL REFERENCES player(id),
    PRIMARY KEY (quiz_id, player_id)
);

CREATE TABLE event (
    id              INTEGER PRIMARY KEY,
    quiz_id         INTEGER NOT NULL REFERENCES quiz(id),
    date            TEXT NOT NULL,
    title           TEXT NOT NULL,
    held_at         TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE event_player (
    event_id        INTEGER NOT NULL REFERENCES event(id),
    player_id       INTEGER NOT NULL REFERENCES player(id),
    PRIMARY KEY (event_id, player_id)
);

CREATE TABLE question_usage (
    question_id     INTEGER NOT NULL REFERENCES question(id),
    version_id      INTEGER NOT NULL REFERENCES question_version(id),
    event_id        INTEGER NOT NULL REFERENCES event(id),
    PRIMARY KEY (question_id, event_id)
);

CREATE TABLE outbox (
    id              INTEGER PRIMARY KEY,
    kind            TEXT NOT NULL,
    payload         TEXT NOT NULL,
    dedupe_key      TEXT UNIQUE,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    sent_at         TEXT,
    failed_at       TEXT,
    last_error      TEXT,
    created_at      TEXT NOT NULL
);
CREATE INDEX outbox_due ON outbox(next_attempt_at) WHERE sent_at IS NULL AND failed_at IS NULL;

CREATE TABLE wizard_state (
    member_id       INTEGER PRIMARY KEY REFERENCES member(id),
    state           TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE login_token (
    token_hash      TEXT PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member(id),
    expires_at      TEXT NOT NULL,
    used_at         TEXT
);

CREATE TABLE session (
    id_hash         TEXT PRIMARY KEY,
    member_id       INTEGER NOT NULL REFERENCES member(id),
    csrf            TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    last_seen_at    TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    checked_at      TEXT NOT NULL
);

CREATE TABLE meta (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL
);

CREATE TABLE audit (
    id              INTEGER PRIMARY KEY,
    at              TEXT NOT NULL,
    actor_id        INTEGER REFERENCES member(id),
    action          TEXT NOT NULL,
    subject         TEXT NOT NULL,
    data            TEXT NOT NULL DEFAULT '{}'
);
