output "instance_id" {
  description = "Máy aegis-server; workflow deploy gửi lệnh `aegis-deploy` tới đây qua SSM."
  value       = aws_instance.server.id
}

output "image_tag_parameter" {
  description = "SSM parameter giữ tag image đang chạy."
  value       = aws_ssm_parameter.image_tag.name
}

output "alb_dns_name" {
  description = "Tên DNS của ALB; trỏ `domain_name` về đây nếu không dùng Route 53."
  value       = aws_lb.api.dns_name
}

output "certificate_validation_records" {
  description = "Bản ghi DNS xác thực chứng chỉ ACM, cần thêm tay khi không dùng Route 53."
  value = [for o in aws_acm_certificate.api.domain_validation_options : {
    name  = o.resource_record_name
    type  = o.resource_record_type
    value = o.resource_record_value
  }]
}

output "db_endpoint" {
  description = "host:port của RDS, cho URL trong secret AEGIS_DATABASE_URL."
  value       = aws_db_instance.main.endpoint
}

output "db_master_secret_arn" {
  description = "Secret do RDS quản lý chứa mật khẩu master (chỉ để tạo user `aegis`)."
  value       = aws_db_instance.main.master_user_secret[0].secret_arn
}

output "secret_prefix" {
  description = "Tiền tố secret của server trong Secrets Manager."
  value       = local.secret_prefix
}

output "log_group" {
  description = "CloudWatch Logs của container aegis-server."
  value       = aws_cloudwatch_log_group.server.name
}
