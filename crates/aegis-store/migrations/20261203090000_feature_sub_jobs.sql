-- A feature's sub-jobs made from its approved plan (#678, ADR 0019 §4):
-- each knows which of the plan's sub-jobs it is, by the plan's own id for
-- it, unique under its parent but for the jobs run again in its place.
ALTER TABLE jobs ADD COLUMN plan_key text
    CHECK (plan_key IS NULL OR (parent_job_id IS NOT NULL AND plan_key ~ '^[a-z0-9-]{1,32}$'));
