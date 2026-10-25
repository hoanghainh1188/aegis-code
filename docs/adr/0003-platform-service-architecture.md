# ADR 0003 – Kiến trúc lớp dịch vụ (Phase 2)

- Ngày: 2026-10-18
- Trạng thái: Đề xuất
- Người quyết định: @Nguyen Han Hoang Hai
- Liên quan: [ADR 0001](0001-platform-in-rust.md), [ADR 0002](0002-full-platform-mvp-as-checkpoint.md), Proposal §7, §9

## Bối cảnh

ADR 0002 đặt mục tiêu là platform đầy đủ: chạy như dịch vụ, nối luồng ticket thật, nhiều repo, dashboard, phân quyền. Thiết kế kỹ thuật hiện chỉ mô tả PoC. Lớp dịch vụ mới có một dòng ở Proposal §9 và các issue #22–#33. Chưa có quyết định nào về DB, hàng đợi, web framework, UI hay cách triển khai, nên mỗi issue sẽ phải tự chọn.

Đặc điểm tải của platform quyết định phần lớn lựa chọn dưới đây:

- Một job (một issue) chạy vài phút tới vài chục phút, phần lớn thời gian là chờ model và chờ build/test.
- Số job mỗi ngày ở mức hàng chục tới vài trăm, không phải hàng nghìn mỗi phút.
- Mỗi job tốn tiền thật (trần $4). Chạy trùng hoặc chạy lại vô ích là mất tiền.
- Dữ liệu của repo chế độ `customer-jp` chỉ được xử lý ở Nhật (Proposal §7).
- Team nhỏ, viết Rust. Mỗi thành phần hạ tầng thêm vào là một thứ phải vận hành.

## Quyết định

### 1. Một binary, chạy theo vai trò

- Binary `aegis-server` chứa API HTTP, bộ lập lịch (poll Jira/Backlog/Sonar) và worker. Cờ `--role api|worker|all` chọn phần chạy; mặc định `all`.
- Giai đoạn đầu chạy một tiến trình `all`. Khi cần tách, chạy nhiều tiến trình cùng image với vai trò khác nhau, không đổi code.
- Mỗi job vẫn chạy test trong container sandbox riêng như Thiết kế §6–§7 (`--network none`, không secret).

### 2. Lưu trạng thái bền: PostgreSQL + sqlx

- Crate `aegis-store` dùng PostgreSQL qua `sqlx`. Migration có version, nhúng vào binary (`sqlx::migrate!`), chạy khi khởi động.
- Truy vấn dùng macro kiểm tra lúc biên dịch; thư mục `.sqlx/` được commit để build không cần DB. CI kiểm tra bằng `cargo sqlx prepare --check` và chạy test tích hợp với Postgres thật.
- Mọi truy vấn danh sách có phân trang và giới hạn.
- SQLite chỉ dùng cho chỉ mục Codebase-Memory của từng repo (Thiết kế §3.2), không dùng cho trạng thái platform.

### 3. Hàng đợi job: bảng trong PostgreSQL

- Job nằm trong bảng `jobs`. Worker lấy job bằng `SELECT … FOR UPDATE SKIP LOCKED` và giữ một lease (`locked_until`), gia hạn định kỳ khi đang chạy.
- Không dùng Redis, NATS hay SQS. Tạo job, chống trùng và kiểm tra ngân sách nằm trong cùng một transaction.
- Giới hạn chạy song song có hai mức: `max_workers` toàn tiến trình và `max_concurrent_jobs` theo repo. Câu lệnh lấy job tự áp dụng giới hạn theo repo.
- **Retry chỉ cho lỗi hạ tầng** (provider ngừng lâu, container hỏng, worker chết): backoff mũ, tối đa 3 lần. Kết quả của agent (`stopped_budget`, `stopped_steps`, `stopped_expert_fail`) là kết quả, không retry.
- Chi phí cộng dồn theo job qua mọi lần thử. Trần $4 áp cho cả job, không phải cho từng lần thử.
- **Khi restart:** lease hết hạn thì job được lấy lại. Lần thử đang dở được đánh dấu `interrupted`, job chạy lại từ đầu trên nhánh mới nếu còn ngân sách. Chưa làm checkpoint giữa chừng (xem Phương án đã cân nhắc).
- **Hủy job và tắt dịch vụ:** dùng `CancellationToken` (tokio-util), kiểm tra trước mỗi lượt gọi model và trong gate runner; khi hủy thì dừng container. Nhận SIGTERM → ngừng lấy job mới, chờ job đang chạy tới hết thời gian drain (cấu hình được), quá hạn thì nhả lease và đánh dấu `interrupted`.

