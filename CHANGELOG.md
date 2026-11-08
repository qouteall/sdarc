## 0.2.1

- Fix memory order UB in sharded allocator.
- Fix `allocate_data_in_each_shard`'s return value is 1 larger than actual weak ref count if weak ref is involved.
- Update dependency parking_lot to 0.13
- Some documentation fixes.

## 0.2.0

- Add `estimate_strong_count`, `estimate_weak_count`
- Add `WeakSdarc::ptr_eq`
- Add some trait impls
- Fallback case `curr_shard_index` handle TLS destruction

## 0.1.1

- fix windows-sys feature
- avoid using sched_getcpu in linux aarch64 musl
