# Kế hoạch chạy thử với một team thật (#158)

Trạng thái: **bản nháp**. Các ô `<…>` được điền lúc triển khai: `install.sh` và `add-repo.sh` hỏi chúng khi chạy (#476), nên không phải sửa file nào bằng tay.

Mốc đo của Phase 2 ([Proposal §9](../design/00-proposal.md)) là: team dùng hằng ngày, chi phí tháng nằm trong ngân sách, và KPI giữ được như MVP ([ADR 0005](../adr/0005-kpi-thresholds-after-phase-1.md)). Đợt chạy thử này kiểm mốc đó trên bản on-premise ([ADR 0007](../adr/0007-on-premise-deployment.md)), không chờ AWS staging (#33).

## Thông số

| Mục | Giá trị |
| --- | --- |
| Máy | `<host-ssh>`: Linux x86_64 hoặc arm64, tối thiểu 4 vCPU, 16 GB RAM, 100 GB SSD, chỉ dùng cho aegis ([runbook on-premise](../runbook/on-premise.md#prerequisites)) |
| Cách triển khai | Docker Compose (`deploy/compose/`), bản **v0.11.0** (bản đầu tiên có `aegis check-repo` trong image server, #474) |
| Repo | `<owner/name>` trên GitHub. Issue và lỗi CI đến qua webhook (#25) |
| Ngôn ngữ | `<language>`. Trần lượt gọi theo profile: 80 cho Rust, 40 cho các ngôn ngữ khác ([ADR 0014](../adr/0014-per-language-call-cap.md)) |
| Ngân sách | **$100 mỗi tháng** cho repo (#31), $4 mỗi job (`[worker] max_cost_usd`) |
| Model | Claude API: Light `claude-haiku-5-5`, Core `claude-sonnet-5-5`, Expert `claude-opus-5-5` |
| Thời gian | 4 tuần, từ `<ngày bắt đầu>` đến `<ngày kết thúc>` |
| Người duyệt | `<email>`, … (vai trò `reviewer`) |
| Người giao việc | `<email>`, … (vai trò `maintainer`) |
| Quản trị | `<email>` (vai trò `admin`) |

## Bước 0: chuẩn bị (trước ngày triển khai)

1. **Phát hành v0.11.0.** Merge PR release của release-please để có tag `v0.11.0` cùng image trên GHCR. Từ bản này image server có `aegis check-repo` (#450, #474), nên máy pilot không cần Rust toolchain. Bản v0.9.0 còn thiếu cả trần lượt gọi theo ngôn ngữ (#471) và các quy tắc prompt của #448.
2. **Gom 9 mục** trong [danh sách chuẩn bị của runbook](../runbook/on-premise.md#preparation-checklist). Các mục thường mất thời gian nhất vì phải chờ team khác:
   - tên DNS cùng chứng chỉ TLS, đặt sau reverse proxy;
   - một OIDC client trong SSO của team;
   - tài khoản bot GitHub, một fork của `<owner/name>`, và token của bot;
   - một token được phép comment trên issue upstream;
   - quyền admin trên repo upstream để thêm webhook;
   - API key của Claude.
3. **Chọn issue cho tuần đầu.** Lấy 5–10 bug nhỏ, mô tả rõ, có cách tái hiện. Đó là loại việc aegis nhắm tới ([Proposal §1](../design/00-proposal.md#1-objectives--scope)).

## Bước 1: triển khai (ngày 0)

Làm theo [runbook on-premise §1](../runbook/on-premise.md#1-installation--setup) trên `<host-ssh>`:

1. `sudo ./install.sh` (chạy một lần). Script hỏi:
   - URL công khai, issuer và client ID của OIDC, email của admin đầu tiên. Email admin chỉ dùng được nếu SSO đánh dấu email là đã xác minh; với Entra ID thì để trống, đăng nhập một lần, rồi cấp quyền (hướng dẫn cài đặt, B9);
   - các sandbox cần kéo về, chỉ chọn ngôn ngữ của repo;
   - API key Claude, OIDC client secret và token để comment. Ba thứ này nhập ẩn.
   Câu nào để trống thì chạy lại script sẽ được hỏi lại.
2. `sudo ./aegis-deploy.sh 0.11.0`. Ở đây ghi version không có chữ `v`. Khi server chạy được, script tự cấp quyền `admin` cho email đã nhập.
3. Kiểm tra sau khi cài (runbook §1, bước 5): healthcheck trả về được, và đăng nhập SSO được.
4. Chạy `backup.sh` lần đầu, rồi đặt lịch chạy hằng ngày bằng cron hoặc systemd timer ([runbook on-premise §4](../runbook/on-premise.md#4-backup--disaster-recovery)).

## Bước 2: đăng ký repo (ngày 0–1)

Làm theo [runbook on-premise §2](../runbook/on-premise.md#2-registering-repositories):

1. **Kiểm tra repo bằng `aegis check-repo`** ([runbook §2, Before Registering](../runbook/on-premise.md#before-registering-aegis-check-repo)). Lệnh có sẵn trong image server và chạy trong container `aegis` qua `docker-proxy`, giống job thật. Clone `<owner/name>` vào `/srv/aegis/.workspace/preflight/`, viết config từ config của mode kèm `[repo]` trỏ tới bản clone đó, rồi chạy:

   ```bash
   docker compose exec aegis aegis check-repo --config .workspace/preflight/<slug>.toml
   ```

   Lệnh này kiểm profile, lockfile, Docker và image sandbox, bước fetch, và chạy gate gốc trong sandbox `--network none` (format, lint, test). Mọi mục chặn phải qua trước khi đăng ký repo. Nếu test của repo cần PostgreSQL, Redis hay MySQL thì khai báo chúng là sidecar trong file repo (`[[settings.sidecars]]`, [runbook repos](../runbook/repos.md#registration-and-cloning) bước 7; #490) và cho image qua proxy (`--sidecar-image`). Nếu test cần Internet thì repo này chưa hợp để chạy thử. Test hỏng trên base cũng chặn (#487). Format hay lint hỏng trên base chỉ là cảnh báo, vì gate chấm bản vá theo các lỗi mà bản vá thêm vào.
2. `sudo ./add-repo.sh`. Script hỏi:
   - URL của `<owner/name>`, slug, nhánh mặc định, ngôn ngữ và mode;
   - fork cùng token của fork, nhập ẩn;
   - email của người giao việc (`maintainer`) và người duyệt (`reviewer`);
   - ngân sách tháng, nhập `100`.
   Script ghi token và file repo, chạy `repo add` và `grant`, rồi in ra cách đặt webhook và đường dẫn trang ngân sách.
3. Đặt ngân sách tháng $100 ở trang ngân sách mà script in ra (một maintainer làm việc này). Khi hết ngân sách, job bị huỷ và issue nhận được comment giải thích.
4. Thêm webhook theo hướng dẫn script in ra: trên repo upstream cho sự kiện **Issues** và **Workflow runs**; trên fork cho **Pull request reviews**, **Pull request review comments** và **Issue comments**.
5. **Job đầu tiên:** gắn nhãn `aegis` cho một bug nhỏ. Webhook phải trả `202`, job phải hiện trên dashboard, và issue phải nhận được comment khi job xong.
6. Gửi [Hướng dẫn duyệt](../reviewing.md) cho người duyệt và [hướng dẫn client](../client.md) cho developer.

## Bước 3: vận hành 4 tuần

- **Giao việc:** người giao việc gắn nhãn `aegis` cho bug, nhắm 5–15 issue mỗi tuần. Lỗi CI trên nhánh mặc định cũng tự tạo job (#25).
- **Duyệt:** người duyệt chấm từng PR trên giao diện duyệt (`/jobs/<id>/review`, #28) và ghi kết quả (accepted, accepted with edits hoặc rejected), số dòng đã sửa và số phút đã bỏ ra. Người duyệt không xem trước kết quả test ẩn. aegis không bao giờ tự merge.
- **Theo dõi hằng ngày:** dashboard xem job đang chờ, job lỗi và chi phí. Log xem bằng `docker compose logs aegis`, chỉ cần khi có sự cố.
- **Họp 15 phút hằng tuần** để điền bảng dưới đây và ghi phản hồi của team.

| Tuần | Job | Qua gate | PR được chấp nhận | Hồi quy đến người duyệt | Chi phí ($) | Chi phí cộng dồn / $100 | Thời gian duyệt trung vị (phút) | Ghi chú |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | | | | | | | | |
| 2 | | | | | | | | |
| 3 | | | | | | | | |
| 4 | | | | | | | | |

Số liệu lấy từ dashboard (KPI, chi phí, thời gian duyệt) và từ `GET /api/v1/jobs`.

### Khi nào dừng hoặc thu hẹp

- **Có một hồi quy đến được người duyệt** (ngưỡng là 0, ADR 0005): tạm ngừng giao việc mới, tìm nguyên nhân, và mở issue cho việc sửa.
- **Chi phí cộng dồn vượt $75 trước tuần 4:** giảm số issue mỗi tuần. Nếu hết $100, đợt chạy dừng ở đó, và số đo vẫn được tính tới thời điểm dừng.
- **Có sự cố bảo mật** (lộ secret, sandbox thoát ra ngoài, prompt injection có hậu quả): dừng worker theo [runbook](../runbook/README.md), xoay secret, và báo cáo trước khi chạy lại.
- **Team không dùng nữa**, ví dụ hai tuần liền không gắn nhãn issue nào: ghi lại lý do và dùng nó làm phản hồi trong báo cáo.

## Bước 4: báo cáo

Viết báo cáo trong `docs/measurements/` theo "Xong khi" của #158:
- số job mỗi tuần;
- KPI theo ngôn ngữ, so với ngưỡng của ADR 0005 và với Phase 1;
- chi phí tháng so với $100;
- thời gian duyệt thật, với người duyệt không xem trước kết quả test ẩn (ADR 0005, Consequences);
- phản hồi của team, và những gì phải sửa trước khi đưa cho team thứ hai.

Nếu `<owner/name>` là repo nội bộ và khách đồng ý bằng văn bản, các số đo này cũng dùng được cho #157. Nếu thời gian duyệt trung vị vượt 15 phút mỗi PR, đó là lúc làm #36.

## Rủi ro

| Rủi ro | Cách giảm |
| --- | --- |
| Test của repo cần DB hoặc mạng, nên không chạy được trong sandbox `--network none` | `check-repo` phát hiện ở bước 2.1. DB, cache: khai báo sidecar (#490). Internet: chọn repo khác |
| Đĩa đầy vì cache build (một repo Rust lớn cần 30–40 GB) | Kiểm tra `df -h` mỗi ngày (bản Compose không có Grafana), dọn cache build. Máy tối thiểu 100 GB SSD |
| API model lỗi (5xx, 429) hàng loạt | Server thử lại 3 lần, giãn cách tăng dần từ 30 giây. Hết lượt thử thì job lỗi; chạy lại bằng `POST /api/v1/jobs/{id}/retry` ([runbook §4](../runbook/README.md#4-troubleshooting)) |
| Team chỉ giao việc khó, nên tỉ lệ chấp nhận thấp | Chọn issue cùng team trong tuần đầu, và ghi độ khó của từng issue vào bảng tuần |
| Repo công khai, model có thể đã thấy bản sửa | Ghi rõ trong báo cáo. Ưu tiên repo nội bộ (#157) |
