-- The last job of a ticket, looked up when its ticket is closed (#657).
CREATE INDEX jobs_repo_issue ON jobs (repo_id, issue_ref, id);
