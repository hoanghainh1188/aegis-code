# ADR 0004 – Repo đích đa ngôn ngữ qua language profile

- Ngày: 2026-10-20
- Trạng thái: Đề xuất
- Người quyết định: @Nguyen Han Hoang Hai
- Liên quan: [ADR 0001](0001-platform-in-rust.md), [ADR 0002](0002-full-platform-mvp-as-checkpoint.md), [ADR 0003](0003-platform-service-architecture.md), Proposal §3, §6, §9, Thiết kế §3.1, §6.1, §7

## Bối cảnh

Proposal chốt repo đích là "repo nội bộ, Rust, > 100k dòng", và thiết kế viết cứng cho Rust ở nhiều chỗ:

| Thành phần | Phần viết cho Rust |
| --- | --- |
| ACI (Thiết kế §3.1, #7) | Grammar tree-sitter Rust, `rust-analyzer`; `edit` chạy `rustfmt` |
| Gate (Thiết kế §6.1, #11) | `cargo fmt` → `clippy` → test tái hiện → `cargo test` → `cargo llvm-cov` |
| Chặn phụ thuộc lạ (Thiết kế §7, #12) | `Cargo.lock`, `cargo deny` |
| Sonar (#17, #27) | Nhập Clippy và LCOV |
| Sandbox, cache (#5, #2) | Image Rust, cache `target/` và registry Cargo |
| Prompt (#13) | Quy ước Rust |

Mục tiêu nay đổi: repo được bảo trì có thể viết bằng ngôn ngữ nào cũng được. Hỗ trợ ngôn ngữ khác đang nằm ở #37 (Phase 3), tức là phải làm lại ACI và gate sau khi lõi đã xong.

Phần đã làm của lõi không phụ thuộc ngôn ngữ repo đích: provider, vòng lặp agent, ledger, router. Phần phụ thuộc ngôn ngữ (ACI, gate, sandbox, chặn phụ thuộc, prompt) phần lớn chưa làm, trừ phần chặn crate của #12.

## Quyết định

### 1. Language profile thay cho cấu hình Rust viết cứng

Mọi chỗ phụ thuộc ngôn ngữ của repo đích đi qua một **language profile**. Một profile gồm:

| Phần | Nội dung | Dạng |
| --- | --- | --- |
| Nhận diện | File đánh dấu (`Cargo.toml`, `pyproject.toml`, `package.json`, `pom.xml` / `build.gradle*`, `go.mod`) | Dữ liệu |
| ACI | Grammar tree-sitter và truy vấn lấy symbol (hàm, kiểu, method…) cho `search_symbol`; lệnh language server (LSP) tùy chọn | Grammar biên dịch sẵn; truy vấn là file `.scm` |
| Kiểm tra khi `edit` | Kiểm tra cú pháp bằng tree-sitter (mọi ngôn ngữ, không cần toolchain); formatter của repo nếu repo dùng | Dữ liệu |
| Chuỗi gate | Các bước có tên: format, lint, test tái hiện (mẫu lệnh có `{test}`), test toàn bộ, coverage, kèm định dạng file coverage | Dữ liệu |
| Phụ thuộc | Đọc lockfile trước và sau bản vá, liệt kê phụ thuộc mới; kiểm advisory và license | Code theo hệ sinh thái |
| Sandbox | Image toolchain, thư mục cache, lệnh lấy phụ thuộc | Dữ liệu |
| Sonar | Báo cáo linter nhập vào và định dạng coverage | Dữ liệu |
| Prompt | Quy ước code của ngôn ngữ (BP2, Thiết kế §4.1) | File văn bản |

- **Lệnh mặc định chỉ là điểm bắt đầu.** Repo thường đã có lệnh riêng trong CI (`npm run lint`, `./gradlew check`, `make test`). `RepoConfig` (ADR 0003 §7) ghi đè từng bước. Bước nào repo không có thì bỏ qua và ghi vào kết quả gate, không tự bịa lệnh.
- **Repo khai báo profile trong `RepoConfig`.** Phần nhận diện chỉ dùng để gợi ý khi thêm repo. Bản đầu: mỗi repo một profile chính. Repo nhiều ngôn ngữ (monorepo) gán profile theo thư mục ở bước sau.
- **Dispatch tĩnh** (ADR 0003 §7): `enum Language { Rust, Python, TypeScript, Java, Go }`, không dùng `Box<dyn …>`. Thêm ngôn ngữ là thêm một nhánh enum cùng dữ liệu của profile.
- Code chung nằm trong crate mới `aegis-lang`: kiểu profile, nạp và kiểm tra cấu hình, grammar tree-sitter, bộ đọc lockfile. `aegis-aci`, `aegis-gate` và vòng đời issue chỉ đọc profile, không chứa tên công cụ của ngôn ngữ nào.

### 2. Năm profile ban đầu

| Profile | ACI (tree-sitter / LSP) | Format / lint | Test / coverage | Lockfile |
| --- | --- | --- | --- | --- |
| Rust | `tree-sitter-rust` / `rust-analyzer` | `rustfmt`, `clippy` | `cargo test` / `cargo llvm-cov` (LCOV) | `Cargo.lock` |
| Python | `tree-sitter-python` / `basedpyright` | `ruff format`, `ruff check` | `pytest` / `coverage.py` (Cobertura XML) | `uv.lock`, `poetry.lock`, `requirements*.txt` có ghim phiên bản |
| TypeScript / JavaScript | `tree-sitter-typescript`, `tree-sitter-javascript` / `typescript-language-server` | `prettier`, `eslint`, `tsc --noEmit` | lệnh test của repo (Jest, Vitest…) / LCOV | `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` |
| Java | `tree-sitter-java` / `jdtls` | formatter và linter của repo (Spotless, Checkstyle…) | Maven / Gradle / JaCoCo XML | Gradle lockfile; Maven: cây phụ thuộc đã resolve |
| Go | `tree-sitter-go` / `gopls` | `gofmt`, `go vet`, `golangci-lint` nếu repo có | `go test` / `-coverprofile` | `go.sum` |

- Cả năm ngôn ngữ đều có grammar tree-sitter ổn định, language server dùng được, lockfile để so phụ thuộc, và được SonarQube Server và Cloud hỗ trợ.
- C#, Kotlin, PHP và ngôn ngữ khác thêm ở Phase 3 theo nhu cầu (#37).
- Tên công cụ trong bảng là mặc định của profile, chốt lại ở issue của từng profile.

### 3. Phụ thuộc: chặn phụ thuộc lạ cho mọi hệ sinh thái

- Giữ cách của #12 cho mọi ngôn ngữ: phụ thuộc đã có trong lockfile trên nhánh chính thì được giữ; phụ thuộc mới phải nằm trong allow-list của repo và đến từ registry chính thức của hệ sinh thái. Trỏ một phụ thuộc đã có sang git hay registry khác cũng bị chặn.
- Mỗi hệ sinh thái có một bộ đọc lockfile trong `aegis-lang`. Bộ cho `Cargo.lock` hiện nằm ở `aegis-gate` (#12) và sẽ chuyển sang đó.
- **Advisory và license:** Rust giữ `cargo deny` với `policies/deny.toml`. Các hệ sinh thái khác dùng một công cụ chung đọc được cả năm loại lockfile (dự kiến `osv-scanner`; chốt ở issue profile Python). Nếu công cụ chung không kiểm được license của hệ sinh thái nào, thêm công cụ riêng cho hệ sinh thái đó.
- Repo Java dùng Maven không có lockfile thì so cây phụ thuộc đã resolve trước và sau bản vá.

### 4. Lấy phụ thuộc khi sandbox không có mạng

Sandbox chạy test không có mạng (Thiết kế §7). Với Rust, `cargo fetch` trước rồi build offline là đủ. Python, Node và Java lấy phụ thuộc phức tạp hơn, và có thể chạy script lúc cài (npm `postinstall`, `setup.py`). Vì vậy:

- Lấy phụ thuộc là một bước riêng, chạy trong container riêng **trước** các bước gate. Container này chỉ đi ra được registry của hệ sinh thái (qua proxy có allow-list), không có secret và không mount workspace khác.
- Bước này chỉ chạy khi kiểm tra phụ thuộc (mục 3) đã qua. Phụ thuộc mới ngoài allow-list không bao giờ được tải về.
- Tắt script lúc cài khi hệ sinh thái cho phép (`npm ci --ignore-scripts`). Repo nào cần script thì bật trong `RepoConfig`.
- Kết quả (thư mục cache, venv, `node_modules`…) mount chỉ đọc vào container gate; gate vẫn chạy `--network none`.

### 5. Thứ tự làm

| Phase | Việc |
| --- | --- |
| 1 – MVP / PoC | `aegis-lang` và mô hình profile; profile **Rust** và **Python**. ACI (#7), gate (#11), sandbox (#5), chặn phụ thuộc (#12), prompt (#13) viết theo profile ngay từ đầu. Đo KPI trên một repo Rust và một repo Python |
| 2 – Platform | Profile **TypeScript/JavaScript**, **Java**, **Go**; repo nhiều ngôn ngữ (profile theo thư mục) cùng việc quản lý nhiều repo (#24) |
| 3 – Mở rộng | Ngôn ngữ thứ sáu trở đi theo nhu cầu (#37) |

Phase 1 làm hai profile, không phải một: một profile thì chưa thử được phần trừu tượng. Rust và Python khác nhau nhiều (biên dịch và thông dịch, `cargo` và nhiều công cụ đóng gói), nên lộ ra sớm chỗ nào của profile còn viết cứng.

### 6. Platform vẫn viết bằng Rust

ADR 0001 chọn Rust một phần vì "cùng ngôn ngữ với repo được bảo trì". Lý do đó không còn đúng với mọi repo, nhưng vẫn giữ Rust:

- Lõi đã viết bằng Rust và đã có test: provider, vòng lặp agent, ledger, router.
- Lý do chính của ADR 0001 vẫn đúng: không cần Agent SDK, tự kiểm soát bố cục prompt và cache.
- Một binary, ít tài nguyên, kiểu dữ liệu chặt, hợp với chạy như dịch vụ (ADR 0003).
- Repo Rust vẫn là một profile, nên platform vẫn tự bảo trì được chính nó.

## Hệ quả

- Thêm crate `aegis-lang` với các crate grammar tree-sitter của năm ngôn ngữ. Mỗi crate mới phải qua `cargo deny` của aegis-code (#42).
- Mỗi profile một image sandbox. Phase 1 có hai image (Rust, Python). Image cũng là nơi giữ đúng phiên bản toolchain của repo đích.
- Mỗi profile cần một repo mẫu nhỏ trong `tests/fixtures/` để test ACI, gate và bộ đọc lockfile mà không cần repo thật.
- KPI (Proposal §8) báo theo từng ngôn ngữ. Bộ thử (#4) có issue của mỗi ngôn ngữ đã có profile. Ngưỡng KPI giữ nguyên cho mọi ngôn ngữ, xem lại sau lần đo đầu.
- Thời gian build và loại cache (5 phút hay 1 giờ, Thiết kế §4.2) đo và chọn theo repo, không theo ngôn ngữ (#2).
- Kiểm tra cú pháp bằng tree-sitter chạy được cho mọi ngôn ngữ, nhưng chỉ bắt lỗi cú pháp. Lỗi kiểu dữ liệu với ngôn ngữ thông dịch (Python, JavaScript) chỉ lộ ra ở lint hoặc test, nên vòng sửa có thể dài hơn Rust.
- `policies/deny.toml` và `policies/allowed-crates.toml` (#12) trở thành chính sách của profile Rust. Allow-list chuyển dần vào cấu hình theo repo (#24).
- Phần chia nhóm cảnh báo Sonar (#17) và Golden Master (Thiết kế §6.2) phải kiểm lại với từng ngôn ngữ.

## Phương án đã cân nhắc

| Phương án | Lý do không chọn |
| --- | --- |
| Giữ Rust, để ngôn ngữ khác tới Phase 3 (#37) | Phải làm lại ACI, gate và sandbox sau khi đã viết cứng cho Rust; không đúng mục tiêu mới |
| Chỉ chạy lệnh shell do repo khai báo, không có ACI theo ngôn ngữ | Agent mất `search_symbol` và kiểm tra cú pháp khi `edit`; theo SWE-agent, công cụ có cấu trúc cho kết quả tốt hơn shell tự do |
| Chỉ dùng LSP cho ACI, bỏ tree-sitter | LSP khởi động chậm, nặng, một số cần mạng hoặc cần build trước; tree-sitter chạy được ngay, kể cả trên code đang lỗi. LSP giữ vai trò tùy chọn để tra định nghĩa |
| Mỗi ngôn ngữ một plugin riêng (binary hay WASM) | Thêm giao thức và vòng đời plugin cho năm ngôn ngữ; enum cùng dữ liệu profile là đủ, và giữ dispatch tĩnh (ADR 0003) |
| Bắt đầu với hơn năm ngôn ngữ | Mỗi profile cần image, repo mẫu, bộ đọc lockfile và bộ thử; năm ngôn ngữ đã phủ phần lớn repo bảo trì thường gặp |
| Viết lại platform bằng Python hoặc TypeScript để dùng Agent SDK | Lõi đã xong bằng Rust; lý do bỏ Agent SDK ở ADR 0001 vẫn đúng |
