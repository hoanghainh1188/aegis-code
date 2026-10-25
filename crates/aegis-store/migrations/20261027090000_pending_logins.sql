-- Sign-ins on their way through the SSO (#174): from `/auth/login` to
-- `/auth/callback`. The browser's cookie holds only a random id; its
-- SHA-256 finds the row, so neither the PKCE verifier nor the nonce ever
-- leaves the server. A row is taken once, whatever the callback's outcome.
CREATE TABLE pending_logins (
    id_hash    bytea PRIMARY KEY CHECK (length(id_hash) = 32),
    state      text NOT NULL,
    nonce      text NOT NULL,
    verifier   text NOT NULL,
    -- The local path to go on to after signing in.
    next       text NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX pending_logins_expires ON pending_logins (expires_at);
