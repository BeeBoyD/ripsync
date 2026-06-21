//! Criterion micro-benches: rolling checksum, delta encode, delta apply.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use ripsync_core::checksum::RollingChecksum;
use ripsync_core::delta::{apply, encode};

/// Deterministic pseudo-random buffer (no rand dep needed in the bench).
fn buf(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x & 0xFF) as u8
        })
        .collect()
}

fn bench_rolling(c: &mut Criterion) {
    let data = buf(1 << 20, 1); // 1 MiB
    let window = 1024usize;
    let mut group = c.benchmark_group("rolling_checksum");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("roll_1MiB_w1024", |b| {
        b.iter(|| {
            let mut rc = RollingChecksum::new(&data[..window]);
            let mut acc = 0u64;
            for i in 1..=(data.len() - window) {
                rc.roll(data[i - 1], data[i + window - 1]);
                acc = acc.wrapping_add(u64::from(rc.value()));
            }
            black_box(acc)
        });
    });
    group.finish();
}

fn bench_rolling_new(c: &mut Criterion) {
    let data = buf(128 * 1024, 3); // 128 KiB
    let mut group = c.benchmark_group("rolling_checksum_new");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("new_128KiB", |b| {
        b.iter(|| black_box(RollingChecksum::new(black_box(&data))));
    });
    group.finish();
}

fn bench_encode(c: &mut Criterion) {
    let old = buf(1 << 20, 2); // 1 MiB
    // `new` = old with a 1 KiB patch in the middle (the realistic delta case).
    let mut new = old.clone();
    for (i, byte) in new.iter_mut().skip(old.len() / 2).take(1024).enumerate() {
        *byte = (i & 0xFF) as u8;
    }

    let mut group = c.benchmark_group("delta");
    group.throughput(Throughput::Bytes(new.len() as u64));
    group.bench_function("encode_1MiB_small_change", |b| {
        b.iter(|| black_box(encode(black_box(&old), black_box(&new), None)));
    });

    let delta = encode(&old, &new, None);
    group.bench_function("apply_1MiB", |b| {
        b.iter(|| black_box(apply(black_box(&old), black_box(&delta)).unwrap()));
    });
    group.finish();
}

criterion_group!(benches, bench_rolling, bench_rolling_new, bench_encode);
criterion_main!(benches);
