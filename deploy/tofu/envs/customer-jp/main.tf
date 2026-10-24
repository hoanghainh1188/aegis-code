# Môi trường `customer-jp` (#33). Cách chạy: deploy/tofu/README.md.

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
  region              = "ap-northeast-1"
  allowed_account_ids = var.offline_plan ? null : [var.aws_account_id]
  # `offline_plan`: kiểm cấu hình khi chưa có tài khoản AWS (CI, máy dev).
  # Không gọi API nào; kết quả chỉ để xem, không dùng để apply.
  skip_credentials_validation = var.offline_plan
  skip_requesting_account_id  = var.offline_plan
  skip_metadata_api_check     = var.offline_plan
  default_tags {
    tags = {
      "aegis:environment" = "customer-jp"
      "managed-by"        = "opentofu"
    }
  }
}

# customer-jp: repo khách, mọi dữ liệu ở ap-northeast-1 (ADR 0003 §6).
# Vùng viết cứng ở provider, module kiểm lại bằng precondition.
module "aegis" {
  source = "../../modules/aegis"

  name               = "customer-jp"
  region             = "ap-northeast-1"
  aws_account_id     = var.aws_account_id
  customer_jp        = true
  enable_poc         = var.enable_poc
  availability_zones = ["ap-northeast-1a", "ap-northeast-1c"]
  domain_name        = var.domain_name
  route53_zone_id    = var.route53_zone_id
  image_repository   = var.image_repository
  repo_secret_names  = var.repo_secret_names
  server_config      = var.server_config

  db_multi_az              = true
  db_backup_retention_days = 14
  log_retention_days       = 90
}
