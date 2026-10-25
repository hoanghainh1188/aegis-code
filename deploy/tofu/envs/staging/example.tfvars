# Giá trị mẫu: `tofu plan -var-file=example.tfvars -var offline_plan=true`
# kiểm cấu hình mà không cần tài khoản AWS. Bản thật: chép thành
# staging.tfvars (không commit, .gitignore) hoặc đặt TF_VAR_* trong CI.
aws_account_id    = "123456789012"
domain_name       = "aegis-staging.example.com"
image_repository  = "123456789012.dkr.ecr.ap-northeast-1.amazonaws.com/aegis-server"
repo_secret_names = ["GITHUB_TOKEN_EXAMPLE_REPO"]
server_config = {
  oidc_issuer = "https://login.example.com/realms/acme"
}
monitoring = {
  alert_emails = ["oncall@example.com"]
}
