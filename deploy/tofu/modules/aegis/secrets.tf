# Chỗ cho các secret của server (#30): OpenTofu chỉ tạo secret, không có
# giá trị (không `aws_secretsmanager_secret_version`), nên giá trị không
# bao giờ vào biến hay state. Người vận hành đặt giá trị bằng
# `aws secretsmanager put-secret-value` (docs/runbook/README.md).

locals {
  server_secret_names = concat(
    [
      "AEGIS_DATABASE_URL",
      "AEGIS_SESSION_KEY",
      "AEGIS_GITHUB_WEBHOOK_SECRET",
      "AEGIS_GITLAB_WEBHOOK_TOKEN",
      "AEGIS_GITHUB_COMMENT_TOKEN",
      "AEGIS_GITLAB_COMMENT_TOKEN",
    ],
    var.server_config.oidc_issuer == "" ? [] : ["AEGIS_OIDC_CLIENT_SECRET"],
    var.enable_poc ? ["ANTHROPIC_API_KEY"] : [],
  )
}

resource "aws_secretsmanager_secret" "server" {
  for_each                = toset(concat(local.server_secret_names, var.repo_secret_names))
  name                    = "${local.secret_prefix}${each.key}"
  description             = "aegis-server ${var.name}: ${each.key}"
  recovery_window_in_days = 7
  tags                    = local.tags
}
