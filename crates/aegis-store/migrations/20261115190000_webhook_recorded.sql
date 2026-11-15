-- A delivery that made no job but recorded something (#456): a merged agent
-- PR's memory. It was logged as `queued`, which said a job was made.
-- Expand only: an older server never writes it.

ALTER TABLE webhook_events DROP CONSTRAINT webhook_events_outcome_check;
ALTER TABLE webhook_events ADD CONSTRAINT webhook_events_outcome_check
    CHECK (outcome IN (
        'queued', 'already_live', 'refused', 'duplicate',
        'ignored', 'ping', 'malformed', 'bad_signature', 'recorded'));
