# Agent có định vị sai file không? Ledger của Phase 1 (#185)

Ngày đo: 2026-10-25. Không chạy model: chỉ đọc ledger của các lượt đo [#18](2026-10-24-eval-kpi.md), [#142](2026-10-24-rust-rerun.md) và [#145](2026-10-24-rust-rerun-18.md).

Cách đo: lấy lượt mới nhất của mỗi issue, tức lượt mà KPI tính. So các file agent mở và sửa (sự kiện `tool_calls`, #129) với `source_files` của issue, tức các file mã nguồn mà bản sửa của người đã đổi. Có 24 lượt có dữ liệu công cụ: 18 typst và 6 scrapy. 8 lượt scrapy còn lại chạy trước #129.

```bash
scripts/eval/localization.py
```

## Kết luận

| | Số lượt |
| --- | --- |
| Mở một file của bản sửa trong 4 lượt gọi đầu | **20/24** |
| Chỉ sửa file khác với bản sửa | **1/24** (scrapy-4330, hidden test fail) |
| Sửa thêm file mà bản sửa không đổi | 2/24 (scrapy-4330, typst-8560) |
| Sửa đúng file nhưng thiếu file khác của bản sửa | **6/24** (typst 2639, 7219, 7541, 8372, 8826, 8828) |
| Chưa sửa file nguồn nào | 2/24 (typst 7777, 8703) |

- **Định vị sai file hiếm.** Ở 20/24 lượt, agent mở đúng file trong 4 lượt gọi đầu. Chỉ một lượt sửa nhầm file.
- **Sửa thiếu khi bản sửa cần nhiều file.** Trong 6 ca sửa thiếu có 2 PR qua gate nhưng fail hidden test (8372 sửa 1/3 file, 8828 sửa 1/2 file), và 3 ca hết 40 lượt. Ba issue mà bản sửa cần ≥ 3 file (scrapy-4330, typst-8372, typst-8826) đều không đạt.
- **Chậm lần ra file khi lỗi hiện ở crate khác.** typst-7777 mở đúng file ở lượt 16, 32, 22 qua ba đợt đo; typst-8703 ở lượt 21, 18, 32. Cả hai hết 40 lượt ở cả ba đợt mà chưa sửa dòng nào. Ở 7777, agent bắt đầu từ `typst-pdf`, nơi lỗi hiện ra, rồi phải tự lần ngược sang `typst-layout/src/inline/line.rs`.
- Sáu issue typst luôn hết lượt (#145) phần lớn không phải do định vị: 3582, 7219, 7541, 8826 mở đúng file ở lượt 2–7. Nghẽn ở việc sửa và chạy test, như #145 đã nêu.
- Language server (`find_definition`, `find_references`, #115) đã có từ trước mọi lượt đo này, nhưng agent chỉ gọi 3 lần trong 47 lượt chạy typst, so với 665 lần `search_text` và 641 lần `open`.

## Quyết định

Bật Codebase-Memory qua chỗ cắm có sẵn cho cả hai repo (Thiết kế §3.2). Mục tiêu là hai điểm yếu ở trên: tìm nơi gọi xuyên module để thấy hết các file cần sửa, và lần từ chỗ lỗi hiện ra về chỗ gây lỗi. Prompt bảo agent kiểm nơi gọi của hàm vừa sửa trước khi trả lời.

Đồ thị của Rust nối lời gọi theo tên và bỏ sót lời gọi xuyên crate ([#3](2026-10-20-codebase-memory-accuracy.md)), đúng loại lời gọi của 7777. Vì vậy prompt vẫn bảo kiểm bằng `find_references` trước khi đổi chữ ký hàm. Lượt đo KPI kế tiếp, so với #145 (Rust 39%) và #18 (Python 64%), cho biết đồ thị có ích hay không.
