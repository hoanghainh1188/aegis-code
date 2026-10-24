#!/usr/bin/env bash
# Kiểm cấu hình OpenTofu không cần tài khoản AWS (#33): fmt, tflint,
# validate, rồi `plan` của từng root với example.tfvars, backend local
# tạm và thông tin đăng nhập giả. Không gọi API AWS nào; kết quả chỉ để
# kiểm, không apply được. Chạy từ bất kỳ đâu:
#
#   deploy/tofu/check.sh            # cần tofu và tflint trong PATH
set -euo pipefail
cd "$(dirname "$0")"

ROOTS=(envs/staging envs/customer-jp bootstrap)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"; for r in "${ROOTS[@]}"; do rm -f "$r/backend_override.tf"; done' EXIT

tofu fmt -check -recursive -diff .
tflint --init --config="$PWD/.tflint.hcl" >/dev/null
tflint --recursive --config="$PWD/.tflint.hcl"

export AWS_ACCESS_KEY_ID=offline-plan AWS_SECRET_ACCESS_KEY=offline-plan AWS_EC2_METADATA_DISABLED=true
for root in "${ROOTS[@]}"; do
  echo "== $root"
  printf 'terraform {\n  backend "local" {\n    path = "%s"\n  }\n}\n' "$OUT/${root//\//-}.tfstate" > "$root/backend_override.tf"
  tofu -chdir="$root" init -input=false -reconfigure -no-color >/dev/null
  tofu -chdir="$root" validate -no-color
  tofu -chdir="$root" plan -input=false -lock=false -refresh=false -no-color \
    -var-file=example.tfvars -var offline_plan=true | grep -E '^(Plan|No changes)'
done
