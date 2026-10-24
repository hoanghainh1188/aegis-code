-- Webhook deliveries (ADR 0003 §4, #25): GitHub and GitLab retry a
-- delivery and may send it twice. Its ID (`X-GitHub-Delivery`,
-- `X-Gitlab-Event-UUID`) is written in the same transaction as the job it
-- creates, so a second copy creates nothing. Only deliveries that ask for
-- a job are kept.
-- Named by timestamp so migrations of parallel branches do not collide.

CREATE TABLE webhook_deliveries (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source      text NOT NULL CHECK (source IN ('github', 'gitlab')),
    delivery_id text NOT NULL CHECK (length(delivery_id) BETWEEN 1 AND 128),
    -- `issues`, `workflow_run`, `Issue Hook`, `Pipeline Hook`.
    event       text NOT NULL,
    -- The job created, or the live job of the same issue that was there.
    job_id      bigint REFERENCES jobs (id),
    received_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source, delivery_id)
);