### 4. HTTP và webhook: axum

- API và webhook dùng `axum` + `tower-http` (tracing, timeout, giới hạn kích thước body).
- API nội bộ có version (`/api/v1/...`) và trả cùng một envelope: `data`, `error`, `meta` (phân trang).
- Rate limit của API (#161, #173) là token bucket tự cài trong `aegis-server` (`auth/api.rs`), không dùng `tower-http`: `tower-http` không có rate limit theo client hay theo service. Chỉ áp cho các route cần token dưới `/api/v1`; `/api/v1/health` và webhook không qua nó.
  - Bucket chứa tối đa N request và nạp lại đều trong một phút. Hết lượt thì trả 429 `rate_limited` kèm `Retry-After`.
  - Mỗi service có bucket riêng (`api_requests_per_minute`, mặc định 120), dùng chung cho mọi token của service đó.
  - Request không có token sống bị đếm theo địa chỉ client (`api_failures_per_minute`, mặc định 20). Địa chỉ nào vượt hạn mức thì bị từ chối ngay, trước khi tra token trong Postgres. Nhờ vậy, gửi dồn token giả không làm tăng tải lên DB dùng chung với worker quá hạn mức đó.
  - Địa chỉ client lấy từ TCP peer. Sau ALB thì lấy từ `X-Forwarded-For`, nhưng chỉ tính số proxy tin cậy đặt ở `trusted_proxies`, nên client không tự ghi đè được.
  - Bucket nằm trong bộ nhớ của từng process. Chạy N process `--role api` thì hạn mức thực tế nhân N; đặt cấu hình theo tổng chia N. Không dùng hạn mức chung qua Postgres, vì như thế mỗi request lại tốn thêm một truy vấn, đúng thứ cần tránh. Nếu cần trần chung cho tất cả process thì đặt ở trước load balancer (AWS WAF rate-based rule), không đặt trong platform.
- Webhook:
  - GitHub: kiểm tra `X-Hub-Signature-256` (HMAC-SHA256) bằng so sánh thời gian hằng.
  - GitLab: kiểm tra `X-Gitlab-Token` bằng so sánh thời gian hằng.
  - Chống trùng bằng ràng buộc unique trên ID lần gửi (`X-GitHub-Delivery`, `X-Gitlab-Event-UUID`), ghi cùng transaction với job.
  - Trả 2xx ngay sau khi ghi job; xử lý chạy nền.
- Poll Jira, Backlog, Sonar chạy trong vai trò `api`/`all`, lịch lưu trong DB (bảng `polls`). Nhiều tiến trình cùng chạy thì mỗi kỳ chỉ một tiến trình poll: tiến trình nào dời được `next_at` của lần poll đã tới hạn (một câu `UPDATE … WHERE next_at <= now()`) thì poll (#26). Không dùng advisory lock: lock phải giữ một kết nối suốt lúc gọi API của tracker, còn dời lịch thì không giữ gì, tiến trình chết giữa chừng chỉ làm lần sau tới muộn. Ticket đã có job thì không tạo thêm, nhờ ID lần gửi `<repo id>:<key>` như webhook.
- Nội dung issue, ticket, comment là dữ liệu không tin cậy (Thiết kế §7), kể cả khi tới qua webhook đã xác thực.

### 5. Dashboard: HTML render phía server

- Dashboard và giao diện người duyệt (#28) render HTML từ `aegis-server` bằng template (`askama`), tương tác thêm bằng `htmx`. Không làm SPA riêng.
- Lý do: một ngôn ngữ, một image, một lần triển khai; CSP chặt (script tự host, không inline, nonce) dễ làm; nhu cầu UI là bảng, lọc, biểu đồ KPI và form review.
- Thư viện biểu đồ JS nhỏ, tự host, không tải từ CDN.
- Đăng nhập OIDC, session cookie lưu phía server trong Postgres, có CSRF token (#29).
- Xem lại khi UI cần tương tác phức tạp (sửa cấu hình nhiều bước, xem diff trực tiếp).

### 6. Triển khai

- Image container cho `aegis-server`. Chạy trên VM (EC2) có Docker; worker tạo container sandbox bằng lệnh `docker` (CLI). Chưa dùng Kubernetes.
  - Không dùng Docker API (`bollard`) (#184). Gate chạy đồng bộ ngoài runtime async (`spawn_blocking`); chạy `docker run` như một tiến trình con cho bước trong container cùng cách xử lý timeout, hủy và ghi log với bước chạy trên máy, và không thêm phụ thuộc. Cách này đã qua Phase 1.
  - Ràng buộc sandbox nằm trong dòng lệnh `docker run` (`--network none`, `--cap-drop ALL`, `no-new-privileges`, `--pids-limit`, chạy bằng user của máy, cache mount chỉ đọc), secret truyền theo tên qua môi trường của client `docker`, không nằm trên dòng lệnh. Client chỉ nhận các biến cần để tìm daemon.
  - Trước khi chạy gate, worker kiểm tra daemon (`docker version`); daemon không chạy thì gate dừng với lỗi của chính nó, không thành mọi bước thất bại với mã 125. Khi hết thời gian hoặc job bị hủy, container bị `docker kill` theo tên; kill lỗi thì lý do ghi vào log của bước.
  - Xem lại khi cần thứ CLI không làm gọn được: theo dõi sự kiện container, nhiều daemon từ xa, hoặc chuyển sang Kubernetes.
- **Dữ liệu ở Nhật:** mọi thứ của một repo `customer-jp` (DB, workspace, cache build, log) nằm trong `ap-northeast-1`. Cách đơn giản nhất: bản triển khai phục vụ repo khách đặt toàn bộ ở `ap-northeast-1` (EC2 + RDS PostgreSQL). Repo nội bộ có thể dùng chung bản đó hoặc một bản riêng; không có bản triển khai nào ngoài Nhật chứa dữ liệu repo khách.
- Hạ tầng dạng code bằng OpenTofu (Terraform). Có môi trường staging.
- Secret lấy từ AWS Secrets Manager khi khởi động và khi xoay vòng (#30); không có trong file cấu hình hay image.
- Log có cấu trúc qua `tracing`, metric dạng Prometheus ở `/metrics` (#32). Không log secret hay nội dung code.
  - `/metrics` nghe ở cổng nội bộ riêng (`[metrics] listen`, mặc định `127.0.0.1:9464`) trong mọi vai trò, không qua token API: tiến trình chỉ chạy worker không có API, và scrape bằng token tốn một truy vấn DB cùng một suất rate limit mỗi lần. Cổng này chỉ mở cho mạng riêng của Prometheus. Luật cảnh báo ở `deploy/alerts/`, dashboard ở `deploy/grafana/`.
  - Trên AWS (#33), mỗi bản triển khai có một EC2 giám sát riêng, cùng vùng: Prometheus (EC2 service discovery, security group của server chỉ cho máy này vào cổng 9464), Alertmanager gửi lên SNS, Grafana; không cổng nào mở ra ngoài, mở bằng SSM port forwarding. Không dùng Amazon Managed Prometheus/Grafana: collector managed của AMP không scrape thẳng EC2 nên vẫn cần một agent trên máy, còn Managed Grafana cần IAM Identity Center hoặc SAML. Với một máy server, tự chạy trên một EC2 nhỏ đơn giản và rẻ hơn.
- Workspace: mỗi job một bản checkout riêng trên đĩa của VM. Hai job cùng repo không được dùng chung một thư mục `target/` (Cargo khóa thư mục build); mỗi job có `target/` riêng, khởi tạo từ cache của repo. Cách khởi tạo (copy, reflink hay `sccache`) chốt sau khi đo ở #2.

### 7. Ràng buộc cho phần lõi (Phase 1)

Phase 2 chỉ bắt đầu khi lõi xong và đo được (ADR 0002), nhưng lõi phải giữ các điểm dưới đây để Phase 2 không phải làm lại:

| Ràng buộc | Áp dụng ở |
| --- | --- |
| Ledger ghi qua trait. MVP có bản ghi file JSON; Phase 2 thêm bản Postgres. Ghi theo từng lượt gọi model và từng lần chạy gate, không chỉ bản tổng kết cuối, để restart không mất số liệu chi phí | #9, #22 |
| Cấu hình theo repo (`RepoConfig`: repo, image, chế độ chạy, Sonar, ngân sách) tách khỏi `ProviderConfig`, truyền vào từ ngoài. Crate lõi không tự đọc biến môi trường hay file cấu hình toàn cục | #10, #11, #16, #24 |
| Future của vòng đời một issue phải `Send + 'static` để `tokio::spawn` được. Đã kiểm tra `run_loop` với `Provider` và `MaxModelCalls` thỏa điều kiện này. Bản cài `StopCheck` của router phải `Send + Sync` (không dùng `Rc`, `RefCell`). #16 thêm một test kiểm tra lúc biên dịch | #10, #16 |
| Giữ dispatch tĩnh. Các trait `ModelProvider`, `ToolExecutor` dùng `impl Future` trong trait nên không dùng được dạng `dyn`; chọn backend lúc chạy bằng enum (như `Provider`), không dùng `Box<dyn …>` | #8, #10 |
| Vòng đời issue nhận tín hiệu hủy từ ngoài và kiểm tra trước mỗi lượt gọi model và trong gate | #11, #16 |
| ID của job, issue, session không nằm trong BP1–BP2 (Thiết kế §4.4) | #13, #16 |

## Hệ quả

- Thêm crate `aegis-store` (thư viện) và `aegis-server` (binary); dashboard nằm trong `aegis-server`.
- Phụ thuộc mới khi làm Phase 2: `sqlx`, `axum`, `tower-http`, `askama`, `tokio-util`, `openidconnect`. Mỗi crate thêm vào phải qua `cargo deny` của chính aegis-code (#42). Allow-list cho repo đích (#12) là việc khác.
- CI cần một service PostgreSQL cho test tích hợp của `aegis-store` và `aegis-server`.
- Chỉ có một thành phần hạ tầng có trạng thái để vận hành (PostgreSQL). Đổi lại, hàng đợi tự làm phải có test cho lease, lấy lại job và giới hạn theo repo.
- Chạy lại từ đầu khi restart tốn thêm tiền cho job bị gián đoạn. Ledger ghi theo từng lượt nên đo được khoản này; nếu đáng kể thì làm checkpoint.
- Dữ liệu repo khách nằm trong `ap-northeast-1` từ thiết kế, không phụ thuộc cấu hình từng job.

## Phương án đã cân nhắc

| Phương án | Lý do không chọn |
| --- | --- |
| Redis / NATS / SQS làm hàng đợi | Thêm một hệ thống phải vận hành cho tải vài trăm job/ngày; tạo job, chống trùng và kiểm tra ngân sách không còn nằm trong một transaction. Xem lại khi tải lên hàng trăm job/phút hoặc cần nhiều vùng |
| Thư viện job queue (`apalis`, …) | Cần giới hạn theo repo, lease và kiểm tra ngân sách gắn với schema của mình; phần tự làm nhỏ. Xem lại nếu phần này phình ra |
| SQLite cho trạng thái platform | Ghi đồng thời từ nhiều worker và nhiều tiến trình kém; không có `SKIP LOCKED` |
| ORM (SeaORM, Diesel) | Schema nhỏ, truy vấn đơn giản; `sqlx` kiểm tra SQL lúc biên dịch là đủ |
| actix-web | Cả hai đều dùng được; axum xây trên tokio (dự án đã dùng) và dùng chung middleware của hệ `tower` |
| SPA (React/Vue) cho dashboard | Thêm ngôn ngữ, bộ build và lần triển khai thứ hai cho một UI chủ yếu là bảng và form |
| Kubernetes, mỗi job một Pod | Nặng cho team nhỏ; sandbox Docker trên VM đã đủ cách ly. Xem lại khi cần co giãn worker tự động |
| Checkpoint session sau mỗi lượt, chạy tiếp sau restart | `Session` là giá trị bất biến nên làm được, nhưng phải lưu cả trạng thái workspace và nhánh. Để sau khi ledger cho thấy chi phí chạy lại đáng kể |
| Một bản triển khai toàn cầu, tách dữ liệu Nhật theo bảng | Khó chứng minh dữ liệu không ra khỏi Nhật; log, cache, backup dễ lọt ra ngoài |
