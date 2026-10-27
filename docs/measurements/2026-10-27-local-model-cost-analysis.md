# Đánh giá chi phí GPU so với Bedrock và Khuyến nghị Model Local (#38)

- Ngày: 2026-10-27
- Người thực hiện: Antigravity & Harry
- Nhiệm vụ: Đánh giá chi phí GPU so với Haiku/Opus trên Bedrock `jp.`, và phương án chạy model local cho khách hàng không cho dữ liệu ra ngoài hoặc môi trường không có ngân sách API đám mây (Proposal §5, §7; Thiết kế §9; #38).

---

## 1. Đơn giá Đám mây (AWS Bedrock Serverless)

Mô hình Bedrock tính phí trả theo số lượng token thực tế tiêu thụ (`pay-as-you-go`), không duy trì chi phí cố định:

* **Claude Haiku 5.5 (Tầng Light):**
  * Input uncached: \$1.00 / 1M token; Cache read: \$0.10 / 1M token; Cache write: \$1.25 / 1M token.
  * Output: \$5.00 / 1M token.
  * Ước tính cho 1 issue: ~2 lượt gọi Light (phân loại ban đầu + tóm tắt gate / sinh commit message). Tổng cộng ~10.000 tokens input + 1.000 tokens output $\approx$ **\$0.006 – \$0.01 / issue**.

* **Claude Opus 5.5 (Tầng Core / Expert với profile `jp.`):**
  * Input uncached: \$15.00 / 1M token; Cache read: \$1.50 / 1M token; Cache write: \$18.75 / 1M token.
  * Output: \$75.00 / 1M token. Phụ phí vùng `jp.` +10%.
  * Ước tính cho 1 issue trung bình (15–20 bước lặp agent, tận dụng 85%+ cache hit): $\approx$ **\$1.20 – \$2.50 / issue**.

---

## 2. Chi phí Thuê / Đầu tư GPU Chạy Local Model

Để vận hành các open-weights models có năng lực code tương đương (như `Qwen2.5-Coder-32B-Instruct` hoặc `DeepSeek-Coder-V2-Lite`):

| Hạ tầng | Phần cứng | VRAM | Chi phí định kỳ | Đặc điểm |
| :--- | :--- | :---: | :---: | :--- |
| **AWS EC2 `g6.xlarge`** | 1x NVIDIA L4 | 24 GB | \$0.804 / giờ (~**\$578 / tháng**) | Chạy model 7B–14B FP16 hoặc 32B INT4 |
| **AWS EC2 `g5.xlarge`** | 1x NVIDIA A10G | 24 GB | \$1.006 / giờ (~**\$724 / tháng**) | Hiệu năng suy luận FP16/INT8 ổn định |
| **AWS EC2 `g5.12xlarge`** | 4x NVIDIA A10G | 96 GB | \$5.672 / giờ (~**\$4.083 / tháng**) | Chạy model 70B FP16 hoặc MoE lớn |
| **On-Premise Workstation** | 2x NVIDIA RTX 4090 | 48 GB | Khấu hao: ~**\$237 / tháng** (vốn \$4.500 / 24 tháng + điện) | Đặt tại văn phòng/data center nội bộ |

---

## 3. Phân tích Điểm Hòa Vốn (Break-even Analysis)

| Kịch bản | Chi phí cố định | Đơn giá Bedrock ước tính | Điểm hòa vốn (Số issue/tháng) |
| :--- | :--- | :--- | :--- |
| **Chỉ thay thế tầng Light** (dùng model 7B/14B local) | \$578 / tháng (L4) | \$0.006 / issue | **> 96.000 issue / tháng** |
| **Thay thế cả Light và Core** (dùng Cloud GPU `g5.xlarge`) | \$724 / tháng (A10G) | ~\$1.50 / issue | **~480 issue / tháng** |
| **Thay thế cả Light và Core** (dùng On-premise 2x 4090) | \$237 / tháng | ~\$1.50 / issue | **~160 issue / tháng** |

### Nhận xét:
1. Với tầng **Light**: Haiku 5.5 trên đám mây quá rẻ (\$0.10 cache read). Việc duy trì một GPU cloud riêng biệt chỉ để chạy tầng Light là không khả thi về mặt tài chính trừ khi quy mô issue đạt hàng trăm nghìn lượt mỗi tháng.
2. Với tầng **Core**: Nếu sử dụng Opus 5.5 (do Sonnet 5.5 chưa có profile `jp.`), chi phí cho mỗi issue tương đối đáng kể. Nếu một tổ chức có trên 160 issue/tháng (với máy trạm nội bộ) hoặc trên 480 issue/tháng (với EC2), việc tự host GPU bắt đầu tiết kiệm chi phí hơn.

---

## 4. Đánh giá Khả năng Kỹ thuật & Chất lượng

* **Tầng Light (Phân loại, Tóm tắt, Commit message):**
  * Mô hình phù hợp: `Qwen2.5-Coder-7B-Instruct` hoặc `14B`.
  * Khả năng: Hoàn thành xuất sắc nhiệm vụ, tốc độ sinh token cao (50-80 tokens/s), tuân thủ định dạng JSON/cấu trúc tốt.
* **Tầng Core (Vòng lặp sửa lỗi và tương tác công cụ ACI):**
  * Mô hình phù hợp: `Qwen2.5-Coder-32B-Instruct` (quantized 4-bit/8-bit qua vLLM / Ollama).
  * Khả năng: Đạt tỷ lệ giải quyết Pass@1 khoảng 40% trên SWE-bench Verified (tương đương với các phiên bản Claude Sonnet ban đầu). Có thể xử lý tốt các bug cục bộ trong 1–2 file.
* **Tầng Expert (Bug khó, đa file, tái kiến trúc):**
  * Các mô hình local hiện tại chưa tiệm cận được năng lực suy luận sâu và bối cảnh rộng lớn của Claude Opus 5.5 / Sonnet 5.5.

---

## 5. Kết luận & Khuyến nghị

1. **Khuyến nghị về Chi phí & Vận hành:**
   * Đối với môi trường sản phẩm và khách hàng thông thường: **Tiếp tục ưu tiên Bedrock Serverless** để tối ưu hóa chi phí (trả theo lượng dùng thực tế) và giữ chất lượng sửa lỗi cao nhất từ frontier model.
2. **Khuyến nghị về Tuân thủ (Air-gapped) & Môi trường không có ngân sách:**
   * Cung cấp bản cài `ModelProvider::Local` hỗ trợ giao thức **OpenAI-compatible** (`/v1/chat/completions`) và **Anthropic Messages** (`/v1/messages`).
   * Cấu hình này cho phép người dùng chạy Aegis Code với **Ollama**, **vLLM**, hoặc **LocalAI** với chi phí API = **0 đồng**.
   * Áp dụng `Local` provider cho tầng `Light` và `Core`. Khi gặp lỗi quá khó cần chuyển lên `Expert`, hệ thống sẽ yêu cầu người duyệt can thiệp hoặc sử dụng model có cấu hình cao hơn.
