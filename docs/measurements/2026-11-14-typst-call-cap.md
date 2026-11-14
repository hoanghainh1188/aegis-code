# Trần 80 lượt gọi cho typst, đo hai lần (#156)

Ngày đo: lần 1 trong đợt [#448](2026-11-13-csharp-prompt-rules.md#rust-typst) (2026-11-13), lần 2 từ 06:46 tới 10:17 ngày 2026-11-14. Cả hai lần dùng cùng một bản dựng: nhánh `448-csharp-prompt-rules` (trên `243391a`). Bản này có cả lời nhắc chạy lại test của router (#209). Model: Light `claude-haiku-5-5`, Core `claude-sonnet-5-5`, Expert `claude-opus-5-5` (effort `high`), $4 mỗi issue. Trần lượt gọi 80 lượt, chỉ đặt trong [config/typst.toml](../../config/typst.toml). Chi tiêu lần 2: **$6.68**.

"Được chấp nhận" là qua gate **và** qua test ẩn, chạy với `--no-pr`, không có người duyệt.

## Câu hỏi

Ở [#145](2026-10-24-rust-rerun-18.md), cả 18 issue typst được chạy với trần 40 lượt. Có 7 issue hết 40 lượt, và 6 trong số đó hết 40 lượt ở cả ba vòng đo của [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md). #156 hỏi trần 80 lượt có giúp được các issue đó không. Issue chọn 8 issue để đo: 7 issue hết 40 lượt ở #145, cùng với `8850`. Mỗi issue đo hai lần để tách tín hiệu khỏi nhiễu.

## Kết luận

**Trần 80 lượt cứu được một issue chắc chắn (`7541`) và một issue nửa số lần (`3582`). Đề xuất giữ 80 lượt cho typst; việc nâng trần mặc định của profile Rust theo dõi ở #471.**

| Issue | #145, trần 40 | Lần 1, trần 80 | Lần 2, trần 80 | Cần > 40 lượt để qua? |
| --- | --- | --- | --- | --- |
| typst-3582 | hết 40 lượt | **pass** ($0.73, 34) | **pass** ($1.05, 46) | lần 2 |
| typst-7219 | hết 40 lượt | fail ($0.79, 36) | fail ($0.71, 36) | — |
| typst-7541 | hết 40 lượt | **pass** ($1.45, 53) | **pass** ($1.94, 61) | cả hai lần |
| typst-7777 | hết 40 lượt | fail ($1.99, 77) | fail ($1.04, 49) | — |
| typst-8560 | hết 40 lượt | **pass** ($0.62, 31) | **pass** ($0.58, 30) | không |
| typst-8703 | hết 40 lượt | agent tự dừng ($0.87, 38) | agent tự dừng ($0.43, 22) | — |
| typst-8826 | hết 40 lượt | fail ($1.20, 51) | fail ($0.72, 31) | — |
| typst-8850 | agent tự dừng | fail ($0.17, 16) | fail ($0.21, 17) | — |
| **Qua** | **0/8** | **3/8** | **3/8** | |

Số trong ngoặc là chi phí và số lượt gọi Core + Expert. "fail" là qua gate nhưng trượt test ẩn.

- **Hai lần đo cho cùng kết quả ở cả 8 issue.** Ba issue qua cả hai lần, năm issue trượt cả hai lần. Số lượt gọi thì dao động nhiều: `3582` dùng 34 rồi 46, `7777` dùng 77 rồi 49, `8826` dùng 51 rồi 31.
- **Chỉ một phần phần tăng đến từ trần lượt gọi.** Trong 6 lượt qua, có 3 lượt dùng quá 40 lượt: `7541` cả hai lần, và `3582` lần 2. `8560` qua với khoảng 30 lượt ở cả hai lần, và `3582` lần 1 qua với 34 lượt. Ở #145 cả hai issue này đều hết 40 lượt, nên phần tăng của chúng đến từ những thay đổi khác giữa #145 và bây giờ: lời nhắc chạy test (#209), các quy tắc prompt (#448) và những sửa khác trên `main`. Các thay đổi này đo gộp, nên không tách được tác dụng riêng của từng cái.
- **Trần 80 không cứu được các lượt trượt.** Năm issue trượt đều dừng trước 80 lượt: 4 issue qua gate nhưng trượt test ẩn, còn `8703` thì agent tự dừng. Lượt dài nhất (`7777` lần 1, 77 lượt) cũng trượt test ẩn. Với các issue này, thêm lượt gọi chỉ làm tốn thêm tiền.
- **Chi phí vẫn xa trần $4.** Lượt đắt nhất là $1.99 (`7777` lần 1). Trung bình lần 2 là $0.84 mỗi issue. Ở #448, trung bình $0.78 trên cả 18 issue, không tính các lượt hỏng vì môi trường.

### Tính cho cả 18 issue typst

Ở #448 (một lượt mỗi issue, trần 80), typst qua **10/18 (56%)**, so với 7/18 (39%) ở #145. Nếu cắt ở 40 lượt, `7541` sẽ trượt ở cả hai lần, và `3582` trượt ở lần 2. Như vậy ở trần 40, typst còn khoảng 9/18 (50%), có lần 8/18 (44%). Cả hai mức đều trên ngưỡng 40% mà ADR 0005 đặt làm mục tiêu Phase 2 cho mỗi ngôn ngữ. Cách tính này giả định các lượt đã qua dưới 40 lượt vẫn qua khi cắt ở 40. Đó là một giả định, không phải số đo.

### Lời nhắc chạy test (#209)

Router nhắc trong mọi lượt, 1–3 lần mỗi lượt (gồm cả 2 lời nhắc trước lần chạy test đầu tiên). Agent gọi `run_tests` từ 1 lần (`8703` lần 2) đến 17 lần (`7541` lần 2). Các lượt qua gọi `run_tests` 3–17 lần (trung vị 9), các lượt trượt 1–8 lần (trung vị 4). Hai khoảng này chồng lên nhau, nên không suy ra được quan hệ nhân quả. Không có lượt nào chạy với lời nhắc tắt, nên chưa đo được tác dụng riêng của nó.

## Quyết định

1. **Giữ `max_model_calls = 80` trong `config/typst.toml`.** Trần này giữ lại được `7541` ở cả hai lần, và chi phí vẫn dưới một nửa trần $4.
2. **Trần mặc định vẫn 40 lượt** cho tới khi #471 xong, theo [ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md) §4. Đề xuất cho issue đó: cho mỗi profile ngôn ngữ một trần mặc định riêng, và đặt Rust ở 80. Ở các ngôn ngữ khác, trần 40 ít khi là giới hạn: ví dụ C# trung bình 14.7 lượt gọi ở #448.
3. **Lời nhắc chạy test của router giữ nguyên.** Muốn biết tác dụng riêng của nó thì cần một lượt đo tắt lời nhắc, và việc này không chặn quyết định về trần.

## Lưu ý

- Hai lượt mỗi issue vẫn là ít. Một lượt đổi kết quả làm tỉ lệ trên 8 issue đổi 12.5 điểm.
- typst là mã nguồn mở, model có thể đã thấy bản sửa khi huấn luyện.
- Trước mỗi issue của lần 2, thư mục `incremental` trong cache build typst được xoá để giữ chỗ trống trên đĩa. Việc này chỉ làm build chậm hơn, không đổi kết quả.
- Ledger nằm ở `.workspace/state/ledger/TYPST-<n>-R<k>.json`, chỉ có trên máy đo. Lần 1: `3582-R5`, `7219-R5`, `7541-R9`, `7777-R7`, `8560-R6`, `8703-R12`, `8826-R10`, `8850-R9`. Lần 2: `3582-R6`, `7219-R6`, `7541-R10`, `7777-R8`, `8560-R7`, `8703-R13`, `8826-R11`, `8850-R10`.
