-- Sign-in, roles and the audit log (ADR 0003 §5, #29).
-- Named by timestamp so migrations of parallel branches do not collide.

-- Who acts: a person signed in through the company SSO, or a service
-- (connector, CI) with API tokens.
CREATE TABLE users (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind         text NOT NULL CHECK (kind IN ('person', 'service')),
    -- A person: the e-mail the SSO gives, lower case. A service: its name.
    name         text NOT NULL CHECK (length(name) BETWEEN 1 AND 320 AND name = lower(name)),
    -- The SSO's `sub`, set at the first sign-in; a person may be granted
    -- roles by e-mail before.
    oidc_subject text UNIQUE,
    is_admin     boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (kind, name),
    CHECK (kind = 'person' OR oidc_subject IS NULL)
);

-- What a user may do in a repo; an admin may do everything everywhere.
-- maintainer ⊃ reviewer ⊃ viewer.
CREATE TABLE repo_roles (
    user_id    bigint NOT NULL REFERENCES users (id),
    repo_id    bigint NOT NULL REFERENCES repos (id),
    role       text NOT NULL CHECK (role IN ('maintainer', 'reviewer', 'viewer')),
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, repo_id)
);

-- Dashboard sessions. The cookie holds the token; only its SHA-256 is
-- kept, so a copy of this table signs no one in.
CREATE TABLE sessions (
    token_hash bytea PRIMARY KEY CHECK (length(token_hash) = 32),
    user_id    bigint NOT NULL REFERENCES users (id),
    -- Sent back by every form of the session (`form_guard.html`).
    csrf_token text NOT NULL CHECK (length(csrf_token) >= 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX sessions_expires ON sessions (expires_at);
CREATE INDEX sessions_user ON sessions (user_id);

-- API tokens of services, by SHA-256 as sessions.
CREATE TABLE api_tokens (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id    bigint NOT NULL REFERENCES users (id),
    token_hash bytea NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);

-- Who did what, written in the same transaction as what was done.
-- Append-only: rows are never changed or removed.
CREATE TABLE audit_log (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at         timestamptz NOT NULL DEFAULT now(),
    -- `user` (dashboard session), `token` (API), `cli`, `webhook`, `system`.
    actor_kind text NOT NULL CHECK (actor_kind IN ('user', 'token', 'cli', 'webhook', 'system')),
    -- users.id for `user` and `token`.
    actor_id   bigint,
    -- An e-mail, a service name, `github`, …: kept as it was then.
    actor_name text NOT NULL,
    -- `job.create`, `job.cancel`, `job.retry`, `review.submit`,
    -- `repo.register`, `role.grant`, `role.revoke`, `token.create`, `token.revoke`,
    -- `session.login`, `session.logout`, `session.revoke`, `user.create`,
    -- `user.link`.
    action     text NOT NULL,
    repo_id    bigint,
    job_id     bigint,
    detail     jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(detail) = 'object')
);

CREATE INDEX audit_log_repo ON audit_log (repo_id, id);
CREATE INDEX audit_log_actor ON audit_log (actor_kind, actor_id, id);

CREATE FUNCTION audit_log_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log is append-only';
END
$$;

CREATE TRIGGER audit_log_append_only
    BEFORE UPDATE OR DELETE OR TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION audit_log_append_only();
