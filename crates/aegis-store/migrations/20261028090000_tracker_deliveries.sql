-- Tickets from Jira and Backlog (#26) are found by polling a filter, not
-- sent by webhook, but they are taken the same way: a "delivery" whose ID
-- is `<repo id>:<ticket key>`, written with the job in one transaction, so
-- a ticket the filter keeps selecting gets one job, whichever worker polls.
-- Expand only: an older server never writes these sources.
-- Named by timestamp so migrations of parallel branches do not collide.

ALTER TABLE webhook_deliveries
    DROP CONSTRAINT webhook_deliveries_source_check,
    ADD CONSTRAINT webhook_deliveries_source_check
        CHECK (source IN ('github', 'gitlab', 'jira', 'backlog'));

-- When each poll is due (ADR 0003 §4): a process polls only after it
-- moved `next_at` on itself, so of many processes one polls each period.
CREATE TABLE polls (
    name    text PRIMARY KEY CHECK (length(name) BETWEEN 1 AND 64),
    next_at timestamptz NOT NULL
);
