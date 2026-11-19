# Chi phí khởi động container của gate và chạy song song các bước (#587)

**Kết luận: NO-GO.** Giữ gate chạy tuần tự, mỗi bước một container. Không mở issue triển khai và không cần ADR, vì thứ tự và ngữ nghĩa của gate không đổi.

- **Khởi động container không phải là chi phí đáng kể:** khoảng 0,13 giây mỗi bước (`docker run` + đọc cờ OOM + `docker rm`), tức 0,6–0,9 giây cho cả một lần gate 5–7 container. Con số "~30 giây mỗi bước" không có căn cứ: thời gian của một bước là thời gian của chính lệnh (`go vet`, `mvn test`, `pytest`).
- **Chạy song song chỉ nhanh hơn khi máy còn thừa lõi:** trên 12 CPU, gin giảm từ 38 xuống 18 giây, NSubstitute từ 21 xuống 14 giây; gson và zod không giảm. Ghim vào 4 CPU (bằng giới hạn `--cpus 4` của một sandbox) thì gin chỉ còn 37 → 35 giây.
- **Giá phải trả:** RAM đỉnh gấp 1,8–3,4 lần, và trên cùng một cây mã thì kết quả gate bị sai: gson báo một bước hỏng ở 3/3 lượt song song, trong khi 3/3 lượt tuần tự đều qua.

