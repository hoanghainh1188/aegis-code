-- A repo's budget per month (#31), on top of each job's own cap ($4, ADR
-- 0005). NULL: no monthly cap. What a repo spent in a month is the sum of
-- `ledger_events.cost_usd` of its jobs' ledgers recorded in that month;
-- the month starts in the time zone the server is configured with.

ALTER TABLE repos
    ADD COLUMN monthly_budget_usd double precision
    CHECK (monthly_budget_usd IS NULL OR monthly_budget_usd >= 0);

-- The spend of the month is read at every claim.
CREATE INDEX ledger_events_recorded ON ledger_events (recorded_at);

-- What `repo` spent since the start of the current month in `zone`.
CREATE FUNCTION repo_month_spent(repo bigint, zone text) RETURNS double precision
LANGUAGE sql STABLE AS $$
    SELECT coalesce(sum(e.cost_usd), 0)
    FROM ledger_events e JOIN jobs j ON j.ledger_key = e.ledger_key
    WHERE j.repo_id = repo
      AND e.recorded_at >= date_trunc('month', now() AT TIME ZONE zone) AT TIME ZONE zone
$$;
