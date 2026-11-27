-- Error spikes from a repo's APM source (#706): a job of source `apm`
-- diagnoses each one before any ticket. `apm_anomalies` keeps what each
-- spike was, for the job to read when it starts, and is how a fingerprint
-- seen again is not diagnosed twice and how many a repo gets per day is
-- capped. Expand only (#176).

ALTER TABLE jobs DROP CONSTRAINT jobs_source_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_source_check
    CHECK (source IN (
        'cli', 'github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow',
        'sonar', 'ci', 'review', 'cve', 'apm'
    ));

CREATE TABLE apm_anomalies (
    repo_id bigint NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    -- The job's `issue_ref`: `apm-<ms>`.
    issue_ref text NOT NULL CHECK (char_length(issue_ref) BETWEEN 1 AND 64),
    -- `sentry:<issue>`, `datadog:<error kind>`.
    fingerprint text NOT NULL CHECK (char_length(fingerprint) BETWEEN 1 AND 300),
    title text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 300),
    body text NOT NULL CHECK (char_length(body) BETWEEN 1 AND 8000),
    found_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, issue_ref)
);
CREATE INDEX apm_anomalies_fingerprint ON apm_anomalies (repo_id, fingerprint, found_at);
CREATE INDEX apm_anomalies_found ON apm_anomalies (repo_id, found_at);
