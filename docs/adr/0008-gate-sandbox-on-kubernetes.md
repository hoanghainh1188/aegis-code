# ADR 0008 – Sandbox của gate trên Kubernetes

- Ngày: 2026-10-29
- Trạng thái: Đã chấp nhận
- Người quyết định: @Nguyen Han Hoang Hai
- Issue: #264 (Helm chart ở #281)
- Số liệu: [bản thử trên kind](../measurements/2026-10-29-k8s-gate-sandbox.md)

## Bối cảnh

Gate chạy mỗi bước trong một container sandbox anh em: `docker run --network none --cap-drop ALL …` qua daemon Docker của máy ([ADR 0003](0003-platform-service-architecture.md) §6, [ADR 0007](0007-on-premise-deployment.md)). Mọi ràng buộc của sandbox nằm trên dòng lệnh đó.

Không chỉ gate gọi `docker`. ACI (`aegis-aci`) cũng chạy formatter của `edit`, `run_tests` và LSP trong container sandbox. LSP là một container sống suốt job, nói chuyện qua stdio.

Node Kubernetes ngày nay chạy containerd, không có daemon Docker. Có khách muốn chạy aegis trên cluster của họ.

## Quyết định

1. **Mỗi Pod worker có daemon Docker riêng trong một sidecar (Docker-in-Docker).**
   - Container `aegis` chạy không quyền (uid 10001, `drop: [ALL]`, không `allowPrivilegeEscalation`).
   - Container `dind` (`docker:<phiên bản>-dind`) chạy `privileged`.
   - Hai container nói chuyện qua một socket unix trong volume chung.
   - Gate và ACI không đổi gì: vẫn `docker run` với cùng ràng buộc như trên EC2 và Compose. Phải sửa #278 trước: `aegis-aci` chưa nhận `DOCKER_HOST`.

2. **Cách cấu hình sidecar** (đã kiểm trên kind, chi tiết trong file số liệu):
   - `dockerd` chỉ nghe trên socket unix, chạy bằng `command: [dind, dockerd, --host=unix://…, --group=10001]`. Không dùng entrypoint của image: không có thư mục TLS thì nó mở thêm `tcp://0.0.0.0:2375` không xác thực, tức là quyền root của Pod cho bất kỳ ai trên mạng Pod.
   - Workspace (`/srv/aegis/.workspace`) là một volume mount cùng đường dẫn vào cả hai container: gate đưa đường dẫn đó cho daemon khi mount cây của job vào sandbox.
   - `fsGroup: 10001`, và một initContainer đặt quyền volume về `2770`. `codebase-memory-mcp` không chạy khi một thư mục cha của HOME cho mọi người ghi, mà `emptyDir` với `fsGroup` được tạo ra ở `2777`.
   - `/var/lib/docker` của sidecar nằm trên volume riêng của từng Pod. Image sandbox (Go 1,4 GB, .NET 1,6 GB) và layer không phải tải lại mỗi lần Pod khởi động lại. Daemon không dùng chung `/var/lib/docker` được, nên worker là StatefulSet, mỗi Pod một PVC.
   - Image sandbox lấy từ GHCR, hoặc `docker load` từ gói offline (#263) vào daemon của Pod.

3. **Giới hạn phần có quyền:**
   - Chỉ Pod vai trò `worker` có sidecar privileged. Vai trò `api` chạy trong Deployment riêng, không có Docker.
   - Worker chạy trong namespace riêng, chỉ namespace đó được Pod Security Admission mức `privileged`.
   - Worker chạy trên node pool riêng (taint/toleration), không chung node với workload khác.
   - `automountServiceAccountToken: false`: worker không cần API Kubernetes, mà bản thử thấy token vẫn được mount vào.
   - Có NetworkPolicy cho Pod worker.

4. **Mức tin cậy giống bản Compose.** Trên máy Compose, ai nắm socket Docker là root của máy. Ở đây, ai chiếm được tiến trình `aegis` của worker là root của node, qua sidecar privileged. Sandbox của agent không chạm được daemon: nó chạy trong daemon của Pod với `--network none`, không capability, không socket. Bản thử đã kiểm: `CapEff` bằng 0, không có route ra ngoài.

## Hệ quả

- Không đổi code của gate hay ACI; sửa #278 xong là chạy được.
- Cần quyền `privileged` cho một container trong cluster. Cluster nào cấm hẳn (một số dịch vụ có quản lý, chính sách của khách) thì chưa chạy được. Hướng đi khi đó là Sysbox hoặc runtime VM (bảng dưới).
- Mỗi Pod worker giữ một bản image sandbox riêng: tốn đĩa theo số Pod, đổi lại không phụ thuộc node.
- Helm chart (#281) dựng theo các điểm trên. Bản thử chạy bằng manifest tay trong file số liệu.

## Các phương án khác

| Phương án | Lý do không chọn bây giờ |
| --- | --- |
| Mount `/var/run/docker.sock` của node | Node chạy containerd, không có Docker. Kể cả có, Pod nắm socket là root của node, và container sandbox nằm ngoài Pod: không theo giới hạn tài nguyên, không bị xoá cùng Pod |
| Mỗi bước gate là một Pod/Job | Phải viết runner thứ hai cho cả `aegis-gate` lẫn `aegis-aci` (formatter mỗi lần `edit`, `run_tests`, LSP sống suốt job qua stdio). Cây của job phải dùng chung giữa Pod worker và Pod bước: cần volume RWX hoặc ép cùng node. Bước fetch cần NetworkPolicy thay cho mạng `--internal` của proxy egress, mà CNI phải hỗ trợ. Bản thử đo được Pod khởi động trong 1,9 s, `docker run` trong 0,26 s, cả hai với image có sẵn. Mỗi bước chậm thêm khoảng 1,6 s: không phải lý do chính, công sửa mới là lý do. Xem lại khi cần co giãn worker theo từng bước, hoặc khi cluster cấm `privileged` |
| DinD rootless (`docker:dind-rootless`) | Theo tài liệu của Docker vẫn cần `privileged` khi chạy trong container; thêm độ phức tạp mà không bỏ được quyền đó. Chưa thử |
| Sysbox (runtime cho DinD không privileged) | Phải cài runtime lên node. Hợp với khách tự quản node; ghi vào chart như tuỳ chọn `runtimeClassName` khi có khách cần |
| gVisor / Kata cho Pod worker | Đổi "root của node" thành "root của sandbox hay VM". Kata chạy cả Pod (kể cả sidecar DinD) trong VM nhẹ. Chưa thử; tuỳ chọn `runtimeClassName` của chart cũng mở đường cho nó |
