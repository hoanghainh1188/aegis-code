# Máy API và máy worker tách nhau (#323): `tofu test` trong thư mục module,
# không cần tài khoản AWS (provider giả). Chạy bởi deploy/tofu/check.sh.

# Giá trị giả có dạng mà provider kiểm ở đầu vào của tài nguyên khác (ARN,
# JSON).
mock_provider "aws" {
  mock_data "aws_iam_policy_document" {
    defaults = { json = "{\"Version\":\"2012-10-17\",\"Statement\":[]}" }
  }
  mock_resource "aws_lb" {
    defaults = { arn = "arn:aws:elasticloadbalancing:ap-northeast-1:123456789012:loadbalancer/app/test/1" }
  }
  mock_resource "aws_lb_target_group" {
    defaults = { arn = "arn:aws:elasticloadbalancing:ap-northeast-1:123456789012:targetgroup/test/1" }
  }
  mock_resource "aws_acm_certificate" {
    defaults = { arn = "arn:aws:acm:ap-northeast-1:123456789012:certificate/test" }
  }
  mock_resource "aws_iam_instance_profile" {
    defaults = { arn = "arn:aws:iam::123456789012:instance-profile/test" }
  }
  mock_resource "aws_launch_template" {
    defaults = { id = "lt-0123456789abcdef0", latest_version = 1 }
  }
  mock_resource "aws_sns_topic" {
    defaults = { arn = "arn:aws:sns:ap-northeast-1:123456789012:test" }
  }
  mock_resource "aws_db_instance" {
    defaults = {
      master_user_secret = [{
        secret_arn    = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:rds-master"
        kms_key_id    = ""
        secret_status = "active"
      }]
    }
  }
}

