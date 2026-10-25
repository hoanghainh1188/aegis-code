# Một lần cho mỗi tài khoản AWS, người quản trị chạy bằng quyền của mình
# (docs/runbook/README.md, "Chuẩn bị tài khoản"): OIDC của GitHub, các role
# CI dùng (không có khóa AWS lâu dài), và ECR cho image. Bucket state tạo
# trước bằng AWS CLI, rồi state của chính phần này cũng nằm ở đó.

terraform {
  required_version = ">= 1.10.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
  }
  backend "s3" {
    use_lockfile = true
    encrypt      = true
  }
}

provider "aws" {
  region                      = var.region
  allowed_account_ids         = var.offline_plan ? null : [var.aws_account_id]
  skip_credentials_validation = var.offline_plan
  skip_requesting_account_id  = var.offline_plan
  skip_metadata_api_check     = var.offline_plan
  default_tags {
    tags = { "managed-by" = "opentofu", "aegis:part" = "bootstrap" }
  }
}

locals {
  oidc_host = "token.actions.githubusercontent.com"
  repo      = "repo:${var.github_repository}"
  ecr_repos = concat(["aegis-server"], [for p in var.sandbox_profiles : "aegis-sandbox-${p}"])
  state_arn = "arn:aws:s3:::${var.state_bucket}"
  ecr_arns  = [for r in local.ecr_repos : "arn:aws:ecr:${var.region}:${var.aws_account_id}:repository/${r}"]
}

resource "aws_iam_openid_connect_provider" "github" {
  url            = "https://${local.oidc_host}"
  client_id_list = ["sts.amazonaws.com"]
}

resource "aws_ecr_repository" "image" {
  for_each             = toset(local.ecr_repos)
  name                 = each.key
  image_tag_mutability = "IMMUTABLE"
  image_scanning_configuration {
    scan_on_push = true
  }
  encryption_configuration {
    encryption_type = "AES256"
  }
}

# Giữ đủ bản cũ để rollback.
resource "aws_ecr_lifecycle_policy" "image" {
  for_each   = aws_ecr_repository.image
  repository = each.value.name
  policy = jsonencode({
    rules = [{
      rulePriority = 1
      description  = "Keep the last ${var.keep_images} images"
      selection    = { tagStatus = "any", countType = "imageCountMoreThan", countNumber = var.keep_images }
      action       = { type = "expire" }
    }]
  })
}

# --- Role `plan`: PR và nhánh main, chỉ đọc. ---------------------------

data "aws_iam_policy_document" "trust_plan" {
  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]
    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github.arn]
    }
    condition {
      test     = "StringEquals"
      variable = "${local.oidc_host}:aud"
      values   = ["sts.amazonaws.com"]
    }
    condition {
      test     = "StringLike"
      variable = "${local.oidc_host}:sub"
      values   = ["${local.repo}:pull_request", "${local.repo}:ref:refs/heads/main"]
    }
  }
}

resource "aws_iam_role" "plan" {
  name                 = "aegis-ci-plan"
  assume_role_policy   = data.aws_iam_policy_document.trust_plan.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy_attachment" "plan_read" {
  role       = aws_iam_role.plan.name
  policy_arn = "arn:aws:iam::aws:policy/ReadOnlyAccess"
}

data "aws_iam_policy_document" "plan_state" {
  statement {
    actions   = ["s3:ListBucket"]
    resources = [local.state_arn]
  }
  statement {
    actions   = ["s3:GetObject"]
    resources = ["${local.state_arn}/*"]
  }
  # Không đọc giá trị secret (ReadOnlyAccess không có GetSecretValue).
}

resource "aws_iam_role_policy" "plan_state" {
  name   = "tofu-state-read"
  role   = aws_iam_role.plan.id
  policy = data.aws_iam_policy_document.plan_state.json
}

# --- Role `image`: nhánh main đẩy image lên ECR. ------------------------

data "aws_iam_policy_document" "trust_image" {
  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]
    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github.arn]
    }
    condition {
      test     = "StringEquals"
      variable = "${local.oidc_host}:aud"
      values   = ["sts.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "${local.oidc_host}:sub"
      values   = ["${local.repo}:ref:refs/heads/main"]
    }
  }
}

