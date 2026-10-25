variable "aws_account_id" {
  description = "Tài khoản AWS của môi trường này."
  type        = string
}

variable "domain_name" {
  description = "Tên miền của API và dashboard."
  type        = string
}

variable "route53_zone_id" {
  description = "Hosted zone của `domain_name`; trống thì thêm bản ghi DNS tay."
  type        = string
  default     = ""
}

variable "image_repository" {
  description = "ECR `aegis-server` cùng tài khoản, cùng vùng (không có tag)."
  type        = string
}

variable "repo_secret_names" {
  description = "Secret riêng của repo (token Git, Sonar…), ngoài secret của server."
  type        = list(string)
  default     = []
}

variable "server_config" {
  description = "Các mục của server.toml theo môi trường (xem module)."
  type        = any
  default     = {}
}

variable "monitoring" {
  description = "Máy giám sát: Prometheus, Alertmanager, Grafana (xem module)."
  type        = any
  default     = {}
}

variable "offline_plan" {
  description = "Chạy `plan` không cần thông tin đăng nhập AWS, chỉ để kiểm cấu hình."
  type        = bool
  default     = false
}

variable "enable_poc" {
  description = "Cho repo nội bộ (chế độ `poc`, Claude API) dùng chung bản này."
  type        = bool
  default     = false
}
