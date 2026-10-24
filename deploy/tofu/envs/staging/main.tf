# Môi trường `staging` (#33). Cách chạy: deploy/tofu/README.md.

terraform {
  required_version = ">= 1.10.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }
  # bucket, key, region: `tofu init -backend-config=backend.hcl`
  # (mẫu: backend.hcl.example). Khóa bằng file lock trên S3.
  backend "s3" {
    use_lockfile = true
    encrypt      = true
  }
}

provider "aws" {
  region              = var.region
  allowed_account_ids = var.offline_plan ? null : [var.aws_account_id]
  # `offline_plan`: kiểm cấu hình khi chưa có tài khoản AWS (CI, máy dev).
  # Không gọi API nào; kết quả chỉ để xem, không dùng để apply.
  skip_credentials_validation = var.offline_plan
  skip_requesting_account_id  = var.offline_plan
  skip_metadata_api_check     = var.offline_plan
  default_tags {
    tags = {
      "aegis:environment" = "staging"
      "managed-by"        = "opentofu"
    }
  }
}

# Staging: thử bản mới trước khi lên customer-jp. Chỉ repo nội bộ, chế độ
# `poc`; không có dữ liệu repo khách (ADR 0003 §6).
module "aegis" {
  source = "../../modules/aegis"

  name               = "staging"
  region             = var.region
  aws_account_id     = var.aws_account_id
  customer_jp        = false
  enable_poc         = true
  availability_zones = var.availability_zones
  domain_name        = var.domain_name
  route53_zone_id    = var.route53_zone_id
  image_repository   = var.image_repository
  repo_secret_names  = var.repo_secret_names
  server_config      = var.server_config

  instance_type          = "m7i.large"
  workspace_volume_gb    = 100
  db_instance_class      = "db.t4g.small"
  db_deletion_protection = false
  log_retention_days     = 14
}
