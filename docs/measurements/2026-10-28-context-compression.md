# Nén context nâng cao (SWEzze / SaraCoder): số liệu và quyết định (#35)

- Ngày: 2026-10-28
- Issue: #35. Tiêu chí của Thiết kế §9: chỉ làm nếu context thường xuyên vượt 60% cửa sổ.
- Dữ liệu: ledger JSON của các lượt eval bằng Claude API (32 issue typst và scrapy, 123 lượt chạy đã kết thúc, 3.575 lượt gọi Core/Expert) và ledger của trial bằng Ollama (8 lượt, cửa sổ 32k). Không gọi model nào. Tính lại bằng `scripts/eval/context_lengths.py`.

## Quyết định

**Không làm (NO-GO).** Context chưa từng vượt 40% cửa sổ, nên cơ chế thu gọn hiện có (ở 60%, giữ 5 lượt gần nhất) chưa từng chạy. Nén lịch sử giữa chừng không thu hồi được chi phí ghi lại cache trong số lượt gọi còn lại. Các nghiên cứu về nén code chưa cho thấy lợi ích trong vòng lặp agent có prompt caching.

**Xem lại khi:**
- #156 nâng trần lên 80 lượt gọi. Với 40 lượt, đỉnh prompt đã là 79k, nên 80 lượt có thể vượt 120k (60%).
- Chạy model local trên repo thật. Ngưỡng 60% của cửa sổ 32k chỉ là 19,7k token.

Khi đó chạy lại script trên ledger mới.

## 1. Độ dài context

Prompt của một lượt gọi = input + đọc cache + ghi cache.

| | Claude API (cửa sổ 200k) | Ollama trên trial (32k) |
| --- | --- | --- |
| Số lượt chạy | 123 | 8 |
| Đỉnh prompt mỗi lượt: trung vị · p90 · p99 · lớn nhất | 48k · 64k · 78k · **79k (39%)** | 7,6k · 12,9k · 12,9k · **12,9k (39%)** |
| Số lượt vượt 40% / 50% / 60% | 0 / 0 / 0 | 0 / 0 / 0 |

Các lượt có prompt dài nhất đều dừng vì hết 40 lượt gọi: context dài ra theo số lượt gọi và bị trần chặn lại. `Warning::Collapsed` không được ghi vào ledger, nên không đếm trực tiếp được; số liệu trên cho cùng câu trả lời.

## 2. Đánh đổi giữa nén và cache

Giá của Sonnet trong `config/pricing.toml`: đọc cache ×0,05, ghi cache 5 phút ×1,25 (so với giá input).

Nén lịch sử từ P xuống C token chỉ làm mất cache **một lần**: lượt ngay sau ghi lại prefix mới, các lượt tiếp theo lại đọc từ cache.
- Tốn thêm một lần: `1,25·C − 0,05·P`
- Tiết kiệm mỗi lượt sau đó: `0,05·(P − C)`
- Hòa vốn khi số lượt còn lại ≥ `(1,25·C − 0,05·P) / (0,05·(P − C))`

Với P = 50k, C = 25k, cần **23 lượt gọi** sau khi nén mới hòa vốn. Lượt chạy trung vị chỉ có 22 lượt gọi tổng cộng, trần là 40. (So sánh "50k đọc cache với 25k mất cache, đắt gấp 5 lần" giả định mất cache ở mọi lượt sau, nên phóng đại; kết luận vẫn như nhau.)

Chính vì vậy Thiết kế §4.4 chỉ thu gọn theo đợt, một lần khi vượt ngưỡng.

## 3. Tinh gọn đầu ra của công cụ

`open` trả 100 dòng mỗi lần, `search_text` tối đa 50 kết quả, mỗi dòng tối đa 200 ký tự. Phần prompt tăng giữa hai lượt gọi, trừ đi output của lượt trước, cho biết kết quả công cụ thêm bao nhiêu token:

