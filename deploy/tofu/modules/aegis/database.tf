# RDS PostgreSQL. Mật khẩu master do RDS sinh và giữ trong Secrets Manager
# (`manage_master_user_password`), không qua biến hay state của OpenTofu.
# Server dùng user riêng `aegis`; URL của nó là secret
# `aegis/<name>/AEGIS_DATABASE_URL`, người vận hành đặt (docs/runbook).

resource "aws_db_subnet_group" "main" {
  name       = local.prefix
  subnet_ids = aws_subnet.private[*].id
  tags       = local.tags
}

resource "aws_db_parameter_group" "main" {
  name   = local.prefix
  family = "postgres17"
  parameter {
    name  = "rds.force_ssl"
    value = "1"
  }
  # Truy vấn chậm hơn 1 s vào log, để tìm nguyên nhân khi hàng đợi dồn.
  parameter {
    name  = "log_min_duration_statement"
    value = "1000"
  }
  tags = local.tags
}

resource "aws_db_instance" "main" {
  identifier                      = local.prefix
  engine                          = "postgres"
  engine_version                  = "17"
  instance_class                  = var.db_instance_class
  allocated_storage               = var.db_allocated_storage_gb
  max_allocated_storage           = var.db_max_storage_gb
  storage_type                    = "gp3"
  storage_encrypted               = true
  db_name                         = "aegis"
  username                        = "aegis_admin"
  manage_master_user_password     = true
  db_subnet_group_name            = aws_db_subnet_group.main.name
  parameter_group_name            = aws_db_parameter_group.main.name
  vpc_security_group_ids          = [aws_security_group.db.id]
  publicly_accessible             = false
  multi_az                        = var.db_multi_az
  backup_retention_period         = var.db_backup_retention_days
  deletion_protection             = var.db_deletion_protection
  skip_final_snapshot             = false
  final_snapshot_identifier       = "${local.prefix}-final"
  copy_tags_to_snapshot           = true
  auto_minor_version_upgrade      = true
  enabled_cloudwatch_logs_exports = ["postgresql"]
  performance_insights_enabled    = true
  tags                            = local.tags
}
