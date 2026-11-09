# `aegis-docker-proxy` trong worker Pod: kiểm trên kind (#411)

- Ngày: 2026-11-09
- Issue: #411. Quyết định: [ADR 0010](../adr/0010-docker-socket-proxy.md) §6. Vận hành: [runbook](../runbook/kubernetes.md#security-model).
- Môi trường: kind v0.33.0 (Kubernetes v1.37.0, một node) trên Docker Desktop, Mac arm64. Sidecar `docker:29-dind` (Docker 29.9.3).
- Image: `aegis-server` build từ nhánh này, thêm `aegis` CLI để replay. Image sandbox kéo thật từ GHCR, bản 0.6.0. Các bước như [lần kiểm chart](2026-11-09-helm-chart-kind.md), `sandboxImages.profiles: [go, python]`.

## Kết quả

| Tiêu chí | Kết quả |
| --- | --- |
| `deploy/helm/check.sh` | Pass |
| Replay `aegis eval --replay --no-pr` gin-4622 trong Pod worker, không gọi model | Qua gate, qua test ẩn, 72 s |
| Log của proxy trong lúc replay | 0 lời gọi bị từ chối |
| `docker run --privileged` từ container `aegis`, các cờ khác đều đúng | Bị từ chối: `HostConfig.Privileged is not allowed` |
| Cùng lệnh, bỏ `--privileged` (đối chứng) | Chạy được |
| `docker run -v /:/host`, các cờ khác đều đúng | Bị từ chối: `mount source "/" is outside the workspace` |
| `docker ps` | Bị từ chối: `GET /containers/json is not a call aegis makes` |
| `kill -9` tiến trình proxy khi worker đang chạy | Sidecar khởi động lại trên cùng đường dẫn, xoá socket cũ; `docker version` từ `aegis` chạy lại được, container `aegis` không khởi động lại |

**Container `aegis`:** không có `/run/dind`, `DOCKER_HOST` là `unix:///run/aegis-docker/docker.sock` (`srw------- aegis aegis`).

**Container `docker-proxy`:** uid 10001, `CapEff` bằng 0, `NoNewPrivs` 1, root chỉ đọc, workspace chỉ đọc. Dùng 2.9 MB RAM khi rảnh.

## Chưa kiểm

- Thứ tự dừng khi worker đang chạy job thật. Thứ tự sidecar theo đặc tả Kubernetes: `aegis` dừng trước, rồi proxy, rồi `dind`.
- Một mode có Sonar qua `worker.dockerProxy.extraArgs`: chỉ kiểm phần render.
- Một job thật qua hàng đợi của server, có gọi model: replay chạy `aegis eval`, không qua `--role worker`.
