-- Pausing a repo (#374): while `paused_at` is set its queued jobs are not
-- claimed but wait, saying why; jobs are still recorded, and running ones
-- end as they would. Who paused it and why, kept as said at the time.
-- Expand only: an older server ignores the columns, and runs the jobs.
-- Named by timestamp so migrations of parallel branches do not collide.

ALTER TABLE repos
    ADD COLUMN paused_at    timestamptz,
    ADD COLUMN paused_by    text CHECK (char_length(paused_by) <= 320),
    ADD COLUMN pause_reason text CHECK (char_length(pause_reason) <= 500);
