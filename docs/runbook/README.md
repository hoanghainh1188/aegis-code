# Runbook vận hành aegis-server

Cho người vận hành bản triển khai trên AWS (#33, [ADR 0003](../adr/0003-platform-service-architecture.md) §6). Bốn việc thường làm: [triển khai](#1-triển-khai), [rollback](#2-rollback), [xoay secret](#3-xoay-secret), [xử lý sự cố](#4-xử-lý-sự-cố), [thêm repo](#6-thêm-repo). Metric, cảnh báo và Grafana: [giám sát](#5-giám-sát). Phần [chuẩn bị tài khoản](#0-chuẩn-bị-tài-khoản) chỉ làm một lần.

## Tổng quan

| Thành phần | Ở đâu |
| --- | --- |
| Image `aegis-server` (kèm `aegis-aci`, `codebase-memory-mcp`, `git`, client `docker`) | [docker/server/Dockerfile](../../docker/server/Dockerfile), ECR `aegis-server:<SHA commit>` |
| Image sandbox theo profile | `docker/<profile>/`, ECR `aegis-sandbox-<profile>:<SHA>`, trên máy đặt tên `aegis-sandbox-<profile>` |
| Hạ tầng | [deploy/tofu/](../../deploy/tofu/): module `modules/aegis`, môi trường `envs/staging`, `envs/customer-jp`, phần một lần `bootstrap` |
| CI/CD | [.github/workflows/deploy.yml](../../.github/workflows/deploy.yml) |
| Máy | Một EC2 (Amazon Linux 2023) chạy container `aegis` vai trò `all` qua systemd unit `aegis.service`; sau ALB có TLS |
| Cấu hình server | `/etc/aegis/server.toml` trên máy, OpenTofu viết từ [server.toml.tftpl](../../deploy/tofu/modules/aegis/server.toml.tftpl). Không có secret |
| Secret | Secrets Manager, tên `aegis/<môi trường>/<TÊN>`; OpenTofu chỉ tạo chỗ, giá trị do người vận hành đặt |
| Tag đang chạy | SSM parameter `/aegis/<môi trường>/image_tag` |
| Log | CloudWatch Logs `/aegis/<môi trường>/server` (cùng vùng) |
| Workspace job, cache build, image Docker | Volume EBS riêng gắn ở `/data`; workspace bind vào `/srv/aegis/.workspace` |
| Giám sát | EC2 thứ hai `aegis-<môi trường>-monitoring`: Prometheus, Alertmanager, Grafana; cảnh báo qua SNS `aegis-<môi trường>-alerts`. Xem [5. Giám sát](#5-giám-sát) |

**customer-jp:** mọi thứ trên (EC2, RDS, EBS, secret, log, ECR, state) ở `ap-northeast-1`. Vùng viết cứng trong `envs/customer-jp`, module kiểm lại; server từ chối khởi động với `customer_jp` nếu secret store không ở `ap-northeast-1`.

### Worker và Docker

Gate chạy mỗi bước trong một container sandbox (`docker run --network none --cap-drop ALL …`, Thiết kế §6–§7). Trên EC2, container `aegis` gọi **daemon Docker của chính máy** qua socket `/var/run/docker.sock` được mount vào, với `--group-add` group của socket (user trong image là uid 10001, không phải root). Sandbox vì thế là container anh em của `aegis`, không phải container lồng.

Hai điều kéo theo:

- **Đường dẫn workspace phải trùng** trong container và trên máy (`/srv/aegis/.workspace`): gate gửi đường dẫn của nó cho daemon khi mount `/work`. Sandbox chạy với uid 10001, nên thư mục này thuộc uid 10001.
- **Rủi ro:** ai điều khiển được socket Docker là root của máy. Một lỗi cho phép chèn tham số vào lệnh `docker run` của gate (ví dụ mount `/`) sẽ thành chiếm máy, kể cả role IAM của máy. Giảm thiểu đang có: gate dựng lệnh bằng mảng tham số, không qua shell; máy chỉ chạy aegis; IMDSv2 hop limit 1 nên container trên mạng bridge/none (sandbox, scanner Sonar) không lấy được credentials của máy, còn `aegis` chạy `--network host` để đọc được; role máy chỉ đọc secret `aegis/<môi trường>/*`, kéo ECR, ghi log; không mở SSH. Còn mở (issue riêng nếu cần): Docker rootless hoặc proxy socket lọc API, máy worker tách khỏi máy API.

## 0. Chuẩn bị tài khoản

Một lần cho mỗi tài khoản AWS, bằng quyền quản trị ngắn hạn (SSO), không tạo access key. Giả sử staging và customer-jp chung một tài khoản ở `ap-northeast-1`; tách tài khoản thì lặp lại cho tài khoản thứ hai và đặt biến theo GitHub environment.

1. **Bucket state** (bật version, mã hóa, chặn public):

   ```bash
   B=aegis-tofu-state-<account-id>
   aws s3api create-bucket --bucket $B --region ap-northeast-1 --create-bucket-configuration LocationConstraint=ap-northeast-1
   aws s3api put-bucket-versioning --bucket $B --versioning-configuration Status=Enabled
   aws s3api put-bucket-encryption --bucket $B --server-side-encryption-configuration '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'
   aws s3api put-public-access-block --bucket $B --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
   ```

   Khóa state là file `.tflock` trên chính bucket này (`use_lockfile`), không cần DynamoDB.

2. **OIDC, role CI, ECR:** trong `deploy/tofu/bootstrap`, chép `backend.hcl.example` → `backend.hcl` và tạo `bootstrap.tfvars` (xem `example.tfvars`), rồi:

   ```bash
   tofu init -backend-config=backend.hcl
   tofu apply -var-file=bootstrap.tfvars
   ```

   Ra ba role: `aegis-ci-plan` (PR và main, chỉ đọc), `aegis-ci-image` (main, đẩy ECR), `aegis-ci-apply` (chỉ job trong GitHub environment; không đọc hay ghi giá trị secret).

3. **GitHub** (Settings → Secrets and variables → Actions → Variables; không có secret AWS nào):
   - Biến repo: `AWS_PLAN_ROLE_ARN`, `AWS_IMAGE_ROLE_ARN`, `TF_STATE_BUCKET`, `TOFU_TFVARS_STAGING`, `TOFU_TFVARS_CUSTOMER_JP` (nội dung HCL như `envs/<env>/example.tfvars`, giá trị thật).
   - Environment `staging` và `customer-jp`: *Required reviewers*, *Deployment branches* chỉ `main`, biến `AWS_APPLY_ROLE_ARN`.

4. **Image đầu tiên:** merge vào `main` (hoặc chạy lại workflow trên main) để job `image` và `sandbox-images` đẩy `<SHA>` lên ECR.

5. **Hạ tầng:** Actions → Deploy → Run workflow, `environment=staging`, `action=apply`. Xem plan ở job `plan`, duyệt job `apply`. Máy được tạo nhưng chưa chạy server (tag `bootstrap`).

6. **DNS:** có `route53_zone_id` thì tự xong. Không có: lấy output `certificate_validation_records` và `alb_dns_name` (`tofu output` trong `envs/<env>`), thêm bản ghi xác thực và CNAME `domain_name` → ALB ở nhà cung cấp DNS; listener HTTPS chỉ tạo được khi chứng chỉ đã xác thực, nên chạy `apply` lại sau đó.

7. **User database và secret:** xem [3. Xoay secret](#3-xoay-secret) để đặt từng secret. Lần đầu tạo user `aegis` (server không dùng user master, vì RDS xoay mật khẩu master):

   ```bash
   # Máy của người vận hành: mở đường tới RDS qua máy aegis (SSM, không cần SSH)
   aws ssm start-session --target <instance_id> --document-name AWS-StartPortForwardingSessionToRemoteHost \
     --parameters host=<db host>,portNumber=5432,localPortNumber=15432
   # Cửa sổ khác: mật khẩu master do RDS giữ (output db_master_secret_arn)
   aws secretsmanager get-secret-value --secret-id <db_master_secret_arn> --query SecretString --output text
   PW=$(openssl rand -hex 24)
   psql "postgres://aegis_admin@localhost:15432/aegis?sslmode=require" \
     -v pw="$PW" -c "CREATE ROLE aegis LOGIN PASSWORD :'pw'" -c "GRANT aegis TO aegis_admin" -c "ALTER DATABASE aegis OWNER TO aegis"
   aws secretsmanager put-secret-value --secret-id aegis/staging/AEGIS_DATABASE_URL \
     --secret-string "postgres://aegis:$PW@<db host>:5432/aegis?sslmode=require"
   unset PW
   ```

8. **Chạy lần đầu:** [1. Triển khai](#1-triển-khai) với tag vừa đẩy. Migration chạy khi server khởi động.

9. Tạo quyền cho người dùng đầu tiên: `aws ssm start-session --target <instance_id>`, rồi `sudo docker exec aegis aegis-server --config /etc/aegis/server.toml grant <email> admin`; token cho dịch vụ: `… token <tên dịch vụ>` (các lệnh: [auth/cli.rs](../../crates/aegis-server/src/auth/cli.rs)).

10. Đưa repo đầu tiên vào dùng: [6. Thêm repo](#6-thêm-repo).

## 1. Triển khai

Mỗi commit vào `main` có image `aegis-server:<SHA>` và `aegis-sandbox-<profile>:<SHA>` trên ECR (tag không ghi đè được).

1. Kiểm migration của bản mới theo kiểu expand/contract (dưới đây). Có migration thì xem trước trên staging.
2. Actions → Deploy → Run workflow: `environment`, `action=deploy`, `image_tag=<SHA>`. Duyệt job.
3. Job kiểm image có trên ECR, ghi tag vào `/aegis/<env>/image_tag`, gọi `aegis-deploy <SHA>` trên máy qua SSM. Script kéo image server và sandbox, `systemctl restart aegis`, chờ `/api/v1/health` trả 200 (tối đa 5 phút). Phần tóm tắt của job ghi **tag trước** để rollback.
4. Restart dừng container cũ bằng SIGTERM: API thôi nhận request, worker không nhận job mới và chờ job đang chạy tối đa `worker.drain_s` (mặc định 600 s, `docker stop` chờ thêm 60 s); job chưa xong được trả về hàng đợi và chạy lại sau. Muốn không cắt job thì triển khai lúc ít job chạy (dashboard).
5. Thứ tự: staging trước, chạy thử một issue, rồi customer-jp cùng tag.

Đổi hạ tầng (instance type, RDS, server.toml…): sửa `deploy/tofu`, mở PR (CI chạy `fmt`, `tflint`, `validate`, `plan` offline), merge, rồi `action=apply`. Đổi user_data làm **thay máy** (plan ghi `must be replaced`): workspace và image Docker ở volume EBS riêng nên còn nguyên, nhưng job đang chạy bị cắt.

### Migration: expand/contract

Rollback chỉ đổi image, không lùi DB. Vì thế mỗi migration phải chạy được với **cả bản trước lẫn bản mới**:

- **Expand** (một bản phát hành): thêm bảng, cột cho phép NULL hoặc có default, index (`CONCURRENTLY` nếu bảng lớn). Code mới ghi cả cột cũ lẫn mới nếu cần.
- **Contract** (bản phát hành sau, khi không còn rollback về bản trước expand): xóa cột/bảng cũ, thêm `NOT NULL`.
- Không đổi tên hay đổi kiểu cột trong một bước; không xóa thứ bản trước còn đọc.
- Server bỏ qua các migration đã áp mà nó không biết (#176), nên image cũ vẫn khởi động trên DB mà bản mới đã expand. Migration nó biết mà bị sửa (checksum khác) thì vẫn dừng với `VersionMismatch`: không sửa một migration đã phát hành, viết migration mới. Đừng xóa dòng trong `_sqlx_migrations`.

## 2. Rollback

Bản mới lỗi (health không lên, lỗi nhiều trong log, hành vi sai):

1. Lấy tag trước: phần tóm tắt của lần deploy vừa rồi, hoặc lịch sử parameter: `aws ssm get-parameter-history --name /aegis/<env>/image_tag --query 'Parameters[].[LastModifiedDate,Value]' --output text`.
2. Actions → Deploy: `action=deploy`, `image_tag=<tag trước>`. Giống hệt triển khai.
3. Workflow không chạy được (GitHub sự cố): `aws ssm start-session --target <instance_id>`, rồi `sudo /usr/local/bin/aegis-deploy <tag trước>` và `aws ssm put-parameter --name /aegis/<env>/image_tag --value <tag trước> --overwrite` để lần khởi động sau dùng đúng tag.
4. DB không lùi (expand/contract ở trên); tag trước vẫn chạy trên DB có migration mới hơn nó. Migration của bản lỗi làm hỏng dữ liệu: dừng server (`sudo systemctl stop aegis`), khôi phục RDS về thời điểm trước deploy (point-in-time restore ra instance mới, rồi đổi host trong `AEGIS_DATABASE_URL`), mở issue.
5. Hạ tầng: revert commit trong `deploy/tofu`, merge, `action=apply`.

## 3. Xoay secret

Server đọc lại mọi secret mỗi `refresh_s` (300 s), và đọc lại ngay secret bị từ chối (API model trả 401, webhook sai chữ ký). Không cần restart; lần gọi đang chạy giữ giá trị cũ. Đặt giá trị bằng file để không lộ trong lịch sử shell:

```bash
aws secretsmanager put-secret-value --secret-id aegis/<env>/<TÊN> --secret-string file:///dev/stdin <<< "$VALUE"
```

| Secret | Cách xoay |
| --- | --- |
| `ANTHROPIC_API_KEY` | Tạo key mới ở console Anthropic → đặt → chờ 5 phút (hoặc lần 401 đầu) → thu hồi key cũ |
| `AEGIS_DATABASE_URL` | `ALTER ROLE aegis PASSWORD …` (qua port-forward ở mục 0) → đặt URL mới ngay. Kết nối đang mở vẫn dùng được; kết nối mới dùng URL mới. Giữa hai bước kết nối mới lỗi tối đa 5 phút: làm lúc vắng, hoặc tạo `aegis2`, đổi URL, rồi bỏ user cũ |
| `AEGIS_SESSION_KEY` (≥ 32 byte, `openssl rand -hex 32`) | Đặt giá trị mới. Phiên đăng nhập và form review đang mở hết hiệu lực; người dùng đăng nhập lại |
| `AEGIS_OIDC_CLIENT_SECRET` | Tạo secret mới ở SSO (giữ cả hai) → đặt → thu hồi cái cũ |
| `AEGIS_GITHUB_WEBHOOK_SECRET`, `AEGIS_GITLAB_WEBHOOK_TOKEN` | Đặt giá trị mới vào store trước, rồi đổi ở cài đặt webhook. Request giữa hai bước sai chữ ký → server đọc lại secret; GitHub/GitLab gửi lại được từ trang webhook |
| `AEGIS_GITHUB_COMMENT_TOKEN`, `AEGIS_GITLAB_COMMENT_TOKEN`, token Git/Sonar/Jira/Backlog của repo (`token_env` trong cài đặt repo) | Tạo token mới → đặt → thu hồi cái cũ |

Lộ secret: thu hồi ở nơi cấp **trước**, rồi đặt giá trị mới. Kiểm CloudTrail ai đã `GetSecretValue`. Mật khẩu master RDS do RDS tự xoay; server không dùng nó.

Thêm secret mới cho repo: thêm tên vào `repo_secret_names` trong `TOFU_TFVARS_<ENV>`, `action=apply`, rồi đặt giá trị.

## 4. Xử lý sự cố

Vào máy: `aws ssm start-session --target <instance_id>`. Log: CloudWatch Logs `/aegis/<env>/server` (JSON; lọc `{ $.level = "ERROR" }`), trên máy `sudo journalctl -u aegis` (systemd, script deploy). Trạng thái: `systemctl status aegis`, `sudo docker ps`. Metric, cảnh báo và dashboard: [5. Giám sát](#5-giám-sát).

### Server không lên

- `journalctl -u aegis -n 100`. Server dừng khi thiếu secret bắt buộc và **nêu tên secret**: đặt nó (mục 3), `sudo systemctl restart aegis`.
- `AccessDenied` đọc secret: tên phải là `aegis/<env>/<TÊN>`; role máy chỉ đọc tiền tố đó.
- Không kéo được image: tag có trên ECR không (`aws ecr describe-images --repository-name aegis-server`), ECR còn giữ không (giữ 30 bản gần nhất).
- `permission denied … docker.sock` trong log job: `/etc/aegis/docker.env` sai group, sửa theo `getent group docker`.

### Provider (API model) lỗi kéo dài

Dấu hiệu: nhiều run lỗi với thông báo từ provider (5xx, 429, timeout), job hết lượt thử hạ tầng (3 lần, chờ 30 s rồi gấp đôi).

1. Xem trang trạng thái của nhà cung cấp; với customer-jp xem Bedrock ở `ap-northeast-1` (Service Health). Không chuyển customer-jp sang vùng khác: dữ liệu chỉ ở Nhật.
2. 401/403: key bị thu hồi hoặc hết hạn → xoay (mục 3). Bedrock `AccessDenied`: model chưa được bật trong tài khoản, hoặc policy `BedrockInJapan` của role máy.
3. 429 kéo dài: giảm `max_workers` (`server_config` trong tfvars, `apply`), hoặc xin tăng hạn mức.
4. Sự cố dài: tạm dừng worker mà vẫn giữ API, webhook và dashboard; job mới nằm chờ trong hàng đợi:

   ```bash
   sudo sed -i 's/--role all/--role api/' /etc/systemd/system/aegis.service
   sudo systemctl daemon-reload && sudo systemctl restart aegis
   # Khi provider ổn lại: đổi ngược `--role api` → `--role all`, daemon-reload, restart
   ```

5. Job đã hỏng vì sự cố: chạy lại bằng `POST /api/v1/jobs/{id}/retry`.

### Hàng đợi dồn

Dấu hiệu: nhiều job `queued` lâu (dashboard, `GET /api/v1/jobs?status=queued`).

1. Worker còn chạy không: log có `worker started`; `sudo docker ps`. Không thấy job nào `running` → worker chết hoặc đang chạy `--role api` (mục trên).
2. Job `running` treo: lease hết hạn sau `lease_s` (60 s) thì job được trả về hàng đợi. Container sandbox treo: `sudo docker ps --filter name=aegis-gate`, `sudo docker kill <tên>`.
3. Bị giới hạn: `max_workers` của máy, `repos.max_concurrent_jobs` của từng repo. Một repo chiếm hàng đợi: hủy job thừa (`POST /api/v1/jobs/{id}/cancel`), kiểm webhook có gửi lặp không.
4. Máy hết sức (CPU/RAM vì test của repo): `top`, `sudo docker stats`. Tăng `instance_type` (thay máy, mục 1) hoặc giảm `max_workers`.
5. Đĩa workspace đầy: `df -h /data`. Dọn image cũ `sudo docker image prune -a --filter until=168h` (giữ image đang chạy), xóa workspace của job đã xong trong `/srv/aegis/.workspace` (job đang chạy: xem `GET /api/v1/jobs?status=running`), hoặc tăng `workspace_volume_gb` (`apply`, rồi `sudo xfs_growfs /data`).

### DB đầy

Dấu hiệu: lỗi ghi (`could not extend file`, `No space left`), RDS báo `storage-full`, server lỗi khi tạo job.

1. RDS tự tăng dung lượng tới `db_max_storage_gb` (mặc định 200 GB). Chạm trần: tăng `db_max_storage_gb` (hoặc `db_allocated_storage_gb`) trong tfvars, `apply`. Sau một lần tăng, RDS chờ 6 giờ mới tăng tiếp.
2. Tìm bảng lớn (port-forward ở mục 0, user `aegis`):

   ```sql
   SELECT relname, pg_size_pretty(pg_total_relation_size(oid)) FROM pg_class
   WHERE relkind = 'r' ORDER BY pg_total_relation_size(oid) DESC LIMIT 10;
   ```

3. Không xóa tay `audit_log` (append-only có trigger) hay ledger (dữ liệu KPI). Dọn dữ liệu cũ cần chính sách giữ dữ liệu: mở issue.
4. `VACUUM` bị chặn bởi transaction treo: `SELECT pid, state, xact_start FROM pg_stat_activity ORDER BY xact_start`; `SELECT pg_terminate_backend(<pid>)` cho transaction treo lâu.

## 5. Giám sát

Mỗi bản triển khai có một máy giám sát riêng, cùng VPC và cùng vùng (`deploy/tofu/modules/aegis/monitoring.tf`; tắt bằng `monitoring = { enabled = false }`). Máy chạy ba container `--network host`, mỗi cái là một systemd unit cùng tên, và **chỉ nghe trên 127.0.0.1**. Security group của máy không mở cổng nào.

| Thành phần | Chi tiết |
| --- | --- |
| Prometheus (`prometheus`) | Tìm máy server qua EC2 service discovery (tag `Name=aegis-<env>-server`, đang chạy) và scrape `/metrics` ở cổng 9464 mỗi 30 s. Server nghe metrics trên `0.0.0.0:9464`; security group của server chỉ cho máy giám sát vào cổng này, ALB không chuyển cổng này ra ngoài. Nạp `deploy/alerts/aegis.rules.yml`, giữ dữ liệu `retention_days` (30) ngày |
| Alertmanager (`alertmanager`) | Gửi mọi cảnh báo, gom theo `alertname` và `repo`, lên SNS topic `aegis-<env>-alerts` (output `alerts_topic_arn`), nhắc lại mỗi 4 giờ khi còn bắn |
| Grafana (`grafana`) | Datasource Prometheus và dashboard `deploy/grafana/*.json` (thư mục `aegis`), nạp từ file |
| Cấu hình | Bucket `aegis-<env>-monitoring-<account>`, OpenTofu viết từ repo. Timer `aegis-monitoring-sync` kéo về `/etc/aegis-monitoring` mỗi 5 phút; có thay đổi thì reload Prometheus và Alertmanager |
| Dữ liệu | Volume EBS riêng gắn ở `/data` (TSDB, trạng thái silence, CSDL Grafana): còn nguyên khi thay máy |

**Mở Grafana** (từ máy của người vận hành, cần quyền SSM như khi vào máy server):

```bash
aws ssm start-session --target <monitoring_instance_id> \
  --document-name AWS-StartPortForwardingSession --parameters portNumber=3000,localPortNumber=3000
# Lần đầu, cửa sổ khác: mật khẩu user admin, sinh khi tạo máy
aws ssm start-session --target <monitoring_instance_id>   # rồi: sudo cat /data/grafana-admin-password
```

Mở http://localhost:3000, user `admin`. Prometheus (cổng 9090) và Alertmanager (cổng 9093) mở cùng cách. Tạm im một cảnh báo (bảo trì): silence ở trang Alertmanager.

**Nhận cảnh báo:** thêm email vào `monitoring.alert_emails` trong `TOFU_TFVARS_<ENV>`, `action=apply`, rồi bấm link xác nhận trong email AWS gửi tới. Kênh khác (Slack qua AWS Chatbot, PagerDuty…) đăng ký vào topic bằng tay. Với customer-jp, nội dung cảnh báo có tên repo khách: chỉ gửi cho người được xem repo đó.

**Đổi luật hoặc dashboard:** sửa `deploy/alerts/aegis.rules.yml` (kèm test trong `aegis.rules.test.yml`, CI chạy `promtool`) hoặc `deploy/grafana/*.json`, merge, `action=apply`. Máy nhận trong vòng 5 phút; không thay máy. Sửa dashboard trên Grafana không được lưu: xuất JSON rồi commit. Đổi phiên bản image (`monitoring.*_version`) thì thay máy; dữ liệu ở `/data` còn nguyên.

| Cảnh báo | Mức | Xem |
| --- | --- | --- |
| `AegisServerDown` | critical | [Server không lên](#server-không-lên); máy dừng, hoặc security group chặn cổng 9464 |
| `AegisProviderErrorsSustained` | critical | [Provider lỗi kéo dài](#provider-api-model-lỗi-kéo-dài) |
| `AegisQueueBacklog` | warning | [Hàng đợi dồn](#hàng-đợi-dồn) |
| `AegisCacheReadRatioLow` | warning | Prompt đổi thứ tự hoặc cache breakpoint sai: so bản mới với bản trước (deploy gần nhất), xem tỉ lệ cache theo repo trên dashboard |

**Máy giám sát lỗi:** `aws ssm start-session --target <monitoring_instance_id>`, rồi `systemctl status prometheus alertmanager grafana aegis-monitoring-sync.timer` và `sudo journalctl -u aegis-monitoring-sync`. Target `aegis` ở trang Targets của Prometheus báo `down` mà server vẫn chạy: kiểm security group cổng 9464 và `[metrics] listen` trong `/etc/aegis/server.toml`. Không thấy target nào: tag `Name` của máy server, quyền `ec2:DescribeInstances` của role `aegis-<env>-monitoring`. Máy giám sát chết thì không có cảnh báo nào; hiện chưa có gì canh chính nó (CloudWatch alarm trên status check của máy là bước tiếp theo nếu cần).

## 6. Thêm repo

Đưa một repo GitHub hoặc GitLab vào dùng, từ chưa có gì tới PR đầu tiên. Lệnh `aegis-server …` dưới đây chạy trên máy như ở [mục 0, bước 9](#0-chuẩn-bị-tài-khoản): `sudo docker exec aegis aegis-server --config /etc/aegis/server.toml <lệnh>`, viết tắt `… <lệnh>`.

### Chuẩn bị

1. **Repo đích của PR (`fork`).** Agent đẩy nhánh `agent/*` và mở PR **trong repo `fork.owner/fork.name`**, vào nhánh chính của nó. Hai cách:
   - Fork của một tài khoản bot: người bảo trì kéo PR từ fork về repo gốc. Bot không cần quyền ghi repo gốc.
   - Chính repo gốc (`fork` = owner/name của repo): PR mở thẳng ở đó. Bot cần quyền ghi, chỉ trên nhánh `agent/*` nếu có branch protection.

   Fork cần nhánh chính trùng tên với `default_branch` và được cập nhật theo repo gốc (GitHub: *Sync fork*, hoặc Action đồng bộ). PR mở dưới dạng draft khi `[run] draft_prs = true` (mặc định) trong cấu hình mode.
2. **Token của fork** (`fork.token_env`, tên một secret). Fine-grained PAT của tài khoản bot:
   - Trên repo đích: *Contents* đọc/ghi, *Pull requests* đọc/ghi.
   - Trên repo gốc: *Issues* đọc (job đọc issue bằng token này), *Actions* đọc (job sửa CI đọc lần chạy thất bại và log, #250). Repo private thì thêm *Contents* đọc: server clone và fetch bằng token này.

   Đặt giá trị như [mục 3](#3-xoay-secret); tên mới thì thêm vào `repo_secret_names` trước.

   **Fork trên GitLab** (#252): `[settings.fork]` thêm `platform = "gitlab"` và `api` là API root của instance (`https://gitlab.com/api/v4` hoặc tự host); `owner` là namespace (có thể có subgroup), `name` là project. Agent đẩy nhánh `agent/*` qua HTTPS (token gửi với user `oauth2`, trong header) và mở **merge request** (draft khi `draft_prs`). Token (project hoặc personal access token) cần scope `api` (đọc issue, mở MR) và `write_repository`; repo private thì thêm `read_repository`. Issue GitLab (nhãn `aegis`, webhook ở mục 10) và pipeline GitLab hỏng trên nhánh chính (job sửa CI, #253: agent đọc các job hỏng và cuối log của chúng) chỉ chạy được trên repo có fork GitLab. Vòng sửa theo review trên MR (#254): comment có một dòng `/aegis fix` trên MR còn mở của agent, của người có quyền Developer trở lên trong project của repo, tạo vòng sửa như trên GitHub (webhook bật thêm **Comments**; server kiểm quyền bằng `AEGIS_GITLAB_COMMENT_TOKEN`, nên token này cần đọc được thành viên project). Agent chỉ đọc note của Developer trở lên, bỏ note hệ thống, bot và trả lời của chính nó.
3. **Comment lên issue** (một lần cho cả server): secret `AEGIS_GITHUB_COMMENT_TOKEN`, token có quyền *Issues* ghi trên các repo gốc; thiếu thì job vẫn chạy nhưng không comment. GitLab: `AEGIS_GITLAB_COMMENT_TOKEN`. GitHub Enterprise hay GitLab tự host: biến `AEGIS_GITHUB_API` / `AEGIS_GITLAB_API` (không phải secret) của container.

### Đăng ký và clone

4. Viết file repo, ví dụ `typst.toml` (mọi khóa của `[settings]`: `RepoSettings` trong [registry.rs](../../crates/aegis-lifecycle/src/registry.rs); khóa lạ bị từ chối):

   ```toml
   slug = "typst"                         # tên trong đường dẫn, nhánh, dashboard
   url = "https://github.com/typst/typst" # repo gốc: webhook khớp theo URL này
   default_branch = "main"
   language = "rust"                      # python, rust, go, typescript, java, csharp
   mode = "poc"                           # hoặc customer-jp
   max_concurrent_jobs = 1

   [settings]
   # Quy ước riêng của repo, thêm vào prompt sau quy ước của ngôn ngữ.
   conventions = """
   Tests for a crate go in its tests/ directory.
   Run `cargo test -p <crate>` for one crate.
   """

   [settings.fork]
   owner = "acme-aegis-bot"
   name = "typst"
   token_env = "AEGIS_TOKEN_TYPST"
   ```

   Chép file vào workspace trên máy (ví dụ `/srv/aegis/.workspace/repo-files/typst.toml`; đường dẫn trong container giống hệt), rồi `… repo add /srv/aegis/.workspace/repo-files/typst.toml`. Lệnh:
   - kiểm cài đặt theo mode trước khi ghi (repo `customer-jp` cần mode `customer-jp` hợp lệ);
   - clone repo vào `<state_dir>/repos/<slug>` (repo private: bằng token của fork);
   - ghi audit `repo.register`, in id của repo.

   Worker fetch nhánh chính trước mỗi job, nên không cần cập nhật clone bằng tay.
5. Sửa sau này: `… repo show typst > typst.json`, sửa, `… repo update typst.json` (slug giữ nguyên). `… repo list` liệt kê các repo.
6. **`conventions`:** viết những gì một người mới cần biết để chạy **một** test của repo và test mới đặt ở đâu. Quy ước chung của ngôn ngữ (cách đặt id test tái hiện) đã có trong `prompts/conventions/`; ở đây chỉ thêm cái riêng của repo. Thiếu hướng dẫn, agent dễ chạy cả bộ test hoặc báo id test sai dạng và hết ngân sách vô ích.

### Quyền và ngân sách

7. Cấp quyền trên dashboard: `… grant <email> typst maintainer|reviewer|viewer` (maintainer: chạy, hủy job, đặt ngân sách; reviewer: chấm PR; viewer: xem).
8. Ngân sách tháng (khuyến nghị đặt ngay): bằng token dịch vụ có vai trò maintainer trên repo (`… token ops`, `… grant ops typst maintainer`):

   ```bash
   curl -X PUT https://<domain>/api/v1/repos/<id>/budget -H "Authorization: Bearer $TOKEN" \
     -H 'content-type: application/json' -d '{"monthly_usd": 50}'
   ```

   Hết ngân sách thì job mới bị hủy kèm lý do (comment lên issue). Trần mỗi job: `[worker] max_cost_usd` của server, hoặc `[settings.budget]` của repo.

### Webhook

9. **GitHub:** repo gốc → Settings → Webhooks → Add webhook:
   - *Payload URL* `https://<domain>/webhooks/github`, *Content type* `application/json`;
   - *Secret*: giá trị của secret `AEGIS_GITHUB_WEBHOOK_SECRET` (một giá trị cho mọi repo);
   - *Let me select individual events*: **Issues** và **Workflow runs**.

   Gắn nhãn `aegis` (biến `AEGIS_WEBHOOK_LABEL` đổi được) lên một issue tạo job; một lần chạy CI thất bại trên nhánh chính của chính repo (không phải PR) tạo job sửa CI: agent đọc job, bước hỏng và cuối log của lần chạy, sửa trên đầu nhánh chính rồi mở PR như issue thường (#250). Lần chạy đã qua khi job bắt đầu thì job dừng với lý do.

   **Vòng sửa theo review** (#243, #244): trên **repo chứa PR của agent** (repo `fork`; nếu khác repo gốc thì thêm webhook như trên ở đó), chọn thêm **Pull request reviews**, **Pull request review comments** và **Issue comments**. Review "Request changes", hoặc comment có một dòng `/aegis fix`, trên PR còn mở của agent tạo một job `review`: agent sửa tiếp trên nhánh của PR, chạy lại gate, đẩy thêm một commit (không force) và trả lời từng ý.
   - Chỉ người có quyền ghi trên repo gốc kích hoạt được. Server hỏi GitHub bằng `AEGIS_GITHUB_COMMENT_TOKEN`, nên token này cần xem được collaborator của repo gốc. Bot và người khác: bỏ qua.
   - Mỗi PR tối đa `[reviews] max_rounds` vòng (mặc định 3), mỗi vòng có ngân sách riêng. Vượt, hoặc hết ngân sách tháng: server comment lý do lên PR. Đang có vòng chạy thì review mới được đọc trong vòng đó.
   - Maintainer cũng xin được một vòng qua API: `POST /api/v1/jobs/<id của job đã mở PR>/review`.
10. **GitLab:** project → Settings → Webhooks: URL `https://<domain>/webhooks/gitlab`, *Secret token* = `AEGIS_GITLAB_WEBHOOK_TOKEN`, sự kiện **Issues events**, **Pipeline events** và **Comments** (vòng sửa trên MR, #254).

Endpoint của nền tảng nào chưa có secret trong store thì trả 404. Webhook khớp repo theo URL (`html_url`/`web_url`, không phân biệt hoa thường, bỏ `.git`): `url` trong file repo phải là URL của repo gốc.

### Kiểm tra

11. Mở một issue nhỏ, rõ ràng (một lỗi có cách tái hiện), gắn nhãn `aegis`. Trong vài giây:
    - trang webhook của GitHub ghi lần gửi trả `202`;
    - dashboard (`https://<domain>/`) có job mới của repo, chuyển `running`; log có `claimed` với `job_id`.
12. Khi job xong: issue có comment kết quả; có PR thì PR nằm ở repo `fork`, nhánh `agent/…`. Chấm PR trên dashboard (trang job → review).
13. Không thấy job: xem trang webhook và log (chữ ký sai → `401`; repo không khớp URL → `200` với `"status": "ignored"`, log `webhook delivery ignored` kèm `repository not registered`), rồi `… repo list`. Job dừng sớm: lý do ở trang job; `rejected` thường là thiếu secret (`no secret …`) hoặc cài đặt repo; `updating the clone` là URL hoặc token không đọc được repo.

### Jira/Backlog và Sonar

#### Nhận ticket Jira/Backlog (#26)

Thêm mục `tracker` vào `[settings]` của file repo (cùng chỗ với `fork`, `sonar`) rồi `repo update`. Token không nằm ở đây: `token_env` là tên secret, đặt giá trị như mục 3 và thêm tên vào `repo_secret_names`.

```json
{ "kind": "jira", "url": "https://acme.atlassian.net", "user": "bot@acme.com",
  "token_env": "AEGIS_JIRA_TOKEN",
  "jql": "project = APP AND labels = aegis AND statusCategory = \"To Do\"",
  "pr_transition": "In Review" }
```

```json
{ "kind": "backlog", "url": "https://acme.backlog.jp", "token_env": "AEGIS_BACKLOG_API_KEY",
  "project_ids": [12], "status_ids": [1], "keyword": "aegis", "pr_status_id": 3 }
```

- Jira Cloud: `user` là e-mail của tài khoản, token là API token của nó. Jira Data Center/Server: `"deployment": "data-center"`, bỏ `user`, token là personal access token.
- Backlog: API key của một tài khoản bot (Cá nhân → API). Lọc theo project, trạng thái (`status_ids`), loại (`issue_type_ids`), danh mục (`category_ids`), từ khóa.
- Tài khoản cần quyền đọc ticket và file đính kèm, viết comment; với `pr_transition`/`pr_status_id`, quyền chuyển trạng thái; Jira thêm quyền tạo link.
- Vai trò `api` poll mỗi `[trackers] poll_s` (300 s), một tiến trình mỗi kỳ. Mỗi ticket chỉ tạo job một lần; muốn chạy lại thì retry job qua API. Job đọc mô tả và tối đa 3 file log dạng text (≤ 1 MiB, giữ 6 000 ký tự cuối). Khi job xong: comment trạng thái; có PR thì gắn link PR (Jira) và chuyển trạng thái nếu đã cấu hình.
- Tracker lỗi (token sai, mạng): log `polling the tracker` / `tracker not polled` theo repo, các repo khác vẫn chạy.

#### Kéo cảnh báo Sonar định kỳ (#27)

Repo đã có `sonar` (SonarQube Server hoặc Cloud, như bước Sonar của gate) thì thêm `sonar_poll` vào `[settings]` của file repo (`repo update`) để server tự kéo cảnh báo mới trên nhánh chính và gom thành batch:

```json
{ "every_s": 3600, "min_severity": "MAJOR", "max_warnings": 50 }
```

- `every_s` (≥ 60, mặc định 3600): lịch riêng của repo. Vai trò `api` xem mỗi `[sonar] check_s` (60 s) repo nào đến hạn, một tiến trình mỗi kỳ.
- `min_severity` (`INFO`, `MINOR`, `MAJOR`, `CRITICAL`, `BLOCKER`; mặc định `MAJOR`): cảnh báo dưới mức này bị bỏ qua. `max_warnings` (1–100, mặc định 50): số cảnh báo mới nhất một batch nhận; phần còn lại vào lần kéo sau.
- Mỗi cảnh báo chỉ vào một batch (bảng `sonar_warnings`), kể cả khi vẫn còn mở. Job `sonar` đọc lại các cảnh báo của batch theo key lúc bắt đầu, chỉ lấy cái còn mở, rồi đi luồng batch (#17) và mở một PR. Muốn chạy lại thì retry job qua API.
- Token là secret mà `sonar.token_env` chỉ tới (cùng secret với bước Sonar của gate), cần quyền Browse project. Sonar lỗi: log `polling Sonar` / `Sonar not polled` theo repo, các repo khác vẫn chạy.
