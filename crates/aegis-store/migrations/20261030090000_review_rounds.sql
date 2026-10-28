-- Review rounds (#243): a job that goes on with an agent's PR after a
-- review, on its branch, linked to the job that opened the PR. Expand
-- only: an older server reads none of this.

ALTER TABLE jobs DROP CONSTRAINT jobs_source_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_source_check
    CHECK (source IN ('cli', 'github', 'gitlab', 'jira', 'backlog', 'sonar', 'ci', 'review'));

ALTER TABLE jobs
    -- The job that opened the PR this round works on.
    ADD COLUMN review_of    bigint REFERENCES jobs (id),
    -- 1 for the PR's first review round; 0 for every other job.
    ADD COLUMN review_round integer NOT NULL DEFAULT 0 CHECK (review_round >= 0),
    ADD CONSTRAINT jobs_review_round CHECK ((review_of IS NULL) = (review_round = 0));

CREATE INDEX jobs_review_of ON jobs (review_of) WHERE review_of IS NOT NULL;
