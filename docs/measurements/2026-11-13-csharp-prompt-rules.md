# Hai quy tắc mới trong prompt cho C# và đo hồi quy (#448)

Ngày đo: từ tối 2026-11-12 tới 06:10 ngày 2026-11-13; 11 issue typst đo lại từ 07:30 tới 23:40 ngày 2026-11-13, cùng bản dựng. Bản dựng: nhánh `448-csharp-prompt-rules` (trên `243391a`), chỉ đổi [prompts/system.md](../../prompts/system.md). Model và trần giống [đợt đo #426](2026-11-10-csharp-rerun-kpi.md): Light `claude-haiku-5-5`, Core `claude-sonnet-5-5`, Expert `claude-opus-5-5` (effort `high`), $4 mỗi issue. Trần lượt gọi là 40, riêng typst là 80 (thí nghiệm của #156). Tổng chi tiêu: **$27.83**, trong đó typst $15.82 (gồm $1.72 của các lượt hỏng vì môi trường) và scrapy $6.78.

"Được chấp nhận" ở đây vẫn là qua gate **và** qua test ẩn, chạy với `--no-pr`, không có người duyệt. Tỉ lệ chấm theo [quy tắc #424](../../eval/README.md#scoring-hidden-tests-bound-to-upstream-choices-424).

## Thay đổi được đo

Hai đề xuất của [#426](2026-11-10-csharp-rerun-kpi.md#đề-xuất), đưa vào prompt chung của mọi ngôn ngữ:

1. Cuối bước 5 (Make the fix), cho `Polly-3286`:
   > When the fix adds a call on a path that used to succeed without it (cleanup, dispose, release, notify), decide what happens if that call throws or blocks: look at how the same code handles that call elsewhere and reuse it. The fix must not give the original operation a new way to fail or hang.
2. Đầu bước 7, cho `NSubstitute-974`:
   > Fix the condition the issue states, not only the shape of its example (a return type, a wrapper, an argument), and when the issue says a behaviour regressed, compare with what the old code did.

Prompt là phần chung, nên đợt này đo lại C# và chạy cả TypeScript, Go, Python, Rust để xem có tụt không.

## Kết luận

**C# đạt ngưỡng 35% theo quy tắc #424: 50% (4/8).** Phần tăng đến hết từ `NSubstitute-974` (quy tắc 2). Quy tắc 1 chưa làm `Polly-3286` qua.

| Chỉ số C# | #259 | #426 | #448 | Ngưỡng |
| --- | --- | --- | --- | --- |
| Chấm nghiêm (cả 5 issue) | 20% (2/10) | 20% (2/10) | **40%** (4/10) | |
| **Chấm theo quy tắc #424** (bỏ `Polly-3086`) | 25% (2/8) | 25% (2/8) | **50%** (4/8) | ≥ 35% ✔ |
| Qua gate | 8/10 | 10/10 | 10/10 | theo dõi |
| Chi phí / lượt | $0.25 | $0.29 | $0.22 | ≤ $4 ✔ |
| Lên Expert | 40% | 40% | 30% (3/10) | theo dõi |
| Số lượt gọi trung bình | 15.0 | 17.1 | 14.7 | theo dõi |

**Các ngôn ngữ khác không tụt** ở những lượt đo được:

| Ngôn ngữ | Lần đo trước, cùng các issue | #448 | Ghi chú |
| --- | --- | --- | --- |
| TypeScript (zod), theo #424 | 4/6 (#259) | **6/6** | `zod-6027` qua cả hai lượt sau #423 |
| Go (gin), theo #424 | 6/6 (#259) | **6/6** | 9.7 lượt gọi, #259 là 9.1 |
| Python (scrapy), bỏ 2 lượt lỗi môi trường | 8/12 | **8/12** | đổi chỗ: thêm `7730`, mất `3778` (hết 40 lượt) |
| Rust (typst), 18 issue | 7/18 (#145), trần 40 lượt | **10/18**, trần 80 lượt | không issue nào tụt; phần tăng chưa quy được cho prompt, xem dưới |

**Đánh giá:**
- Quy tắc 2 có tác dụng như mong đợi: cả hai lượt `NSubstitute-974` so toàn bộ kiểu trả về thay vì chỉ tham số kiểu bên trong `Task<T>`, và qua test ẩn. Ở #426 hai lượt chỉ sửa đúng dạng ví dụ và trượt.
- Quy tắc 1 có tác dụng một nửa ở `Polly-3286`: một lượt bỏ cách chờ đồng bộ, một lượt vẫn giữ. Cả hai lượt vẫn không báo lỗi của dispose callback, nên vẫn trượt (mục dưới).
- Không thấy bản vá nào rộng quá mức vì quy tắc 2 ở TypeScript, Go và Python.
- Mẫu nhỏ: C# có 8 lượt được tính, nên một lượt đổi kết quả làm tỉ lệ đổi 12.5 điểm. 4/8 vẫn đạt khi mất một lượt (3/8 = 37.5%), nhưng mất hai lượt thì không đạt.

## Kết quả C#

"Lượt" là số lượt gọi Core + Expert. E là có lên Expert. Bỏ các lượt dừng ở bước lấy dependency với $0 và 0 lượt gọi (`Polly-3086` R5–R6, `Polly-3286` R5–R6, `NSubstitute-828` R5). Các lượt này hỏng vì image `dotnet` đang được dựng lại khi đợt đo bắt đầu, nên không nói gì về agent.

| Issue | Lượt 1 | Lượt 2 | Test ẩn | #426 |
| --- | --- | --- | --- | --- |
| Polly-3086 | PR, $0.28, 10, E | PR, $0.15, 13 | **fail**, **fail** | upstream, bị loại |
| Polly-3286 | PR, $0.20, 15 | PR, $0.15, 10 | **fail**, **fail** | fail, fail |
| NSubstitute-828 | PR, $0.37, 18, E | PR, $0.20, 17 | pass, pass | pass, pass |
| NSubstitute-974 | PR, $0.22, 21 | PR, $0.38, 18, E | **pass, pass** | fail, fail |
| StackExchange.Redis-2689 | PR, $0.12, 14 | PR, $0.10, 11 | **fail**, **fail** | fail, fail |

Số hiệu: `Polly` R7–R8, `NSubstitute-828` R6–R7, `NSubstitute-974` R8–R9, `StackExchange.Redis-2689` R5–R6.

`StackExchange.Redis-2689` vẫn ném `ArgumentException`, còn test ẩn đòi `RedisCommandException`, như ở #426.

### `Polly-3286`: bỏ được cách chờ nhưng vẫn nuốt lỗi

Test ẩn đòi hai điều khi pipeline mới tạo bị bỏ: dispose chạy mà không chặn `GetOrAddPipeline`, và nếu dispose callback lỗi thì lỗi đó được báo qua telemetry `DisposeFailed`. Tiền lệ trong code là `ReloadableComponent.DisposeDiscardedComponentSafeAsync`: không chờ, bắt lỗi, rồi báo `DisposeFailed`.

| Lượt | Cách dispose pipeline bị bỏ | Chặn luồng gọi | Báo `DisposeFailed` |
| --- | --- | --- | --- |
| #259, #426 (4 lượt) | `ForceDisposeAsync().AsTask().GetAwaiter().GetResult()` | có | không |
| R7 | `_ = DisposeHelper.TryDisposeSafeAsync(component, false)` | **không** | không, helper nuốt mọi lỗi |
| R8 | như #426 | có | không |

Cả hai lượt trượt `DiscardedRegistryPipeline_ReportsCallbackFailure` (generic và non-generic), và bước test ẩn chạy hết 1800 giây như cả bốn lượt trước.

**Sửa kết luận của #426.** #426 cho rằng bước test ẩn hết giờ "nhiều khả năng" vì bản vá chặn luồng gọi. R7 không chặn mà vẫn hết giờ. Vậy chặn luồng gọi không phải nguyên nhân duy nhất. Giải thích hợp với cả năm lượt là test chờ telemetry `DisposeFailed` mà không đặt timeout, và không bản vá nào phát telemetry đó. Điều này suy ra từ đọc test và log, chưa chạy riêng từng test để xác nhận.

Quy tắc 1 bảo agent "reuse" cách code xử lý ở chỗ khác. R7 dùng lại một helper có sẵn, nhưng là helper nuốt lỗi, không phải helper báo lỗi. Muốn qua, agent phải tìm đúng tiền lệ trong `ReloadableComponent`. Prompt chung khó nói cụ thể hơn mà không thành gợi ý riêng cho issue này, nên báo cáo này không đề xuất sửa prompt tiếp.

## Hồi quy ở các ngôn ngữ khác

### TypeScript (zod) và Go (gin)

Mỗi issue hai lượt (zod R3–R6, gin R3–R4).

| Issue | #259 | #448 | Tính theo #424 |
| --- | --- | --- | --- |
| zod-6027 | gate từ chối oan ×2 | pass ($0.27), pass ($0.08) | có |
| zod-6641 | pass, pass | pass ($0.49, 24), pass ($0.51, 21) | có |
| zod-6659 | pass, pass | pass, pass | có |
| zod-6643 | fail, dừng | dừng, dừng | không |
| zod-6657 | fail, fail | fail, fail | không |
| gin-4622, 4818, 4851 | pass ×6 | pass ×6 | có |
| gin-4638, 4760 | fail ×4 | fail ×4 | không |

Hai lượt `zod-6643` dừng với cùng lý do như `r2` ở #259: issue là một CVE không có cách tái hiện, và cách sửa sẽ đổi API công khai. Issue này bị loại khỏi phần chấm.

### Python (scrapy)

Mỗi issue một lượt. Đem so với lượt có test ẩn gần nhất trước đó của cùng issue (tháng 10):

| Kết quả | Issue |
| --- | --- |
| Qua cả hai lần | 1280, 2702, 3095, 4729, 7029, 7604, 8186 |
| Trượt cả hai lần | 3666, 4330, 4999 |
| Mới qua | 7730 |
| Mới trượt | 3778 (hết 40 lượt, $1.11) |
| **Lỗi môi trường** | 4466, 7597 |

**Lỗi môi trường ở `scrapy-4466` và `scrapy-7597`.** Bước lấy dependency cài bản mới nhất theo `w3lib>=2.1.1`. `w3lib` 2.6.0 ra ngày 2026-11-05, sau các lượt tháng 10:
- `scrapy-4466`: 25 test crawl trượt vì cảnh báo deprecation của `w3lib.encoding.html_to_unicode()`. Gate qua vì đó là bộ test của agent; bộ test ẩn thay `tests/test_crawl.py` nên gặp lỗi.
- `scrapy-7597`: `test_thumbnail_name` trượt ở mã băm đường dẫn của `file://foo.png`. Bản vá chỉ đổi ACL của S3/GCS, không đụng `thumb_path` hay cách chuẩn hoá URL.

Hai lượt này không được tính. Cả hai issue đều qua ở mọi lượt tháng 10. Cần có một issue để ghim phiên bản dependency của scrapy trong eval, nếu không các lần đo sau sẽ không so được.

### Rust (typst)

Đủ 18 issue, mỗi issue một lượt được tính. Lần đầu chỉ đo được 7 issue. 11 issue còn lại được đo lại trong ngày 13/11, với lý do như sau.

Lượt đầu của các issue này không được tính vì lỗi môi trường, không phải lỗi của agent:
- **Hết đĩa trên máy đo.** Docker Desktop chuyển sang `read-only file system` rồi không khởi động được. `typst-8476` hỏng ở bước test ẩn, `typst-8560` hỏng giữa chừng (`No space left on device` khi link). 9 issue khác dừng ngay ở bước tạo network, với $0. Mỗi lượt typst cần khoảng 29 GB cho thư mục làm việc tạm, cộng cache build khoảng 40 GB.
- **API trả `503 Service Unavailable` lúc 20:20–20:45.** Năm lượt bị cắt: `typst-8703` sau 28 lượt gọi, `typst-8850` sau 9 lượt, ba issue còn lại ngay từ đầu.

| Issue | [Rust rerun (#145)](2026-10-24-rust-rerun-18.md), trần 40 | #448, trần 80 |
| --- | --- | --- |
| typst-2639 | pass | pass ($0.87, 38) |
| typst-3582 | hết 40 lượt | **pass** ($0.73, 34) |
| typst-6822 | pass | pass ($0.88, 43) |
| typst-7219 | hết 40 lượt | fail ($0.79, 36) |
| typst-7541 | hết 40 lượt | **pass** ($1.45, 53) |
| typst-7777 | hết 40 lượt | fail ($1.99, 77) |
| typst-8372 | fail test ẩn | fail ($1.36, 48) |
| typst-8476 | pass | pass ($0.63, 37) |
| typst-8560 | hết 40 lượt | **pass** ($0.62, 31) |
| typst-8616 | pass | pass ($0.26, 21) |
| typst-8648 | pass | pass ($1.25, 48) |
| typst-8672 | fail test ẩn | fail ($0.57, 36) |
| typst-8703 | hết 40 lượt | agent tự dừng ($0.87, 38) |
| typst-8725 | pass | pass ($0.10, 9) |
| typst-8826 | hết 40 lượt | fail ($1.20, 51) |
| typst-8828 | fail test ẩn | fail ($0.27, 25) |
| typst-8850 | agent tự dừng | fail ($0.17, 16) |
| typst-8857 | pass | pass ($0.09, 12) |

Số trong ngoặc là chi phí và số lượt gọi Core + Expert. Cột #145 dùng ký hiệu của báo cáo đó: `rej` ở #145 là qua gate nhưng trượt test ẩn. Bảng lần trước ghi nhầm `typst-8372` là "gate từ chối".

**10/18 (56%), so với 7/18 (39%) ở #145.** Không issue nào đang qua ở #145 lại trượt ở đây. Ba issue mới qua là `3582`, `7541` và `8560`. Phần tăng này vẫn chưa quy được cho prompt, vì trần lượt gọi cũng đổi từ 40 lên 80:
- `7541` dùng 53 lượt, nên chỉ qua được nhờ trần mới.
- `3582` (34 lượt) và `8560` (31 lượt) qua dưới 40 lượt. Nhưng ở #145 cả hai hết 40 lượt, và mỗi issue chỉ có một lượt, nên chưa tách được tác dụng của prompt khỏi độ dao động giữa các lượt.

Các lượt trượt:
- **Năm issue qua gate nhưng trượt đúng một test ẩn:** `8672` (`link-url-svg-escape`, SVG khác bản tham chiếu), `8826` (`figure-tags-equation-only-marked-content-with-alt`, cây tag PDF), `8828` (`html-frame-position`), `8850` (`mathml-multiline-matrix-spacing`) và `8372`. Mỗi bản vá cho ra kết quả khác với bản sửa của upstream ở một ca.
- **`typst-8703` tự dừng:** issue chỉ nêu phép kiểm PAC bị trượt trong một ảnh chụp màn hình, và agent không tái hiện được cây tag sai. Agent còn báo hai file nháp nó không xoá được bằng công cụ.

Trong log test ẩn của `typst-7219`, `8372`, `8672` và `8826` có `fatal: not a git repository`. Dòng này đến từ bước dựng báo cáo HTML của typst, sau khi test đã trượt vì sai kết quả tham chiếu. Nó không phải nguyên nhân trượt.

## Lưu ý

- Mỗi issue chỉ có một hoặc hai lượt. Mọi tỉ lệ ở đây đổi 8 đến 17 điểm khi một lượt đổi kết quả.
- Các repo là mã nguồn mở, model có thể đã thấy bản sửa khi huấn luyện.
- Lượt dài nhất vẫn là `Polly-3286`, khoảng 40 phút mỗi lượt, trong đó 30 phút là bước test ẩn hết giờ. Nên có giới hạn thời gian cho từng test ẩn để một test treo không chiếm hết 1800 giây.
- Bản vá nằm ở `.workspace/state/jobs/<id>/patch.diff`, chỉ có trên máy đo.

## Chạy lại

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH target/release/aegis eval --config config/nsubstitute.toml --set eval/issues --no-pr --only NSubstitute-974
```

Lặp cho từng issue với config tương ứng (`polly`, `nsubstitute`, `stackexchange-redis`, `zod`, `gin`, `poc`, `typst`). Số liệu từng lượt đọc từ `.workspace/state/ledger/<ID>-R<n>.events.jsonl`.
