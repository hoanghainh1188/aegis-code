-- What became of each webhook delivery (#331), the ignored ones too: the
-- repo's connection page says when the forge last reached the server and
-- why a delivery made no job. Never the delivery's content: its ID and
-- event as the forge named them, the repo it was matched to, the outcome
-- and a short reason. A delivery with a bad signature is not trusted to
-- name either, so it has none. Rows older than 30 days are pruned on
-- insert. Expand only.

CREATE TABLE webhook_events (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source      text NOT NULL CHECK (source IN ('github', 'gitlab')),
    delivery_id text CHECK (char_length(delivery_id) <= 128),
    event       text CHECK (char_length(event) <= 128),
    repo_id     bigint REFERENCES repos (id) ON DELETE CASCADE,
    outcome     text NOT NULL CHECK (outcome IN (
                    'queued', 'already_live', 'refused', 'duplicate',
                    'ignored', 'ping', 'malformed', 'bad_signature')),
    reason      text CHECK (char_length(reason) <= 500),
    received_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX webhook_events_repo ON webhook_events (repo_id, received_at DESC);
CREATE INDEX webhook_events_received ON webhook_events (received_at);
