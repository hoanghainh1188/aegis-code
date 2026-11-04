# Role của từng nhóm máy (#323): đọc secret, kéo image ECR, ghi log, SSM
# (Session Manager và lệnh deploy), Bedrock ở Nhật khi có `customer_jp`.
# Mọi máy đọc secret của bản triển khai này (`aegis/<name>/*`); máy
# `worker` bị Deny các secret chỉ API dùng (`local.api_only_secret_names`):
# khoá phiên, secret webhook, OIDC. Container sandbox không lấy được
# role nào: IMDSv2 với hop limit 1 (compute.tf) chặn mọi container không
# chạy `--network host`.

data "aws_iam_policy_document" "assume_ec2" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "node" {
  for_each           = local.nodes
  name               = "${local.prefix}-${each.key}"
  assume_role_policy = data.aws_iam_policy_document.assume_ec2.json
  tags               = local.tags
}

resource "aws_iam_role_policy_attachment" "ssm" {
  for_each   = local.nodes
  role       = aws_iam_role.node[each.key].name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_iam_policy_document" "node" {
  for_each = local.nodes
  statement {
    sid       = "ReadOwnSecrets"
    actions   = ["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"]
    resources = ["arn:aws:secretsmanager:${var.region}:${var.aws_account_id}:secret:${local.secret_prefix}*"]
  }
  # Theo tên (hậu tố ngẫu nhiên 6 ký tự của Secrets Manager), không theo
  # ARN: vẫn chặn khi secret bị xoá rồi tạo lại.
  dynamic "statement" {
    for_each = each.value.serves_api ? [] : [1]
    content {
      sid     = "NoApiSecrets"
      effect  = "Deny"
      actions = ["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"]
      resources = [
        for name in local.api_only_secret_names :
        "arn:aws:secretsmanager:${var.region}:${var.aws_account_id}:secret:${local.secret_prefix}${name}-??????"
      ]
    }
  }
  statement {
    sid       = "EcrLogin"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }
  # Image sandbox chỉ cho máy chạy job.
  statement {
    sid     = "EcrPull"
    actions = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer", "ecr:BatchCheckLayerAvailability"]
    resources = concat(
      ["arn:aws:ecr:${var.region}:${var.aws_account_id}:repository/aegis-server"],
      each.value.runs_jobs ? [
        for p in var.sandbox_profiles : "arn:aws:ecr:${var.region}:${var.aws_account_id}:repository/aegis-sandbox-${p}"
      ] : [],
    )
  }
  statement {
    sid       = "Logs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.server.arn}:*"]
  }
  statement {
    sid       = "ImageTag"
    actions   = ["ssm:GetParameter"]
    resources = [aws_ssm_parameter.image_tag.arn]
  }
  # Worker gọi model; API thử kết nối provider từ trang admin (#367).
  dynamic "statement" {
    for_each = var.customer_jp ? [1] : []
    content {
      sid       = "BedrockInJapan"
      actions   = ["bedrock:InvokeModel", "bedrock:InvokeModelWithResponseStream"]
      resources = ["*"]
      condition {
        test     = "StringEquals"
        variable = "aws:RequestedRegion"
        values   = ["ap-northeast-1"]
      }
    }
  }
}

resource "aws_iam_role_policy" "node" {
  for_each = local.nodes
  name     = "aegis-${each.key}"
  role     = aws_iam_role.node[each.key].id
  policy   = data.aws_iam_policy_document.node[each.key].json
}

resource "aws_iam_instance_profile" "node" {
  for_each = local.nodes
  name     = "${local.prefix}-${each.key}"
  role     = aws_iam_role.node[each.key].name
  tags     = local.tags
}
