-- The feature lane's design gate (#677, ADR 0019). An approval records how
-- the feature is delivered (`null`: as its plan recommends); the plan then
-- waits for its sub-jobs (#678). A writer may instead ask for a revision,
-- with feedback, up to the repo's cap: the job plans again.
ALTER TABLE job_approvals ADD COLUMN delivery_mode text
    CHECK (delivery_mode IN ('integration_pr', 'stacked_prs'));

CREATE TABLE job_revisions (
    job_id       bigint NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    revision     integer NOT NULL CHECK (revision >= 1),
    feedback     text NOT NULL CHECK (char_length(feedback) BETWEEN 1 AND 4000),
    requested_by text NOT NULL,
    requested_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (job_id, revision)
);
