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

variable "offline_plan" {
  description = "Chạy `plan` không cần thông tin đăng nhập AWS, chỉ để kiểm cấu hình."
  type        = bool
  default     = false
}

variable "region" {
  description = "Vùng của staging."
  type        = string
  default     = "ap-northeast-1"
}

variable "availability_zones" {
  description = "Hai AZ trong `region`."
  type        = list(string)
  default     = ["ap-northeast-1a", "ap-northeast-1c"]
}
