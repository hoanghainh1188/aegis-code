variable "name" {
  description = "Tên bản triển khai, dùng làm tiền tố tài nguyên và secret: `aegis-<name>`."
  type        = string
  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,20}$", var.name))
    error_message = "name: chữ thường, số, gạch ngang, 2–21 ký tự."
  }
}

variable "region" {
  description = "Vùng AWS của toàn bộ bản triển khai (EC2, RDS, secret, log)."
  type        = string
}

variable "aws_account_id" {
  description = "Tài khoản AWS; truyền vào thay vì đọc qua API để `plan` chạy được khi chưa có thông tin đăng nhập."
  type        = string
  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id: 12 chữ số."
  }
}

variable "customer_jp" {
  description = "Bản triển khai có chế độ `customer_jp` (repo khách): bắt buộc ở ap-northeast-1 (ADR 0003 §6) và được gọi Bedrock ở đó."
  type        = bool
}

variable "enable_poc" {
  description = "Bản triển khai có chế độ `poc` (Claude API)."
  type        = bool
  default     = true
}

variable "availability_zones" {
  description = "Hai AZ trong `region` cho subnet; RDS cần ít nhất hai."
  type        = list(string)
  validation {
    condition     = length(var.availability_zones) == 2
    error_message = "availability_zones: đúng hai AZ."
  }
}

variable "vpc_cidr" {
  description = "Dải địa chỉ của VPC; chia thành 2 subnet public (ALB, EC2) và 2 subnet private (RDS)."
  type        = string
  default     = "10.40.0.0/16"
}

variable "domain_name" {
  description = "Tên miền của API và dashboard, ví dụ `aegis-staging.example.com`; chứng chỉ ACM xác thực qua DNS."
  type        = string
}

variable "route53_zone_id" {
  description = "Hosted zone chứa `domain_name`. Để trống thì tự thêm bản ghi xác thực ACM và CNAME về ALB ở nhà cung cấp DNS."
  type        = string
  default     = ""
}

variable "image_repository" {
  description = "Repository ECR của image, không có tag, ví dụ `<account>.dkr.ecr.ap-northeast-1.amazonaws.com/aegis-server`. Các image sandbox nằm cạnh: `.../aegis-sandbox-<profile>`."
  type        = string
}

variable "initial_image_tag" {
  description = "Tag image khi tạo máy lần đầu. Sau đó tag nằm ở SSM parameter `/aegis/<name>/image_tag`, do workflow deploy đổi, OpenTofu không ghi đè."
  type        = string
  default     = "bootstrap"
}

variable "sandbox_profiles" {
  description = "Image sandbox kéo về máy và đặt tên `aegis-sandbox-<profile>` như profiles/*.toml cần."
  type        = list(string)
  default     = ["python", "rust"]
}

variable "instance_type" {
  description = "Máy chạy API và worker (vai trò `all`). Test của repo chạy trên chính máy này, trong container sandbox."
  type        = string
  default     = "m7i.xlarge"
}

variable "workspace_volume_gb" {
  description = "Đĩa riêng cho workspace của job, cache build và image sandbox, gắn ở /srv/aegis/.workspace."
  type        = number
  default     = 200
}

variable "db_instance_class" {
  description = "Lớp máy RDS PostgreSQL."
  type        = string
  default     = "db.t4g.medium"
}

variable "db_allocated_storage_gb" {
  description = "Dung lượng ban đầu của RDS; tự tăng tới `db_max_storage_gb`."
  type        = number
  default     = 50
}

variable "db_max_storage_gb" {
  description = "Trần dung lượng tự tăng của RDS."
  type        = number
  default     = 200
}

variable "db_multi_az" {
  description = "RDS chạy Multi-AZ."
  type        = bool
  default     = false
}

variable "db_deletion_protection" {
  description = "Chặn xóa RDS bằng `tofu destroy` hay console."
  type        = bool
  default     = true
}

variable "db_backup_retention_days" {
  description = "Số ngày giữ bản sao lưu tự động của RDS."
  type        = number
  default     = 7
}

variable "log_retention_days" {
  description = "Số ngày giữ log của container trong CloudWatch Logs (cùng vùng)."
  type        = number
  default     = 30
}

variable "server_config" {
  description = "Các mục của config/server.toml thay đổi theo môi trường. `[secrets]`, `[modes]` và `api.listen` do module viết."
  type = object({
    oidc_issuer             = optional(string, "")
    oidc_client_id          = optional(string, "aegis")
    session_hours           = optional(number, 12)
    api_requests_per_minute = optional(number, 120)
    max_workers             = optional(number, 4)
    max_cost_usd            = optional(number, 4.0)
    drain_s                 = optional(number, 600)
  })
  default = {}
}

variable "monitoring" {
  description = "Máy giám sát (monitoring.tf): Prometheus scrape cổng 9464 của server, nạp deploy/alerts/aegis.rules.yml, gửi cảnh báo qua SNS; Grafana nạp deploy/grafana/*.json. `alert_emails` đăng ký nhận cảnh báo (phải xác nhận qua email)."
  type = object({
    enabled              = optional(bool, true)
    instance_type        = optional(string, "t3.small")
    volume_gb            = optional(number, 30)
    retention_days       = optional(number, 30)
    alert_emails         = optional(list(string), [])
    prometheus_version   = optional(string, "v3.5.0")
    alertmanager_version = optional(string, "v0.28.1")
    grafana_version      = optional(string, "12.1.0")
  })
  default = {}
}

variable "repo_secret_names" {
  description = "Secret riêng của repo (token Git, Sonar…) cần tạo chỗ trong Secrets Manager, ngoài các secret của server."
  type        = list(string)
  default     = []
}

variable "tags" {
  description = "Tag thêm cho mọi tài nguyên."
  type        = map(string)
  default     = {}
}
