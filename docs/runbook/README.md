# Runbook vận hành aegis-server

Cho người vận hành bản triển khai trên AWS (#33, [ADR 0003](../adr/0003-platform-service-architecture.md) §6). Bốn việc thường làm: [triển khai](#1-triển-khai), [rollback](#2-rollback), [xoay secret](#3-xoay-secret), [xử lý sự cố](#4-xử-lý-sự-cố). Phần [chuẩn bị tài khoản](#0-chuẩn-bị-tài-khoản) chỉ làm một lần.

## Tổng quan

| Thành phần | Ở đâu |
| --- | --- |
| Image `aegis-server` (kèm `aegis-aci`, `git`, client `docker`) | [docker/server/Dockerfile](../../docker/server/Dockerfile), ECR `aegis-server:<SHA commit>` |
| Image sandbox theo profile | `docker/<profile>/`, ECR `aegis-sandbox-<profile>:<SHA>`, trên máy đặt tên `aegis-sandbox-<profile>` |
| Hạ tầng | [deploy/tofu/](../../deploy/tofu/): module `modules/aegis`, môi trường `envs/staging`, `envs/customer-jp`, phần một lần `bootstrap` |
| CI/CD | [.github/workflows/deploy.yml](../../.github/workflows/deploy.yml) |
| Máy | Một EC2 (Amazon Linux 2023) chạy container `aegis` vai trò `all` qua systemd unit `aegis.service`; sau ALB có TLS |
| Cấu hình server | `/etc/aegis/server.toml` trên máy, OpenTofu viết từ [server.toml.tftpl](../../deploy/tofu/modules/aegis/server.toml.tftpl). Không có secret |
| Secret | Secrets Manager, tên `aegis/<môi trường>/<TÊN>`; OpenTofu chỉ tạo chỗ, giá trị do người vận hành đặt |
| Tag đang chạy | SSM parameter `/aegis/<môi trường>/image_tag` |
| Log | CloudWatch Logs `/aegis/<môi trường>/server` (cùng vùng) |
| Workspace job, cache build, image Docker | Volume EBS riêng gắn ở `/data`; workspace bind vào `/srv/aegis/.workspace` |

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
| `AEGIS_GITHUB_COMMENT_TOKEN`, token Git/Sonar của repo | Tạo token mới → đặt → thu hồi cái cũ |

Lộ secret: thu hồi ở nơi cấp **trước**, rồi đặt giá trị mới. Kiểm CloudTrail ai đã `GetSecretValue`. Mật khẩu master RDS do RDS tự xoay; server không dùng nó.

Thêm secret mới cho repo: thêm tên vào `repo_secret_names` trong `TOFU_TFVARS_<ENV>`, `action=apply`, rồi đặt giá trị.

## 4. Xử lý sự cố

Vào máy: `aws ssm start-session --target <instance_id>`. Log: CloudWatch Logs `/aegis/<env>/server` (JSON; lọc `{ $.level = "ERROR" }`), trên máy `sudo journalctl -u aegis` (systemd, script deploy). Trạng thái: `systemctl status aegis`, `sudo docker ps`. Metric và cảnh báo: #32.

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