# Mỗi secret một ARN, để phân biệt secret nào worker đọc được.
override_resource {
  target = aws_secretsmanager_secret.server["AEGIS_DATABASE_URL"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/AEGIS_DATABASE_URL-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["AEGIS_SESSION_KEY"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/AEGIS_SESSION_KEY-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["AEGIS_GITHUB_WEBHOOK_SECRET"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/AEGIS_GITHUB_WEBHOOK_SECRET-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["AEGIS_GITLAB_WEBHOOK_TOKEN"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/AEGIS_GITLAB_WEBHOOK_TOKEN-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["AEGIS_GITHUB_COMMENT_TOKEN"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/AEGIS_GITHUB_COMMENT_TOKEN-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["AEGIS_GITLAB_COMMENT_TOKEN"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/AEGIS_GITLAB_COMMENT_TOKEN-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["ANTHROPIC_API_KEY"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/ANTHROPIC_API_KEY-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.server["FORK_TOKEN"]
  values = { arn = "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/FORK_TOKEN-AbCdEf" }
}

# Mỗi security group một id, để biết luật nào thuộc nhóm nào.
override_resource {
  target = aws_security_group.node["api"]
  values = { id = "sg-0api" }
}

override_resource {
  target = aws_security_group.node["worker"]
  values = { id = "sg-0worker" }
}

override_resource {
  target = aws_security_group.node["all"]
  values = { id = "sg-0all" }
}

override_resource {
  target = aws_security_group.monitoring[0]
  values = { id = "sg-0monitoring" }
}

override_resource {
  target = aws_security_group.alb
  values = { id = "sg-0alb" }
}

override_resource {
  target = aws_security_group.db
  values = { id = "sg-0db" }
}

variables {
  name               = "test"
  region             = "ap-northeast-1"
  aws_account_id     = "123456789012"
  customer_jp        = false
  availability_zones = ["ap-northeast-1a", "ap-northeast-1c"]
  domain_name        = "aegis-test.example.com"
  image_repository   = "123456789012.dkr.ecr.ap-northeast-1.amazonaws.com/aegis-server"
  repo_secret_names  = ["FORK_TOKEN"]
  monitoring         = { enabled = true }
}

run "split_by_default" {
  command = apply

  assert {
    condition     = keys(aws_instance.node) == ["api", "worker"]
    error_message = "split: một máy api và một máy worker."
  }
  assert {
    condition     = keys(aws_lb_target_group_attachment.server) == ["api"]
    error_message = "Chỉ máy api nằm sau ALB."
  }
  assert {
    condition     = keys(aws_vpc_security_group_ingress_rule.server_from_alb) == ["api"]
    error_message = "ALB chỉ vào được máy api."
  }
  assert {
    condition = [
      for r in aws_vpc_security_group_ingress_rule.server_metrics_from_monitoring : r.referenced_security_group_id
      if r.security_group_id == aws_security_group.node["worker"].id
    ] == [aws_security_group.monitoring[0].id]
    error_message = "Vào máy worker chỉ có Prometheus."
  }
  assert {
    condition     = length([for r in aws_vpc_security_group_ingress_rule.server_from_alb : r if r.security_group_id == aws_security_group.node["worker"].id]) == 0
    error_message = "Không luật nào cho ALB vào máy worker."
  }
  assert {
    condition     = keys(aws_ebs_volume.workspace) == ["worker"]
    error_message = "Chỉ máy chạy job có volume workspace."
  }
  assert {
    condition     = [for r in aws_vpc_security_group_egress_rule.worker_out : r.from_port] == [443]
    error_message = "Worker chỉ ra ngoài ở cổng 443."
  }
  assert {
    condition     = aws_vpc_security_group_egress_rule.worker_to_db[0].referenced_security_group_id == aws_security_group.db.id
    error_message = "Worker tới được PostgreSQL."
  }
  assert {
    condition     = keys(aws_vpc_security_group_egress_rule.api_out) == ["api"]
    error_message = "Worker không có luật ra ngoài tự do."
  }
}

run "the_worker_cannot_read_the_secrets_of_the_api" {
  command = apply

  assert {
    condition = toset(one([
      for st in data.aws_iam_policy_document.node["worker"].statement : st.resources if st.sid == "NoApiSecrets"
      ])) == toset([
      for n in ["AEGIS_SESSION_KEY", "AEGIS_OIDC_CLIENT_SECRET", "AEGIS_GITHUB_WEBHOOK_SECRET", "AEGIS_GITLAB_WEBHOOK_TOKEN"] :
      "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/${n}-??????"
    ])
    error_message = "Worker bị Deny khoá phiên, OIDC và secret webhook."
  }
  assert {
    condition = one([
      for st in data.aws_iam_policy_document.node["worker"].statement : st.effect if st.sid == "NoApiSecrets"
    ]) == "Deny"
    error_message = "NoApiSecrets là Deny."
  }
  assert {
    condition = alltrue([
      for n in ["AEGIS_SESSION_KEY", "AEGIS_GITHUB_WEBHOOK_SECRET", "AEGIS_GITLAB_WEBHOOK_TOKEN"] :
      startswith(aws_secretsmanager_secret.server[n].arn, "arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/${n}-")
    ])
    error_message = "ARN thật của secret khớp mẫu Deny (tên, gạch ngang, hậu tố)."
  }
  assert {
    condition     = length([for st in data.aws_iam_policy_document.node["api"].statement : st if st.sid == "NoApiSecrets"]) == 0
    error_message = "Máy api đọc được secret của nó."
  }
  assert {
    condition = alltrue([
      for role in ["api", "worker"] : one([
        for st in data.aws_iam_policy_document.node[role].statement : st.resources if st.sid == "ReadOwnSecrets"
      ]) == toset(["arn:aws:secretsmanager:ap-northeast-1:123456789012:secret:aegis/test/*"])
    ])
    error_message = "Mọi máy đọc secret của bản triển khai này, không của bản khác."
  }
  assert {
    condition = length(one([
      for st in data.aws_iam_policy_document.node["api"].statement : st.resources if st.sid == "EcrPull"
    ])) == 1
    error_message = "Máy api chỉ kéo image aegis-server, không kéo sandbox."
  }
  assert {
    condition = length(one([
      for st in data.aws_iam_policy_document.node["worker"].statement : st.resources if st.sid == "EcrPull"
    ])) == 1 + length(var.sandbox_profiles)
    error_message = "Worker kéo image aegis-server và mọi sandbox."
  }
}

run "the_api_machine_has_no_docker_proxy" {
  command = apply

  assert {
    condition     = strcontains(base64decode(aws_launch_template.node["api"].user_data), "AEGIS_ROLE=api")
    error_message = "Máy api chạy `--role api`."
  }
  assert {
    condition     = !strcontains(base64decode(aws_launch_template.node["api"].user_data), "aegis-docker-proxy.service <<")
    error_message = "Máy api không cài aegis-docker-proxy."
  }
  assert {
    condition     = !strcontains(base64decode(aws_launch_template.node["api"].user_data), "aegis-sandbox-")
    error_message = "Máy api không kéo image sandbox."
  }
  assert {
    condition     = !strcontains(base64decode(aws_launch_template.node["api"].user_data), "Requires=aegis-docker-proxy.service")
    error_message = "Unit của máy api không cần proxy."
  }
  assert {
    condition     = strcontains(base64decode(aws_launch_template.node["worker"].user_data), "AEGIS_ROLE=worker")
    error_message = "Máy worker chạy `--role worker`."
  }
  assert {
    condition     = strcontains(base64decode(aws_launch_template.node["worker"].user_data), "aegis.service.d/jobs.conf <<'UNIT'\n# On a machine")
    error_message = "Máy worker cài drop-in jobs.conf."
  }
  assert {
    condition     = strcontains(base64decode(aws_launch_template.node["worker"].user_data), "Requires=aegis-docker-proxy.service")
    error_message = "Máy worker cần proxy để aegis chạy."
  }
}

run "one_machine_when_single" {
  command = apply

  variables {
    layout = "single"
  }

  assert {
    condition     = keys(aws_instance.node) == ["all"]
    error_message = "single: một máy all."
  }
  assert {
    condition     = keys(aws_lb_target_group_attachment.server) == ["all"]
    error_message = "Máy all nằm sau ALB."
  }
  assert {
    condition     = length(aws_vpc_security_group_egress_rule.worker_out) == 0
    error_message = "single: không có luật của worker."
  }
  assert {
    condition     = strcontains(base64decode(aws_launch_template.node["all"].user_data), "AEGIS_ROLE=all")
    error_message = "Máy all chạy `--role all`."
  }
}

run "unknown_layout" {
  command = plan

  variables {
    layout = "both"
  }

  expect_failures = [var.layout]
}
