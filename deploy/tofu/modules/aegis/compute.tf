# Máy chạy aegis-server, sau một ALB có TLS (#323). `layout = "split"`:
# máy `api` (API, webhook, dashboard; không chạy job, không có proxy Docker
# nên aegis không điều khiển được daemon) và máy `worker` (chạy job và
# container sandbox của gate; không nhận traffic từ ALB). `layout =
# "single"`: một máy `all` làm cả hai, cho môi trường nhỏ; cái giá về bảo
# mật ghi ở docs/runbook/README.md, "Worker and Docker Isolation".
# Workspace của máy chạy job nằm trên một volume EBS riêng để thay máy
# (đổi user_data, AMI) không mất cache build.

locals {
  nodes = var.layout == "split" ? {
    api    = { runs_jobs = false, serves_api = true, instance_type = var.api_instance_type }
    worker = { runs_jobs = true, serves_api = false, instance_type = var.instance_type }
    } : {
    all = { runs_jobs = true, serves_api = true, instance_type = var.instance_type }
  }
  job_nodes = { for role, n in local.nodes : role => n if n.runs_jobs }
  api_nodes = { for role, n in local.nodes : role => n if n.serves_api }
}

resource "aws_ebs_volume" "workspace" {
  for_each          = local.job_nodes
  availability_zone = var.availability_zones[0]
  size              = var.workspace_volume_gb
  type              = "gp3"
  encrypted         = true
  tags              = merge(local.tags, { Name = "${local.prefix}-${each.key}-workspace" })
}

resource "aws_launch_template" "node" {
  for_each    = local.nodes
  name_prefix = "${local.prefix}-${each.key}-"
  # Amazon Linux 2023 mới nhất, đọc lúc tạo máy: không cần gọi API khi `plan`.
  image_id      = "resolve:ssm:/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
  instance_type = each.value.instance_type

  iam_instance_profile {
    arn = aws_iam_instance_profile.node[each.key].arn
  }
  vpc_security_group_ids = [aws_security_group.node[each.key].id]

  # IMDSv2, hop limit 1: container trên mạng bridge (sandbox, scanner
  # Sonar) không tới được metadata nên không lấy được role của máy.
  # aegis-server chạy `--network host` nên vẫn đọc được.
  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
  }

  block_device_mappings {
    device_name = "/dev/xvda"
    ebs {
      volume_size = 40
      volume_type = "gp3"
      encrypted   = true
    }
  }

  user_data = base64encode(templatefile("${path.module}/user_data.sh.tftpl", {
    role             = each.key
    runs_jobs        = each.value.runs_jobs
    region           = var.region
    name             = var.name
    image_repository = var.image_repository
    registry         = split("/", var.image_repository)[0]
    image_tag_param  = aws_ssm_parameter.image_tag.name
    log_group        = aws_cloudwatch_log_group.server.name
    workspace_dir    = local.workspace_dir
    workspace_volume = each.value.runs_jobs ? replace(aws_ebs_volume.workspace[each.key].id, "-", "") : ""
    aegis_uid        = local.aegis_uid
    sandbox_profiles = join(" ", var.sandbox_profiles)
    stop_timeout_s   = var.server_config.drain_s + 60
    # The unit of every deployment, on premises too (#262).
    aegis_unit = file("${path.module}/../../../systemd/aegis.service")
    # Between aegis and the daemon (#321), on machines that run jobs (#323).
    docker_proxy_unit = file("${path.module}/../../../systemd/aegis-docker-proxy.service")
    jobs_dropin       = file("${path.module}/../../../systemd/aegis.service.d/jobs.conf")
    server_toml = templatefile("${path.module}/server.toml.tftpl", {
      region        = var.region
      secret_prefix = local.secret_prefix
      enable_poc    = var.enable_poc
      customer_jp   = var.customer_jp
      public_url    = "https://${var.domain_name}"
      config        = var.server_config
    })
  }))

  tag_specifications {
    resource_type = "instance"
    tags          = merge(local.tags, { Name = "${local.prefix}-${each.key}", "aegis:role" = each.key })
  }
  tags = local.tags
}

resource "aws_instance" "node" {
  for_each  = local.nodes
  subnet_id = aws_subnet.public[0].id
  launch_template {
    id      = aws_launch_template.node[each.key].id
    version = aws_launch_template.node[each.key].latest_version
  }
  tags = merge(local.tags, { Name = "${local.prefix}-${each.key}", "aegis:role" = each.key })
  depends_on = [
    aws_iam_role_policy.node,
    terraform_data.residency,
  ]
}

resource "aws_volume_attachment" "workspace" {
  for_each    = local.job_nodes
  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.workspace[each.key].id
  instance_id = aws_instance.node[each.key].id
}

resource "aws_acm_certificate" "api" {
  domain_name       = var.domain_name
  validation_method = "DNS"
  tags              = local.tags
  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_route53_record" "cert_validation" {
  for_each = var.route53_zone_id == "" ? {} : {
    for o in aws_acm_certificate.api.domain_validation_options : o.domain_name => o
  }
  zone_id = var.route53_zone_id
  name    = each.value.resource_record_name
  type    = each.value.resource_record_type
  records = [each.value.resource_record_value]
  ttl     = 300
}

resource "aws_acm_certificate_validation" "api" {
  count                   = var.route53_zone_id == "" ? 0 : 1
  certificate_arn         = aws_acm_certificate.api.arn
  validation_record_fqdns = [for r in aws_route53_record.cert_validation : r.fqdn]
}

resource "aws_lb" "api" {
  name                       = local.prefix
  load_balancer_type         = "application"
  security_groups            = [aws_security_group.alb.id]
  subnets                    = aws_subnet.public[*].id
  drop_invalid_header_fields = true
  tags                       = local.tags
}

resource "aws_lb_target_group" "server" {
  name     = "${local.prefix}-server"
  port     = 8080
  protocol = "HTTP"
  vpc_id   = aws_vpc.main.id
  # Jobs dài: chờ lâu trước khi bỏ máy cũ (server tự drain khi SIGTERM).
  deregistration_delay = 30
  health_check {
    path                = "/api/v1/health"
    matcher             = "200"
    interval            = 15
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
  tags = local.tags
}

# Chỉ máy có API; máy `worker` không nằm sau ALB.
resource "aws_lb_target_group_attachment" "server" {
  for_each         = local.api_nodes
  target_group_arn = aws_lb_target_group.server.arn
  target_id        = aws_instance.node[each.key].id
  port             = 8080
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.api.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate.api.arn
  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.server.arn
  }
  depends_on = [aws_acm_certificate_validation.api]
}

resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.api.arn
  port              = 80
  protocol          = "HTTP"
  default_action {
    type = "redirect"
    redirect {
      port        = "443"
      protocol    = "HTTPS"
      status_code = "HTTP_301"
    }
  }
}

resource "aws_route53_record" "api" {
  count   = var.route53_zone_id == "" ? 0 : 1
  zone_id = var.route53_zone_id
  name    = var.domain_name
  type    = "A"
  alias {
    name                   = aws_lb.api.dns_name
    zone_id                = aws_lb.api.zone_id
    evaluate_target_health = true
  }
}
