# Đo lại KPI C# sau khi sửa gate (#426)

Ngày đo: 2026-11-10, từ 21:07 đến 23:42. Bộ thử: 5 issue C# của [eval/issues](../../eval/issues) (Polly ×2, NSubstitute ×2, StackExchange.Redis), mỗi issue hai lượt, tổng 10 lượt. Bản dựng: commit `4d39c8e`, đã có bản sửa gate của #422 và quy tắc chấm của #424. Model và trần giống [đợt đo #259](2026-11-10-new-profiles-kpi.md): Light `claude-haiku-5-5`, Core `claude-sonnet-5-5`, Expert `claude-opus-5-5` (effort `high`), $4 và 40 lượt gọi mỗi issue. Tổng chi tiêu: **$2.86**.

"Được chấp nhận" ở đây vẫn là qua gate **và** qua test ẩn, chạy với `--no-pr`, không có người duyệt.

## Kết luận

**C# vẫn chưa đạt ngưỡng 35%.** Tỉ lệ không đổi so với #259.

| Chỉ số | #259 | #426 | Ngưỡng |
| --- | --- | --- | --- |
| Chấm nghiêm (cả 5 issue) | 20% (2/10) | **20%** (2/10) | |
| **Chấm theo quy tắc #424** (bỏ `Polly-3086`) | 25% (2/8) | **25%** (2/8) | ≥ 35% ✘ |
| Qua gate | 8/10 | **10/10** | theo dõi |
| Test ẩn qua, trong số qua gate | 25% (2/8) | 20% (2/10) | theo dõi |
| Chi phí / lượt | $0.25 | $0.29 | ≤ $4 ✔ |
| Tỉ lệ đọc cache | 86% | 88% | ≥ 70% ✔ |
| Lên Expert | 40% | 40% (4/10) | theo dõi |
| Số lượt gọi trung bình | 15.0 | 17.1 | theo dõi |

**Đánh giá:**
- #422 có tác dụng: `NSubstitute-974` không còn bị gate từ chối oan. Nhưng hai lượt mới đều trượt test ẩn, nên cận trên 50% (4/8) nêu ở #259 không thành hiện thực.
- Để đạt ngưỡng cần thêm ít nhất một lượt qua (3/8 = 37.5%). Riêng `NSubstitute-974` qua ổn định cả hai lượt là đủ, nên đây là việc đáng làm nhất.
- `StackExchange.Redis-2689` là ca ranh giới. Nếu loại nó, tỉ lệ là 33% (2/6), vẫn dưới ngưỡng.

**Con số cộng dồn, không dùng làm kết quả.** Nếu gộp mọi lượt đã có của các issue được tính, bỏ hai lượt `NSubstitute-974` bị gate từ chối oan, thì được 37.5% (6/16): `NSubstitute-828` 4/4, `NSubstitute-974` 2/4, `Polly-3286` 0/4, `StackExchange.Redis-2689` 0/4. Con số này trộn ba bản dựng và được tính sau khi đã thấy kết quả, nên không nên coi là đạt ngưỡng. Điều nó cho thấy là `NSubstitute-974` chưa ổn định.

## Kết quả từng issue

"Lượt" là số lượt gọi Core + Expert. E là có lên Expert. Số hiệu là `R3`, `R4` của mỗi issue, riêng `NSubstitute-974` là `R5`, `R6`, vì `R3`, `R4` đã dùng cho #422.

| Issue | Lượt 1 | Lượt 2 | Test ẩn | Nhóm khi trượt |
| --- | --- | --- | --- | --- |
| Polly-3086 | PR, $0.10, 7 | PR, $0.14, 12 | **fail**, **fail** | upstream, bị loại |
| Polly-3286 | PR, $0.42, 18, E | PR, $0.17, 12 | **fail**, **fail** | agent |
| NSubstitute-828 | PR, $0.18, 17 | PR, $0.42, 22, E | pass, pass | |
| NSubstitute-974 | PR, $0.55, 25, E | PR, $0.67, 31, E | **fail**, **fail** | agent |
| StackExchange.Redis-2689 | PR, $0.13, 15 | PR, $0.09, 12 | **fail**, **fail** | ranh giới, vẫn tính |

"PR" nghĩa là qua gate và tới bước mở PR. Với `--no-pr`, không PR nào được mở thật.

## Vì sao còn trượt

### `NSubstitute-974`: sửa đúng ví dụ, chưa sửa cả lớp lỗi

Test ẩn dùng `TResponse Send<TResponse>(...)`, còn ví dụ trong issue dùng `Task<TResponse> Send<TResponse>(...)`. Tiêu đề issue nói chung về "generic method with inheritance-related type arguments", nên đây không phải lựa chọn riêng của upstream.

Cách sửa của bốn lượt sau #422 khác nhau rõ:

