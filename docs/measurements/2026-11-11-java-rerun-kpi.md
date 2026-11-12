# Đo lại KPI Java sau khi thêm bước tự kiểm (#425)

Ngày đo: 2026-11-11, từ 00:55 đến 01:26. Bộ thử: 5 issue gson của [eval/issues](../../eval/issues), mỗi issue hai lượt, tổng 10 lượt. Bản dựng: nhánh `425-java-kpi` (từ `adbc5aa`), có bước 7 mới trong [prompts/system.md](../../prompts/system.md). Model và trần giống [đợt đo #259](2026-11-10-new-profiles-kpi.md): Light `claude-haiku-5-5`, Core `claude-sonnet-5-5`, Expert `claude-opus-5-5` (effort `high`), $4 và 40 lượt gọi mỗi issue. Tổng chi tiêu: **$3.70**.

"Được chấp nhận" ở đây vẫn là qua gate **và** qua test ẩn, chạy với `--no-pr`, không có người duyệt.

## Thay đổi được đo

Thêm bước 7 vào phần Workflow của prompt hệ thống, sau bước chạy lại test tái hiện:

> 7. Before you answer, check the rule, not only the example. An example in the issue is one instance of the problem. When the issue names a rule the code must keep (a contract such as `equals` and `hashCode`, an ordering, a round trip, a limit), read the code that decides it, note which inputs it treats as the same or as different, edge values included (zero and negative zero, NaN, empty, null, the largest values), and add those cases to your reproduction test. Fix what the rule needs, no more. When your fix changes text or a value that callers see (a message, a path, a format), search the tests for the old text and run the tests that hold it: the checks run every test, and one still expecting the old text fails them. Update those tests and list them in `allowed_test_changes`.

Câu đầu nhắm vào `gson-992`, câu sau nhắm vào `gson-1768`. Lý do ở mục [Vì sao trượt ở #259](#vì-sao-trượt-ở-259).

## Kết luận

**Java đạt ngưỡng 35% theo cả hai cách chấm.**

| Chỉ số | #259 | #425 | Ngưỡng |
| --- | --- | --- | --- |
| Chấm nghiêm (cả 5 issue) | 30% (3/10) | **50%** (5/10) | ≥ 35% ✔ |
| **Chấm theo quy tắc #424** (bỏ `gson-1247`, `gson-3113`) | 50% (3/6) | **83%** (5/6) | ≥ 35% ✔ |
| Qua gate | 9/10 | 9/10 | theo dõi |
| Chi phí / lượt | $0.32 | $0.37 | ≤ $4 ✔ |
| Tỉ lệ đọc cache (trung bình các lượt) | | 86% (72–94%) | ≥ 70% ✔ |
| Lên Expert | 40% (4/10) | 60% (6/10) | theo dõi |
| Số lượt gọi trung bình | 18.2 | 18.5 | theo dõi |

**Đánh giá:**
- Câu đầu của bước 7 có tác dụng rõ: cả hai lượt `gson-992` thêm ca `-0.0` với `0` vào test tái hiện và sửa `hashCode()` cho đúng, với 6 và 9 lượt gọi. Ở #259 cả hai lượt trượt đúng ca này.
- Câu sau chưa có tác dụng: cả hai lượt `gson-1768` vẫn mang test cũ tới gate. `gson-1768` vẫn qua một trong hai lượt, như ở #259.
- Phần tăng 20 điểm chấm nghiêm đến từ `gson-992` (+2 lượt). `gson-1768` giữ nguyên 1/2.
- Tỉ lệ lên Expert tăng chủ yếu ở `gson-1247` và `gson-3113`. Hai issue này bị loại khỏi phần chấm, và kết quả của chúng không đổi.

## Kết quả từng issue

"Lượt" là số lượt gọi Core + Expert. E là có lên Expert. Số hiệu là `R3`, `R4` của mỗi issue.

| Issue | Lượt 1 | Lượt 2 | Test ẩn | Ghi chú |
| --- | --- | --- | --- | --- |
| gson-992 | PR, $0.09, 6 | PR, $0.11, 9 | pass, pass | #259: fail, fail |
| gson-1247 | PR, $0.58, 28, E | PR, $0.35, 20, E | **fail**, **fail** | upstream, bị loại |
| gson-1587 | PR, $0.04, 4 | PR, $0.07, 8 | pass, pass | |
| gson-1768 | Hết 40 lượt, $0.92, 40, E | PR, $0.66, 28, E | chưa chạy, pass | agent (r3) |
| gson-3113 | PR, $0.41, 20, E | PR, $0.47, 22, E | **fail**, **fail** | upstream, bị loại |

"PR" nghĩa là qua gate và tới bước mở PR. Với `--no-pr`, không PR nào được mở thật.

## Vì sao trượt ở #259

### `gson-992`: agent dừng khi ví dụ của issue đã qua

Issue chỉ đưa một ví dụ, `42` với `LazilyParsedNumber("42")`. Cả hai lượt chép đúng ví dụ đó làm test tái hiện, đổi `hashCode()` sang băm theo bit của giá trị double, chạy lại test thấy qua, rồi trả lời sau 7 và 10 lượt gọi. Test ẩn trượt ở `-0.0` với `0`: `equals()` so bằng `a == b` nên coi hai giá trị này bằng nhau, còn `Double.doubleToLongBits` cho hai giá trị bit khác nhau.

Agent đã mở đúng đoạn `equals()` có phép so đó. Nhưng quy trình cũ chỉ bảo agent chạy lại test tái hiện và xem nơi gọi hàm. Không bước nào bảo agent kiểm cả quy tắc mà issue nêu (hợp đồng `equals`/`hashCode`), nên dừng ở ví dụ là đúng theo quy trình.

Sau bước 7, R3 thêm `0.0`/`-0.0`, `NaN`, `BigDecimal("1.50")`/`1.5` vào test. R4 thêm `0`/`-0.0` và `LazilyParsedNumber("-0.0")`. Cả hai chuẩn hoá `-0.0` trước khi băm.

### `gson-1768`: test cũ vẫn đòi đường dẫn sai, và tốn lượt ở test tái hiện

Bản sửa đổi đường dẫn JSON trong thông báo lỗi từ `$.` thành `$.name`. Các test có sẵn khẳng định chuỗi cũ ở bốn chỗ: ba dòng trong `MapTest` (dòng 700, 710, 719, trong đó có `testMapDeserializationWithDuplicateKeys`) và `JsonParserTest.testExtraCommasInMaps` (`"... path $.\n"`). Cả bốn lượt ở #259 và #425 đều tới gate với ít nhất một test còn chuỗi cũ, nên gate đầu tiên luôn trượt ở bước `test` và tốn thêm một vòng sửa.

So sánh hai lượt ở #259:
- **R1** tới gate ở lượt 15. Gate trượt, agent tìm chuỗi `at path $.` trong test, sửa ba test rồi qua ở lượt 32.
- **R2** sửa code ở lượt 9 trước khi có test tái hiện, rồi mất lượt 10–22 để viết test cho khớp cách repo viết (tìm file mẫu, sửa lại test). Tới lượt 15 chưa lần nào tới gate, nên router lên Expert. Expert làm lại bản sửa ba lần (lượt 23–35), sửa `MapTest` nhưng sót `JsonParserTest`, và trả lời đúng ở lượt 40. Gate trượt, hết lượt.

Ở #425, R3 lặp gần đúng R2: lượt 10–20 cho test tái hiện, lên Expert ở lượt 15, tới lượt 36–37 mới tìm chuỗi cũ. Agent tìm `Duplicate key` và `path $.\n`, sửa `MapTest` nhưng vẫn sót `JsonParserTest:140`, và trả lời ở lượt 40. R4 trả lời ở lượt 13, gate trượt, Expert sửa cả hai file và qua ở lượt 30.

Vậy câu sau của bước 7 có được làm theo (R3, lượt 36–37), nhưng quá muộn. Lần tìm `path $.\n` **có ra** `JsonParserTest:140`. #449 đã chạy lại đúng truy vấn đó trên base commit: 3 kết quả, gồm `JsonParserTest:140`, `MapTest:666` và `JsonReaderTest:455`. Sau lần tìm, agent chạy test `MapTest`, `JsonReaderPathTest` và `JsonReaderTest` nhưng không chạy `JsonParserTest`, rồi trả lời. Gate trượt ở bước `test`, và agent mở `JsonParserTest:130` thì chạm trần. Vậy nguyên nhân là trần lượt và việc không chạy test của file vừa tìm thấy, không phải `search_text`.

## Đề xuất

Chưa áp dụng và chưa đo.
- ~~**Kiểm `search_text` với chuỗi có `\n`.**~~ Đã kiểm ở #449: công cụ tìm đúng `\n` viết trong mã nguồn. #449 chỉ thêm test hồi quy, và từ chối truy vấn có ký tự xuống dòng thật (trước đây trả "No matches").
- **`gson-1768` nằm sát trần 40 lượt** (30, 40, 40, 28 qua bốn lượt). Bỏ được vòng gate trượt do test cũ thì có thể tiết kiệm 10–15 lượt. Nếu cần sửa tiếp, nên mở issue riêng.

## Lưu ý

- Bước 7 nằm trong prompt chung của mọi ngôn ngữ, nhưng đợt này chỉ đo Java. Câu đầu có thể làm bản vá rộng hơn cần thiết, hoặc tốn thêm lượt ở các issue đang qua. Lần đo tới của các ngôn ngữ khác cần so với số cũ.
- Mỗi cấu hình chỉ có 6 đến 10 mẫu, một lượt đổi kết quả làm tỉ lệ đổi 10 đến 17 điểm.
- gson là mã nguồn mở, model có thể đã thấy bản sửa khi huấn luyện.
- Thời gian trung bình 3.1 phút mỗi lượt, lâu nhất 5.0 phút.
- Bản vá của 10 lượt nằm ở `.workspace/state/jobs/<id>/patch.diff`, chỉ có trên máy đo, không nằm trong repo.

## Chạy lại

```bash
PATH=$PWD/.workspace/tools/cbm:$PATH target/release/aegis eval --config config/gson.toml --set eval/issues --no-pr --only gson-992
```

Lặp cho từng issue gson. Số liệu từng lượt đọc từ `.workspace/state/ledger/GSON-<số>-R<n>.json`.
