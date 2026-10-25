-- Sonar warnings polled from a repo's project (#27): each warning is taken
-- by one batch, whose job's `issue_ref` is `batch`, written with the job in
-- one transaction. A warning that stays open is not taken again; the batch
-- reads its warnings back by key when its job (or a retry of it) starts.
-- Expand only: an older server never reads or writes this table.
-- Named by timestamp so migrations of parallel branches do not collide.

CREATE TABLE sonar_warnings (
    repo_id     bigint NOT NULL REFERENCES repos (id),
    warning_key text NOT NULL CHECK (length(warning_key) BETWEEN 1 AND 128),
    batch       text NOT NULL CHECK (length(batch) BETWEEN 1 AND 128),
    taken_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, warning_key)
);

CREATE INDEX sonar_warnings_batch ON sonar_warnings (repo_id, batch);
