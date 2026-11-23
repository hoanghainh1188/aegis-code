-- Webhook deliveries from Bitbucket Data Center (#635).

ALTER TABLE webhook_deliveries DROP CONSTRAINT webhook_deliveries_source_check;
ALTER TABLE webhook_deliveries ADD CONSTRAINT webhook_deliveries_source_check
    CHECK (source IN (
        'github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow', 'bitbucket', 'bitbucket-dc'
    ));

ALTER TABLE webhook_events DROP CONSTRAINT webhook_events_source_check;
ALTER TABLE webhook_events ADD CONSTRAINT webhook_events_source_check
    CHECK (source IN (
        'github', 'gitlab', 'jira', 'backlog', 'jsm', 'servicenow', 'bitbucket', 'bitbucket-dc'
    ));
