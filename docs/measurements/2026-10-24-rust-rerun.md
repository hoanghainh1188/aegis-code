# Đo lại KPI Rust sau các sửa lõi (#142)

Ngày đo: 2026-10-24. Máy, image, model và trần như [lượt đo chính](2026-10-24-eval-kpi.md): $4 và 40 lượt gọi Core + Expert mỗi issue. Code là `main` sau ba sửa lõi mà báo cáo chính đề xuất:

| Issue / PR | Thay đổi |
| --- | --- |
| #136 / #138 | Gate nhận test tái hiện khai báo theo class hoặc file |
| #137 / #139 | Công cụ ACI `run_tests`: agent chạy test tái hiện của nó trong sandbox của gate. Đã thử với Docker thật: typst fail ở code gốc và pass với bản sửa trong 36–115 giây khi cache build ấm |
| #140 / #141 | Prompt yêu cầu đặt test tái hiện ngoài các file nguồn mà bản vá sửa; gate nêu rõ lý do khi test nằm trong đó |

Lệnh:

```bash
aegis eval --config config/typst.toml --set eval/issues --only typst-3582 … --only typst-8850
scripts/eval/tool_usage.py typst
```

**Chạy lại 10 issue, không phải 18.** Lượt chạy bắt đầu với cả 18 issue typst, nhưng sau issue đầu (`typst-2639-r2`) thì được dừng bằng Ctrl-C, theo quyết định chỉ chạy lại 10 issue typst chưa qua gate ở #18 để tiết kiệm thời gian và tiền. 8 issue đã qua gate giữ kết quả cũ. Vì vậy lượt này **không đo được hồi quy** ở 8 issue đó, trừ một tín hiệu ở `typst-2639` nêu dưới đây.

## Kết luận

| KPI Rust | #18 | #142, tính `2639-r2` | #142, không tính `2639-r2` | Ngưỡng PoC |
| --- | --- | --- | --- | --- |
| PR được chấp nhận | 33% (6/18) | **39%** (7/18) ✘ | **44%** (8/18) ✔ | ≥ 40% |
| Qua gate | 8/18 | 9/18 | 10/18 | |
| PR làm vỡ test cũ | 0 | 0 | 0 | 0 |
| Chi phí / issue | $0.59 | $0.54 | | ≤ $4 |
| Hidden test pass, trong số qua gate | 6/8 | 7/10 | | theo dõi |

Toàn bộ bộ thử, mỗi issue tính lượt mới nhất (có `2639-r2`): **50% (16/32)** PR được chấp nhận, Python 64%, Rust 39%. Lượt chạy lại tốn **$7.81**: 10 issue cộng lượt `2639-r2` bị dừng.

**Rust đã cải thiện nhưng vẫn sát ngưỡng.** Hai issue chưa qua ở #18 nay đã qua gate và được chấp nhận (6822, 8648). Một issue khác qua gate nhưng fail hidden test (8828). Kết luận đạt hay không phụ thuộc vào lượt `2639-r2`, nên có thể coi là chưa chắc chắn.

### Lượt `typst-2639-r2`

`typst-2639` đã qua gate ở #18 với 39 lượt gọi. Ở lượt mới, Core đọc 15 lượt rồi lên Expert; Expert đọc tiếp và chỉ tạo test, gọi `run_tests` ở lượt thứ 40, tức lượt cuối cùng được phép. Đúng lúc đó lượt chạy bị dừng. Kể cả không bị dừng, lượt chạy cũng đã hết 40 lượt, nên kết quả vẫn là dừng vì hết lượt.

Lượt này được giữ lại, không xoá, vì nó là dữ liệu thật. `aegis report` tính nó là lượt mới nhất của issue. Cột "không tính" ở bảng trên là để thấy nó quyết định kết luận tới mức nào. Một lần chạy không đủ để nói chắc là hồi quy, vì issue này vốn đã sát trần.

## Kết quả từng issue

