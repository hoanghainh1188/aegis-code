# Role của máy: đọc secret của bản triển khai này, kéo image ECR, ghi
# log, SSM (Session Manager và lệnh deploy), Bedrock ở Nhật khi có
# `customer_jp`. Container sandbox không lấy được role này: IMDSv2 với
# hop limit 1 (compute.tf) chặn mọi container không chạy `--network host`.

data "aws_iam_policy_document" "assume_ec2" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "server" {
  name               = "${local.prefix}-server"
  assume_role_policy = data.aws_iam_policy_document.assume_ec2.json
  tags               = local.tags
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.server.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_iam_policy_document" "server" {
  statement {
    sid       = "ReadOwnSecrets"
    actions   = ["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"]
    resources = ["arn:aws:secretsmanager:${var.region}:${var.aws_account_id}:secret:${local.secret_prefix}*"]
  }
  statement {
    sid       = "EcrLogin"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }
  statement {
    sid     = "EcrPull"
    actions = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer", "ecr:BatchCheckLayerAvailability"]
    resources = concat(
      ["arn:aws:ecr:${var.region}:${var.aws_account_id}:repository/aegis-server"],
      [for p in var.sandbox_profiles : "arn:aws:ecr:${var.region}:${var.aws_account_id}:repository/aegis-sandbox-${p}"],
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

resource "aws_iam_role_policy" "server" {
  name   = "aegis-server"
  role   = aws_iam_role.server.id
  policy = data.aws_iam_policy_document.server.json
}

resource "aws_iam_instance_profile" "server" {
  name = "${local.prefix}-server"
  role = aws_iam_role.server.name
  tags = local.tags
}
