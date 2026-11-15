# Bằng chứng review kiểu SpecRover: số liệu và quyết định (#36)

- Ngày: 2026-11-15
- Issue: #36. Tiêu chí của Thiết kế §9 / Proposal §9: xem lại khi người duyệt mất hơn 15 phút mỗi PR.
- Phụ thuộc đã đóng: #15 (mẫu PR), #18 (KPI có thời gian duyệt).
- Dữ liệu: [eval-kpi](2026-10-24-eval-kpi.md) (#18), [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md), [pilot plan](../pilot/2026-11-team-pilot-plan.md), mẫu PR hiện tại (`aegis-pr::body::render`), bài [SpecRover](https://arxiv.org/abs/2408.02232) (ICSE 2025). Không gọi model; không đổi hành vi mặc định của PR.

## Quyết định

**Không làm (NO-GO) lúc này.** Thời gian duyệt trung bình Phase 1 là **1.7 phút/PR**, dưới xa ngưỡng 15 phút của issue và của [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md). Các lượt KPI sau Phase 1 chạy `--no-pr`, nên chưa có “sau” từ người duyệt thật. Mẫu PR hiện tại đã phủ phần lớn bằng chứng mà SpecRover xuất kèm patch; thêm mục điều kiện/bất biến của từng hàm sẽ kéo dài thân PR mà chưa có bằng chứng là rút ngắn được thời gian duyệt.

**Xem lại khi:**
- Đợt chạy thử #158 (hoặc KPI người duyệt thật trên repo nội bộ #157) ghi **thời gian duyệt trung vị > 15 phút/PR**, đúng điều kiện kích hoạt trong [pilot plan](../pilot/2026-11-team-pilot-plan.md).
- Khi đó: bật section bằng chứng trên một nhánh A/B (cùng tập job, cùng reviewer, form Duyệt PR mở khi bắt đầu đọc), đo mean **và** median `review_minutes` trước/sau, rồi mới quyết định bật mặc định hay chỉ với patch lớn (`files_changed` / số hunk).

## 1. SpecRover xuất gì, và Aegis đã có gì

SpecRover ([arxiv:2408.02232](https://arxiv.org/abs/2408.02232)) suy ra “specification” bằng ngôn ngữ tự nhiên trong lúc sửa issue, rồi đưa chúng cho người (và cho reviewer agent) như bằng chứng. Artifact chính:

| Artifact SpecRover | Vai trò với người duyệt | Tương ứng trong PR Aegis hôm nay |
| --- | --- | --- |
| Buggy locations + *intended behavior* (tóm tắt hàm theo ngữ cảnh issue) | Hiểu hàm phải hành xử thế nào trước khi đọc diff | Một phần: `## Nguyên nhân` + từng dòng `## Thay đổi` (`location` + `note`); **chưa** có pre/post/invariant theo symbol |
| Reproducer test | Kiểm intent bằng FAIL→PASS | `## Test` → Test tái hiện |
| Lý do reviewer agent chấp nhận / feedback sửa | Tín hiệu tin cậy của patch | Không có reviewer agent riêng; thay bằng `## Kết quả gate` (FAIL→PASS thật trong sandbox) + vòng `/aegis fix` do người |
| Lý do chọn patch khi nhiều ứng viên | Giải thích lựa chọn | Không áp dụng: một nhánh `agent/…`, một patch sau gate |

Issue #36 diễn đạt artifact còn thiếu theo hướng “điều kiện, bất biến của hàm bị sửa”. Trong SpecRover đó là **tóm tắt intended behavior bằng ngôn ngữ tự nhiên**, không phải hợp đồng hình thức (Hoare / Design-by-Contract).

## 2. Số liệu thời gian duyệt (baseline “trước”)

| Nguồn | Cách đo | Thời gian duyệt | So với ngưỡng 15 phút |
| --- | --- | --- | --- |
| Phase 1, [eval-kpi](2026-10-24-eval-kpi.md) (#18) | Người duyệt ghi `--minutes` qua `aegis review` trên 15 PR được chấp nhận | **1.7 phút** mean (Python 1.8, Rust 1.5) | Dưới ~9 lần |
| ADR 0005 | Cùng đợt Phase 1 | **1.8 phút** (làm tròn) | Đạt ngưỡng; lưu ý reviewer đã biết kết quả test ẩn |
| KPI Phase 2 profile mới | `--no-pr` | **Không đo được** thời gian duyệt người | — |
| Pilot #158 | Form Duyệt PR → submit (`review_minutes`), người không xem test ẩn | Chưa có số (kế hoạch đo trung vị) | Điều kiện kích hoạt #36 |

Không có số “sau” vì section bằng chứng chưa được bật: đo A/B khi chưa vượt ngưỡng kích hoạt sẽ tốn ngân sách model và thời gian reviewer mà lợi ích kỳ vọng nhỏ (review đã dưới 2 phút trên bộ thử Phase 1).

## 3. Chi phí / rủi ro nếu trích thêm vào PR ngay

1. **Độ dài PR.** Mỗi symbol thêm 2–4 dòng (precondition / postcondition / invariant / intended behavior), qua `inert()`, vẫn đếm vào trần ~60k ký tự của body. Patch nhiều hàm (typst đa file) dễ làm mục bằng chứng dài hơn mục Thay đổi.
2. **Token / lượt gọi.** Nếu agent tự viết evidence trong `FinalAnswer` (nhánh B1): thêm field JSON + vài trăm token output mỗi PR thành công. Nếu platform trích sau gate bằng model Light (nhánh B2): thêm ít nhất một lượt Light trên danh sách hàm từ `detect_changes`.
3. **Nhiễu cho reviewer.** Khi review đã ~2 phút, mục mới dễ thành việc phải đọc thêm mà không đổi quyết định chấp nhận (Phase 1: mọi PR pass hidden test đều được chấp nhận, không sửa dòng).
4. **Trùng với gate.** SpecRover dùng reviewer agent vì thiếu test suite tin cậy cho issue; Aegis đã có repro FAIL→PASS + full suite + Golden Master trong gate trước khi mở PR — đó đã là bằng chứng mạnh hơn tóm tắt ngôn ngữ.

## 4. Thiết kế giữ sẵn khi xem lại

Khi điều kiện kích hoạt đạt, ưu tiên thứ tự sau (không cài trong PR này):

1. **Section PR** `## Bằng chứng (hàm bị sửa)`: mỗi mục một symbol đã đổi, các trường ngôn ngữ tự nhiên `intended` / `pre` / `post` / `invariant` (tối đa N symbol, mỗi field ≤ `MAX_FIELD_CHARS`, toàn bộ qua `inert()`).
2. **Nguồn:** agent điền trong `FinalAnswer` (đồng bộ `prompts/system.md`) *hoặc* Light tóm tắt sau gate từ diff + `CodeGraph::impact` / `detect_changes`; seed danh sách hàm từ đồ thị, không thay thế gate.
3. **Cờ cấu hình** tắt mặc định; A/B trên pilot; KPI so mean và **median** `review_minutes` (pilot dùng trung vị; `aegis-ledger` hiện chỉ aggregate mean).
4. **Chỉ bật cho patch lớn** nếu A/B trên toàn bộ PR không giảm median (ví dụ ≥ 3 file hoặc ≥ 10 hunk).

## 5. Tiêu chí “Xong khi” của #36

| Hạng mục issue | Kết quả báo cáo này |
| --- | --- |
| Trích điều kiện/bất biến vào PR | **Không làm mặc định**; thiết kế giữ ở §4 khi kích hoạt |
| Đo thời gian review trước/sau | **Trước:** 1.7–1.8 phút (#18 / ADR 0005). **Sau:** chưa đo vì chưa bật; điều kiện đo lại ở mục Quyết định |
| Có số liệu và quyết định | **Có:** NO-GO cho đến khi trung vị duyệt > 15 phút/PR |
