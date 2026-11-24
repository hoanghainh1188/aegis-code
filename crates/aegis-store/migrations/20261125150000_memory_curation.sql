-- Curation of a repo's memory (#658): a maintainer marks a recalled
-- memory obsolete from its job's page, and recall leaves it out; the
-- memories each job was given are kept, for that page.

ALTER TABLE repo_memories
    ADD COLUMN obsolete_at timestamptz,
    ADD COLUMN obsolete_by text;

CREATE TABLE job_recalled_memories (
    job_id     bigint NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    memory_id  bigint NOT NULL REFERENCES repo_memories (id) ON DELETE CASCADE,
    -- Its place among the job's memories, from 1.
    rank       integer NOT NULL CHECK (rank > 0),
    recalled_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (job_id, memory_id)
);

CREATE INDEX job_recalled_memories_memory ON job_recalled_memories (memory_id);
