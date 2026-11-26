-- A feature delivered as one integration PR (#678, ADR 0019 §4): each
-- sub-job ends `integrated`, merged into the feature's integration branch
-- with no PR of its own; the job that planned it opens the one PR.
ALTER TABLE jobs DROP CONSTRAINT jobs_outcome_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_outcome_check
    CHECK (outcome IN ('pr_opened', 'stopped_budget', 'stopped_steps', 'stopped_expert_fail',
                       'diagnosed', 'awaiting_approval', 'integrated'));
