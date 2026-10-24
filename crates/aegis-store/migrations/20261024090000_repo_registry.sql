-- Repo registry (#24): how many of a repo's jobs may run at once, applied
-- by the queue's claim (#23). The rest of a repo's settings (image, Sonar,
-- allow-list, overrides) stays in `config`, read by aegis-lifecycle.
-- Named by timestamp so migrations of parallel branches do not collide.

ALTER TABLE repos
    ADD COLUMN max_concurrent_jobs integer NOT NULL DEFAULT 1
    CHECK (max_concurrent_jobs >= 1);