| Lượt | Cách sửa | Kiểu trả về `IResult` và `IResult<int>` | Test ẩn |
| --- | --- | --- | --- |
| R3 (PR #430) | Kiểm tra giá trị đã lưu có hợp với kiểu trả về của lời gọi | phân biệt | pass |
| R4 (PR #430) | So chặt toàn bộ kiểu trả về, như upstream | phân biệt | pass |
| R5 | Chỉ so chặt tham số kiểu **bên trong** generic, như `T` của `Task<T>` | vẫn coi là tương đương | **fail** |
| R6 | Như R5 | vẫn coi là tương đương | **fail** |

Test tái hiện của R5 và R6 chép đúng dạng `Task<TResponse>` của issue, nên qua được dù bản vá còn hẹp. Lỗi vẫn là `InvalidCastException` như issue mô tả, chỉ khác dạng trả về.

### `Polly-3286`: dọn tài nguyên nhưng thêm đường lỗi mới

Cả bốn lượt (#259 và #426) đều trượt theo cùng một cách. Agent gọi `DisposeHelper.ForceDisposeAsync().AsTask().GetAwaiter().GetResult()` ngay trong `GetOrAddPipeline`, nên:
- Lỗi của dispose callback văng ra `GetOrAddPipeline`. Test `DiscardedRegistryPipeline_ReportsCallbackFailure` trượt vì lý do này.
- Luồng gọi bị chặn cho tới khi dispose xong. Đây nhiều khả năng là lý do bước test ẩn chạy hết 1800 giây rồi hết giờ, log không có dòng tổng kết. Test `DiscardedRegistryPipeline_ObservesAsyncDisposalWithoutBlocking` chỉ cho dispose chạy tiếp sau khi registry đã trả về, nên sẽ treo với bản vá này. Điều này suy ra từ việc đọc test, chưa chạy riêng để xác nhận.

**Agent có thể thấy yêu cầu này ở đâu.** Issue không nhắc tới xử lý lỗi, và còn nói vấn đề là dọn thiếu chứ không phải exception. Nhưng code tại base đã có tiền lệ: `ReloadableComponent.DisposeDiscardedComponentSafeAsync` ở dòng 83–92 của `src/Polly.Core/Utils/Pipeline/ReloadableComponent.cs` dispose component bị bỏ theo kiểu không chờ (`_ = ...`), bắt lỗi và báo telemetry `DisposeFailed`. Cả bốn lượt đều mở file này. Công cụ `open` hiện 100 dòng một lần và file khá ngắn, nên gần như chắc chắn agent đã thấy đoạn đó, dù event không ghi tham số dòng.

Bản sửa upstream dùng đúng helper này, sau khi đổi nó thành dùng được từ ngoài và lấy thêm `telemetry` từ `CreateComponent()`. Vậy việc dùng lại không phải một dòng, nhưng hướng đi đã nằm sẵn trong code agent đọc.

**Kết luận:** đây là lỗi cách làm của agent, không phải test ẩn gắn với upstream. Test biên dịch được với code tại base, và event `DisposeFailed` đã có sẵn.

### `StackExchange.Redis-2689`: khác loại exception

Cả bốn lượt ném `ArgumentException`, test ẩn đòi `RedisCommandException`. Hành vi đúng và cùng ý nghĩa. #424 giữ issue này trong phần chấm vì tiêu đề đã nêu whitespace và loại exception suy ra được từ code, nên báo cáo này vẫn tính là trượt.

### `Polly-3086`

Trượt vì test ẩn biên dịch với helper nội bộ mà issue không nêu, đúng như lý do loại ở #424. Không tính vào tỉ lệ.

## Đề xuất

Chưa áp dụng và chưa đo. Cả hai cần issue riêng, rồi đo lại `Polly-3286` và `NSubstitute-974`. Mỗi cặp hai lượt tốn dưới $1.50, riêng `Polly-3286` mất khoảng 80 phút nếu bản vá còn treo.

**1. Thêm quy tắc vào bước "Make the fix" của `prompts/system.md`, cho `Polly-3286`:**

> When the fix adds a call on a path that used to succeed without it (cleanup, dispose, release, notify), decide what happens if that call throws or blocks. Look at how the same code handles that call elsewhere and reuse it. The fix must not give the original operation a new way to fail or hang.

**2. Thêm quy tắc về tái hiện, cho `NSubstitute-974`:**

> An example in the issue is one instance of the problem. Fix the condition the issue states, not only the shape of its example, and when the issue says a behaviour regressed, compare against what the old code did.

Cần chú ý quy tắc 2 có thể làm bản vá rộng hơn cần thiết, nên phải theo dõi cả các issue đang qua.

## Lưu ý

- Mỗi cấu hình chỉ có 8 đến 10 mẫu, một lượt đổi kết quả làm tỉ lệ đổi 10 đến 13 điểm.
- Các repo C# là mã nguồn mở, model có thể đã thấy bản sửa khi huấn luyện.
- Thời gian trung bình 15.5 phút mỗi lượt, cao nhất là `Polly-3286` với 38 đến 41 phút, trong đó khoảng 30 phút là bước test ẩn hết giờ.
- Bản vá của 10 lượt nằm ở `.workspace/state/jobs/<id>/patch.diff`, chỉ có trên máy đo, không nằm trong repo.

## Chạy lại

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH target/release/aegis eval --config config/polly.toml --set eval/issues --no-pr --only Polly-3286
```

Lặp cho từng issue với config tương ứng (`polly`, `nsubstitute`, `stackexchange-redis`). Số liệu từng lượt đọc từ `.workspace/state/ledger/<ID>-R<n>.json`.
