# Một bản triển khai aegis-server (ADR 0003 §6): một EC2 có Docker chạy
# vai trò `all`, RDS PostgreSQL, secret trong Secrets Manager, ALB có TLS.
# Mọi tài nguyên ở `var.region`; với `customer_jp` vùng đó là
# ap-northeast-1.

terraform {
  required_version = ">= 1.10.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }
}

locals {
  prefix = "aegis-${var.name}"
  # Secret X của server là `<secret_prefix>X` (config/server.toml [secrets]).
  secret_prefix = "aegis/${var.name}/"
  # Đường dẫn workspace phải trùng trong container và trên máy: gate gửi
  # đường dẫn này cho daemon Docker của máy khi mount `/work` vào sandbox.
  workspace_dir = "/srv/aegis/.workspace"
  # uid:gid của user trong image (docker/server/Dockerfile).
  aegis_uid = 10001
  tags      = merge(var.tags, { "aegis:deployment" = var.name })
}

resource "terraform_data" "residency" {
  lifecycle {
    precondition {
      condition     = !var.customer_jp || var.region == "ap-northeast-1"
      error_message = "customer_jp: dữ liệu repo khách chỉ được nằm ở ap-northeast-1 (ADR 0003 §6)."
    }
    precondition {
      condition     = startswith(var.image_repository, "${var.aws_account_id}.dkr.ecr.${var.region}.")
      error_message = "image_repository: ECR của cùng tài khoản và cùng vùng."
    }
  }
}

resource "aws_cloudwatch_log_group" "server" {
  name              = "/aegis/${var.name}/server"
  retention_in_days = var.log_retention_days
  tags              = local.tags
}

# Tag image đang chạy. Workflow deploy (và rollback) đổi giá trị rồi gọi
# `aegis-deploy` trên máy qua SSM; OpenTofu chỉ tạo lần đầu.
resource "aws_ssm_parameter" "image_tag" {
  name  = "/aegis/${var.name}/image_tag"
  type  = "String"
  value = var.initial_image_tag
  tags  = local.tags
  lifecycle {
    ignore_changes = [value]
  }
}
