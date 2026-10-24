-- Platform state (ADR 0003 §2, #22). Text columns with CHECK instead of
-- Postgres enums: adding a value is then a plain migration.

CREATE TABLE repos (
    id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    slug           text NOT NULL UNIQUE CHECK (slug ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$'),
    url            text NOT NULL,
    default_branch text NOT NULL,
    language       text NOT NULL,
    mode           text NOT NULL CHECK (mode IN ('poc', 'customer-jp')),
    -- The rest of the repo's RepoConfig until #24 needs columns to filter on.
    config         jsonb NOT NULL DEFAULT '{}',
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- One issue to handle. The queue's claim and lease queries come with #23.
CREATE TABLE jobs (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id      bigint NOT NULL REFERENCES repos (id),
    -- The issue in its tracker: `owner/repo#12`, a Jira key, a Sonar rule.
    issue_ref    text NOT NULL,
    -- The job's ledger (`ledger_events.ledger_key`): unique across repos.
    ledger_key   text COLLATE "C" NOT NULL UNIQUE,
    source       text NOT NULL
                 CHECK (source IN ('cli', 'github', 'gitlab', 'jira', 'backlog', 'sonar', 'ci')),
    status       text NOT NULL DEFAULT 'queued'
                 CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
    -- The agent's result; only and always set once the job is done.
    outcome      text CHECK (outcome IN
                 ('pr_opened', 'stopped_budget', 'stopped_steps', 'stopped_expert_fail')),
    attempts     integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    locked_by    text,
    locked_until timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CHECK ((status = 'done') = (outcome IS NOT NULL))
);

-- No second live job for the same issue; a finished one may be run again.
CREATE UNIQUE INDEX jobs_one_live_per_issue ON jobs (repo_id, issue_ref)
    WHERE status IN ('queued', 'running');
CREATE INDEX jobs_repo_status ON jobs (repo_id, status, id);
CREATE INDEX jobs_status ON jobs (status, id);

-- One attempt at a job. Infrastructure failures retry with a new run.
CREATE TABLE runs (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    job_id     bigint NOT NULL REFERENCES jobs (id),
    attempt    integer NOT NULL CHECK (attempt >= 1),
    status     text NOT NULL DEFAULT 'running'
               CHECK (status IN ('running', 'finished', 'interrupted', 'failed')),
    error      text,
    started_at timestamptz NOT NULL DEFAULT now(),
    ended_at   timestamptz,
    UNIQUE (job_id, attempt),
    CHECK ((status = 'running') = (ended_at IS NULL))
);

-- The append-only ledger. `event` is the LedgerEvent exactly as one line
-- of `<key>.events.jsonl`; model calls, gate runs and reviews are rows with
-- their `kind`, so a new event kind needs no migration.
CREATE TABLE ledger_events (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ledger_key  text COLLATE "C" NOT NULL,
    -- Filled by the worker (#23); NULL for CLI and eval runs.
    run_id      bigint REFERENCES runs (id),
    event       jsonb NOT NULL CHECK (jsonb_typeof(event) = 'object' AND event ? 'event'),
    kind        text NOT NULL GENERATED ALWAYS AS (event ->> 'event') STORED,
    cost_usd    double precision GENERATED ALWAYS AS ((event ->> 'cost_usd')::double precision) STORED,
    recorded_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX ledger_events_key ON ledger_events (ledger_key, id);
CREATE INDEX ledger_events_kind ON ledger_events (kind, recorded_at);

-- The summary record of a ledger, replaced on every save.
CREATE TABLE issue_records (
    ledger_key text COLLATE "C" PRIMARY KEY,
    record     jsonb NOT NULL,
    kind       text GENERATED ALWAYS AS (record ->> 'kind') STORED,
    outcome    text GENERATED ALWAYS AS (record ->> 'outcome') STORED,
    cost_usd   double precision GENERATED ALWAYS AS ((record ->> 'cost_usd')::double precision) STORED,
    updated_at timestamptz NOT NULL DEFAULT now()
);
