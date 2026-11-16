# Chi phí chạy sandbox dưới gVisor (runsc) (#322)

**Kết luận:** giữ runc làm mặc định; `[run] sandbox_runtime = "runsc"` vẫn là tuỳ chọn, và chưa dùng được cho tới khi sửa xong #548. Không đổi mặc định nên không có ADR mới.

- Khi bật được, runsc làm gate chậm hơn:
  - ×1.5–1.9 với các job ngắn (Go, Java, C#), khoảng 10–30 giây mỗi job;
  - ×1.07–1.2 với các job dài và nặng CPU (Python, Rust, TypeScript).
- RAM tối đa của mỗi container tăng thêm khoảng 10–220 MB. `cargo test` của typst tăng gần 1 GB.
- Mọi job trong bộ đo đều qua gate và test ẩn dưới runsc, sau khi đã tránh hai lỗi nêu ở mục "Những gì hỏng dưới runsc".

Nên bật runsc theo từng bản triển khai (repo không tin cậy, nhiều tenant), không theo profile. Lý do nằm ở mục [Quyết định](#quyết-định).

## Cách đo

**Môi trường đo:**
- Docker daemon riêng (Docker-in-Docker, `scripts/gvisor/up.sh`), chạy trên Docker Desktop của máy Mac M4 Pro.
  - VM có 12 vCPU, 8 GB RAM, kernel 7.0.14-linuxkit, aarch64.
  - Docker 29.9.3, gVisor release-20261109.0.
- Daemon đăng ký hai runtime: `runc` và `runsc` (platform `systrap`).
- Cấu hình Docker Desktop của máy không bị đổi.

**Chạy đo:**
- Mỗi profile có một job trong `eval/issues`, chạy lại bằng `aegis eval --replay --no-pr`.
  - Chế độ này không gọi model. Bản sửa upstream được ghi qua công cụ ACI.
  - Job vẫn đi đủ các bước: fetch qua egress proxy, formatter, gate (nhánh gốc và bản vá), coverage, test ẩn.
- Hai lượt của cùng một job chỉ khác nhau ở `sandbox_runtime`. Cache của repo giống nhau.
- Lượt khởi động dưới runc (làm ấm image và cache) không được tính.

**Thu số liệu:**
- `scripts/gvisor/watch.py` theo dõi `docker events` và ghi mỗi container một dòng:
  - thời gian chạy và mã thoát;
  - `memory.peak` và `cpu.stat` của cgroup, lấy mẫu mỗi 0.2 giây. Dưới runsc, cgroup tính cả sentry và gofer.
- `scripts/gvisor/summarize.py` dựng các bảng bên dưới.
- Có thể chạy lại bằng `scripts/gvisor/bench.sh <config> <issue> [số lượt]`.

| Profile | Job | Lượt đo mỗi runtime |
| --- | --- | --- |
| Go | gin-4851 | 2 |
| Java | gson-992 | 2 |
| C# | NSubstitute-828 | 2 |
| TypeScript | zod-6027 | 2 |
| Python | scrapy-4466 | 1 (gate khoảng 10 phút) |
| Rust | typst-7541 | 1 (gate khoảng 19 phút) |

PHP không có repo nào trong bộ thử nên không được đo.

## Những gì hỏng dưới runsc

Lượt đầu chạy runsc nguyên trạng, và cả 6 job đều dừng sớm. Có hai nguyên nhân. Chỉ nguyên nhân thứ hai xảy ra ngoài máy đo.

1. **Lỗi `Permission denied` khi ghi vào thư mục mount. Chỉ xảy ra trên Docker Desktop.**
   - Job bị ảnh hưởng: scrapy (cache của uv), gson (`target/`), NSubstitute (`obj/`), typst, và bước coverage của gin.
   - Nguyên nhân: cơ chế chia sẻ file của Docker Desktop báo chủ của file là chính tiến trình đang đọc nó. Gofer của runsc đọc bằng root nên thấy mọi file thuộc `0:0`, rồi từ chối user `501:20` của sandbox.
   - Kiểm chứng: trên một thư mục Linux thật trong daemon đo, cùng các tham số đó, runsc đọc ghi bình thường. Máy chạy aegis thật là Linux nên không gặp lỗi này.
2. **Bước fetch không phân giải được tên egress proxy. Lỗi này thật, gặp trên mọi máy, đã ghi thành #548.**
   - Sandbox nhận proxy qua `HTTP(S)_PROXY=http://<tên container>:3128`. Trên network riêng, tên đó do DNS nội bộ của Docker ở `127.0.0.11` phân giải, và Docker chuyển truy vấn tới đó bằng luật iptables trong network namespace của container.
   - Network stack riêng của gVisor không đi qua các luật đó, nên lần phân giải thất bại. Ví dụ ở zod: `getaddrinfo EAI_AGAIN aegis-egress-…`.
   - Đã tái hiện riêng: trên cùng một network internal, container runc phân giải được tên container bên cạnh, container runsc thì không.
   - Gate, formatter và LSP chạy với `--network none` nên không bị ảnh hưởng.

**Cách đi tiếp để đo:** lượt đo thứ hai dùng một bản vá tạm, `scripts/gvisor/docker-desktop.patch`, không merge.
- Dưới runsc, sandbox chạy với `--user 0:0`, vẫn `--cap-drop ALL`.
- Riêng bước fetch (bước duy nhất có mạng) chạy bằng runc.
- Bản vá phải có tác dụng cả trong `aegis-aci`. Tiến trình này được khởi động với môi trường đã xoá sạch (`env_clear`), nên bản vá thêm hai biến của bench vào `INHERITED_ENV`. Thiếu bước này thì formatter dưới runsc vẫn hỏng, ACI bỏ bản sửa, và test ẩn của zod và scrapy trượt.

Hệ quả: các bước gate, formatter và test ẩn chạy hoàn toàn dưới runsc, còn số liệu fetch trong lượt này là của runc. Ngoài hai lỗi trên, không bước nào chạy sai dưới runsc:
- ptrace của `go test` chạy bình thường;
- `/tmp` có `exec` (binary test, jansi của Maven) chạy bình thường;
- cache mount read-only chạy bình thường.

## Kết quả theo job

Thời gian của cả job và của các lượt gate (giây), lấy trung bình các lượt đo:

| Profile | Job | runc: job / gate | runsc: job / gate | Job ×runsc | Gate ×runsc | Qua gate và test ẩn dưới runsc |
| --- | --- | --- | --- | --- | --- | --- |
| Go | gin-4851 | 59 / 42 | 93 / 67 | ×1.56 | ×1.59 | có |
| Java | gson-992 | 43 / 20 | 60 / 32 | ×1.39 | ×1.60 | có |
| C# | NSubstitute-828 | 38 / 29 | 69 / 55 | ×1.82 | ×1.88 | có |
| TypeScript | zod-6027 | 87 / 19 | 90 / 23 | ×1.04 | ×1.20 | có |
| Python | scrapy-4466 | 592 / 571 | 634 / 613 | ×1.07 | ×1.07 | có |
| Rust | typst-7541 | 1318 / 1140 | 1332 / 1155 | ×1.01 | ×1.01 | có |

- Job ×runsc của zod thấp vì phần lớn thời gian của job là fetch (`npm ci`, khoảng 57 giây), mà lượt này fetch chạy bằng runc.
- Con số ×1.01 của typst thấp hơn thực tế. `cargo clippy` dưới runc mất 120 giây, dưới runsc chỉ 34 giây, do khác trạng thái cache build giữa các lượt chứ không do runtime. Bỏ clippy ra thì gate của typst chậm khoảng ×1.08.

## Kết quả theo bước

Mỗi ô ghi: giây / giây CPU / RAM tối đa (MB). Bảng bỏ qua các bước dưới 1 giây (`gofmt -w`, `rustfmt`, `ruff format <file>`), egress proxy, và fetch (lượt này fetch chạy bằng runc).

| Profile | Bước | runc | runsc | Thời gian ×runsc | RAM tối đa Δ |
| --- | --- | --- | --- | --- | --- |
| Go | `go vet ./...` | 13.0 / 45 / 1755 | 21.3 / 76 / 1971 | ×1.63 | +216 MB |
| Go | `go test ./...` (gate, bước `test`) | 28.0 / 99 / 1825 | 44.8 / 163 / 1922 | ×1.60 | +97 MB |
| Go | `go test -v` (test ẩn) | 13.7 / 48 / 1778 | 21.5 / 78 / 1939 | ×1.57 | +160 MB |
| Java | `mvn -B -o` (build, test) | 12.9 / 34 / 586 | 19.8 / 55 / 622 | ×1.53 | +36 MB |
| Java | test ẩn (`mvn test -Dtest=…`) | 7.9 / 22 / 680 | 11.7 / 33 / 689 | ×1.48 | +9 MB |
| Java | coverage (JaCoCo) | 6.6 / 17 / 516 | 11.7 / 32 / 592 | ×1.78 | +75 MB |
| C# | `dotnet build --no-restore` | 13.5 / 46 / 749 | 30.4 / 110 / 950 | ×2.25 | +201 MB |
| C# | `dotnet format --verify-no-changes` | 9.9 / 26 / 618 | 16.4 / 41 / 651 | ×1.66 | +32 MB |
| C# | test ẩn (`dotnet test --filter`) | 2.1 / 3 / 332 | 4.6 / 9 / 402 | ×2.17 | +70 MB |
| TypeScript | `vitest` | 19.4 / 42 / 1546 | 23.8 / 77 / 1555 | ×1.23 | +9 MB |
| TypeScript | `biome` (lint, format) | 1.4 / 1 / 138 | 1.9 / 3 / 127 | ×1.30 | −11 MB |
| Python | `pytest -n auto` (gate, coverage, test ẩn) | 584.9 / 2225 / 3512 | 626.5 / 2256 / 1880 | ×1.07 | xem ghi chú |
| Python | `ruff check .` | 0.3 / 0.2 / 75 | 0.4 / 0.5 / 54 | ×1.26 | −21 MB |
| Rust | `cargo test --workspace` | 498.8 / 850 / 3973 | 530.5 / 1033 / 4912 | ×1.06 | +939 MB |
| Rust | `cargo llvm-cov --workspace` | 681.7 / 1045 / 4876 | 751.5 / 1614 / 5314 | ×1.10 | +438 MB |

**Thời gian:**
- Bước càng ngắn thì runsc càng chậm tương đối: ×1.5–2.2 với các bước 2–30 giây, ×1.06–1.10 với các bước dài nhiều phút.
- Chi phí cố định khi khởi động một sandbox runsc là khoảng 0.5–2 giây. Ngoài ra mỗi syscall đi qua sentry, nên các bước gọi nhiều syscall (build .NET, `go vet`) chịu nặng nhất.

**CPU:**
- Số giây CPU tăng nhiều hơn thời gian thực: gin khoảng +65%, C# khoảng ×2.4, `cargo llvm-cov` +54%.
- Khi lập kế hoạch máy chủ, đây là con số đáng tính, vì nhiều job chạy song song cùng chia CPU.

**RAM:**
- RAM tối đa thường tăng 10–220 MB mỗi container, gồm phần của sentry và gofer.
- Bước nhiều tiến trình và nhiều file như `cargo test` tăng tới +0.9 GB.
- Giới hạn `--memory` của sandbox không đổi theo runtime, nên một bước đã sát giới hạn dưới runc có thể bị OOM dưới runsc.

**Ghi chú về `pytest` của scrapy:** RAM tối đa dưới runsc thấp hơn runc 1.6 GB. Đây không phải chỗ tiết kiệm: gVisor cấp bộ nhớ cho tiến trình con qua sentry, và cgroup tính phần đó khác với runc. Không nên đọc con số này như một mức giảm.

## LSP

Lượt replay không khởi động container LSP nào: ACI chỉ bật language server khi agent dùng đến (#268). Vì vậy LSP được kiểm riêng bằng `scripts/gvisor/lsp_smoke.py`:
- khởi động language server của profile giống cách ACI làm (`docker run -i`, `--network none`, bỏ hết quyền đặc biệt);
- gửi `initialize`, mở một file, hỏi `documentSymbol`.

| Profile | Server | Số symbol (runc / runsc) | `documentSymbol`, giây (runc / runsc) | Lỗi inotify hoặc theo dõi file |
| --- | --- | --- | --- | --- |
| Go | gopls | 27 / 27 | 0.61 / 1.13 | không |
| TypeScript | typescript-language-server | 27 / 27 | 0.35 / 0.49 | không |
| Rust | rust-analyzer | 37 / 37 | 0.01 / 0.01 | không |
| Java | jdtls | 0 / 0 | 0.68 / 0.98 | không |
| C# | csharp-ls | 1 / 1 | 2.14 / 4.11 | không |
| Python | basedpyright | không chạy được | không chạy được | — |

- Cả 5 server khởi động được đều trả lời giống nhau dưới hai runtime, và không có lỗi inotify.
- jdtls trả 0 symbol dưới cả hai runtime vì chưa kịp lập chỉ mục dự án. runsc không làm khác đi điều đó.
- **Python không đo được.** Image `aegis-sandbox-python` không cài `basedpyright-langserver`, dù `profiles/python.toml` khai báo nó, nên LSP của Python không bao giờ chạy được trong sandbox, với bất kỳ runtime nào. Đã ghi thành #549.

## Quyết định

**Giữ runc làm mặc định. `sandbox_runtime = "runsc"` vẫn là tuỳ chọn và chưa bật theo profile.** Lý do:

1. **runsc chưa dùng được ngay.** Bước fetch hỏng ở mọi profile cho tới khi #548 được sửa. Đổi mặc định bây giờ sẽ làm mọi job dừng ở fetch.
2. **Chọn theo profile không hợp lý.**
   - Mối nguy runsc chặn lại là mã của repo thoát khỏi sandbox. Mối nguy đó gắn với từng repo, từng tenant, không gắn với ngôn ngữ.
   - Chi phí runsc ở profile nào cũng chịu được: nặng nhất là C#, thêm khoảng 30 giây mỗi job; ở các job dài chỉ thêm 7–10%.
   - Vì vậy câu hỏi đúng là "bản triển khai này có chạy mã không tin cậy không", và đó là một cấu hình của cả bản triển khai.
3. **Chưa đủ số liệu để đổi mặc định.**
   - Đợt đo chạy trên VM của Docker Desktop, lồng trong Docker-in-Docker, và file dự án đi qua virtiofs.
   - Máy này không có `/dev/kvm`, nên `--platform=kvm` chưa được thử. Trên máy Linux có KVM, kvm thường cho syscall nhanh hơn systrap, nên mức chậm có thể thấp hơn số đo ở đây.
   - Muốn đổi mặc định cần đo lại trên đúng loại máy chủ của bản triển khai, ví dụ máy của đợt chạy thử #158, sau khi #548 xong.

**Khi bật runsc, cần lưu ý:**
- **Giới hạn bộ nhớ:** cần thêm khoảng 0.2 GB cho mỗi sandbox, khoảng 1 GB cho Rust.
- **CPU:** cần tính thêm 1.2–2.4 lần số giây CPU.
- **Timeout:** `step_timeout_s` hiện tại vẫn dư cho các con số trên.

## Giới hạn của đợt đo

- Số lượt đo nhỏ: 1–2 lượt mỗi runtime cho mỗi job. Với các bước dài, chênh lệch giữa hai lượt cùng runtime khoảng 1–3%. Riêng `cargo clippy` lệch nhiều hơn, xem ghi chú ở bảng theo job.
- Số liệu fetch là của runc, vì bước này không chạy được dưới runsc (#548).
- Dưới runsc, sandbox chạy với user `0:0` thay vì user của máy, để tránh lỗi chia sẻ file của Docker Desktop. Trên Linux, aegis chạy với user của máy như bình thường.
- `memory.peak` của các container chạy rất ngắn (dưới 0.2 giây) có thể báo 0 vì chưa kịp lấy mẫu.
- PHP và `--platform=kvm` chưa được đo.
