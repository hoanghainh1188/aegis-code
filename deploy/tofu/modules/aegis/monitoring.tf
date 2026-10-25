# Giám sát (#32, #33): một EC2 nhỏ chạy Prometheus, Alertmanager và
# Grafana trong Docker, cùng VPC và cùng vùng với máy aegis-server.
# Prometheus tìm máy server qua EC2 service discovery và scrape cổng 9464;
# security group của server chỉ cho máy này vào cổng đó. Không cổng nào mở
# ra ngoài: Grafana mở bằng SSM port forwarding (docs/runbook, "Giám sát").
#
# Cấu hình (prometheus.yml, rules, alertmanager.yml, dashboard) nằm trên
# một bucket S3; máy kéo về mỗi 5 phút và reload, nên sửa luật hay
# dashboard chỉ cần `apply`, không thay máy. Dữ liệu TSDB và Grafana nằm
# trên volume EBS riêng, giữ nguyên khi thay máy.

locals {
  monitoring       = var.monitoring.enabled ? 1 : 0
  monitoring_files = "${path.module}/monitoring"
  repo_deploy_dir  = "${path.module}/../../.."

  # Đường dẫn trên bucket = đường dẫn dưới /etc/aegis-monitoring trên máy.
  monitoring_config = {
    "prometheus/prometheus.yml" = templatefile("${local.monitoring_files}/prometheus.yml.tftpl", {
      name            = var.name
      region          = var.region
      server_name     = "${local.prefix}-server"
      scrape_interval = "30s"
    })
    "prometheus/rules/aegis.rules.yml" = file("${local.repo_deploy_dir}/alerts/aegis.rules.yml")
    "alertmanager/alertmanager.yml" = templatefile("${local.monitoring_files}/alertmanager.yml.tftpl", {
      region    = var.region
      topic_arn = var.monitoring.enabled ? aws_sns_topic.alerts[0].arn : ""
    })
    "grafana/provisioning/datasources/prometheus.yml" = file("${local.monitoring_files}/grafana-datasources.yml")
    "grafana/provisioning/dashboards/aegis.yml"       = file("${local.monitoring_files}/grafana-dashboards.yml")
    "grafana/dashboards/aegis-operations.json"        = file("${local.repo_deploy_dir}/grafana/aegis-operations.json")
  }
}

resource "aws_s3_bucket" "monitoring" {
  count  = local.monitoring
  bucket = "${local.prefix}-monitoring-${var.aws_account_id}"
  # Chỉ có cấu hình sinh từ repo, không có dữ liệu.
  force_destroy = true
  tags          = local.tags
}

resource "aws_s3_bucket_public_access_block" "monitoring" {
  count                   = local.monitoring
  bucket                  = aws_s3_bucket.monitoring[0].id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_object" "monitoring" {
  for_each = var.monitoring.enabled ? local.monitoring_config : {}
  bucket   = aws_s3_bucket.monitoring[0].id
  key      = each.key
  content  = each.value
  etag     = md5(each.value)
  tags     = local.tags
}

# Cảnh báo của Alertmanager đi qua SNS (cùng vùng). Email đăng ký phải
# bấm xác nhận một lần; thêm kênh khác (Slack qua Chatbot…) bằng tay.
resource "aws_sns_topic" "alerts" {
  count = local.monitoring
  name  = "${local.prefix}-alerts"
  tags  = local.tags
}

resource "aws_sns_topic_subscription" "alerts_email" {
  for_each  = var.monitoring.enabled ? toset(var.monitoring.alert_emails) : toset([])
  topic_arn = aws_sns_topic.alerts[0].arn
  protocol  = "email"
  endpoint  = each.key
}

resource "aws_security_group" "monitoring" {
  count       = local.monitoring
  name        = "${local.prefix}-monitoring"
  description = "Prometheus, Alertmanager, Grafana: no ingress, opened over SSM"
  vpc_id      = aws_vpc.main.id
  tags        = local.tags
}

# Ra ngoài: kéo image (Docker Hub), S3, SNS, API EC2 cho service discovery,
# SSM; và tới cổng metrics của server.
resource "aws_vpc_security_group_egress_rule" "monitoring_out" {
  count             = local.monitoring
  security_group_id = aws_security_group.monitoring[0].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
}

