-- An error spike's report, posted and turned into a fix (#706, 3/3): the
-- ticket it was posted to, so `/aegis fix` there finds it. A spike's fix
-- job is an `apm` job whose `issue_ref` is the spike's with `-fix`.
-- Expand only (#176).

ALTER TABLE apm_anomalies ADD COLUMN ticket_ref text
    CHECK (ticket_ref IS NULL OR char_length(ticket_ref) BETWEEN 1 AND 300);
CREATE UNIQUE INDEX apm_anomalies_ticket ON apm_anomalies (repo_id, ticket_ref)
    WHERE ticket_ref IS NOT NULL;
