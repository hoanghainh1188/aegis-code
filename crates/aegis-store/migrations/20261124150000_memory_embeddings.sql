-- Embeddings of repository memories for semantic recall (#655). Vectors
-- are plain `real[]`, so this table exists on every PostgreSQL. Where the
-- `vector` extension (pgvector) is installed, the server searches them
-- through an HNSW index on `embedding::vector(<dimensions>)`, created at
-- runtime for the dimensions its modes use; without it, recall stays
-- lexical (#596).

CREATE TABLE repo_memory_embeddings (
    memory_id   bigint PRIMARY KEY REFERENCES repo_memories (id) ON DELETE CASCADE,
    model       text NOT NULL CHECK (char_length(btrim(model)) BETWEEN 1 AND 200),
    dimensions  integer NOT NULL CHECK (dimensions BETWEEN 1 AND 2000),
    embedding   real[] NOT NULL CHECK (cardinality(embedding) = dimensions),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX repo_memory_embeddings_model_idx
    ON repo_memory_embeddings (model, dimensions);
