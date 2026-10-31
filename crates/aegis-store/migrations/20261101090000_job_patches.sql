-- The patch a job's PR holds (#312), for the job's page and the PR's
-- review page: the diff the gate checked, redacted, at most 1 MiB, with
-- `truncated` set when it was cut. One per job: the attempt that ended it.
-- Expand only.

CREATE TABLE job_patches (
    job_id      bigint PRIMARY KEY REFERENCES jobs (id) ON DELETE CASCADE,
    diff        text NOT NULL CHECK (octet_length(diff) <= 1048576),
    truncated   boolean NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
