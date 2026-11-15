-- ITSM intake and SLA metadata (#453, ADR 0011 amendment): accept Jira Service
-- Management (`jsm`) and ServiceNow (`servicenow`) tracker sources and store
-- mapped SLA priority (`P1`–`P3`) and deadline (`sla_deadline`) on jobs.
-- Expand only (#176).

ALTER TABLE jobs DROP CONSTRAINT jobs_source_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_source_check
    CHECK (source IN (
        'cli', 'github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow',
        'sonar', 'ci', 'review', 'cve'
    ));

ALTER TABLE webhook_deliveries DROP CONSTRAINT webhook_deliveries_source_check;
ALTER TABLE webhook_deliveries ADD CONSTRAINT webhook_deliveries_source_check
    CHECK (source IN ('github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow'));

ALTER TABLE webhook_events DROP CONSTRAINT webhook_events_source_check;
ALTER TABLE webhook_events ADD CONSTRAINT webhook_events_source_check
    CHECK (source IN ('github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow'));

ALTER TABLE jobs
    ADD COLUMN priority text CHECK (priority IN ('P1', 'P2', 'P3')),
    ADD COLUMN sla_deadline timestamptz;
