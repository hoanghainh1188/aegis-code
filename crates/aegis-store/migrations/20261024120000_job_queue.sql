-- The job queue (ADR 0003 §3, #23): workers claim queued jobs with
-- `FOR UPDATE SKIP LOCKED`, hold a lease they renew, and requeue a job
-- after an infrastructure failure or an expired lease.

ALTER TABLE jobs
    -- Not claimed before this: the backoff after an infrastructure failure.
    ADD COLUMN run_after        timestamptz NOT NULL DEFAULT now(),
    -- Infrastructure failures and expired leases so far; at most 3 retry.
    ADD COLUMN retries          integer NOT NULL DEFAULT 0 CHECK (retries >= 0),
    -- Asked to stop; the worker holding the lease sees it when it renews.
    ADD COLUMN cancel_requested boolean NOT NULL DEFAULT false,
    -- Why the job ended: the agent's stop reason, the failure, the cancel.
    ADD COLUMN reason           text,
    ADD COLUMN pr_url           text,
    -- The job this one runs again (`POST /api/v1/jobs/{id}/retry`).
    ADD COLUMN retry_of         bigint REFERENCES jobs (id);

CREATE INDEX jobs_claimable ON jobs (id) WHERE status = 'queued';
CREATE INDEX jobs_leased ON jobs (locked_until) WHERE status = 'running';
