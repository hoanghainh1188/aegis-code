# Infrastructure Provisioning: aegis-server (OpenTofu)

For operations, deployment, and rollback guides, refer to [AWS Operations Runbook](../../docs/runbook/README.md).

| Directory | Scope & Contents |
| --- | --- |
| `modules/aegis` | Complete deployment module: VPC, EC2 host with Docker (`all` role), RDS PostgreSQL, ALB + ACM certificate, Secrets Manager placeholders (empty values), SSM parameter for image tags, CloudWatch Logs, instance IAM roles; dedicated monitoring EC2 (Prometheus, Alertmanager, Grafana, SNS) consuming rules from `deploy/alerts/` and dashboards from `deploy/grafana/` |
| `envs/staging` | Staging environment, `poc` mode, defaults to `ap-northeast-1` |
| `envs/customer-jp` | Customer production environment, region hardcoded to `ap-northeast-1` (Tokyo), Multi-AZ RDS |
| `bootstrap` | One-time setup per AWS account: GitHub OIDC provider, IAM roles `aegis-ci-plan`/`-image`/`-apply`, ECR repositories |

State is stored in an S3 bucket with native state locking (`use_lockfile`); `bucket`, `key`, and `region` are passed via `-backend-config` (template: `backend.hcl.example`). Never commit state files, `backend.hcl`, or real `*.tfvars` files (`.gitignore` enforced); `example.tfvars` contains only mock schema templates.

Validate configuration locally without requiring AWS credentials (CI executes this exact script):

```bash
deploy/tofu/check.sh
```

Executes `tofu fmt -check`, `tflint`, `tofu validate`, and generates dry-run execution plans across roots with `example.tfvars`, `-var offline_plan=true` (bypasses credential verification), temporary local backend, and `-refresh=false`. All data sources avoid runtime API lookups (AMIs resolve via SSM aliases during boot; account IDs pass via variables), ensuring comprehensive validation offline.

For real AWS accounts: run `tofu init -backend-config=backend.hcl`, followed by `tofu plan -var-file=<env>.tfvars`. The `apply` action runs strictly via CI/CD workflows under GitHub protected environments with mandatory reviewers.
