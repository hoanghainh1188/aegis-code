# Chạy khô bộ thử bốn profile mới (#259)

- Ngày: 2026-10-29
- Issue: #259, phần chạy khô, làm trước lượt đo KPI bằng Claude API.
- Cách chạy: `aegis eval --replay --no-pr` (#104). Replay ghi bản sửa upstream qua tool của ACI, rồi chạy fetch, gate và test ẩn trong sandbox, không gọi model. Lượt nào fail ở đây là fail vì platform, không phải vì model.
- Cấu hình: `config/{gin,zod,gson,polly,nsubstitute,stackexchange-redis}.toml`, Codebase-Memory bật như `config/poc.toml`. Cách chạy ghi trong [eval/README.md](../../eval/README.md).

## Kết quả

| Repo | Issue | Qua gate và test ẩn |
| --- | --- | --- |
| gin (Go) | 5 | 5 |
| zod (TypeScript) | 5 | 5 |
| gson (Java) | 5 | 5 |
| Polly (C#) | 2 | 2 |
| NSubstitute (C#) | 2 | 2 |
| StackExchange.Redis (C#) | 1 | chưa chạy được (#291) |
| **Cộng** | **20** | **19** |

StackExchange.Redis-2689 chưa qua được gate, bản vá nào cũng vậy. Không có Redis server, khoảng 90 trên 5487 test fail ngay ở base. Gate chưa đọc được từng test fail của .NET, nên không so được với base (#291).

## Lỗi tìm ra

Lần chạy đầu chỉ gin qua (5/20). Lỗi của platform tách thành issue, đã sửa và merge:

| Issue | Lỗi | Ảnh hưởng |
| --- | --- | --- |
| #279 | Profile Go, TypeScript, Java, C# không có `fetch_hosts`; Maven không đọc proxy từ môi trường | Bước fetch bị chặn ở mọi repo có phụ thuộc |
| #278 | `aegis-aci` không nhận `DOCKER_HOST` | Formatter và `run_tests` không tới được daemon khi nó không ở socket mặc định |
| #280 | Replay dừng khi gặp lời nhắn impact của Codebase-Memory | Lượt chạy khô của issue nào có mã bị ảnh hưởng đều dừng |
| #289 | `edit` bị chặn vì lỗi tree-sitter đã có sẵn trong file (variance `out` của TypeScript) | zod-6641, zod-6643, zod-6659, NSubstitute-974 |
| #290 | Profile C# không coi `artifacts/` của MSBuild là build cache | Polly-3086, Polly-3286 fail ở bước dependencies |

Còn mở:
- **#288:** bước fetch của profile Java tải provider JUnit 4.7 của Surefire, trong khi gate cần JUnit 4. `config/gson.toml` đi vòng bằng `dependency:get`.
- **#291:** gate không đọc được từng test fail của Go, TypeScript, Java, C#.

Phần còn lại là cấu hình của từng repo, ghi trong file cấu hình:
- **zod:** cài bằng pnpm, không đọc lockfile vì lockfile ở các base thời `nub` thiếu mục. Gate dùng biome, bỏ project `@zod/treeshaking` vì nó cần bản build. Không đo coverage.
- **gson:** chỉ chạy module `gson`. JaCoCo gắn qua `JAVA_TOOL_OPTIONS`, vì `argLine` của gson thay cái mà `prepare-agent` đặt.
- **C#:**
  - Một project test, `net10.0`, runtime duy nhất trong image.
  - Project nêu bằng `--project` khi base chạy Microsoft.Testing.Platform, bằng đường dẫn trần khi chạy VSTest.
  - Tắt coverlet, NuGet audit và Nerdbank.GitVersioning; Polly tắt cả coverage.

## Bước tiếp theo

Lượt đo KPI bằng Claude API trên 19 issue, mỗi issue hai lượt. Chưa chạy, đang chờ duyệt ngân sách (khoảng $20–25).
