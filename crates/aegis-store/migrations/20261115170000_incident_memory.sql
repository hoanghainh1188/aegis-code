-- Incident & Domain Memory per repository (#456): merged PR summaries,
-- review lessons (/aegis fix), operational runbooks, and business rules.

CREATE TABLE repo_memories (
    id          bigserial PRIMARY KEY,
    repo_id     bigint NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (
        kind IN ('merged_pr', 'review_lesson', 'runbook', 'business_rule')
    ),
    source_ref  text NOT NULL CHECK (
        char_length(btrim(source_ref)) > 0 AND char_length(source_ref) <= 256
    ),
    title       text NOT NULL CHECK (
        char_length(btrim(title)) > 0 AND char_length(title) <= 500
    ),
    summary     text NOT NULL CHECK (
        char_length(btrim(summary)) > 0 AND char_length(summary) <= 4000
    ),
    paths       text[] NOT NULL DEFAULT '{}',
    keywords    text NOT NULL DEFAULT '' CHECK (char_length(keywords) <= 2000),
    job_id      bigint REFERENCES jobs (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, kind, source_ref)
);

CREATE INDEX repo_memories_repo_created_idx
    ON repo_memories (repo_id, created_at DESC, id DESC);
