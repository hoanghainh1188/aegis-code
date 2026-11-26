-- Shadow mode for a pilot (#699): a repo whose settings say `shadow`
-- runs its jobs to the end of the gate, but they push no branch, open no
-- PR and write nothing on the ticket. A job that would have opened a PR
-- ends `shadowed`, its patch kept for its page.
--
-- A job is a shadow job when its repo is in shadow mode as it is queued:
-- turning the mode off does not let a queued shadow job write. Set here,
-- on insert, so every way a job is queued (API, webhooks, trackers,
-- Sonar, CVE, a feature's parts, retries) takes it alike.
ALTER TABLE jobs ADD COLUMN shadow boolean NOT NULL DEFAULT false;

CREATE FUNCTION jobs_take_repo_shadow() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.shadow := NEW.shadow OR COALESCE(
        (SELECT (config ->> 'shadow')::boolean FROM repos WHERE id = NEW.repo_id),
        false);
    RETURN NEW;
END
$$;

CREATE TRIGGER jobs_take_repo_shadow
    BEFORE INSERT ON jobs
    FOR EACH ROW EXECUTE FUNCTION jobs_take_repo_shadow();

ALTER TABLE jobs DROP CONSTRAINT jobs_outcome_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_outcome_check
    CHECK (outcome IN ('pr_opened', 'stopped_budget', 'stopped_steps', 'stopped_expert_fail',
                       'diagnosed', 'awaiting_approval', 'integrated', 'shadowed'));
