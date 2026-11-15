-- Pausing every agent of the instance at once (#510), for an incident
-- that is not one repo's: at most one row, present while paused. `intake`:
-- no queued job is claimed, jobs are still recorded; `emergency`: the
-- running ones are asked to cancel too. Who paused it and why, kept as
-- said at the time.
-- Expand only: an older server ignores the table, and runs the jobs.

CREATE TABLE instance_pause (
    id        boolean     PRIMARY KEY DEFAULT true CHECK (id),
    level     text        NOT NULL CHECK (level IN ('intake', 'emergency')),
    paused_at timestamptz NOT NULL DEFAULT now(),
    paused_by text        NOT NULL CHECK (char_length(paused_by) <= 320),
    reason    text        CHECK (char_length(reason) <= 500)
);