| Công cụ | Lượt | Token thêm (trung vị) | Tỉ trọng |
| --- | --- | --- | --- |
| `open` | 853 | 1.558 | 67% |
| `scroll_down` | 175 | 1.550 | 15% |
| `search_text` | 718 | 98 | 10% |
| `run_tests` | 99 | 747 | 4% |

- Mỗi lượt chạy thêm khoảng 28k token từ kết quả công cụ. Mỗi token được ghi vào cache một lần (×1,25), rồi đọc lại ở các lượt sau (×0,05).
- Tính ra khoảng **$0,10 mỗi lượt chạy**, trên mức trung bình $0,55.
- Một chế độ "chỉ chữ ký hàm" kiểu SWEzze cắt được một nửa đầu ra của `open` và `scroll_down` thì tiết kiệm khoảng **$0,04 (~8%)**.
- Đổi lại, agent phải mở lại phần bị ẩn. Ledger không lưu nội dung kết quả công cụ, nên không mô phỏng lại offline được việc agent thiếu thông tin. Muốn thử thì phải ghi transcript trước.

## 4. Chi phí đi đâu

| Kết quả của lượt chạy | Lượt | Chi phí | Mỗi lượt | Lượt gọi/lượt |
| --- | --- | --- | --- | --- |
| Dừng không có PR (`stopped_steps`) | 55 | $39,18 (57%) | $0,71 | 36 |
| Mở được PR | 64 | $26,39 (39%) | $0,41 | 22 |
| Expert cũng không sửa được | 4 | $2,72 (4%) | $0,68 | 26 |

- Đọc cache đạt 89–94% ở mọi nhóm.
- Đòn bẩy chi phí là **các lượt chạy không ra PR**, không phải độ dài context: dừng sớm khi không có tiến triển, hoặc tăng tỉ lệ thành công (#156, #259). Dừng sớm thì không đáng: mọi quy tắc đơn giản đều làm mất PR tốt ([#273](2026-10-29-early-stop.md)).

## 5. Các nghiên cứu

| Bài | Bối cảnh đo | Kết quả | Áp vào aegis-code |
| --- | --- | --- | --- |
| SWEzze ([2603.28119](https://arxiv.org/abs/2603.28119), 3/2026) | Agentless, SWE-bench Verified, mỗi issue sửa một lần; GPT-5.2, DeepSeek-V3.2, Qwen3-Coder-Next; context 8–16k | +2,0 đến +4,6 điểm tuyệt đối ("5,0–9,2%" là tương đối); tổng token −52% đến −71%; tốn thêm 2,5–10 giây chạy model nén mỗi issue. Bỏ hẳn context chỉ làm giảm 0,8–1,6 điểm | Không có vòng lặp agent, không có prompt caching |
| SaraCoder ([2508.10068](https://arxiv.org/abs/2508.10068)) | Gợi ý code mức repo (CrossCodeEval, RepoEval) | Hơn các baseline gợi ý code | Không phải sửa issue |
| SWE-Pruner ([2601.16746](https://arxiv.org/abs/2601.16746), 1/2026) | Coding agent | Tác giả báo cáo giảm 23–54% token | Bị đo lại bên dưới |
| Skip the Compressor ([2610.21073](https://arxiv.org/html/2610.21073), 10/2026) | 300 issue Python, GPT-5.6 và Claude Opus 5, mỗi issue sửa một lần | Chọn đúng file rồi đưa **nguyên file** đạt 46,7% (Claude), hơn compressor tốt nhất 6,3 điểm. SWE-Pruner chỉ 39,3%. Compressor sửa đổi văn bản code nên patch không áp được (28/48 issue chênh lệch); xoá 20% token làm tỉ lệ giải quyết giảm 9,7 điểm | Agent của ta đã tự chọn code (`open`, `search_text`, `search_symbol`, 100 dòng nguyên văn mỗi lần) |

Chưa bài nào đo trong vòng lặp agent nhiều lượt có prompt caching. Ở đó, token giảm được chỉ tiết kiệm một phần nhỏ chi phí (§2, §3).
