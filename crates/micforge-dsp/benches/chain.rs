//! İşleme zinciri için blok başına süre ölçümü.
//!
//! Her iterasyon ses thread'inin bir blokta yaptığı işi taklit eder:
//! `update` (parametre okuma) + `process`. Girdi, 48 kHz'de ~−20 dBFS tepe
//! genlikli, sabit tohumlu pseudo-rastgele gürültüdür. Ölçümden önce zincir
//! ~1 sn ses ile ısıtılır.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use micforge_dsp::chain::{Chain, MODULE_TOGGLES};
use micforge_dsp::params::{EqField, ParamId, ParamStore};

const SAMPLE_RATE: f32 = 48_000.0;
const BLOCK: usize = 256;
const WARM_SAMPLES: usize = 48_000;
/// Tepe genlik: 0.1 ≈ −20 dBFS.
const AMP: f32 = 0.1;

/// Sabit tohumlu LCG gürültüsü, `[-amp, amp]` aralığında.
fn noise(len: usize, amp: f32) -> Vec<f32> {
    let mut seed = 1u32;
    (0..len)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

/// Verilen modülleri açar.
fn enable(p: &ParamStore, ids: &[ParamId]) {
    for &id in ids {
        p.set(id, 1.0);
    }
}

fn voice_typical() -> ParamStore {
    let p = ParamStore::default();
    enable(
        &p,
        &[
            ParamId::HpfOn,
            ParamId::GateOn,
            ParamId::CompOn,
            ParamId::DeessOn,
            ParamId::EqOn,
        ],
    );
    for (band, gain) in [(0u8, 3.0), (1, -2.0), (2, 4.0), (3, 2.0)] {
        p.set(ParamId::Eq(band, EqField::Gain), gain);
    }
    p
}

fn noise_suppression() -> ParamStore {
    let p = ParamStore::default();
    enable(&p, &[ParamId::RnnoiseOn, ParamId::SpecGateOn]);
    p
}

fn effects() -> ParamStore {
    let p = ParamStore::default();
    p.set(ParamId::PitchSemitones, 5.0);
    enable(
        &p,
        &[
            ParamId::PitchOn,
            ParamId::VoiceOn,
            ParamId::ChorusOn,
            ParamId::DelayOn,
            ParamId::ReverbOn,
        ],
    );
    p
}

fn everything() -> ParamStore {
    let p = ParamStore::default();
    enable(&p, &MODULE_TOGGLES);
    p
}

/// Zinciri kurar, RNNoise'u ısıtır ve `WARM_SAMPLES` sesi `block` parçalarıyla işler.
fn warmed_chain(params: &ParamStore, block: usize) -> Chain {
    let mut chain = Chain::new(SAMPLE_RATE, params);
    chain.warm_up();
    let mut warm = noise(WARM_SAMPLES, AMP);
    for chunk in warm.chunks_mut(block) {
        chain.update(params);
        chain.process(chunk);
    }
    chain
}

/// Ölçüm döngüsü: her blokta girdiyi kopyala, `update` + `process` çalıştır.
fn bench_block(b: &mut criterion::Bencher, params: &ParamStore, block: usize) {
    let mut chain = warmed_chain(params, block);
    let input = noise(block, AMP);
    let mut buf = input.clone();
    b.iter(|| {
        buf.copy_from_slice(&input);
        chain.update(params);
        chain.process(&mut buf);
        black_box(&buf);
    });
}

fn chain_groups(c: &mut Criterion) {
    let scenarios = [
        ("default", ParamStore::default()),
        ("voice_typical", voice_typical()),
        ("noise_suppression", noise_suppression()),
        ("effects", effects()),
        ("everything", everything()),
    ];

    let mut group = c.benchmark_group("chain");
    group.throughput(Throughput::Elements(BLOCK as u64));
    for (name, params) in &scenarios {
        group.bench_function(*name, |b| bench_block(b, params, BLOCK));
    }
    group.finish();
}

fn everything_by_block(c: &mut Criterion) {
    let params = everything();
    let mut group = c.benchmark_group("everything_by_block");
    for block in [64usize, BLOCK, 1024] {
        group.throughput(Throughput::Elements(block as u64));
        group.bench_with_input(BenchmarkId::from_parameter(block), &block, |b, &block| {
            bench_block(b, &params, block)
        });
    }
    group.finish();
}

criterion_group!(benches, chain_groups, everything_by_block);
criterion_main!(benches);
