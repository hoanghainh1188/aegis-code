# Đồ thị gọi hàm do platform hỏi trước gate (#203)

Ngày đo: 2026-10-26. Máy, image, model và trần như [lượt đo #185](2026-10-26-codebase-memory-kpi.md): $4 và 40 lượt gọi Core + Expert mỗi issue. Đo trên 8 issue sửa thiếu hoặc chậm định vị ở #185, mỗi issue hai lần, không mở PR:

```bash
aegis eval --config config/poc.toml   --set eval/issues --no-pr --only scrapy-3666 --only scrapy-4999 --only scrapy-7029
aegis eval --config config/typst.toml --set eval/issues --no-pr --only typst-7541 --only typst-7777 --only typst-8703 --only typst-8826 --only typst-8828
scripts/eval/tool_usage.py
```

## Quyết định

**Giữ Codebase-Memory, nhưng platform hỏi đồ thị thay cho agent.** Ở #185, agent có `search_graph` và `trace_path` nhưng không gọi lần nào trong 32 lượt. Vì vậy:
- Gỡ hai công cụ khỏi agent (`codebase_memory.tools` để trống). Mỗi lượt gọi model không còn mang schema của chúng.
- Khi agent trả lời cuối lần đầu, lifecycle chưa chạy gate ngay. Nó hỏi `detect_changes` của Codebase-Memory: hàm nào gọi tới, hay được gọi từ, các hàm bản vá vừa đổi, mà nằm ngoài các file của bản vá.
- Có kết quả thì agent nhận một lời nhắn liệt kê chúng theo file, xem lại, rồi trả lời cuối lần nữa. Lúc đó bản vá mới vào gate. Mỗi job hỏi một lần. Ledger ghi sự kiện `impact_checked`.
- Lời nhắn chỉ gửi khi job còn ít nhất 4 lượt gọi để làm theo.

| | #185 (1 lượt mỗi issue) | #203 (2 lượt mỗi issue, 16 lượt) |
| --- | --- | --- |
| Pass hidden test | 1/8 (scrapy-7029) | 2/16 (cả hai lượt scrapy-7029) |
| Qua gate nhưng fail hidden test | 3/8 | 6/16 |
| Hết 40 lượt / agent tự dừng | 4/8 / 0 | 7/16 / 1 |
| Lượt có hỏi đồ thị trước gate | — | 7/16 |
| Lượt agent nhận lời nhắn | — | 5/16 |
| Lời nhắn nêu file còn thiếu của bản sửa thật | — | 2/5 |
| Agent sửa file đó sau lời nhắn | — | 1/2 |

- **Đồ thị đã thật sự được dùng**: platform hỏi ở mọi lượt có câu trả lời cuối mà còn ít nhất 4 lượt gọi. Trước đây agent không hỏi lần nào.
- **Có ích rõ ở một lượt.** Ở scrapy-7029-r6, bản vá chỉ đổi `scrapy/core/scraper.py`. Lời nhắn nêu `scrapy/core/engine.py`, file thứ hai của bản sửa thật. Agent đọc, sửa thêm và pass hidden test.
- **Agent có thể bỏ qua.** Ở typst-8828-r8, lời nhắn nêu `crates/typst-library/src/introspection/position.rs`, đúng file còn thiếu. Agent chỉ mở file kia trong lời nhắn (`typst-html/src/introspect.rs`) rồi trả lời lại y như cũ. Bản vá qua gate nhưng fail hidden test. Lượt typst-8828-r6 (không tính, xem dưới) cũng bỏ qua file đó.
- **Đồ thị không thấy mọi chỗ thiếu.**
  - scrapy-3666: hàng đợi gọi qua thuộc tính (`self.mqs.pop()`), đồ thị không nối được sang `scrapy/pqueues.py`. Cả hai lượt không nhận lời nhắn nào.
  - scrapy-4999: hai file còn thiếu (`spidermiddlewares/base.py`, `start.py`) không gọi và không bị gọi bởi hàm nào agent đã đổi. Lời nhắn nêu 5–7 file, không file nào là chỗ cần sửa.
- **Không chữa được việc chậm định vị.** Cả 8 lượt typst ở 7541, 7777, 8703, 8826 hết 40 lượt hoặc tự dừng trước khi trả lời. Lời nhắn đến sau câu trả lời đầu tiên nên không tới lượt.
- **Chi phí của lời nhắn thấp**: thêm 2–4 lượt gọi (trung bình 2.6) ở 5 lượt nhận nó, $0.05–0.10 mỗi lần (trung bình $0.06). Lập chỉ mục vẫn 2–6 s mỗi job.

Kết luận: giữ đồ thị ở dạng platform tự hỏi, vì nó rẻ và đã đổi được một kết quả. Nhưng nó chỉ chạm tới các lượt đã có bản vá; hai điểm yếu của #185 không đổi đáng kể trên 16 lượt này. Hai hướng nên đo tiếp:
- Hỏi đồ thị sớm hơn, ngay sau lần sửa file nguồn đầu tiên, để lời nhắn tới cả các lượt hết 40 lượt (typst-7541-r6 có lời nhắn trúng nhưng tới ở lượt 40).
- Viết lời nhắn sao cho agent không bỏ qua file trúng, như ở typst-8828.

## Từng lượt

`acc` = pass hidden test; `rej` = qua gate nhưng fail hidden test; `40` = hết 40 lượt; `stop` = agent tự dừng. Cột "Lời nhắn" là số file đồ thị đưa cho agent và lượt gọi lúc đưa; "trúng" là file của bản sửa thật mà bản vá còn thiếu và lời nhắn có nêu.

| Issue | #185 | Lượt 1 | Lượt 2 | Lời nhắn (lượt 1 / lượt 2) | Lượt gọi / chi phí |
| --- | --- | --- | --- | --- | --- |
| scrapy-3666 | rej | rej (r4) | rej (r5) | 0 file ở lượt 15 / 0 file ở lượt 32 | 15 / $0.16; 32 / $0.58 |
| scrapy-4999 | rej | rej (r5) | rej (r6) | 5 file ở lượt 31 / 7 file ở lượt 31, không trúng | 33 / $0.65; 34 / $0.72 |
| scrapy-7029 | acc | acc (r5) | acc (r6) | 3 file ở lượt 21 (bản vá đã đủ) / 4 file ở lượt 17, **trúng `engine.py`, agent sửa** | 23 / $0.49; 21 / $0.48 |
| typst-7541 | 40 | 40 (r5) | 40 (r8) | chưa trả lời / chưa trả lời | 40 / $0.74; 40 / $0.98 |
| typst-7777 | 40 | 40 (r5) | 40 (r6) | chưa trả lời / chưa trả lời | 40 / $0.76; 40 / $0.97 |
| typst-8703 | 40 | 40 (r6) | stop (r8) | chưa trả lời / agent dừng: issue thiếu thông tin để tái hiện | 40 / $0.86; 31 / $0.54 |
| typst-8826 | 40 | 40 (r5) | 40 (r6) | chưa trả lời / chưa trả lời | 40 / $0.75; 40 / $0.77 |
| typst-8828 | rej | rej (r8) | rej (r9) | 2 file ở lượt 30, **trúng `position.rs`, agent bỏ qua** / trả lời ở lượt 38, không gửi lời nhắn (còn dưới 4 lượt) | 32 / $0.63; 38 / $0.73 |

Ở 8 lượt hết lượt hoặc tự dừng, agent có lượt chưa sửa file nguồn nào (typst-7541-r5, 7777-r5, cả hai lượt 8703), có lượt sửa nhầm sang `typst-pdf` (7777-r6), có lượt sửa một phần (7541-r8: `line.rs`, thiếu `linebreak.rs`; 8826: 2 trong 6 file). Nhưng không lượt nào tới được câu trả lời cuối, nên đồ thị không được hỏi.

## Lỗi tìm ra trong lúc đo

Cả hai lỗi đã sửa trong PR này. Các lượt dính lỗi không được tính.

1. **Daemon giữ thư mục của job đầu tiên.** Server Codebase-Memory chạy qua một daemon chung cho mỗi user. Daemon đó lấy thư mục làm việc của server khởi động nó, ở đây là cây của job đầu tiên. Hết job đó thì cây bị xoá, và từ đó `detect_changes` không chạy được git: "git revision resolution failed … is not a commit" (scrapy-4999-r4). `trace_path` ở #185 không cần git nên không lộ lỗi này. Bản sửa: server chạy trong `<state_dir>/index`.
2. **Lời nhắn ở lượt gọi cuối.** typst-7541-r6 trả lời cuối ở lượt 40. Lời nhắn đến đúng lúc hết trần, nên job dừng ở trần thay vì đưa bản vá vào gate. Bản sửa: chỉ gửi lời nhắn khi còn ít nhất 4 lượt gọi và đủ tiền cho chừng ấy lượt.

Các lượt bị dừng giữa chừng để sửa hai lỗi trên (scrapy-7029-r4, typst-8703-r5, typst-8703-r7, typst-8828-r5) không có kết quả. Ngoài ra không tính:
- typst-7541-r6, dính lỗi 2. Lời nhắn của nó trúng `linebreak.rs`.
- typst-8828-r6: gate không chép được cache build vì đĩa máy đo đầy. Lời nhắn của nó trúng `position.rs`, và agent cũng bỏ qua.
- typst-8828-r7 và typst-7541-r7: Docker từ chối chạy khi đĩa đầy, $0.

## Chi phí

16 lượt được tính tốn **$10.82**: trung bình $0.51 mỗi lượt scrapy và $0.77 mỗi lượt typst (#185: $0.29 và $0.62). Lượt này scrapy tốn hơn vì 5/6 lượt lên Expert sau 15 lượt chưa có bản vá. Lời nhắn chỉ thêm $0.06 mỗi lần. Tính cả các lượt không được tính, tổng là **$13.34**.

Lưu ý khi đọc số: mỗi ô chỉ là hai lượt, dao động giữa các lượt vẫn lớn (#145). typst và scrapy là repo OSS.
