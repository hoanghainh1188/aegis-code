# Đánh giá Incident & Domain Memory theo repo (#456)

- **Ngày đo:** 2026-11-15
- **Issue:** `#456` (`store: Incident & Domain Memory lưu postmortem, review feedback và business rules theo repo`)
- **Milestone:** `Phase 4 – AMS Quy mô Doanh nghiệp`
- **Phụ thuộc:** `#451` (đã hợp nhất trong PR `#522`)

---

## 1. Thiết kế đã triển khai

1. **Schema PostgreSQL (`crates/aegis-store/migrations/20261115170000_incident_memory.sql` & `crates/aegis-store/src/memory.rs`):**
   - Bảng `repo_memories` phân tách theo từng `repo_id` (`REFERENCES repos (id) ON DELETE CASCADE`) với 4 loại tri thức (`kind`):
     - `merged_pr`: bản tóm tắt nguyên nhân gốc rễ và mẫu sửa lỗi từ PR đã merge.
     - `review_lesson`: bài học rút ra từ các vòng review `/aegis fix` (`review_rounds_of`).
     - `runbook`: hướng dẫn chẩn đoán và xử lý sự cố vận hành theo repo.
     - `business_rule`: quy tắc nghiệp vụ / bất biến miền (domain invariant) của repo.
   - Khóa chống trùng lặp `(repo_id, kind, source_ref)` giúp việc ghi nhận từ webhook hoặc API hoàn toàn idempotent (`ON CONFLICT DO UPDATE`), mọi thao tác ghi/xóa đều được ghi vào `audit_log` (`memory.upsert`, `memory.delete`).
   - Hàm truy hồi `Store::recall_repo_memories` và `rank_memories` chấm điểm độ liên quan theo chồng lấp đường dẫn file (`paths`), từ khóa (`keywords`), tiêu đề (`title`), nội dung tóm tắt (`summary`) và cộng điểm ưu tiên cho `business_rule` / `runbook` khi có từ khóa khớp.

2. **Tự động trích xuất ở tier `Light` sau khi PR được merge (`crates/aegis-lifecycle/src/memory.rs` & `crates/aegis-server/src/memory.rs`):**
   - `extract_from_merged_pr` gọi model tier `Light` (`EXTRACT_SYSTEM`) kèm cơ chế dự phòng tất định (`fallback_extract_from_merged_pr`) đọc trực tiếp từ `IssueRecord.answer` và các vòng review (`review_rounds_of`) của PR gốc.
   - Tự động kích hoạt khi nhận webhook PR/MR của nhánh `agent/*` được merge (`pull_request` `closed` + `merged: true` trên GitHub; `Merge Request Hook` `merged` trên GitLab) hoặc khi người duyệt chấp nhận PR trên trang review (`/jobs/{id}/review`).
   - Cung cấp REST API `GET/POST /api/v1/repos/{id}/memories` và `DELETE /api/v1/repos/{id}/memories/{memory_id}` để maintainer quản lý `runbook` và `business_rule` theo từng repo.

3. **Đưa tri thức truy hồi vào `BP3`, giữ nguyên cache prefix `BP2` và bảo vệ chống prompt injection (ADR 0009):**
   - Trước khi chạy job trong `LifecycleRunner::attempt`, `enrich_with_memory` truy hồi tối đa 5 bài học liên quan nhất của repo và gắn vào ngữ cảnh issue qua `IssueInput::attach_memories` (`format_for_bp3`).
   - Toàn bộ dữ liệu trích xuất và truy hồi đều đi qua `aegis_gate::secrets::redact` và `defang_memory_text` (phá các thẻ `<issue>`, `</issue>`, `<repo-memory>`, `</repo-memory>` bằng zero-width space), giới hạn kích thước tối đa `3500` ký tự và đặt trong khối `<repo-memory count="N">...</repo-memory>` ở `BP3` (`IssueInput::first_message`, `light::classify`, `diagnose::prompt`).
   - `BP1` và `BP2` (`RepoPrompt` / `system_blocks`) không thay đổi bất kỳ byte nào khi bật hoặc tắt Incident Memory, giữ ổn định tuyệt đối tỉ lệ đọc prompt cache (`cache_read`).

---

## 2. Kết quả kiểm chứng và đo so sánh trước / sau khi bật Incident Memory

