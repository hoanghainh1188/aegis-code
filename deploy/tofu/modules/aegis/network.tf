# VPC riêng: hai subnet public cho ALB và EC2 (ra Internet qua Internet
# gateway, không cần NAT), hai subnet private cho RDS. Máy API chỉ nhận kết
# nối từ ALB và từ máy Prometheus (cổng metrics, monitoring.tf); máy worker
# chỉ từ Prometheus. Không mở SSH, vào máy bằng SSM Session Manager.

resource "aws_vpc" "main" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags                 = merge(local.tags, { Name = local.prefix })
}

resource "aws_internet_gateway" "main" {
  vpc_id = aws_vpc.main.id
  tags   = merge(local.tags, { Name = local.prefix })
}

resource "aws_subnet" "public" {
  count                   = 2
  vpc_id                  = aws_vpc.main.id
  availability_zone       = var.availability_zones[count.index]
  cidr_block              = cidrsubnet(var.vpc_cidr, 8, count.index)
  map_public_ip_on_launch = true
  tags                    = merge(local.tags, { Name = "${local.prefix}-public-${count.index}" })
}

resource "aws_subnet" "private" {
  count             = 2
  vpc_id            = aws_vpc.main.id
  availability_zone = var.availability_zones[count.index]
  cidr_block        = cidrsubnet(var.vpc_cidr, 8, 10 + count.index)
  tags              = merge(local.tags, { Name = "${local.prefix}-private-${count.index}" })
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.main.id
  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.main.id
  }
  tags = merge(local.tags, { Name = "${local.prefix}-public" })
}

resource "aws_route_table_association" "public" {
  count          = 2
  subnet_id      = aws_subnet.public[count.index].id
  route_table_id = aws_route_table.public.id
}

resource "aws_security_group" "alb" {
  name        = "${local.prefix}-alb"
  description = "HTTPS from anywhere: dashboard, API, webhooks"
  vpc_id      = aws_vpc.main.id
  tags        = local.tags
}

resource "aws_vpc_security_group_ingress_rule" "alb_https" {
  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

resource "aws_vpc_security_group_ingress_rule" "alb_http" {
  security_group_id = aws_security_group.alb.id
  description       = "Redirected to HTTPS"
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 80
  to_port           = 80
}

resource "aws_vpc_security_group_egress_rule" "alb_to_server" {
  for_each                     = local.api_nodes
  security_group_id            = aws_security_group.alb.id
  referenced_security_group_id = aws_security_group.node[each.key].id
  ip_protocol                  = "tcp"
  from_port                    = 8080
  to_port                      = 8080
}

# Một nhóm cho mỗi loại máy (#323). Không mở SSH, vào máy bằng SSM. Máy
# `worker` không nhận kết nối nào ngoài Prometheus; không nhóm nào cho
# worker vào, nên worker không gọi được vào máy API (chỉ qua ALB như mọi
# người ngoài, cần đăng nhập hoặc chữ ký webhook).
resource "aws_security_group" "node" {
  for_each    = local.nodes
  name        = "${local.prefix}-${each.key}"
  description = each.value.serves_api ? "aegis ${each.key}: 8080 from the ALB, 9464 from Prometheus" : "aegis ${each.key}: 9464 from Prometheus only"
  vpc_id      = aws_vpc.main.id
  tags        = local.tags
}

resource "aws_vpc_security_group_ingress_rule" "server_from_alb" {
  for_each                     = local.api_nodes
  security_group_id            = aws_security_group.node[each.key].id
  referenced_security_group_id = aws_security_group.alb.id
  ip_protocol                  = "tcp"
  from_port                    = 8080
  to_port                      = 8080
}

# Máy có API ra ngoài tự do: forge, tracker, Sonar, OIDC có thể ở cổng
# bất kỳ; nó không chạy code của repo.
resource "aws_vpc_security_group_egress_rule" "api_out" {
  for_each          = local.api_nodes
  security_group_id = aws_security_group.node[each.key].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
}

# Worker (#323): HTTPS ra ngoài (API model, forge, ECR, Secrets Manager,
# SSM, registry gói qua proxy egress của gate, #66), thêm các cổng trong
# `worker_egress_ports`, và PostgreSQL. Security group không lọc theo tên
# miền; tên miền do proxy egress lọc. DNS của VPC, NTP và IMDS ở địa chỉ
# link-local, không qua security group.
resource "aws_vpc_security_group_egress_rule" "worker_out" {
  for_each          = var.layout == "split" ? toset([for p in var.worker_egress_ports : tostring(p)]) : toset([])
  security_group_id = aws_security_group.node["worker"].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = tonumber(each.key)
  to_port           = tonumber(each.key)
}

resource "aws_vpc_security_group_egress_rule" "worker_to_db" {
  count                        = var.layout == "split" ? 1 : 0
  security_group_id            = aws_security_group.node["worker"].id
  referenced_security_group_id = aws_security_group.db.id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}

resource "aws_security_group" "db" {
  name        = "${local.prefix}-db"
  description = "PostgreSQL from aegis-server only"
  vpc_id      = aws_vpc.main.id
  tags        = local.tags
}

resource "aws_vpc_security_group_ingress_rule" "db_from_server" {
  for_each                     = local.nodes
  security_group_id            = aws_security_group.db.id
  referenced_security_group_id = aws_security_group.node[each.key].id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}
