# KPI của bốn profile mới trên bộ thử 20 issue (#259)

Ngày đo: 2026-11-09 → 2026-11-10. Bộ thử: [eval/issues](../../eval/issues), 5 issue zod (TypeScript), 5 gin (Go), 5 gson (Java), 5 C# (Polly ×2, NSubstitute ×2, StackExchange.Redis). Mỗi issue chạy hai lượt, tổng 40 lượt. Model: Light `claude-haiku-5-5`, Core `claude-sonnet-5-5`, Expert `claude-opus-5-5` (effort `high`). Trần mỗi issue: $4 và 40 lượt gọi Core + Expert, như lần đo Phase 1. Tổng chi tiêu: **$8.69**, thấp hơn dự toán $20–25 của #259.

Báo cáo này nối tiếp [lượt chạy khô](2026-10-29-new-profiles-dry-run.md). Bản dựng là v0.8.1 cộng bản sửa parser C# của #421, vì nếu thiếu bản sửa đó `StackExchange.Redis-2689` bị gate từ chối oan.

## Khác với lần đo Phase 1

- **Không có người duyệt.** Sáu fork chưa tồn tại nên chạy với `--no-pr`. "Được chấp nhận" ở đây là qua gate **và** qua test ẩn. Ở Phase 1 hai điều kiện này trùng nhau hoàn toàn với quyết định của người duyệt, nhưng người duyệt khi đó thấy kết quả test ẩn.
- **Chưa đo được:** thời gian duyệt, điều kiện "sửa ≤ 10 dòng", và số PR làm vỡ test cũ tới tay người.
- **Tỉ lệ tính trên lượt chạy** (10 lượt mỗi ngôn ngữ), không phải lượt mới nhất của mỗi issue. Kết quả hai lượt của cùng issue chỉ lệch nhau ở `gson-1768`, nên tính theo issue cũng cho kết quả tương đương.

## Kết luận

| KPI ([ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md)) | Tổng | TypeScript | Go | Java | C# | Ngưỡng | |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PR được chấp nhận (qua gate và test ẩn) | **37.5%** (15/40) | **40%** (4/10) | **60%** (6/10) | **30%** (3/10) | **20%** (2/10) | Tổng ≥ 40%, mỗi ngôn ngữ ≥ 35% | Tổng ✘, TS ✔, Go ✔, Java ✘, C# ✘ |
| Chi phí / lượt, cả ca fail | $0.22 | $0.22 | $0.08 | $0.32 | $0.25 | ≤ $4 | ✔ |
| Tỉ lệ đọc cache | 89% | 88% | 87% | 92% | 86% | ≥ 70% | ✔ |
| Lên Expert | 30% (12/40) | 40% | 0% | 40% | 40% | theo dõi | |
| Số lượt gọi trung bình | 14.2 | 14.3 | 9.1 | 18.2 | 15.0 | theo dõi | |
| Hết 40 lượt | 1 | 0 | 0 | 1 | 0 | theo dõi | |
| Qua gate | 85% (34/40) | 7/10 | 10/10 | 9/10 | 8/10 | theo dõi | |
| Test ẩn qua, trong số qua gate | 44% (15/34) | 57% | 60% | 33% | 25% | theo dõi | |
| Lỗi môi trường (sandbox, phụ thuộc) | 0 | 0 | 0 | 0 | 0 | | |

**Đánh giá:** ngưỡng chi phí và cache đạt rộng. TypeScript và Go đạt ngưỡng từng ngôn ngữ, Java và C# chưa đạt, tổng thiếu 2.5 điểm. Con số thấp một phần là do cách chấm và hai lỗi gate, không phải toàn bộ do agent (mục tiếp theo). Không nên coi 30% và 20% là năng lực thực của agent trên Java và C# trước khi xong #422 và #424.

Tín hiệu đáng chú ý nhất là tỉ lệ test ẩn qua trong số qua gate: 44%, so với 79% ở Phase 1.

## Chấm lại theo quy tắc của #424

