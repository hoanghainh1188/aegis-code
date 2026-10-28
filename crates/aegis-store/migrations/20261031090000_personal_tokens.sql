-- Personal API tokens (#231): a person makes one on the dashboard for the
-- developer client, named, and it ends. Expand only: service tokens keep
-- no label and no end.

ALTER TABLE api_tokens
    ADD COLUMN label      text CHECK (label IS NULL OR length(label) BETWEEN 1 AND 64),
    ADD COLUMN expires_at timestamptz;

CREATE INDEX api_tokens_user ON api_tokens (user_id) WHERE revoked_at IS NULL;