Bài kiểm thử thực nghiệm đầu-cuối (`crates/aegis-lifecycle/tests/memory_eval.rs`) và kiểm thử tích hợp PostgreSQL (`crates/aegis-store/tests/memory.rs`, `crates/aegis-server/tests/api.rs`) đo trực tiếp hai điều kiện trên cùng repo fixture Rust với ACI server, 6-step verification gate và GitHub PR mock server:

### 2.1. Kiểm chứng truy hồi ≥ 3 bài học lịch sử và tính bất biến của `BP2`

| Chỉ tiêu kiểm chứng | Kết quả đo |
| --- | --- |
| Số bài học lịch sử liên quan được truy hồi vào `BP3` | **4 bài học** (≥ 3 theo tiêu chí nghiệm thu): 1 `merged_pr`, 1 `review_lesson`, 1 `business_rule`, 1 `runbook` |
| Rò rỉ tri thức từ repo khác hoặc PR không liên quan | **0** (`rank_memories` chấm 0 điểm và loại bỏ PR OAuth không liên quan; `recall_repo_memories` cô lập theo `repo_id`) |
| Khác biệt trên `BP1 + BP2` (`request.system`) trước và sau khi bật memory | **0 byte** (`no_mem_core_reqs[0].system == with_mem_core_reqs[0].system`) |
| Redact secret & defang thẻ XML (`</issue>`, `</repo-memory>`) | **Đạt 100%** (`format_for_bp3_redacts_secrets_and_defangs_tags`) |

### 2.2. So sánh tỉ lệ chấp nhận và số lượt gọi model (`before_vs_after_incident_memory_improves_acceptance_and_reduces_model_calls`)

Trên cặp ca lỗi biên nghiệp vụ (`Cart::add` vượt ngưỡng `MAX_ITEMS`), nơi quy tắc bất biến (`len() >= MAX_ITEMS`) và yêu cầu regression test (`tests/fourth_item.rs`) nằm trong lịch sử PR/review và business rule của repo:

| Điều kiện | Số ca chạy | Số PR vượt Gate & được chấp nhận (`accepted`) | Tỉ lệ chấp nhận | Số lượt gọi model `Core`/`Expert` từng ca | Trung bình lượt gọi model / issue |
| --- | ---: | ---: | ---: | --- | ---: |
| **Trước khi bật Incident Memory** (`no-mem-1`, `no-mem-2`) | 2 | 1 / 2 (`no-mem-1` mở PR; `no-mem-2` dừng ở `StoppedSteps` do thiếu bất biến miền) | **50.0%** | `7` lượt (`no-mem-1`), `5` lượt (`no-mem-2`) | **6.0 lượt** |
| **Sau khi bật Incident Memory** (`with-mem-1`, `with-mem-2`, truy hồi 4 bài học vào `BP3`) | 2 | 2 / 2 (cả 2 mở PR và được chấp nhận ngay ở vòng đầu, 0 lần leo thang Expert) | **100.0%** | `5` lượt (`with-mem-1`), `5` lượt (`with-mem-2`) | **5.0 lượt** (-16.7%) |

- Khi chưa có Incident Memory, agent phải tốn thêm các lượt gọi `search` và `open` để dò tìm hằng số `MAX_ITEMS`, hoặc dừng lại (`StoppedSteps`) khi không chắc chắn về quy tắc nghiệp vụ của repo.
- Khi bật Incident Memory với 4 bài học lịch sử được truy hồi vào `BP3`, agent định vị trực tiếp `src/lib.rs:24` và viết đúng bài test hồi quy theo `review_lesson`, giúp giảm số lượt gọi `Core`/`Expert` trung bình từ **6.0 xuống 5.0 lượt/issue** (-16.7%) và nâng tỉ lệ chấp nhận trong bộ thử có kiểm soát từ **50.0% lên 100.0%**.

---

## 3. Giới hạn hiện tại và bước tiếp theo

- Bộ đo trên là phép thử A/B có kiểm soát trong `aegis-lifecycle` để xác nhận cơ chế trích xuất (`Tier::Light`), truy hồi (≥ 3 bài học lịch sử), tính bất biến của cache prefix `BP2` và tác động giảm lượt gọi tìm kiếm khi có tri thức lịch sử.
- Theo [ADR 0011 §4–5](../adr/0011-ams-direction.md), số liệu thực tế trên chuỗi ticket vận hành dài ngày sẽ tiếp tục được thu thập khi chạy pilot với đội thật (`#158`) và đo KPI trên repo nội bộ (`#157`).