Lý do đầy đủ ở mục [Quyết định](#quyết-định). Ba hướng tối ưu khác lộ ra trong lúc đo nằm ở mục [Quan sát thêm](#quan-sát-thêm).

## Cách đo

- **Máy:** Apple M4 Pro, Docker Desktop 29.9.3 (VM linux/arm64, 12 CPU, 7,7 GB RAM), runtime `runc`. Image sandbox build từ `docker/<profile>` ngày đo.
- **Chi phí container** (`scripts/gate-bench/container_cost.py`): chạy `true` trong image sandbox với đúng các cờ `Sandbox::wrap` đặt cho một bước gate (`--network none`, `--cap-drop ALL`, `--read-only`, tmpfs `/tmp`, volume `/home/aegis`, giới hạn RAM/CPU/pids, mount `/work`), rồi `docker inspect` và `docker rm --volumes` như gate làm sau mỗi bước. 20 lượt mỗi image sau 2 lượt làm nóng.
- **Khoảng trống giữa các bước trong job thật:** tính lại từ số liệu replay của #322 (`.workspace/gvisor/containers.jsonl`, runc): thời điểm container sau bắt đầu trừ thời điểm container trước kết thúc. Khoảng này gồm cả việc gate làm trên máy chủ giữa hai bước.
- **Thử chạy song song** (`scripts/gate-bench/trial.sh`):
  1. `capture.py` chạy một job replay (`aegis eval --replay --no-pr`), ghi `docker inspect` của mọi container `aegis-gate-*` và sao một bản cây mã ngay khi bước đầu tiên không có mạng bắt đầu (đã áp bản vá, đã fetch xong).
  2. `parallel.py` chạy lại các bước không có mạng (trừ bước cuối là test ẩn của eval) với đúng cờ, mount và giới hạn đã ghi: lần lượt từng bước, rồi tất cả cùng lúc. 3 lượt mỗi chế độ, xen kẽ.
  3. Hai cách đặt cây mã: mỗi bước một bản sao riêng (copy-on-write), hoặc chung một cây như gate thật (`--shared-tree`).
  4. Hai mức CPU: toàn bộ 12 CPU, hoặc ghim mọi container vào 4 CPU (`--cpuset 0-3`) để mô phỏng worker nhỏ.
- **Job:** gin-4851 (Go), gson-992 (Java), NSubstitute-828 (C#), zod-6027 (TypeScript). Số liệu scrapy (Python) và typst (Rust) lấy từ #322, không chạy lại.

## Chi phí của một container

| Image | `docker run true` | `inspect` | `rm` | Tổng (trung vị / p90) |
| --- | --- | --- | --- | --- |
| `aegis-sandbox-go` | 0,086 s | 0,016 s | 0,022 s | 0,126 / 0,133 s |
| `aegis-sandbox-java` | 0,087 s | 0,016 s | 0,021 s | 0,126 / 0,136 s |
| `aegis-sandbox-node` | 0,086 s | 0,016 s | 0,022 s | 0,124 / 0,128 s |
| `aegis-sandbox-dotnet` | 0,087 s | 0,016 s | 0,022 s | 0,125 / 0,131 s |
| `aegis-sandbox-python` | 0,084 s | 0,016 s | 0,022 s | 0,124 / 0,129 s |

Chi phí không phụ thuộc image (612 MB đến 1,63 GB): image chỉ đọc nên không có lớp nào phải chép.

Trong job thật (#322, runc, 10 lượt của 6 job), khoảng trống giữa hai container gate liên tiếp có trung vị **0,16 giây**, p90 0,30 giây (41 khoảng). Riêng khoảng sau bước fetch dài 2,9–7,6 giây: đó là lúc gate dọn mạng fetch và agent định dạng file, không phải khởi động container.

Tỉ trọng trong một lần gate, lấy 0,13 giây mỗi container:

| Job | Container gate | Thời gian gate | Chi phí container | Tỉ trọng |
| --- | --- | --- | --- | --- |
| zod-6027 | 5 | 19 s | 0,6 s | 3,3% |
| gson-992 | 5 | 20 s | 0,6 s | 3,1% |
| NSubstitute-828 | 7 | 29 s | 0,9 s | 3,0% |
| gin-4851 | 7 | 42 s | 0,9 s | 2,1% |
| scrapy-4466 | 7 | 571 s | 0,9 s | 0,2% |
| typst-7541 | 7 | 1140 s | 0,9 s | 0,1% |

Dùng lại một container cho nhiều bước vì thế tiết kiệm được tối đa khoảng 1 giây mỗi lần gate, và đổi lại mất việc đọc cờ OOM theo từng bước.

## Chạy song song các bước

Thời gian là trung vị của 3 lượt; RAM và CPU là đỉnh của tổng các container đang chạy (`docker stats`, lấy mẫu mỗi giây).

### 12 CPU, chung một cây mã (như gate thật)

| Job | Các bước | Tuần tự | Song song | RAM đỉnh | CPU đỉnh | Kết quả các bước |
| --- | --- | --- | --- | --- | --- | --- |
| gin-4851 | `gofmt -d` ×2, `go vet`, `go test`, `go test -coverprofile` | 38,0 s | 17,9 s (×0,47) | 1103 → 3647 MB | 4,1 → 11,4 | giống nhau |
| NSubstitute-828 | `dotnet format`, `dotnet build` ×2, coverage | 21,1 s | 14,0 s (×0,66) | 652 → 1849 MB | 4,0 → 9,0 | **khác**: lần `dotnet build` thứ hai qua khi tuần tự, hỏng khi song song |
| gson-992 | `mvn test-compile`, `mvn test`, JaCoCo | 19,5 s | 18,9 s (×0,97) | 557 → 992 MB | 3,1 → 5,0 | **khác**: 3/3 lượt song song có một bước hỏng (`test-compile` 1 lượt, `test` 2 lượt), tuần tự qua cả 3 |

### 12 CPU, mỗi bước một bản sao cây mã

| Job | Tuần tự | Song song | RAM đỉnh | Kết quả các bước |
| --- | --- | --- | --- | --- |
| gin-4851 | 37,9 s | 17,9 s (×0,47) | 1554 → 3456 MB | giống nhau |
| NSubstitute-828 | 27,9 s | 14,8 s (×0,53) | 699 → 1816 MB | giống nhau |
| gson-992 | 35,9 s | 21,1 s (×0,59) | 870 → 1598 MB | giống nhau |
| zod-6027 | 17,4 s | 16,5 s (×0,95) | 1305 → 1464 MB | giống nhau |

Tách cây thì kết quả ổn định, nhưng các bước không còn dùng lại sản phẩm build của nhau: gson tuần tự mất 35,9 giây thay vì 19,5 giây, NSubstitute 27,9 thay vì 21,1. So với gate hiện tại (chung cây, tuần tự), song song tách cây làm gson **chậm hơn** (19,5 → 21,1 giây) và NSubstitute nhanh hơn (21,1 → 14,8 giây).

### Ghim vào 4 CPU, mỗi bước một bản sao cây mã

| Job | Tuần tự | Song song | RAM đỉnh |
| --- | --- | --- | --- |
| gin-4851 | 36,9 s | 34,6 s (×0,94) | 1280 → 4323 MB |
| gson-992 | 36,5 s | 30,7 s (×0,84) | 842 → 1981 MB |
| NSubstitute-828 | 25,5 s | 20,6 s (×0,81) | 692 → 1934 MB |
| zod-6027 | 17,9 s | 17,6 s (×0,98) | 1188 → 1321 MB |

Với 4 CPU, gson song song (30,7 giây) vẫn chậm hơn gate hiện tại (19,5 giây), NSubstitute ngang (20,6 so với 21,1 giây), gin nhanh hơn 6%.

### Các bước dài (số liệu #322, không chạy lại)

Mỗi bước nặng đã dùng gần hết 4 CPU được cấp, nên không còn chỗ cho bước khác trên cùng số lõi:

| Job | Bước | Thời gian | CPU trung bình | RAM đỉnh |
| --- | --- | --- | --- | --- |
| gin | `go vet` / `go test` / coverage | 12,6 / 13,7 / 14,3 s | 3,5 lõi | 1,7–1,8 GB |
| scrapy | `pytest -n auto` (test, trên base, coverage) | 135 / 136 / 299 s | 3,9 lõi | 3,3–3,5 GB |
| typst | `cargo clippy` ×2 / `cargo test` / `cargo llvm-cov` | 62 / 59 / 334 / 682 s | 1,5–1,8 lõi | 1,5 / 4,0 / 4,9 GB |

Chạy `cargo test` cùng lúc với `cargo llvm-cov` của typst cần khoảng 8,9 GB RAM, vượt giới hạn 8 GB của một sandbox và cả RAM của VM đo. Các lệnh cargo trên cùng `target/` còn chờ nhau ở khoá thư mục build.

## Vì sao các bước không độc lập

1. **Chung cây mã, chung sản phẩm build.** `mvn test` dùng lại `target/` của `mvn test-compile`, `dotnet test` dùng lại `obj/` của `dotnet build`, các lệnh cargo dùng chung `target/` (seed từ build cache, #104). Chạy cùng lúc thì chúng ghi đè nhau (gson ở trên) hoặc chờ khoá.
2. **Dừng sớm.** Gate dừng ở bước hỏng đầu tiên. Trong 330 lần gate đã ghi ở `.workspace/state/ledger`: 73% qua, 17% dừng ở `repro_test`, 6% ở `test`, 2% ở `lint`. Chạy song song thì 27% số lần gate vẫn chạy trọn `test` và `coverage` dù kết quả đã định.
3. **Trạng thái dùng chung của một lần gate.** Cây base được dựng một lần khi cần và dùng cho mọi bước chạy lại trên base; sidecar (#454) được reset giữa `repro_test`, `test`, `coverage`; bước `test` có thể ghi lại file tham chiếu (#105) trước khi `coverage` chạy; `integrity` so cây với ảnh chụp ban đầu sau khi mọi bước xong.
4. **Lõi CPU đã được dùng ở tầng job.** Server chạy `[worker] max_workers` job cùng lúc, mỗi sandbox được 4 CPU. Song song trong một job chỉ lấy lõi của job khác.

## Quyết định

**NO-GO cho cả hai phương án: chạy song song các bước và dùng lại container.**

1. **Phần có thể tiết kiệm nhỏ hoặc không có.** Container tốn khoảng 1 giây mỗi lần gate (0,1–3,3%). Song song không giúp gì ở zod, gson; ở gin và NSubstitute chỉ giúp khi worker thừa 8 lõi trở lên cho một job.
2. **Sai kết quả là không chấp nhận được.** Chung cây thì gate báo hỏng giả (gson 3/3 lượt). Tách cây thì phải sao cây và sản phẩm build cho từng bước, và mất phần build dùng lại, đúng thứ làm các gate ngắn nhanh.
3. **Tốn tài nguyên ở chỗ đang khan.** RAM đỉnh gấp 1,8–3,4 lần; với Rust và Python, hai bước nặng cùng lúc vượt giới hạn 8 GB của sandbox.
4. **Phải viết lại vòng chạy bước.** `run_gate` giữ một `Run` có trạng thái (cây base, sidecar, file tham chiếu, dừng sớm) cho cả chuỗi bước; song song hoá là đổi ngữ nghĩa gate và cần ADR, cho một lợi ích không chắc có.

Xem lại khi worker của bản triển khai có nhiều lõi rảnh cho mỗi job (ví dụ `max_workers = 1` trên máy 16 lõi) và thời gian gate của các repo Go hay C# trở thành điểm nghẽn đo được trên pilot (#158).

## Quan sát thêm

Ba chỗ dưới đây chiếm thời gian gate nhiều hơn hẳn khởi động container. Chúng nằm ngoài phạm vi #587 và chưa được thử; số liệu lấy từ các lượt replay ở trên và của #322.

1. **Bước `coverage` chạy lại toàn bộ test.** gin: `go test` 13,7 giây rồi `go test -coverprofile` 14,3 giây. scrapy: `pytest` 135 giây rồi `pytest --cov` 299 giây. typst: `cargo test` 334 giây rồi `cargo llvm-cov` 682 giây. Bước này chiếm 34–60% thời gian gate của ba job đó.
2. **Mỗi container Go có build cache riêng và trống.** Cache ngoài cây được mount chỉ đọc nên `GOCACHE` nằm trong volume của từng container (#210): `go vet`, `go test`, `go test -coverprofile` mỗi lệnh biên dịch lại từ đầu, khoảng 13 giây mỗi lệnh.
3. **Chạy lại trên base khi base vốn đã hỏng.** scrapy chạy `pytest` hai lần (135 + 136 giây) và typst chạy `cargo clippy` hai lần (62 + 59 giây) vì bước đó hỏng cả trên base. Kết quả trên base chỉ phụ thuộc commit base, nên có thể dùng lại giữa các job cùng base.

## Giới hạn của đợt đo

- Đo trên VM của Docker Desktop (macOS, arm64); file dự án đi qua lớp chia sẻ file của Docker Desktop. Trên máy Linux, chi phí container và I/O có thể khác.
- Mỗi cấu hình 3 lượt; chênh giữa các lượt cùng cấu hình dưới 6% (riêng NSubstitute tuần tự 12 CPU: 26,7–28,9 giây).
- `parallel.py` chạy lại các bước ngoài gate: các bước chạy trên base ở job thật (`gofmt -d` lần hai, `dotnet build` lần hai) ở đây chạy trên cây đã vá, và không có dừng sớm. Vì vậy thời gian "tuần tự" ở đây gần với, nhưng không bằng, thời gian gate thật.
- `capture.py` bỏ sót các container sống dưới khoảng 50 ms (ví dụ `biome lint` của zod); chúng không ảnh hưởng đáng kể tới thời gian.
- Ghim CPU bằng `--cpuset` mô phỏng số lõi, không mô phỏng RAM hay đĩa của một worker nhỏ.
- Python, Rust và PHP không được chạy lại; sidecar không có trong các job đo.
- Thống kê bước hỏng lấy từ ledger của máy phát triển (330 lần gate của các đợt eval), không phải từ pilot.
