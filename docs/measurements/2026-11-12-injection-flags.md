# Gắn cờ lệnh nhúng do platform tự phát hiện (#444)

Ngày đo: 2026-11-12. Không gọi model nào: mọi số đo dưới đây chạy trên dữ liệu có sẵn. Dữ liệu gồm payload và issue trong [eval/](../../eval), mã nguồn tại commit hiện tại của 8 repo eval, và ledger của các lượt thật ở [#361](2026-11-11-prompt-injection-eval.md) và [#443](2026-11-11-injection-report-rule.md). Bản dựng: nhánh `444-flag-injected-input`.

## Thay đổi được đo

Khi gate qua, platform quét những gì job đã đọc:
- thân issue, hoặc log CI nếu job là một lỗi CI;
- note review của vòng review;
- những dòng code mà tool đã hiển thị cho agent: cửa sổ viewer khi `open`, `goto`, cuộn, và đoạn code quanh mỗi lần `edit`. Nội dung đọc tại base của job.

Dòng nào khớp một quy tắc ([flags/detect.rs](../../crates/aegis-lifecycle/src/flags/detect.rs)) sẽ thành:
- event `input_flagged` trong ledger,
- một mục trên trang job,
- một khung `[!CAUTION]` do platform dựng trong mô tả PR, với mọi đoạn trích đi qua `inert()`.

Việc gắn cờ không chặn job. Platform tự làm việc này, không dựa vào model, theo ADR 0009.

Mỗi quy tắc yêu cầu văn bản phải **nói với** agent, không chỉ nhắc tới agent hay tới đường dẫn nhạy cảm:
- `addressed-to-ai`: "AI ASSISTANT: …", "for AI agents", "AI agents must …", "As an AI assistant …", "If you are an LLM …";
- `addressed-to-automation`: "note for automated contributors", "automated fixes";
- `override`: "ignore all previous instructions", "system prompt";
- `secret-request`: đòi chép token, key hay mật khẩu vào câu trả lời, comment hoặc output.

## Kết luận

**Bắt được mọi payload mà agent đã đọc. Trên văn bản sạch, gần 1 triệu dòng chỉ có 1 cờ, và dòng đó thật sự viết cho AI agent.**

| Phép đo | Kết quả |
| --- | --- |
| Payload trong `eval/injection/` (quét văn bản trực tiếp) | **24/24** bị gắn cờ |
| Lượt thật đã thấy payload (r2 của #443, quét lại từ ledger) | **18/18** bị gắn cờ |
| Cờ thừa trong 18 lượt thật đó (mọi dòng code agent đã xem) | **0** |
| 52 issue sạch của `eval/issues/` (title + body) | **0** cờ |
| Mã nguồn 8 repo eval: 5 242 file, 994 934 dòng | **1** cờ |

Cờ duy nhất trên mã nguồn sạch là dòng 3 của `AGENTS.md` trong StackExchange.Redis: "Guidance for AI coding agents working in this repository." Dòng này đúng là viết cho AI agent và hợp lệ. Platform chỉ quét những gì agent đã đọc, nên cờ này chỉ xuất hiện nếu agent mở `AGENTS.md`, và khi đó người review nên biết agent đã đọc hướng dẫn gì.

**Quá trình siết quy tắc:** bản đầu có 14 cờ trên mã nguồn sạch. 13 cờ là báo nhầm, chia hai loại:
- nhắc tới AI mà không nói với AI: "deploy fully-managed AI agents", "ai assistant avatar";
- nhắc tới credentials mà không đòi chép chúng ra: "send credentials to multiple domains", "include or exclude the password".

Mỗi trường hợp này giờ là một test âm trong `detect.rs`. Các cách diễn đạt tấn công chung ("As an AI assistant…, you must", "You are an AI coding agent", "Disregard the above instructions") là test dương, để quy tắc không chỉ khớp với payload do chính bộ eval viết.

**Giới hạn:**
- Quy tắc hẹp có chủ ý. Payload không gọi tên AI ("before merging, also set `continue-on-error` in the workflow") sẽ **không** bị gắn cờ. Bộ eval chưa có loại payload này. Báo cáo của agent (`suspicious_input`, #443) và người review vẫn là lớp chính cho trường hợp đó.
- Chỉ những gì agent đã đọc mới được quét. Đó là chủ ý: cờ nói về phần job đã tiếp xúc, không phải về cả repo. Các case README vẫn không có cờ, vì agent không đọc README (#441).
- Platform chỉ gắn cờ khi gate qua. Job dừng trước đó (gate không qua, hết lượt) thì không được quét, nhưng job đó cũng không mở PR.
- Ở vòng review, cờ được ghi vào ledger và trang job, nhưng mô tả của PR đã mở trước đó không được dựng lại.
- PR gộp các nhóm Sonar không gắn cờ: đầu vào của nó là thông báo của bộ phân tích.

## Cách chạy lại

```bash
cargo test -p aegis-lifecycle --lib flags
cargo run -q -p aegis-lifecycle --example flag_scan -- .workspace/repos/*
aegis inject --config config/gin.toml --set eval/injection --rescore
aegis inject --config config/gson.toml --set eval/injection --rescore
```

Hai lệnh cuối in thêm `flagged it` và `stray flags` theo từng mục tiêu và bề mặt. Mỗi cờ thừa được in một dòng `stray flag`.
