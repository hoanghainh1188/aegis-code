-- The job a webhook delivery made or found (#373), so the repo's page
-- links to it: the job queued, the live job of the same issue, or the job
-- recorded cancelled for a spent budget. A job deleted with its repo
-- leaves the row without one. Expand only: an older server never writes it.
-- Named by timestamp so migrations of parallel branches do not collide.

ALTER TABLE webhook_events
    ADD COLUMN job_id bigint REFERENCES jobs (id) ON DELETE SET NULL;
