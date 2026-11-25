-- What the agent learns from more than merged PRs (#657): the approach a
-- reviewer rejected, and a read-only diagnosis an operator confirmed.

ALTER TABLE repo_memories DROP CONSTRAINT repo_memories_kind_check;
ALTER TABLE repo_memories ADD CONSTRAINT repo_memories_kind_check CHECK (
    kind IN (
        'merged_pr', 'review_lesson', 'runbook', 'business_rule',
        'rejected_pattern', 'confirmed_diagnosis'
    )
);
