-- The instance's GitHub App installations (ADR 0017 §5, #540), from the
-- app's `installation` and `installation_repositories` events: which
-- account each is on, which repos it was given, and whether it was
-- suspended or removed. A repo on the app path whose installation is
-- suspended or removed takes no job. Expand only: an older server never
-- reads it, and never writes the new webhook outcome.

CREATE TABLE github_installations (
    id bigint PRIMARY KEY CHECK (id > 0),
    -- The org or user it is installed on, as GitHub spells it.
    account text NOT NULL CHECK (length(account) BETWEEN 1 AND 255),
    repository_selection text NOT NULL CHECK (repository_selection IN ('all', 'selected')),
    -- `owner/name`, lower case, as events named them.
    repositories text[] NOT NULL DEFAULT '{}',
    suspended boolean NOT NULL DEFAULT false,
    removed boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX github_installations_account ON github_installations (lower(account));

ALTER TABLE webhook_events DROP CONSTRAINT webhook_events_outcome_check;
ALTER TABLE webhook_events ADD CONSTRAINT webhook_events_outcome_check
    CHECK (outcome IN (
        'queued', 'already_live', 'refused', 'duplicate',
        'ignored', 'ping', 'malformed', 'bad_signature', 'recorded', 'installation'));