resource "aws_iam_role" "image" {
  name               = "aegis-ci-image"
  assume_role_policy = data.aws_iam_policy_document.trust_image.json
}

data "aws_iam_policy_document" "image_push" {
  statement {
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }
  statement {
    actions = [
      "ecr:BatchCheckLayerAvailability", "ecr:BatchGetImage", "ecr:CompleteLayerUpload",
      "ecr:DescribeImages", "ecr:GetDownloadUrlForLayer", "ecr:InitiateLayerUpload",
      "ecr:PutImage", "ecr:UploadLayerPart",
    ]
    resources = local.ecr_arns
  }
}

resource "aws_iam_role_policy" "image_push" {
  name   = "ecr-push"
  role   = aws_iam_role.image.id
  policy = data.aws_iam_policy_document.image_push.json
}

# --- Role `apply`: chỉ job chạy trong GitHub environment có người duyệt. --

data "aws_iam_policy_document" "trust_apply" {
  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]
    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github.arn]
    }
    condition {
      test     = "StringEquals"
      variable = "${local.oidc_host}:aud"
      values   = ["sts.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "${local.oidc_host}:sub"
      values   = [for e in var.environments : "${local.repo}:environment:${e}"]
    }
  }
}

resource "aws_iam_role" "apply" {
  name                 = "aegis-ci-apply"
  assume_role_policy   = data.aws_iam_policy_document.trust_apply.json
  max_session_duration = 3600
}

# Tạo VPC, EC2, RDS, ALB, ACM, Route 53, Secrets Manager (không giá trị),
# SSM, CloudWatch Logs, S3 và SNS của máy giám sát. IAM ở dưới, giới hạn ở
# role `aegis-*-server` và `aegis-*-monitoring`.
resource "aws_iam_role_policy_attachment" "apply_power" {
  role       = aws_iam_role.apply.name
  policy_arn = "arn:aws:iam::aws:policy/PowerUserAccess"
}

data "aws_iam_policy_document" "apply_extra" {
  statement {
    sid = "OwnRolesOnly"
    actions = [
      "iam:CreateRole", "iam:DeleteRole", "iam:GetRole", "iam:TagRole", "iam:UntagRole",
      "iam:UpdateAssumeRolePolicy", "iam:PutRolePolicy", "iam:GetRolePolicy", "iam:DeleteRolePolicy",
      "iam:ListRolePolicies", "iam:ListAttachedRolePolicies", "iam:AttachRolePolicy", "iam:DetachRolePolicy",
      "iam:ListInstanceProfilesForRole", "iam:PassRole",
      "iam:CreateInstanceProfile", "iam:DeleteInstanceProfile", "iam:GetInstanceProfile",
      "iam:AddRoleToInstanceProfile", "iam:RemoveRoleFromInstanceProfile", "iam:TagInstanceProfile",
    ]
    resources = [
      "arn:aws:iam::${var.aws_account_id}:role/aegis-*-server",
      "arn:aws:iam::${var.aws_account_id}:instance-profile/aegis-*-server",
      "arn:aws:iam::${var.aws_account_id}:role/aegis-*-monitoring",
      "arn:aws:iam::${var.aws_account_id}:instance-profile/aegis-*-monitoring",
    ]
  }
  statement {
    sid       = "ServiceLinkedRoles"
    actions   = ["iam:CreateServiceLinkedRole"]
    resources = ["arn:aws:iam::${var.aws_account_id}:role/aws-service-role/*"]
  }
  statement {
    sid       = "State"
    actions   = ["s3:ListBucket", "s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = [local.state_arn, "${local.state_arn}/*"]
  }
  # Không bao giờ đọc giá trị secret: OpenTofu không quản giá trị.
  statement {
    sid       = "NoSecretValues"
    effect    = "Deny"
    actions   = ["secretsmanager:GetSecretValue", "secretsmanager:PutSecretValue"]
    resources = ["*"]
  }
}

resource "aws_iam_role_policy" "apply_extra" {
  name   = "aegis-apply"
  role   = aws_iam_role.apply.id
  policy = data.aws_iam_policy_document.apply_extra.json
}
