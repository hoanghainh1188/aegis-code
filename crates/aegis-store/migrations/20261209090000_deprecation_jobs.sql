-- A repo's deprecation warnings, fixed as a batch on its schedule (#705):
-- jobs of source `deprecation`. The warnings each batch took are in its
-- ledger (`deprecations_taken`), read by the next one to take only new
-- ones. Expand only (#176).

ALTER TABLE jobs DROP CONSTRAINT jobs_source_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_source_check
    CHECK (source IN (
        'cli', 'github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow',
        'sonar', 'ci', 'review', 'cve', 'apm', 'deprecation'
    ));

CREATE INDEX ledger_events_deprecations ON ledger_events (ledger_key)
    WHERE kind = 'deprecations_taken';
