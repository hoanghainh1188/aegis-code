-- A feature's plans (#677, ADR 0019): each revision the Architect phase
-- wrote, as JSON, the last one the one a person approves.
CREATE TABLE feature_plans (
    job_id     bigint NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    revision   integer NOT NULL CHECK (revision >= 1),
    plan       jsonb NOT NULL CHECK (octet_length(plan::text) <= 200000),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (job_id, revision)
);
