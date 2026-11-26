-- A shadow job's PR, opened by a person from its patch (#699): a job of
-- its own that points at the shadow job it opens the PR of. It runs no
-- agent, and it is not a shadow job even while its repo is in shadow mode.
ALTER TABLE jobs ADD COLUMN publish_of bigint REFERENCES jobs (id) ON DELETE SET NULL;
CREATE INDEX jobs_publish_of ON jobs (publish_of) WHERE publish_of IS NOT NULL;

CREATE OR REPLACE FUNCTION jobs_take_repo_shadow() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.publish_of IS NOT NULL THEN
        NEW.shadow := false;
        RETURN NEW;
    END IF;
    NEW.shadow := NEW.shadow OR COALESCE(
        (SELECT (config ->> 'shadow')::boolean FROM repos WHERE id = NEW.repo_id),
        false);
    RETURN NEW;
END
$$;
