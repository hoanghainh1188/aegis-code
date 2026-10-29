# Sandbox của gate trên Kubernetes: bản thử trên kind (#264)

- Ngày: 2026-10-29
- Issue: #264. Quyết định: [ADR 0008](../adr/0008-gate-sandbox-on-kubernetes.md).
- Môi trường: kind v0.33.0 (Kubernetes v1.37.0, containerd 2.3.4, một node) trên Docker Desktop, Mac arm64. Sidecar `docker:29-dind` (Docker 29.9.2, overlayfs, cgroup v2).
- Bài thử: `aegis eval --replay --no-pr` cho gin-4622 (Go), chạy trong Pod. Replay ghi bản sửa upstream qua tool của ACI rồi chạy gate và test ẩn, không gọi model.

## Kết quả

- **Thời gian chạy:**

  | | Trong Pod (DinD) | Đối chứng trên Mac (Docker Desktop) |
  | --- | --- | --- |
  | Gate | qua, 26,7 s | qua, 31,6 s |
  | Test ẩn | qua | qua |
  | Cả lượt chạy | 36 s | 49 s |

  Hai lượt dùng cùng cấu hình: Codebase-Memory tắt, có `fetch_hosts` của Go (lý do ở mục lỗi bên dưới). Bản đối chứng dùng build debug, nên thời gian không so sánh trực tiếp được. Chỉ cần thấy trong Pod không chậm hơn.

- **Bên trong sandbox của daemon trong Pod:**
  - `--network none`: chỉ có `lo` (cùng các thiết bị tunnel của kernel, đều tắt); `wget http://1.1.1.1` báo `Network is unreachable`.
  - `CapEff: 0000000000000000`.
  - Daemon dùng seccomp mặc định và cgroupns.

- **Container `aegis`:** `CapEff` bằng 0. Token ServiceAccount vẫn được mount (`/var/run/secrets/kubernetes.io/serviceaccount`), nên chart phải tắt.

- **Bước fetch:** proxy egress chạy được trong daemon của Pod, qua mạng `--internal` của nó, không cần gì từ Kubernetes.

- **Khởi động** (`busybox`, image có sẵn trên node, 5 lần):
  - Pod (`kubectl run --rm -i … true`): 1,84–2,41 s, thường 1,9 s;
  - `docker run --rm --network none … true`: 0,26 s.

- **Nạp image vào daemon của Pod** (`docker save | kubectl exec -i … docker load`): Go 1,37 GB mất 9 s, Python 538 MB mất 4 s.

## Lỗi tìm ra trên đường thử

| Lỗi | Gặp khi | Issue |
| --- | --- | --- |
| `aegis-aci` không nhận `DOCKER_HOST`: formatter của `edit` gọi socket mặc định, cả hai `edit` bị từ chối. Agent bị đẩy lên tier expert, gate chạy trên cây chưa sửa, test ẩn fail | Worker dùng `DOCKER_HOST=unix:///run/dind/docker.sock`. Đi vòng: mount socket ở `/run/docker.sock` | #278 |
| Profile Go, TypeScript, Java, C# không có `fetch_hosts`, nên proxy chặn `proxy.golang.org` | Mọi repo có phụ thuộc, khi bật sandbox; không riêng Kubernetes. Đi vòng: `[repo.overrides] fetch_hosts` | #279 |
| Replay dừng trước gate khi platform gửi mã bị ảnh hưởng của Codebase-Memory | Bật `[codebase_memory]`; không riêng Kubernetes. Đi vòng: tắt Codebase-Memory | #280 |
| `codebase-memory-mcp` thoát với `could not load saved runtime settings` khi một thư mục cha của HOME cho mọi người ghi (`0757`, `1777`, `2777` đều lỗi; `0775` thì chạy) | `emptyDir` với `fsGroup` được tạo ở `2777`. Đi vòng: initContainer `chmod 2770` | ghi vào #281 |
| Entrypoint của `docker:dind` mở `tcp://0.0.0.0:2375` không xác thực khi không có thư mục TLS | Chạy sidecar với `args` | ADR 0008 §2 |

## Manifest của bản thử

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: aegis-worker
spec:
  securityContext:
    fsGroup: 10001
  initContainers:
    # fsGroup leaves the volume 2777; codebase-memory-mcp refuses a HOME
    # under a directory anyone may write.
    - name: workspace-mode
      image: busybox:1.37
      command: ["chmod", "2770", "/srv/aegis/.workspace"]
      volumeMounts:
        - {name: workspace, mountPath: /srv/aegis/.workspace}
  containers:
    - name: aegis
      # aegis-server's image with the `aegis` CLI added, for the replay.
      image: aegis-k8s-trial:dev
      command: ["sleep", "infinity"]
      securityContext:
        runAsUser: 10001
        runAsGroup: 10001
        allowPrivilegeEscalation: false
        capabilities: {drop: [ALL]}
      volumeMounts:
        - {name: workspace, mountPath: /srv/aegis/.workspace}
        # The socket at Docker's default path until #278.
        - {name: dind-sock, mountPath: /run}
    - name: dind
      image: docker:29-dind
      # Not the image's entrypoint, which also opens tcp://0.0.0.0:2375.
      command:
        - dind
        - dockerd
        - --host=unix:///run/dind/docker.sock
        - --group=10001
      securityContext:
        privileged: true
      volumeMounts:
        - {name: workspace, mountPath: /srv/aegis/.workspace}
        - {name: dind-sock, mountPath: /run/dind}
        - {name: dind-storage, mountPath: /var/lib/docker}
  volumes:
    - {name: workspace, emptyDir: {}}
    - {name: dind-sock, emptyDir: {}}
    - {name: dind-storage, emptyDir: {}}
```

## Các bước

1. Tạo cluster: `kind create cluster --name aegis`.
2. Build image thử từ `docker/server/Dockerfile`, thêm `-p aegis-cli` vào `cargo build` và cài thêm `target/release/aegis`.
3. Nạp image thử, `docker:29-dind` và `busybox` vào node: `docker save --platform linux/arm64 <image> | docker exec -i aegis-control-plane ctr -n k8s.io images import -`. `kind load docker-image` không dùng được với image store containerd của Docker Desktop: Pod báo `ErrImageNeverPull`.
4. `kubectl apply` manifest trên, rồi nạp image sandbox vào daemon của Pod: `docker save aegis-sandbox-go | kubectl exec -i aegis-worker -c aegis -- docker load`. Làm tương tự với `aegis-sandbox-python`, image của proxy egress.
5. Trong container `aegis`:
   - `git clone https://github.com/gin-gonic/gin .workspace/repos/gin`;
   - chép `eval/issues/gin-4622.toml` vào `.workspace/set/`;
   - cấu hình là `config/poc.toml` với `[repo]` của gin (`path = ".workspace/repos/gin"`, `language = "go"`, `hidden_test = ["go", "test", "-v", "-run", "{tests_regex}", "./..."]`), `[repo.overrides] fetch_hosts = ["proxy.golang.org", "sum.golang.org"]`, và bỏ `[codebase_memory]`.
6. Chạy `aegis eval --config .workspace/go.toml --set .workspace/set --only gin-4622 --replay --no-pr`.
