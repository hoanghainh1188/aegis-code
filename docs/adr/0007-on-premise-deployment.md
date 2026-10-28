# ADR 0007 – Triển khai tại chỗ (on-premise)

- Ngày: 2026-10-29
- Trạng thái: Đã chấp nhận
- Người quyết định: @Nguyen Han Hoang Hai
- Issue: #262 (cùng #261, #263; Kubernetes ở #264)

## Bối cảnh

[ADR 0003](0003-platform-service-architecture.md) §6 chốt một cách triển khai: EC2, RDS, Secrets Manager và ECR trên AWS, dựng bằng OpenTofu. Có khách cần dựng toàn bộ hệ thống trong mạng của họ, không có tài khoản AWS, có khi không ra được Internet.

Hệ thống vốn nhỏ:
- một binary `aegis-server` (vai trò `api`, `worker` hoặc `all`);
- PostgreSQL, giữ cả hàng đợi job (`FOR UPDATE SKIP LOCKED`), nên không cần Redis hay message broker;
- các image sandbox của gate, chạy trên daemon Docker của máy.

## Quyết định

1. **Giữ nguyên lõi.**
   - Cùng image `aegis-server` và image sandbox như bản AWS.
   - Cùng cách chạy sandbox: container `aegis` gọi daemon Docker của máy qua socket. Workspace có cùng đường dẫn `/srv/aegis/.workspace` trong và ngoài container, thuộc uid 10001.
   - Cùng migration: server tự chạy khi khởi động, theo expand/contract.

2. **Những gì đổi so với AWS:**

   | | AWS (ADR 0003 §6) | On-premise |
   | --- | --- | --- |
   | Secret | Secrets Manager | File trong một thư mục, `[secrets] source = "file"` (#261) |
   | Image | ECR, tag = SHA | GHCR, tag = phiên bản `X.Y.Z`; hoặc gói offline (#263) |
   | PostgreSQL | RDS | Container trong Compose, hoặc PostgreSQL của khách |
   | Model | Claude API, Bedrock `jp.` | Thêm model local (`provider.kind = "local"`, #38, #233), file mode của khách |
   | TLS | ALB | Reverse proxy của khách |
   | Log | CloudWatch | Log driver mặc định của Docker, hoặc của khách |

3. **Hai cách dựng trên một máy Linux có Docker:**
   - **Docker Compose** (`deploy/compose/`), cách mặc định:
     - gồm PostgreSQL và `aegis-server --role all`;
     - PostgreSQL ở mạng nội bộ của Compose, không mở ra ngoài;
     - `install.sh` chuẩn bị máy, `aegis-deploy.sh <phiên bản>` để dựng, nâng cấp và rollback, `backup.sh` để sao lưu.
   - **systemd** (`deploy/systemd/`), cho khách đã có PostgreSQL và muốn quản lý bằng systemd:
     - **một unit `aegis.service` dùng chung với bản AWS**: OpenTofu chép đúng file này vào `user_data`;
     - phần riêng của từng máy (network, cổng, secret, log driver) nằm trong `/etc/aegis/aegis.env`, biến `AEGIS_RUN_ARGS`.

4. **Phiên bản là đơn vị nâng cấp và rollback.**
   - Server và sandbox luôn cùng tag.
   - Rollback chạy lại `aegis-deploy` với phiên bản trước. DB không lùi: server bỏ qua migration nó không biết (#176).

5. **Kubernetes chưa làm.** Gate cần một daemon Docker, mà node Kubernetes thường không có. Cách chạy sandbox trong cluster được quyết định riêng ở #264.

## Hệ quả

- Khách chỉ cần một máy Linux có Docker và Compose. Không cần tài khoản cloud hay Internet: dùng gói offline và model local.
- **Rủi ro về quyền như bản AWS:** container `aegis` nắm socket Docker, tức nắm root của máy. Máy nên chỉ chạy aegis.
- **Hai đường dẫn cố định:** workspace ở `/srv/aegis/.workspace`, uid 10001.
- **Unit systemd dùng chung:** sửa `deploy/systemd/aegis.service` là đổi cả bản AWS khi tạo lại máy.
- **Không có SLA hạ tầng.** Sao lưu (`pg_dump`), giám sát và TLS là việc của khách. Runbook on-premise hướng dẫn từng việc.

## Phương án đã cân nhắc

- **Chỉ Helm chart:** phần lớn khách nhỏ không có cluster. Vướng mắc của gate làm Kubernetes thành việc nghiên cứu riêng (#264).
- **Docker-in-Docker cho sandbox:** cần container privileged, và cache build cùng image sandbox phải kéo lại trong mỗi container. Không an toàn hơn việc mount socket, mà chậm hơn.
- **Gói `.deb` / `.rpm` chạy binary trực tiếp:** server vẫn cần Docker cho gate, Codebase-Memory và git đúng phiên bản. Image đã gói sẵn tất cả.
