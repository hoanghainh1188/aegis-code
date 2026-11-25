-- The feature lane's job model (#675, ADR 0019).
--
-- A plan that waits for a person ends `awaiting_approval`: done, so it
-- holds no lease, and not failed. An approval queues it again.
ALTER TABLE jobs DROP CONSTRAINT jobs_outcome_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_outcome_check
    CHECK (outcome IN ('pr_opened', 'stopped_budget', 'stopped_steps', 'stopped_expert_fail',
                       'diagnosed', 'awaiting_approval'));

-- A feature's sub-jobs: each is a job of its own, under the job that
-- planned them, in the same repo.
ALTER TABLE jobs ADD COLUMN parent_job_id bigint REFERENCES jobs (id);
CREATE INDEX jobs_parent ON jobs (parent_job_id) WHERE parent_job_id IS NOT NULL;

-- Sub-jobs share their feature's ticket: one live job per ticket counts
-- the jobs with no parent only.
DROP INDEX jobs_one_live_per_issue;
CREATE UNIQUE INDEX jobs_one_live_per_issue ON jobs (repo_id, issue_ref)
    WHERE status IN ('queued', 'running') AND parent_job_id IS NULL;

-- `job_id` is claimed only once every job it depends on opened its PR.
CREATE TABLE job_dependencies (
    job_id     bigint NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    depends_on bigint NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    PRIMARY KEY (job_id, depends_on),
    CHECK (job_id <> depends_on)
);
CREATE INDEX job_dependencies_on ON job_dependencies (depends_on);

-- The last approval of a job's plan: who gave it, and when.
CREATE TABLE job_approvals (
    job_id      bigint PRIMARY KEY REFERENCES jobs (id) ON DELETE CASCADE,
    approved_by text NOT NULL,
    approved_at timestamptz NOT NULL DEFAULT now()
);
