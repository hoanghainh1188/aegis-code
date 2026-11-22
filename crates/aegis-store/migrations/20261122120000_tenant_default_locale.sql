-- The language a tenant's repos write in when a repo names none (#589):
-- `en`, `ja` or `vi`; NULL: the server's `[web] default_locale`. Expand
-- only: an older server ignores the column.

ALTER TABLE tenants
    ADD COLUMN default_locale text
    CHECK (default_locale IS NULL OR default_locale IN ('en', 'ja', 'vi'));
