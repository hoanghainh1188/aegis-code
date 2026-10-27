# Rà soát Profile Nhật trên AWS Bedrock và GCP Vertex AI (#19)

Ngày rà soát: 2026-10-27.

Tài liệu này giải quyết và đóng issue [#19](https://github.com/hoanghainh1188/aegis-code/issues/19): theo dõi profile `jp.` cho Sonnet 5.5 trên AWS Bedrock và tình trạng hỗ trợ vùng Nhật (`asia-northeast1`) trên Google Cloud Vertex AI.

---

## 1. Bối cảnh & Yêu cầu của Chế độ `customer-jp`

Theo Proposal §7 và [ADR 0003](../adr/0003-platform-service-architecture.md) §6:
* Mã nguồn và dữ liệu của các repository khách hàng thuộc chế độ `customer-jp` **bắt buộc phải được lưu trữ và xử lý 100% trong lãnh thổ Nhật Bản** (không được rời khỏi biên giới Nhật Bản).
* Toàn bộ hạ tầng của `customer-jp` được cố định tại vùng Tokyo `ap-northeast-1` (EC2, RDS PostgreSQL, Secrets Manager, ECR, S3 state).
* Tại tầng Model Provider (`aegis-provider`), cờ `require_jp_profile = true` được kích hoạt. Bất kỳ request nào có Model ID không bắt đầu bằng tiền tố `jp.` đều bị từ chối ngay lập tức (`ModelPolicy::JapanOnly`).

Cấu hình hiện tại trong `config/customer-jp.toml`:
* **Light:** `jp.anthropic.claude-haiku-5-5`
* **Core:** `jp.anthropic.claude-opus-5-5` (với `effort = "low"`)
* **Expert:** `jp.anthropic.claude-opus-5-5` (với `effort = "high"`)

Tầng Core đang phải tạm thời sử dụng Opus 5.5 với mức effort thấp vì lúc đầu Bedrock chưa có profile `jp.` cho Sonnet 5.5.

---

## 2. Kết quả Rà soát chi tiết

### 2.1. AWS Bedrock: Profile `jp.` cho Claude Sonnet 5.5
* **Cơ chế định tuyến địa lý của Bedrock (CRIS - Cross-Region Inference):**
  AWS Bedrock sử dụng tiền tố `jp.` cho các inference profile hệ thống (System-defined Inference Profiles) chỉ định tuyến lưu lượng giữa 2 region nội địa Nhật Bản: **Tokyo (`ap-northeast-1`)** và **Osaka (`ap-northeast-3`)**.
* **Trạng thái hiện tại:**
  * Các model Anthropic đã có profile `jp.`:
    * `jp.anthropic.claude-haiku-5-5`
    * `jp.anthropic.claude-opus-5-5`
  * Đối với **Claude Sonnet 5.5**: AWS Bedrock hiện tại chỉ cung cấp profile Global (`global.anthropic.claude-sonnet-5-5`) và các profile US/EU. AWS **chưa phát hành** system-defined inference profile mang tiền tố `jp.` (`jp.anthropic.claude-sonnet-5-5`) cho cặp region Tokyo/Osaka.
* **Lệnh kiểm tra định kỳ (dành cho runbook / vận hành):**
  ```bash
  aws bedrock list-inference-profiles \
      --region ap-northeast-1 \
      --type-equals SYSTEM_DEFINED \
      --query "inferenceProfileSummaries[?status=='ACTIVE' && starts_with(inferenceProfileId, 'jp.')].[inferenceProfileId, inferenceProfileName]" \
      --output table
  ```
* **Quyết định cấu hình:**
  * **Giữ nguyên** cấu hình tầng Core trong `config/customer-jp.toml` là `jp.anthropic.claude-opus-5-5` (effort thấp).
  * **Không chuyển** sang `global.anthropic.claude-sonnet-5-5` vì vi phạm chính sách lưu trữ dữ liệu Nhật Bản (`customer-jp` sẽ từ chối gọi model nếu không có tiền tố `jp.`).

---

### 2.2. GCP Vertex AI: Hỗ trợ Claude tại `asia-northeast1` (Tokyo)
* **Trạng thái hiện tại:**
  * Google Cloud Vertex AI có hạ tầng tại vùng Tokyo (`asia-northeast1`), tuy nhiên các model đối tác bên thứ ba của Anthropic (Claude) chủ yếu được phục vụ qua các endpoint đa vùng tại Mỹ (`us-central1`, `us-east5`) hoặc châu Âu (`europe-west1`).
  * Vertex AI hiện chưa cung cấp cam kết xử lý hoàn toàn khép kín trong nội địa Nhật Bản (In-country Data Residency isolation) cho Claude 5.5 tương đương với giải pháp Bedrock CRIS `jp.` giữa Tokyo và Osaka.
* **Kết luận:**
  * Tiếp tục duy trì **AWS Bedrock tại `ap-northeast-1`** là backend Model Provider độc quyền cho chế độ `customer-jp`.

---

## 3. Tổng kết & Hành động

1. **Về cấu hình:** Cấu hình `config/customer-jp.toml` giữ nguyên như thiết kế ban đầu; bổ sung chú thích rõ ràng về tình trạng chưa có `jp.anthropic.claude-sonnet-5-5`.
2. **Về Proposal:** Đánh dấu hoàn thành các mục nghiên cứu tương ứng của Issue #19 trong `docs/design/00-proposal.md`.
