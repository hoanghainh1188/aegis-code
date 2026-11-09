# Helm chart của aegis: kiểm trên kind (#281)

- Ngày: 2026-11-09
- Issue: #281. Quyết định: [ADR 0008](../adr/0008-gate-sandbox-on-kubernetes.md). Vận hành: [runbook](../runbook/kubernetes.md).
- Môi trường: kind v0.33.0 (Kubernetes v1.37.0, một node) trên Docker Desktop, Mac arm64, Helm 4.3.1. Sidecar `docker:29-dind` (Docker 29.9.3).
- Image: `aegis-server` build từ nhánh này, thêm `aegis` CLI để replay. Image sandbox kéo thật từ GHCR, bản 0.6.0.

## Kết quả

| Tiêu chí | Kết quả |
| --- | --- |
| `helm install`, PostgreSQL của chart | API và PostgreSQL được nhận ở namespace Pod Security `restricted`, worker ở `privileged` |
| `aegis eval --replay --no-pr` gin-4622 trong Pod worker của chart, không gọi model | Qua gate, qua test ẩn. 104 s; 389 s ở lượt cuối với image cuối, khi máy đang chạy `cargo test --workspace` song song |
| `helm upgrade` (đổi `max_workers`) | Pod được thay; bảng đánh dấu trong DB, file trong workspace, clone, `state/` và image trong daemon còn nguyên; khởi động lại 7 s vì không phải kéo lại image |
| `helm rollback` | Như trên; `max_workers` về giá trị cũ |
| `sandboxImages.source: offline` | Nạp `aegis-sandbox-python-0.6.0-arm64.tar.gz` của Release 0.6.0 từ PVC, chạy được sandbox |

**Container `aegis`:** uid 10001, `CapEff` bằng 0, root chỉ đọc, không có token ServiceAccount, workspace và `/tmp` là `2770`. Daemon không mở cổng 2375.

**NetworkPolicy** (kindnet có thực thi):

| Từ | Đến | Kết quả |
| --- | --- | --- |
| worker | API 8080 | chặn |
| worker | PostgreSQL | mở |
| worker | github.com 443 | mở |
| worker | github.com 22 | chặn |
| worker | 169.254.169.254 | chặn |
| API | metrics của worker | chặn |
| Pod khác | API 8080 | mở (`apiFrom` rỗng) |
| Pod khác | metrics của API | chặn (`metricsFrom` rỗng) |
| Pod khác | PostgreSQL | chặn |

Thêm CIDR của Pod và Service vào `blockedCidrs`: worker vẫn tới được PostgreSQL của chart, không còn tới được Service API của Kubernetes trên 443.

## Lỗi tìm ra trên đường thử

| Lỗi | Gặp khi | Cách sửa |
| --- | --- | --- |
| `FileSource` từ chối file secret: Secret volume có `fsGroup` luôn là `root:<fsGroup> 0440`, bất kể `defaultMode`. Bỏ `fsGroup` thì uid 10001 không đọc được | Pod đọc Secret, `ls -l` cho `-r--r----- root 10001` | `0440` được nhận khi file do root sở hữu và nhóm là nhóm của tiến trình (`aegis-secrets`). Cần bản phát hành có sửa này: image 0.6.0 chưa có |
| `codebase-memory-mcp` từ chối `/tmp`: `emptyDir` có `fsGroup` là `2777` | Replay với root chỉ đọc và `/tmp` là `emptyDir` | initContainer đặt `/tmp` về `2770` |
| `toToml` của Helm làm hỏng số: `12` thành `12.0`, `1000000` thành `1e+06` | Thử render | `server.toml` viết tay trong template, `check.sh` giữ tính chất này |

## Lựa chọn của chart khác với bản thử

- **`dind` là native sidecar** (initContainer có `restartPolicy: Always`, nên chart cần Kubernetes từ 1.29). Sidecar thường nhận SIGTERM cùng lúc với worker, các gate đang chạy sẽ mất daemon giữa lúc drain.
- **`/var/lib/docker` là `subPath` của volume.** Daemon đặt thư mục gốc của nó về `0710`; nếu đó là gốc volume, lần khởi động sau kubelet thấy lệch `fsGroup` và đổi nhóm của mọi file trong image.
- **Hai Secret tách theo vai trò**, như bản EC2 (#323): worker không giữ session key, webhook secret, OIDC client secret.
- **PostgreSQL tuỳ chọn nằm trong chart**, không phải chart phụ thuộc bên thứ ba: chỉ để chạy thử, và không thêm nguồn phải nạp khi cài offline.

## Chưa kiểm

- Thứ tự dừng khi worker đang chạy job thật: replay chạy trong tiến trình riêng, nên chưa quan sát được drain cùng một gate đang chạy. Thứ tự sidecar theo đặc tả Kubernetes.
- `fsGroupChangePolicy` với `subPath` trên CSI thật: PV của kind là `hostPath`, kubelet không áp `fsGroup`.
- `ServiceMonitor` chỉ được render (kind không có CRD của Prometheus Operator). `Ingress` chỉ được kiểm bằng dry-run ở server.
- Một job thật qua hàng đợi của server, có gọi model: replay chạy `aegis eval`, không qua `--role worker`. Worker của chart đã khởi động, nối DB và daemon.

## Các bước

1. `kind create cluster --name aegis`.
2. Build image từ `docker/server/Dockerfile`, thêm `-p aegis-cli` và cài `target/release/aegis`; gắn tag `ghcr.io/hoanghainh1188/aegis-server:0.6.0`, nạp vào node: `docker save --platform linux/arm64 <image> | docker exec -i aegis-control-plane ctr -n k8s.io images import --platform linux/arm64 -`.
3. Tạo namespace và Secret như [runbook](../runbook/kubernetes.md) mục 1. `ANTHROPIC_API_KEY` là giá trị giả: replay không gọi model.
4. `helm install aegis deploy/helm/aegis -n aegis -f values.yaml`, với `sandboxImages.profiles: [go, python]` và `postgresql.enabled: true`.
5. Trong container `aegis`: chép `config/gin.toml` và `eval/issues/gin-4622.toml` vào `.workspace`, `git clone https://github.com/gin-gonic/gin .workspace/repos/gin`, rồi `aegis eval --config .workspace/gin.toml --set .workspace/set --only gin-4622 --replay --no-pr`.
