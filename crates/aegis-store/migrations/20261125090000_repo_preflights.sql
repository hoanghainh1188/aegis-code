-- The latest pre-flight check of each repo, run from its page (#599): its
-- items as the checks give them, so the page shows them while it runs, and
-- how it ended. A repo added from the pages waits, paused, for one that
-- passes before an admin turns it on.

CREATE TABLE repo_preflights (
    repo_id     bigint PRIMARY KEY REFERENCES repos (id) ON DELETE CASCADE,
    state       text NOT NULL CHECK (state IN ('running', 'passed', 'warned', 'failed')),
    items       jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(items) = 'array'),
    started_by  text NOT NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);