resource "aws_vpc_security_group_ingress_rule" "server_metrics_from_monitoring" {
  count                        = local.monitoring
  security_group_id            = aws_security_group.server.id
  referenced_security_group_id = aws_security_group.monitoring[0].id
  description                  = "Prometheus scrapes /metrics"
  ip_protocol                  = "tcp"
  from_port                    = 9464
  to_port                      = 9464
}

resource "aws_iam_role" "monitoring" {
  count              = local.monitoring
  name               = "${local.prefix}-monitoring"
  assume_role_policy = data.aws_iam_policy_document.assume_ec2.json
  tags               = local.tags
}

resource "aws_iam_role_policy_attachment" "monitoring_ssm" {
  count      = local.monitoring
  role       = aws_iam_role.monitoring[0].name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_iam_policy_document" "monitoring" {
  count = local.monitoring
  statement {
    sid       = "ReadConfig"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.monitoring[0].arn]
  }
  statement {
    sid       = "ReadConfigObjects"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.monitoring[0].arn}/*"]
  }
  # ec2_sd_configs của Prometheus. DescribeInstances không giới hạn theo
  # tài nguyên được; chỉ trong vùng của bản triển khai.
  statement {
    sid       = "DiscoverServer"
    actions   = ["ec2:DescribeInstances", "ec2:DescribeAvailabilityZones"]
    resources = ["*"]
    condition {
      test     = "StringEquals"
      variable = "aws:RequestedRegion"
      values   = [var.region]
    }
  }
  statement {
    sid       = "PublishAlerts"
    actions   = ["sns:Publish"]
    resources = [aws_sns_topic.alerts[0].arn]
  }
}

resource "aws_iam_role_policy" "monitoring" {
  count  = local.monitoring
  name   = "aegis-monitoring"
  role   = aws_iam_role.monitoring[0].id
  policy = data.aws_iam_policy_document.monitoring[0].json
}

resource "aws_iam_instance_profile" "monitoring" {
  count = local.monitoring
  name  = "${local.prefix}-monitoring"
  role  = aws_iam_role.monitoring[0].name
  tags  = local.tags
}

resource "aws_ebs_volume" "monitoring" {
  count             = local.monitoring
  availability_zone = var.availability_zones[0]
  size              = var.monitoring.volume_gb
  type              = "gp3"
  encrypted         = true
  tags              = merge(local.tags, { Name = "${local.prefix}-monitoring-data" })
}

resource "aws_launch_template" "monitoring" {
  count         = local.monitoring
  name_prefix   = "${local.prefix}-monitoring-"
  image_id      = "resolve:ssm:/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
  instance_type = var.monitoring.instance_type

  iam_instance_profile {
    arn = aws_iam_instance_profile.monitoring[0].arn
  }
  vpc_security_group_ids = [aws_security_group.monitoring[0].id]

  # Các container chạy `--network host` nên hop limit 1 vẫn đủ để đọc role
  # (service discovery, SNS); máy này không chạy container nào khác.
  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
  }

  block_device_mappings {
    device_name = "/dev/xvda"
    ebs {
      volume_size = 20
      volume_type = "gp3"
      encrypted   = true
    }
  }

  user_data = base64encode(templatefile("${local.monitoring_files}/user_data.sh.tftpl", {
    region             = var.region
    bucket             = aws_s3_bucket.monitoring[0].id
    data_volume        = replace(aws_ebs_volume.monitoring[0].id, "-", "")
    retention_days     = var.monitoring.retention_days
    prometheus_image   = "prom/prometheus:${var.monitoring.prometheus_version}"
    alertmanager_image = "prom/alertmanager:${var.monitoring.alertmanager_version}"
    grafana_image      = "grafana/grafana:${var.monitoring.grafana_version}"
  }))

  tag_specifications {
    resource_type = "instance"
    tags          = merge(local.tags, { Name = "${local.prefix}-monitoring" })
  }
  tags = local.tags
}

resource "aws_instance" "monitoring" {
  count     = local.monitoring
  subnet_id = aws_subnet.public[0].id
  launch_template {
    id      = aws_launch_template.monitoring[0].id
    version = aws_launch_template.monitoring[0].latest_version
  }
  tags = merge(local.tags, { Name = "${local.prefix}-monitoring" })
  # Máy kéo cấu hình ngay khi khởi động.
  depends_on = [
    aws_iam_role_policy.monitoring,
    aws_s3_object.monitoring,
    terraform_data.residency,
  ]
}

resource "aws_volume_attachment" "monitoring" {
  count       = local.monitoring
  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.monitoring[0].id
  instance_id = aws_instance.monitoring[0].id
}
