# Runbook on-premise

Cho người vận hành aegis trên máy của mình, không dùng AWS ([ADR 0007](../adr/0007-on-premise-deployment.md), #262). Bản trên AWS: [README.md](README.md).

Bốn việc: [dựng](#1-dựng), [đăng ký repo](#2-đăng-ký-repo), [nâng cấp và rollback](#3-nâng-cấp-và-rollback), [sao lưu và khôi phục](#4-sao-lưu-và-khôi-phục). Có hai cách dựng; mặc định là **Docker Compose**. Cách **systemd** ở [mục 5](#5-systemd-thay-cho-compose).

## Cần có

| | |
| --- | --- |
| Máy | Linux x86_64 hoặc arm64. Từ 4 vCPU, 16 GB RAM; đĩa 100 GB, vì image sandbox, clone và cache build đều nằm trên máy. Máy chỉ nên chạy aegis |
| Docker | Engine 24+ và plugin Compose v2 |
| Mạng ra ngoài | Tới forge (GitHub, GitLab) và API model (Claude API, Bedrock), trừ khi model là local. Không ra được Internet thì dùng [gói offline](../release.md#máy-không-ra-được-internet) |
| Vào | Cổng 8080 sau reverse proxy TLS của bạn: dashboard, API, webhook |

**Rủi ro:** container `aegis` nắm socket Docker để chạy sandbox của gate, tức nắm root của máy ([README](README.md#worker-và-docker), "Worker và Docker").

## 1. Dựng

Làm từ một bản checkout của repo, ở tag của phiên bản (`git checkout vX.Y.Z`), trong `deploy/compose/`:

1. Chuẩn bị máy, một lần:

   ```bash
   sudo ./install.sh
   ```

   Script không ghi đè gì đã có. Nó tạo:
   - `/srv/aegis/.workspace`, thuộc uid 10001;
   - thư mục `./secrets`. Mật khẩu và URL database, session key và secret webhook GitHub được sinh sẵn;
   - file `.env`, chứa group của socket Docker và danh sách sandbox cần kéo;
   - file `server.toml` (chép từ `server.toml.example`).
2. Sửa `server.toml`:
   - `public_url`: địa chỉ người dùng và webhook đi tới;
   - `[auth.oidc]`: SSO của bạn;
   - `[modes]`: mode cho các repo. Model local thì viết file mode vào `./modes/local.toml`, theo mẫu [config/local.toml](../../config/local.toml), rồi đặt `poc = "/etc/aegis/modes/local.toml"`.
3. Thêm secret, mỗi secret một file trong `./secrets`, quyền `0400`, thuộc uid 10001 ([xoay secret](README.md#3-xoay-secret)):
   - `ANTHROPIC_API_KEY`: khóa của model (mode `poc`); model local thì không cần;
   - `AEGIS_OIDC_CLIENT_SECRET`;
   - `AEGIS_GITHUB_COMMENT_TOKEN`;
   - token fork của từng repo, tên đặt ở `token_env` của repo.

   ```bash
   sudo sh -c 'umask 077; printf "%s\n" "$VALUE" > secrets/ANTHROPIC_API_KEY; chown 10001:10001 secrets/ANTHROPIC_API_KEY'
   ```

4. Chạy một phiên bản:

   ```bash
   sudo ./aegis-deploy.sh X.Y.Z             # kéo image từ GHCR
   sudo ./aegis-deploy.sh --offline X.Y.Z   # image đã nạp từ gói offline (load-images.sh)
   ```

   Script kéo image server và sandbox (hoặc kiểm chúng đã được nạp), gắn tên trần cho sandbox, ghi `AEGIS_VERSION` vào `.env`, `docker compose up`, rồi chờ healthcheck. Server tự chạy migration khi khởi động.
5. Kiểm tra:
   - `curl http://127.0.0.1:8080/api/v1/health`;
   - mở dashboard, đăng nhập SSO;
   - cấp vai trò admin đầu tiên:

   ```bash
   docker compose exec aegis aegis-server --config /etc/aegis/server.toml grant you@example.com admin
   ```

`/metrics` chỉ mở trên `127.0.0.1:9464` của máy. Prometheus của bạn đọc nó ở đó; quy tắc cảnh báo dùng lại [deploy/alerts](../../deploy/alerts/).

## 2. Đăng ký repo

Như [mục 6 của runbook AWS](README.md#6-thêm-repo). Lệnh `…` ở đó trở thành `docker compose exec aegis aegis-server --config /etc/aegis/server.toml`.
- **File repo:** đặt trong workspace để container đọc được, ví dụ `sudo install -o 10001 -g 10001 typst.toml /srv/aegis/.workspace/repo-files/`.
- **Token fork:** là file `./secrets/<token_env>`.
- **Webhook GitHub:** URL `https://<public_url>/webhooks/github`. Secret là nội dung file `./secrets/AEGIS_GITHUB_WEBHOOK_SECRET`.

## 3. Nâng cấp và rollback

1. Đọc CHANGELOG của phiên bản mới. Có migration thì xem mục [expand/contract](README.md#migration-expandcontract).
2. Sao lưu DB ([mục 4](#4-sao-lưu-và-khôi-phục)).
3. Lấy file của phiên bản mới (`git checkout vX.Y.Z`; `server.toml`, `.env` và `secrets/` không bị đụng), rồi chạy `sudo ./aegis-deploy.sh X.Y.Z`. Job đang chạy được tối đa `drain_s` (10 phút) để xong trước khi container cũ dừng.
4. **Rollback:** `sudo ./aegis-deploy.sh <phiên bản trước>`; script in sẵn lệnh này sau mỗi lần dựng. DB không lùi: phiên bản cũ chạy được trên DB đã có migration mới hơn nó. Nếu migration của bản lỗi làm hỏng dữ liệu, khôi phục DB từ bản sao lưu ở bước 2.

## 4. Sao lưu và khôi phục

```bash
sudo ./backup.sh /backup              # DB: aegis-db-<thời điểm>.dump (pg_dump -Fc)
sudo ./backup.sh /backup --workspace  # thêm .workspace/state (clone, map, index; không bắt buộc)
```

Chạy hằng ngày (cron hoặc systemd timer). Chép kết quả ra máy khác cùng với `server.toml` và `secrets/`; hai thứ này không có trong bản sao lưu.

**Khôi phục** lên máy mới, hoặc sau sự cố:

```bash
sudo ./install.sh                     # rồi chép lại server.toml và secrets/ cũ
docker compose up --detach --wait postgres
docker compose exec -T postgres pg_restore -U aegis -d aegis --clean --if-exists < /backup/aegis-db-<thời điểm>.dump
sudo tar -C /srv/aegis -xzf /backup/aegis-workspace-<thời điểm>.tar.gz   # nếu có
sudo ./aegis-deploy.sh <phiên bản đang dùng>
```

Không có workspace thì worker clone lại repo trước job đầu tiên, và module map được dựng lại.

## 5. systemd thay cho Compose

Dùng khi bạn đã có PostgreSQL (trên máy này hoặc máy khác) và muốn quản lý bằng systemd. Unit [deploy/systemd/aegis.service](../../deploy/systemd/aegis.service) là cùng một file với bản AWS.

```bash
sudo deploy/systemd/install.sh   # /etc/aegis, /srv/aegis/.workspace, unit, /usr/local/bin/aegis-deploy
```

- `/etc/aegis/server.toml` đổi `[metrics] listen` thành `127.0.0.1:9464`: unit chạy `--network host`, nên cổng này sẽ mở trên máy.
- `/etc/aegis/secrets/AEGIS_DATABASE_URL`: ví dụ `postgres://aegis:…@127.0.0.1:5432/aegis`.
- Tham số riêng của máy nằm trong `/etc/aegis/aegis.env`: `AEGIS_RUN_ARGS` (network, mount), `AEGIS_SANDBOXES`.

Rồi chạy `sudo aegis-deploy X.Y.Z` (hoặc `--offline X.Y.Z`); rollback cũng vậy, với phiên bản trước. Log xem bằng `journalctl -u aegis`. Sao lưu bằng `pg_dump` trên PostgreSQL của bạn.

## Sự cố

| Hiện tượng | Xem |
| --- | --- |
| `aegis-deploy.sh: … is not healthy` | `docker compose logs aegis`. Server dừng khi thiếu secret bắt buộc, và nêu tên secret |
| `secret … is open to others than its owner` | `chmod 0400` file đó, `chown 10001:10001` |
| `permission denied … docker.sock` trong log job | `DOCKER_GID` trong `.env` (systemd: `/etc/aegis/aegis.env`) phải là group của `/var/run/docker.sock` |
| Gate lỗi `No such image: aegis-sandbox-…` | Sandbox của profile chưa có trong `AEGIS_SANDBOXES`, hoặc chưa được nạp. Bước fetch luôn cần `aegis-sandbox-python` (proxy egress) |
| Job dừng với `API root … must use https` | API của forge (`fork.api`, `AEGIS_GITHUB_API`) phải là `https://`. `http://` chỉ được tới chính máy (loopback) |
| `aegis-job` báo `not_found` với token service | Token service cần vai trò: `… grant <tên token> maintainer` (hoặc `admin`) |
| Job lỗi mount `/work` | `/srv/aegis/.workspace` phải có thật trên máy (cùng đường dẫn với trong container) và thuộc uid 10001 |
