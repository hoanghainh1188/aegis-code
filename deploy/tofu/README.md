# Hạ tầng aegis-server (OpenTofu)

Vận hành, triển khai, rollback: [docs/runbook/README.md](../../docs/runbook/README.md).

| Thư mục | Nội dung |
| --- | --- |
| `modules/aegis` | Một bản triển khai: VPC, EC2 có Docker (vai trò `all`), RDS PostgreSQL, ALB + ACM, chỗ cho secret trong Secrets Manager (không giá trị), SSM parameter tag image, CloudWatch Logs, IAM của máy |
| `envs/staging` | Staging, chế độ `poc`, mặc định `ap-northeast-1` |
| `envs/customer-jp` | Repo khách, vùng viết cứng `ap-northeast-1`, RDS Multi-AZ |
| `bootstrap` | Một lần mỗi tài khoản: OIDC của GitHub, role `aegis-ci-plan`/`-image`/`-apply`, ECR |

State nằm trên S3, khóa bằng file lock (`use_lockfile`); `bucket`/`key`/`region` truyền qua `-backend-config` (mẫu: `backend.hcl.example`). Không commit state, `backend.hcl`, hay `*.tfvars` có giá trị thật (`.gitignore`); `example.tfvars` chỉ có giá trị mẫu.

Kiểm cấu hình không cần tài khoản AWS (CI chạy đúng script này):

```bash
deploy/tofu/check.sh
```

`fmt -check`, `tflint`, `validate`, rồi `plan` từng root với `example.tfvars`, `-var offline_plan=true` (provider bỏ kiểm credentials), backend local tạm, `-refresh=false`. Không có data source nào gọi API (AMI đọc qua `resolve:ssm:` lúc tạo máy, tài khoản truyền qua biến) nên `plan` này đầy đủ, nhưng không dùng để `apply`.

Với tài khoản thật: `tofu init -backend-config=backend.hcl`, `tofu plan -var-file=<env>.tfvars`. `apply` chỉ chạy qua workflow (GitHub environment có người duyệt).
