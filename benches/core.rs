use criterion::{criterion_group, criterion_main, Criterion};
use ninemm::movegen::moves_movement;

fn bench_movegen(c: &mut Criterion) {
    let white = 0b0000_0000_1110_0111u32;
    let black = 0b0111_0011_0000_1000_0000u32;
    c.bench_function("moves_movement", |b| {
        b.iter(|| moves_movement(white, black))
    });
}

criterion_group!(benches, bench_movegen);
criterion_main!(benches);
