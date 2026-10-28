# Dừng sớm lượt chạy không có tiến triển: số liệu và quyết định (#273)

- Ngày: 2026-10-29
- Issue: #273, tách ra từ rà soát chi phí của #35 ([báo cáo](2026-10-28-context-compression.md) §4).
- Dữ liệu: ledger JSON của các lượt eval bằng Claude API trên typst và scrapy. Gồm 106 lượt chạy đã kết thúc và có sự kiện `tool_calls`; 17 lượt cũ hơn sự kiện này bị loại, vì không biết được tiến triển. Không gọi model nào. Tính lại bằng `scripts/eval/early_stop.py`.

## Quyết định

**Không thêm quy tắc dừng sớm (NO-GO).** Mọi quy tắc đơn giản đều dừng cả những lượt chạy về sau mở được PR qua hidden test. Quy tắc ít hại nhất (`no-edit@20`) tiết kiệm $6,70 trên 106 lượt, nhưng mất 2 trong 37 PR tốt. Tức là bớt $3,35 cho mỗi PR tốt bị mất, trong khi trung bình mỗi PR tốt chỉ tốn $1,64.

Phần lớn lượt chạy thất bại **có sửa code**: 41 trong 48 lượt, lần sửa đầu ở lượt gọi trung vị 13. Chúng thất bại sau khi đã sửa, không phải vì loay hoay không sửa gì. Muốn giảm tiền ở đây thì phải tăng tỉ lệ sửa đúng (#156, #259, #267), không phải cắt ngắn lượt chạy.

**Xem lại khi:** #156 nâng trần lên 80 lượt gọi. Khi đó chạy lại script trên ledger mới, nhất là `stale@20`: những lượt sửa rồi loay hoay tới trần là nơi trần 80 có thể đốt thêm tiền.

## 1. Tổng quan

| | Lượt | Chi phí |
| --- | --- | --- |
| Mở được PR | 58 (37 qua hidden test) | $24,61 |
| Không có PR | 48 | $36,14 (59%) |

Lượt gọi Core/Expert có sửa code (`edit` / `create_file` áp dụng được) lần đầu ở lượt gọi:

| Nhóm | Không sửa gì | Trung vị | p90 | Muộn nhất |
| --- | --- | --- | --- | --- |
| PR qua hidden test | 0 | 9 | 14 | 30 |
| PR không qua hidden test | 0 | 9 | 13 | 14 |
| Không có PR | 7 | 13 | 38 | 40 |

PR tốt có lần sửa đầu muộn nhất ở lượt 30: Core không sửa được, platform chuyển lên Expert ở lượt 15 ("no patch after 15 calls"), và Expert sửa đúng.

## 2. Phát lại các quy tắc

Tiến triển là một lần sửa áp dụng được. Các quy tắc dừng lượt chạy ở lượt gọi đầu tiên mà:
- `no-edit@N`: tới lượt gọi N vẫn chưa sửa gì;
- `stale@M`: đã M lượt gọi kể từ lần sửa cuối (hoặc từ đầu);
- `repeat@K`: cùng một công cụ trên cùng một đích được gọi tới lần thứ K.

"Tiết kiệm" là chi phí của các lượt gọi sau điểm dừng, chỉ tính trên các lượt không có PR.

| Quy tắc | Dừng (lượt không PR) | Tiết kiệm | Tỉ trọng | PR bị mất | PR tốt bị mất |
| --- | --- | --- | --- | --- | --- |
| `no-edit@10` | 36/48 | $26,20 | 72% | 23/58 | 16/37 |
| `no-edit@15` | 22/48 | $14,90 | 41% | 4/58 | 4/37 |
| `no-edit@20` | 15/48 | $6,70 | 19% | 2/58 | 2/37 |
| `no-edit@25` | 15/48 | $5,27 | 15% | 2/58 | 2/37 |
| `stale@10` | 43/48 | $28,32 | 78% | 30/58 | 17/37 |
| `stale@15` | 33/48 | $17,70 | 49% | 10/58 | 6/37 |
| `stale@20` | 23/48 | $8,26 | 23% | 4/58 | 3/37 |
| `repeat@3` | 34/48 | $14,91 | 41% | 27/58 | 15/37 |
| `repeat@4` | 27/48 | $9,75 | 27% | 14/58 | 9/37 |
| `repeat@5` | 18/48 | $5,96 | 17% | 8/58 | 6/37 |

- **`no-edit@15` và `@20`.** Hai quy tắc này cắt đúng những lượt mà việc chuyển lên Expert ở lượt 15 cứu được, nên PR bị mất đều là PR Expert sửa đúng.
- **`repeat@*`.** Đây là hành vi bình thường, không phải loay hoay: với `repeat@3`, 27 PR bị mất gồm 19 lượt chạy lại `run_tests` cùng một test sau mỗi lần sửa, và 8 lượt `scroll_down` qua một file dài.
- **`stale@*`.** Nhóm này dừng cả những lượt đang đọc và chạy test giữa hai lần sửa.
