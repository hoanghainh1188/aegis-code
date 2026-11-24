-- A job's progress between its model calls (#662): another worker goes on
-- from it after a crash or a release instead of from the first turn. One
-- row per ledger (a job's `ledger_key`, or a CLI run's issue), removed
-- when the job ends.

CREATE TABLE job_checkpoints (
    ledger_key text COLLATE "C" PRIMARY KEY,
    version    integer NOT NULL,
    turn       integer NOT NULL CHECK (turn >= 0),
    base_sha   text NOT NULL,
    start_sha  text NOT NULL,
    -- `git diff --binary` from `start_sha`, paths from the repo root.
    diff_patch text NOT NULL,
    -- The session, the router's counters and the loop's own state.
    state      jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