Đợt này được chấm lại theo [quy tắc cho test ẩn gắn với lựa chọn của upstream](../../eval/README.md#scoring-hidden-tests-bound-to-upstream-choices-424), từ ledger và không gọi model. Việc phân loại dựa trên issue, test ẩn và code tại base, không nhìn kết quả lượt chạy, và áp dụng cho cả 20 issue.

| Tỉ lệ qua gate và test ẩn | Tổng | TypeScript | Go | Java | C# |
| --- | --- | --- | --- | --- | --- |
| Chấm nghiêm (cả 20 issue) | 37.5% (15/40) | 40% (4/10) | 60% (6/10) | 30% (3/10) | 20% (2/10) |
| **Chấm theo quy tắc** (bỏ 7 issue) | **57.7%** (15/26) | **66.7%** (4/6) | **100%** (6/6) | **50%** (3/6) | **25%** (2/8) |
| Ngưỡng | ≥ 40% | ≥ 35% | ≥ 35% | ≥ 35% | ≥ 35% |

Bảy issue bị loại: `zod-6643`, `zod-6657`, `gin-4638`, `gin-4760`, `gson-1247`, `gson-3113`, `Polly-3086`. Lý do từng issue nằm ở trường `upstream_bound` trong [eval/issues](../../eval/issues).

So với bảng nhóm nguyên nhân bên dưới, có hai điểm khác:
- `zod-6643` trước đây xếp vào nhóm agent, nay bị loại: issue nói về mảng không giới hạn độ dài, còn test ẩn chỉ kiểm tra mảng có giới hạn trên.
- `StackExchange.Redis-2689` trước đây xếp vào nhóm upstream, nay vẫn được tính: tiêu đề đã nêu whitespace, và `RedisCommandException` suy ra được từ code. Hai lượt này tiếp tục tính là trượt.

Cách đọc:
- Theo quy tắc, tổng và ba ngôn ngữ đạt ngưỡng. C# vẫn chưa đạt.
- Loại một issue nghĩa là issue đó không đo được agent, không có nghĩa bản vá của agent đúng. Bản vá của đợt này không được lưu lại nên không ai đọc lại được.
- Mẫu nhỏ hơn: mỗi ngôn ngữ chỉ còn 6 đến 8 lượt, một lượt đổi kết quả làm tỉ lệ đổi 13 đến 17 điểm. `Go 100%` là 6/6 từ ba issue.
- `zod-6027` và `NSubstitute-974` vẫn tính là trượt, dù #423 và #422 đã sửa gate. Nếu cả bốn lượt đều qua test ẩn khi đo lại, TypeScript lên 6/6 và C# lên 50% (4/8). Đây là cận trên, chưa đo.
- "Được chấp nhận" ở đây vẫn là qua gate và test ẩn, chưa có người duyệt như ADR 0005.

## Vì sao không đạt

25 lượt không đạt, chia theo nguyên nhân:

| Nguyên nhân | Tổng | TS | Go | Java | C# | Lượt |
| --- | --- | --- | --- | --- | --- | --- |
| Gate từ chối oan | 4 | 2 | | | 2 | zod-6027, NSubstitute-974 |
| Test ẩn gắn với lựa chọn riêng của upstream | 14 | 2 | 4 | 4 | 4 | xem dưới |
| Agent sửa thiếu, từ chối sửa, hoặc hết lượt | 7 | 2 | | 3 | 2 | xem dưới |

Việc xếp nhóm dựa trên log, nội dung issue và test ẩn. Chưa có người duyệt xác nhận các bản vá ở nhóm 2 là đúng, nên không cộng nhóm này vào tỉ lệ chấp nhận.

**Gate từ chối oan (4 lượt).** Test tái hiện đã fail đúng trên base nhưng gate không nhận ra, nên test ẩn chưa từng chạy.
- `NSubstitute-974`: NUnit dưới Microsoft.Testing.Platform chỉ in tên phương thức, còn agent lọc theo lớp (#422).
- `zod-6027`: Vitest nhiều project in đường dẫn tính từ gốc project, còn agent đưa đường dẫn từ gốc repo (#423).

Nếu cả bốn lượt sau khi sửa gate đều qua test ẩn, TypeScript lên 60% và C# lên 40%. Đây là cận trên, chưa đo.

**Test ẩn gắn với lựa chọn của upstream (14 lượt).** Issue không nêu điều test đòi hỏi, nên một bản vá đúng hành vi vẫn có thể trượt (#424):

| Issue | Test ẩn đòi hỏi | Agent làm |
| --- | --- | --- |
| gin-4760 | field `ReadHeaderTimeout`, hàm `getReadHeaderTimeout` | không biên dịch được với test |
| Polly-3086 | helper nội bộ `WithCallerCancellationToken` | không biên dịch được với test |
| gin-4638 | lỗi bọc `http.ErrNotSupported` | lỗi riêng, không bọc |
| StackExchange.Redis-2689 | `RedisCommandException` | `ArgumentException`, cùng ý nghĩa |
| gson-3113 | thông báo bắt đầu bằng một câu cố định | câu khác, cùng ý nghĩa |
| gson-1247 | upstream đổi sang `LinkedHashMap`, cho phép key null | `IllegalArgumentException`, đúng như issue đề xuất |
| zod-6657 | câu chữ tiếng Hebrew của upstream | câu chữ khác, có đơn vị |

**Agent sửa thiếu, từ chối sửa, hoặc hết lượt (7 lượt).**
- `gson-992` (2 lượt): `hashCode()` băm theo double, nên `-0.0` và `0` được coi là bằng nhau nhưng có hash khác nhau.
- `Polly-3286` (2 lượt): dọn pipeline bị bỏ, nhưng để lỗi của dispose callback văng ra `GetOrAddPipeline`.
- `zod-6643`: r1 sửa lỗi tràn stack không phải điều issue nói, r2 từ chối sửa vì coi đây là quyết định của maintainer. Issue là một CVE không có cách tái hiện.
- `gson-1768-r2`: hết 40 lượt ($1.03), trong khi r1 qua ở lượt 30.

## Kết quả từng issue

"Lượt" là số lượt gọi Core + Expert. E là có lên Expert.

| Issue | r1 | r2 | Test ẩn | Nhóm khi trượt |
| --- | --- | --- | --- | --- |
| zod-6027 | Expert fail 2 lần, $0.44, 21, E | Expert fail 2 lần, $0.32, 18, E | chưa chạy | gate |
| zod-6641 | PR, $0.37, 19, E | PR, $0.18, 14 | pass, pass | |
| zod-6643 | PR, $0.13, 14 | Dừng, $0.07, 7 | **fail**, chưa chạy | agent |
| zod-6657 | PR, $0.14, 9 | PR, $0.11, 9 | **fail**, **fail** | upstream |
| zod-6659 | PR, $0.12, 15 | PR, $0.31, 17, E | pass, pass | |
| gin-4622 | PR, $0.06, 8 | PR, $0.06, 8 | pass, pass | |
| gin-4638 | PR, $0.09, 13 | PR, $0.06, 7 | **fail**, **fail** | upstream |
| gin-4760 | PR, $0.13, 12 | PR, $0.15, 14 | **fail**, **fail** | upstream |
| gin-4818 | PR, $0.07, 9 | PR, $0.05, 6 | pass, pass | |
| gin-4851 | PR, $0.06, 7 | PR, $0.07, 7 | pass, pass | |
| gson-992 | PR, $0.08, 7 | PR, $0.11, 10 | **fail**, **fail** | agent |
| gson-1247 | PR, $0.33, 22, E | PR, $0.39, 24, E | **fail**, **fail** | upstream |
| gson-1587 | PR, $0.03, 4 | PR, $0.06, 7 | pass, pass | |
| gson-1768 | PR, $0.41, 30 | Hết 40 lượt, $1.03, 40, E | pass, chưa chạy | agent (r2) |
| gson-3113 | PR, $0.61, 26, E | PR, $0.16, 12 | **fail**, **fail** | upstream |
| Polly-3086 | PR, $0.23, 8, E | PR, $0.13, 11 | **fail**, **fail** | upstream |
| Polly-3286 | PR, $0.17, 14 | PR, $0.31, 19 | **fail**, **fail** | agent |
| NSubstitute-828 | PR, $0.52, 26, E | PR, $0.18, 19 | pass, pass | |
| NSubstitute-974 | Dừng, $0.32, 15, E | Dừng, $0.41, 19, E | chưa chạy | gate |
| StackExchange.Redis-2689 | PR, $0.10, 9 | PR, $0.10, 10 | **fail**, **fail** | upstream |

"PR" nghĩa là qua gate và tới bước mở PR. Với `--no-pr`, không PR nào được mở thật.

## Việc tiếp theo

| Issue | Việc |
| --- | --- |
| #422 | Gate nhận test NUnit dưới MTP khi lọc theo lớp |
| #423 | Gate khớp đường dẫn tính từ gốc project của Vitest |
| #424 | Quy tắc chấm khi test ẩn gắn với lựa chọn của upstream, rồi chấm lại đợt này (xem mục "Chấm lại theo quy tắc của #424") |
| #425 | Đưa Java lên ngưỡng 35% |
| #426 | Đưa C# lên ngưỡng 35% |

## Lưu ý

- Mỗi issue chỉ có hai lượt, nên một ngôn ngữ có 10 mẫu: một lượt đổi kết quả làm tỉ lệ đổi 10 điểm.
- `gin`, `zod`, `gson` và các repo C# là mã nguồn mở, model có thể đã thấy bản sửa khi huấn luyện.
- Thời gian chạy C# dài nhất: trung bình 18 phút mỗi lượt, `Polly-3286-r2` mất 73 phút. Phần lớn là thời gian gate chạy test.

## Chạy lại

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH aegis eval --config config/gin.toml --set eval/issues --no-pr --only gin-4622
```

Lặp cho từng issue với config tương ứng (`gin`, `zod`, `gson`, `polly`, `nsubstitute`, `stackexchange-redis`). Số liệu của cả hai lượt đọc từ `.workspace/state/ledger/<ID>-R<n>.json`, vì `aegis report` chỉ tính lượt mới nhất.