| Run | Kết quả | Chi phí | Lượt | `run_tests` | Sửa/tạo file đầu ở lượt | `run_tests` đầu ở lượt | Gate đầu ở lượt | Người duyệt |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| typst-2639-r2 | Hết 40 lượt, bị dừng | $0.80 | 40 | 1 | 40 | 40 | — | |
| typst-3582-r2 | Hết 40 lượt | $0.79 | 40 | 2 | 15 | 38 | — | |
| typst-6822-r2 | **PR**, hidden pass | $0.66 | 38 | 3 | 15 | 31 | 38 | accepted |
| typst-7219-r2 | Hết 40 lượt | $0.67 | 40 | 0 | 12 | — | — | |
| typst-7541-r2 | Hết 40 lượt | $0.91 | 40 | 0 | — | — | — | |
| typst-7777-r2 | Hết 40 lượt | $0.68 | 40 | 0 | — | — | — | |
| typst-8648-r2 | **PR**, hidden pass | $0.79 | 40 | 4 | 10 | 36 | 40 | accepted |
| typst-8703-r2 | Hết 40 lượt | $0.88 | 40 | 0 | — | — | — | |
| typst-8826-r2 | Hết 40 lượt | $0.70 | 40 | 0 | 13 | — | — | |
| typst-8828-r2 | PR, **hidden fail** | $0.54 | 27 | 3 | 5 | 6 | 27 | rejected |
| typst-8850-r3 | Agent tự dừng | $0.39 | 29 | 3 | 6 | 15 | 12 | |

Người duyệt tự ghi số phút: 2, 2 và 3 phút. Các cột về công cụ lấy từ `scripts/eval/tool_usage.py`, đọc sự kiện `tool_calls` (#129).

## Vì sao vẫn chưa đủ

1. **Agent có `run_tests` nhưng dùng quá muộn hoặc không dùng.** Chỉ 6/11 lượt có gọi `run_tests`. Ở 4/6 lượt đó, lần gọi đầu rơi vào lượt 31–40. Bốn lượt (7541, 7777, 8703 và 2639 cho tới lượt 40) chỉ đọc code, không sửa file nào: 13–25 lần `open` và 13–24 lần `search_text`. Ca duy nhất dùng sớm là `typst-8828-r2` (`run_tests` ở lượt 6), và nó cũng nộp gate sớm nhất (lượt 27). Như vậy công cụ có ích khi được dùng sớm, nhưng prompt hiện tại không khiến agent dùng sớm.
2. **Expert lặp lại việc đọc của Core.** Luật #127 đưa issue lên Expert ở lượt 15, nhưng Expert lại đọc tiếp 15–25 lượt rồi mới sửa. Lượt của cả hai tầng cộng vào cùng một trần 40, nên chỗ còn lại cho việc sửa và kiểm rất ít.
3. **Thay đổi tham chiếu hàng loạt.** Ở `typst-8850-r3`, bản sửa đổi output HTML của khoảng 368 test. Agent viết: "the `test` check can't pass without regenerating about 368 reference hashes. I have no shell, and `run_tests` runs one test per call". Gate cập nhật được tham chiếu của các test mà agent khai báo (`allowed_test_changes`, #105), nhưng khai báo từng test một thì không khả thi với số lượng này.

## Đề xuất

Theo [ADR 0002](../adr/0002-full-platform-mvp-as-checkpoint.md), mốc đo là để điều chỉnh hướng, không phải cổng dừng dự án. Tổng (50%) và Python (64%) đạt rộng; Rust nằm ở ngưỡng. Đề xuất:

1. **Một sửa lõi nữa cho Rust**, nhắm đúng điểm 1 và 2:
   - Prompt yêu cầu viết và chạy test tái hiện bằng `run_tests` trước khi đọc sâu.
   - Router nhắc agent (hoặc lên Expert) khi đã khoảng 10 lượt mà chưa gọi `run_tests`.
   - Expert nhận tóm tắt những gì Core đã đọc, thay vì tự đọc lại.
2. **Cho phép cập nhật tham chiếu theo nhóm**, ví dụ mọi test của một thư mục `tests/ref/<stage>`, khi người duyệt sẽ xác nhận (điểm 3).
3. **Đo lại cả 18 issue typst**, không chỉ các ca fail, để thấy cả hồi quy.
4. Song song, **có thể bắt đầu Phase 2 từ #22** (`aegis-store`), vì các ngưỡng an toàn và chi phí đã đạt rộng ở cả hai ngôn ngữ. Đây là quyết định của team. Proposal §8 cũng nêu các ngưỡng cần chốt lại với team theo số liệu thật.

Các lưu ý khi đọc số ở báo cáo chính vẫn áp dụng: duyệt nhanh, người duyệt biết kết quả hidden test, repo OSS, mỗi issue chạy một lần.
