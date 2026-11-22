-- A repo's mode is the name of one of the server's `[modes.<name>]`
-- (#591), not only `poc` or `customer-jp`: a self-hosted instance names
-- its own (`customer-eu`, `local-airgap`, …). Expand only: every row
-- `poc` or `customer-jp` passes the new rule, and an older binary
-- still reads every row it could have written.

ALTER TABLE repos DROP CONSTRAINT IF EXISTS repos_mode_check;
ALTER TABLE repos
    ADD CONSTRAINT repos_mode_check
    CHECK (mode ~ '^[a-z0-9][a-z0-9_-]{0,63}$');
