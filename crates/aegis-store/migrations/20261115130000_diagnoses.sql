-- The diagnosis lane (#451, ADR 0012): a ticket triaged as needing no code
-- change ends `diagnosed`, with a read-only report written back to it. The
-- report is kept here, one per job, for the job's page and for the comment
-- on the ticket. Expand only: an older server never runs the lane, which
-- is off unless a repo turns it on.

ALTER TABLE jobs DROP CONSTRAINT jobs_outcome_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_outcome_check
    CHECK (outcome IN
        ('pr_opened', 'stopped_budget', 'stopped_steps', 'stopped_expert_fail', 'diagnosed'));

CREATE TABLE job_diagnoses (
    job_id  bigint PRIMARY KEY REFERENCES jobs (id) ON DELETE CASCADE,
    report  text NOT NULL CHECK (char_length(report) <= 20000)
);
