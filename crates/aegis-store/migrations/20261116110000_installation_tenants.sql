-- Each GitHub App installation belongs to at most one tenant (ADR 0017
-- §6, #541): a platform admin binds it, and no token is minted for an
-- installation that is not bound to the tenant of the repo asking.
-- Expand only: new installations arrive unbound.

ALTER TABLE github_installations
    ADD COLUMN tenant_id bigint REFERENCES tenants (id) ON DELETE RESTRICT;

CREATE INDEX github_installations_tenant ON github_installations (tenant_id);
