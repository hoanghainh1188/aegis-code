-- The clone of a repo an admin added or changed from the pages (#330),
-- made in the background: whether it is being made, is there, or failed,
-- with the end of git's output when it failed. One per repo: its latest.
-- Expand only.

CREATE TABLE repo_clones (
    repo_id     bigint PRIMARY KEY REFERENCES repos (id) ON DELETE CASCADE,
    state       text NOT NULL CHECK (state IN ('cloning', 'done', 'failed')),
    log_tail    text CHECK (octet_length(log_tail) <= 8192),
    started_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);
