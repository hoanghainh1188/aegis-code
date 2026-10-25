# KPI sau khi bật Codebase-Memory (#185)

Ngày đo: 2026-10-25 → 2026-10-26. Máy, image, model và trần như [lượt đo chính](2026-10-24-eval-kpi.md): $4 và 40 lượt gọi Core + Expert mỗi issue. Code là `main` sau PR #194: agent có thêm `search_graph` và `trace_path` của [Codebase-Memory](2026-10-25-localization.md), và prompt bảo agent kiểm nơi gọi của chỗ vừa sửa. Chạy cả 32 issue, scrapy và typst song song:

```bash
aegis eval --config config/poc.toml   --set eval/issues
aegis eval --config config/typst.toml --set eval/issues
scripts/eval/tool_usage.py
scripts/eval/localization.py
```

So với các lượt mới nhất trước đó: scrapy theo [#18](2026-10-24-eval-kpi.md), typst theo [#145](2026-10-24-rust-rerun-18.md).

## Kết luận

| KPI | Trước | #185 | Ngưỡng PoC |
| --- | --- | --- | --- |
| Pass hidden test, Python | 64% (9/14) | **71% (10/14)** | ≥ 40% |
| Pass hidden test, Rust | 39% (7/18) | **33% (6/18)** | ≥ 40% |
| Pass hidden test, tổng | 50% (16/32) | **50% (16/32)** | ≥ 40% |
| Qua gate, Python / Rust | 11/14 / 10/18 | **14/14** / 9/18 | |
| Hết 40 lượt, Rust | 7 | **9** | |
| PR làm vỡ test cũ | 0 | 0 | 0 |
| Chi phí / issue, Python / Rust | $0.47 / $0.62 | **$0.29** / $0.62 | ≤ $4 |
| Lượt gọi / issue, Python / Rust | — / — | 15.6 / 31.5 | |

Lượt này tốn **$15.12**: $4.00 cho scrapy, $11.12 cho typst.

"Pass hidden test" đứng thay cho "PR được chấp nhận": các PR của lượt này chưa được duyệt bằng `aegis review`. Ở ba lượt đo trước, PR nào pass hidden test cũng được chấp nhận, và PR nào fail hidden test cũng bị từ chối.

**Agent không gọi `search_graph` hay `trace_path` lần nào trong 32 lượt.** Công cụ có mặt ở mọi lượt: daemon nhận 33 lần `tools/list` (32 job và một lượt chạy khô), và mỗi job đều `index_repository` rồi `delete_project`. `find_references` và `find_definition` (#115) cũng chỉ được gọi 1 lần. Vì vậy thay đổi KPI ở trên không đến từ đồ thị: chênh lệch 1–2 issue giữa các đợt nằm trong mức dao động đã thấy ở #145, khi mỗi issue chỉ chạy một lần.

## Từng issue

`acc` = pass hidden test; `rej` = qua gate nhưng fail hidden test; `40` = hết 40 lượt; `exp` = Expert fail 2 lần; `stop` = agent tự dừng.

| Issue | Trước | #185 | Lượt / chi phí |
| --- | --- | --- | --- |
| scrapy-1280 | acc | acc | 13 / $0.15 |
| scrapy-2702 | acc | acc | 7 / $0.08 |
| scrapy-3095 | acc | acc | 11 / $0.11 |
| scrapy-3666 | rej | rej | 16 / $0.32 |
| scrapy-3778 | 40 | **acc** | 14 / $0.19 |
| scrapy-4330 | rej | rej | 17 / $0.37 |
| scrapy-4466 | acc | acc | 35 / $1.06 |
| scrapy-4729 | acc | acc | 17 / $0.24 |
| scrapy-4999 | 40 | rej | 20 / $0.42 |
| scrapy-7029 | acc | acc | 26 / $0.56 |
| scrapy-7597 | acc | acc | 9 / $0.10 |
| scrapy-7604 | acc | acc | 5 / $0.04 |
| scrapy-7730 | acc | **rej** | 11 / $0.13 |
| scrapy-8186 | exp | **acc** | 18 / $0.22 |
| typst-2639 | acc | **40** | 40 / $0.86 |
| typst-3582 | 40 | 40 | 40 / $0.77 |
| typst-6822 | acc | **40** | 40 / $1.00 |
| typst-7219 | 40 | 40 | 40 / $0.76 |
| typst-7541 | 40 | 40 | 40 / $0.98 |
| typst-7777 | 40 | 40 | 40 / $0.77 |
| typst-8372 | rej | 40 | 40 / $0.71 |
| typst-8476 | acc | acc | 27 / $0.55 |
| typst-8560 | 40 | **acc** | 31 / $0.62 |
| typst-8616 | acc | acc | 23 / $0.42 |
| typst-8648 | acc | acc | 28 / $0.59 |
| typst-8672 | rej | rej | 32 / $0.54 |
| typst-8703 | 40 | 40 | 40 / $0.84 |
| typst-8725 | acc | acc | 7 / $0.08 |
| typst-8826 | 40 | 40 | 40 / $0.73 |
| typst-8828 | rej | rej | 34 / $0.63 |
| typst-8850 | stop | rej | 19 / $0.20 |
| typst-8857 | acc | acc | 6 / $0.07 |

- Scrapy: 3778 và 8186 lên `acc`, 7730 xuống `rej`. Cả 14 issue đều qua gate.
- Typst: 2639 và 6822 xuống `40`, còn 8560 lên `acc`. 2639 dao động `acc`/`40` qua cả bốn đợt (#18, #142, #145 và đợt này).
- `typst-8850` qua gate lần đầu, nhờ gate cập nhật tham chiếu theo nhóm (#150), nhưng fail hidden test.

## Định vị file

`scripts/eval/localization.py`, trên 32 lượt mới nhất:

| | [Phase 1](2026-10-25-localization.md) (24 lượt) | #185 (32 lượt) |
| --- | --- | --- |
| Mở file của bản sửa trong 4 lượt gọi đầu | 20/24 | 26/32 |
| Chỉ sửa file khác với bản sửa | 1/24 | 2/32 (scrapy-4330, typst-6822) |
| Sửa đúng file nhưng thiếu file khác | 6/24 | 6/32 (scrapy 3666, 4999, 7029; typst 7541, 8826, 8828) |
| Chưa sửa file nguồn nào | 2/24 | 5/32 |

Hai điểm yếu mà #185 nhắm tới vẫn còn nguyên:
- Sửa thiếu vẫn 6 ca.
- typst-7777 và typst-8703 lần ra file đúng ở lượt 25 và 23, rồi hết lượt mà chưa sửa dòng nào, như ở cả ba đợt trước.

## Đề xuất

Giữ Codebase-Memory trong cấu hình cho tới khi có quyết định ở [#203](https://github.com/hoanghainh1188/aegis-code/issues/203), vì công cụ chạy ổn: lập chỉ mục 2–6 s, không lỗi ở job nào. Nhưng chỉ có công cụ và một dòng trong prompt thì chưa đủ để agent dùng tới, cũng như `find_references` ở #115 và `run_tests` trước #144. #203 sẽ chọn một trong ba hướng: router nhắc dùng, platform tự hỏi đồ thị nơi gọi của chỗ vừa sửa và đưa kết quả cho agent, hoặc gỡ hai công cụ.

Lưu ý khi đọc số: như các lượt trước, mỗi issue chỉ chạy một lần, người duyệt biết kết quả hidden test, và typst cùng scrapy là repo OSS.
