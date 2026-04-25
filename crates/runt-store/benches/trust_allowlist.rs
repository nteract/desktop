//! libSQL-backed trust allowlist benchmarks.
//!
//! Same shape as the Lance spike's benches (PR #2176) — any divergence
//! in numbers is a real backend difference.
//!
//! Run with:
//!   cargo bench -p runt-store --bench trust_allowlist

use std::time::Instant;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use runt_store::{PackageManager, TrustAllowlist};
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn seed_names(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}-pkg-{i:05}")).collect()
}

async fn open_seeded(tmp: &TempDir, count: usize) -> TrustAllowlist {
    let store = TrustAllowlist::open(tmp.path()).await.unwrap();
    let names = seed_names("seed", count);
    store.add(PackageManager::Uv, &names).await.unwrap();
    store
}

fn bench_cold_load(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("cold_load");
    for &count in &[100usize, 1_000, 10_000] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(format!("rows={count}"), |b| {
            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let tmp = TempDir::new().unwrap();
                    rt.block_on(async {
                        let store = TrustAllowlist::open(tmp.path()).await.unwrap();
                        let names = seed_names("cold", count);
                        store.add(PackageManager::Uv, &names).await.unwrap();
                        drop(store);
                    });
                    let start = Instant::now();
                    rt.block_on(async {
                        let _ = TrustAllowlist::open(tmp.path()).await.unwrap();
                    });
                    total += start.elapsed();
                }
                total
            });
        });
    }
    group.finish();
}

fn bench_contains(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let tmp = TempDir::new().unwrap();
    let store = rt.block_on(open_seeded(&tmp, 1_000));

    let mut group = c.benchmark_group("contains");
    group.bench_function("hit", |b| {
        b.iter(|| store.contains(PackageManager::Uv, "seed-pkg-00500"));
    });
    group.bench_function("miss", |b| {
        b.iter(|| store.contains(PackageManager::Uv, "this-will-never-match"));
    });
    group.finish();
}

fn bench_novel(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let tmp = TempDir::new().unwrap();
    let store = rt.block_on(open_seeded(&tmp, 1_000));

    let candidates: Vec<String> = (0..18)
        .map(|i| format!("seed-pkg-{i:05}"))
        .chain((0..2).map(|i| format!("brand-new-pkg-{i}")))
        .collect();

    c.bench_function("novel_20_vs_1000", |b| {
        b.iter(|| {
            let refs = candidates
                .iter()
                .map(|s| (PackageManager::Uv, s.as_str()))
                .collect::<Vec<_>>();
            let _ = store.novel(refs);
        });
    });
}

fn bench_add_single(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("add_single");
    group.sample_size(30);
    group.bench_function("append_one", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for i in 0..iters {
                let tmp = TempDir::new().unwrap();
                let store = rt.block_on(TrustAllowlist::open(tmp.path())).unwrap();
                let name = format!("one-off-{i}");
                let start = Instant::now();
                rt.block_on(store.add(PackageManager::Uv, &[name.clone()]))
                    .unwrap();
                total += start.elapsed();
            }
            total
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_cold_load,
    bench_contains,
    bench_novel,
    bench_add_single
);
criterion_main!(benches);
