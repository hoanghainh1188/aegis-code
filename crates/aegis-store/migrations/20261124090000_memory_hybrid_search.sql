-- Hybrid recall of Incident & Domain Memory (#596): every row of a repo is
-- searched through indexes, not only its 200 newest, and a memory can be
-- shared with the other repos of its repo's tenant. Expand-only: columns
-- are added, none is changed or dropped.

-- Trigram indexes for substring search (Japanese and Vietnamese text has
-- no spaces between, or between all, its words). pg_trgm ships with
-- PostgreSQL and is a trusted extension: the database's owner may create it.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- What a stored path matches in an issue: the path, its file name and the
-- file name's stem, lowercased (`src/Webhook_Sender.rs` gives
-- `src/webhook_sender.rs`, `webhook_sender.rs`, `webhook_sender`).
CREATE FUNCTION aegis_memory_path_keys(paths text[]) RETURNS text[]
    LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT
AS $$
    SELECT coalesce(array_agg(DISTINCT k ORDER BY k), '{}')
      FROM unnest(paths) AS p,
           LATERAL (SELECT lower(btrim(p)) AS path) AS l,
           LATERAL unnest(ARRAY[
               l.path,
               regexp_replace(l.path, '^.*/', ''),
               split_part(regexp_replace(l.path, '^.*/', ''), '.', 1)
           ]) AS k
     WHERE char_length(k) >= 3
$$;

-- `repo`: recalled for its own repo only. `tenant`: also for every other
-- repo of the tenant its repo belongs to, read from `repos.tenant_id` when
-- recalled, so a repo moved to another tenant takes its memories with it.
ALTER TABLE repo_memories
    ADD COLUMN scope text NOT NULL DEFAULT 'repo' CHECK (scope IN ('repo', 'tenant')),
    ADD COLUMN path_keys text[] GENERATED ALWAYS AS (aegis_memory_path_keys(paths)) STORED,
    ADD COLUMN search_text text GENERATED ALWAYS AS (
        title || ' ' || keywords || ' ' || summary
    ) STORED,
    ADD COLUMN search_tsv tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple'::regconfig, title), 'A')
            || setweight(to_tsvector('simple'::regconfig, keywords), 'A')
            || setweight(to_tsvector('simple'::regconfig, summary), 'B')
    ) STORED;

CREATE INDEX repo_memories_path_keys_idx ON repo_memories USING GIN (path_keys);
CREATE INDEX repo_memories_search_tsv_idx ON repo_memories USING GIN (search_tsv);
CREATE INDEX repo_memories_search_trgm_idx
    ON repo_memories USING GIN (search_text gin_trgm_ops);
CREATE INDEX repo_memories_tenant_scope_idx
    ON repo_memories (repo_id) WHERE scope = 'tenant';
