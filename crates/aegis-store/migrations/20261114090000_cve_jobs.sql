-- CVE jobs (#502, ADR 0015): a maintainer asks the platform to raise one
-- package to the version that fixes an advisory. The target is the job's
-- `issue_ref`, `cve:<advisory> <package> <version>`, checked again by the
-- worker. Expand only: an older server never sees a `cve` job it would
-- not know.

ALTER TABLE jobs DROP CONSTRAINT jobs_source_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_source_check
    CHECK (source IN ('cli', 'github', 'gitlab', 'jira', 'backlog', 'sonar', 'ci', 'review', 'cve'));
