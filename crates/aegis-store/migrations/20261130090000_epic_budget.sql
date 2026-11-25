-- A feature's own budget (#675, ADR 0019): a ceiling over what the job
-- that planned it and all its sub-jobs spend, on top of each job's cap.
ALTER TABLE jobs ADD COLUMN epic_budget_usd double precision
    CHECK (epic_budget_usd >= 0);
ALTER TABLE jobs ADD CONSTRAINT jobs_epic_budget_on_parent
    CHECK (epic_budget_usd IS NULL OR parent_job_id IS NULL);

-- What a feature spent: the cost in the ledgers of the job that planned
-- it and of its sub-jobs, whenever it was recorded.
CREATE FUNCTION epic_spent(parent bigint) RETURNS double precision
LANGUAGE sql STABLE AS $$
    SELECT coalesce(sum(e.cost_usd), 0)
    FROM ledger_events e
    WHERE e.ledger_key IN (SELECT ledger_key FROM jobs
                           WHERE id = parent OR parent_job_id = parent)
$$;
