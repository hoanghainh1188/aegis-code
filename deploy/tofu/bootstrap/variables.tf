variable "aws_account_id" {
  description = "Tài khoản AWS được chuẩn bị."
  type        = string
}

variable "region" {
  description = "Vùng của ECR và các môi trường trong tài khoản này; customer-jp: ap-northeast-1."
  type        = string
  default     = "ap-northeast-1"
}

variable "github_repository" {
  description = "Repo GitHub được assume role, dạng `owner/name`."
  type        = string
  default     = "hoanghainh1188/aegis-code"
}

variable "state_bucket" {
  description = "Bucket S3 giữ state OpenTofu (tạo trước bằng AWS CLI)."
  type        = string
}

variable "environments" {
  description = "GitHub environment (cũng là tên môi trường) được apply vào tài khoản này."
  type        = list(string)
  default     = ["staging"]
}

variable "sandbox_profiles" {
  description = "Image sandbox đẩy lên ECR cạnh aegis-server."
  type        = list(string)
  default     = ["python", "rust"]
}

variable "keep_images" {
  description = "Số image giữ lại mỗi repository ECR (để rollback)."
  type        = number
  default     = 30
}

variable "offline_plan" {
  description = "Chạy `plan` không cần thông tin đăng nhập AWS, chỉ để kiểm cấu hình."
  type        = bool
  default     = false
}
