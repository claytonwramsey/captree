#![feature(portable_simd)]

use std::{hint::black_box, simd::Simd, time::Instant};

use kiddo::SquaredEuclidean;
use pkdt::{AffordanceTree, PkdForest};
use pkdt_bench::get_points;
use rand::{seq::SliceRandom, Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

const N: usize = 1 << 12;
const L: usize = 16;

fn main() {
    let mut points = get_points(N);
    let mut rng = ChaCha20Rng::seed_from_u64(2707);
    points.shuffle(&mut rng);
    let n_trials = 1 << 20;

    println!("{} points", points.len());
    println!("generating PKDT...");
    let tic = Instant::now();
    let kdt = pkdt::PkdTree::new(&points);
    println!("generated tree in {:?}", Instant::now().duration_since(tic));

    println!("generating competitor's KDT");
    let tic = Instant::now();
    let kiddo_kdt = kiddo::ImmutableKdTree::new_from_slice(&points);
    println!(
        "generated kiddo tree in {:?}",
        Instant::now().duration_since(tic)
    );

    println!("forward tree memory: {:?}B", kdt.memory_used());

    println!("testing for performance...");

    let (seq_needles, simd_needles) = pkdt_bench::make_needles(&mut rng, n_trials);
    // let (seq_needles, simd_needles) = pkdt_bench::make_correlated_needles(&mut rng, n_trials);

    bench_affordance(&points, &simd_needles, &seq_needles, &mut rng);

    println!("testing sequential...");

    let tic = Instant::now();
    for needle in &seq_needles {
        black_box(kiddo_kdt.within_unsorted::<SquaredEuclidean>(needle, 0.01f32.powi(2)));
    }
    let toc = Instant::now();
    let kiddo_range_time = toc.duration_since(tic);

    println!(
        "completed kiddo (range) in {:?} ({:?}/q)",
        kiddo_range_time,
        kiddo_range_time / seq_needles.len() as u32
    );

    let tic = Instant::now();
    for needle in &seq_needles {
        black_box(kiddo_kdt.nearest_one::<SquaredEuclidean>(needle).distance < 0.01f32.powi(2));
    }
    let toc = Instant::now();
    let kiddo_exact_time = toc.duration_since(tic);

    println!(
        "completed kiddo (exact) in {:?} ({:?}/q)",
        kiddo_exact_time,
        kiddo_exact_time / seq_needles.len() as u32
    );

    // let tic = Instant::now();
    // for needle in &seq_needles {
    //     black_box(kdt.query1_exact(*needle));
    // }
    // let toc = Instant::now();
    // let exact_time = toc.duration_since(tic);
    // println!(
    //     "completed exact in {:?} ({:?}/q)",
    //     exact_time,
    //     exact_time / seq_needles.len() as u32
    // );

    let tic = Instant::now();
    for &needle in &seq_needles {
        black_box(kdt.might_collide(needle, 0.0001));
    }
    let toc = Instant::now();
    let seq_time = toc.duration_since(tic);
    println!(
        "completed sequential in {:?} ({:?}/q)",
        seq_time,
        seq_time / seq_needles.len() as u32
    );

    bench_forest::<1>(&points, &simd_needles, &mut rng);
    bench_forest::<2>(&points, &simd_needles, &mut rng);
    bench_forest::<3>(&points, &simd_needles, &mut rng);
    bench_forest::<4>(&points, &simd_needles, &mut rng);
    bench_forest::<5>(&points, &simd_needles, &mut rng);
    // bench_forest::<6>(&points, &simd_needles, &mut rng);
    // bench_forest::<7>(&points, &simd_needles, &mut rng);
    // bench_forest::<8>(&points, &simd_needles, &mut rng);
    // bench_forest::<9>(&points, &simd_needles, &mut rng);
    // bench_forest::<10>(&points, &simd_needles, &mut rng);

    let tic = Instant::now();
    for needle in &simd_needles {
        black_box(kdt.might_collide_simd::<L>(needle, Simd::splat(0.0001)));
    }
    let toc = Instant::now();
    let simd_time = toc.duration_since(tic);
    println!(
        "completed simd in {:?}s, ({:?}/pt, {:?}/q)",
        simd_time,
        simd_time / seq_needles.len() as u32,
        simd_time / simd_needles.len() as u32
    );

    println!(
        "speedup: {}% vs single-query",
        (100.0 * (seq_time.as_secs_f64() / simd_time.as_secs_f64() - 1.0)) as u64
    );
}

fn bench_affordance(
    points: &[[f32; 3]],
    simd_needles: &[[Simd<f32, L>; 3]],
    seq_needles: &[[f32; 3]],
    rng: &mut impl Rng,
) {
    let tic = Instant::now();
    let aff_tree = AffordanceTree::<3, _, u64>::new(
        points,
        (
            0.01f32.powi(2) - f32::EPSILON,
            0.01f32.powi(2) + f32::EPSILON,
        ),
        // (0.0f32, 0.02f32.powi(2)),
        rng,
    )
    .unwrap();
    let toc = Instant::now();
    println!("constructed affordance tree in {:?}", toc - tic);
    println!("affordance tree memory: {:?}B", aff_tree.memory_used());
    println!("affordance size: {}", aff_tree.affordance_size());

    let tic = Instant::now();
    for needle in seq_needles {
        black_box(aff_tree.collides(needle, 0.0001));
    }
    let toc = Instant::now();
    let aff_time = toc - tic;
    println!(
        "completed sequential queries for affordance trees in {:?} ({:?}/q)",
        aff_time,
        aff_time / seq_needles.len() as u32
    );

    let tic = Instant::now();
    for simd_needle in simd_needles {
        black_box(aff_tree.collides_simd(simd_needle, Simd::splat(0.0001)));
    }
    let toc = Instant::now();
    let aff_time = toc - tic;
    println!(
        "completed SIMD queries for affordance trees in {:?} ({:?}/pt, {:?}/q)",
        aff_time,
        aff_time / seq_needles.len() as u32,
        aff_time / simd_needles.len() as u32
    );
}

fn bench_forest<const T: usize>(
    points: &[[f32; 3]],
    simd_needles: &[[Simd<f32, L>; 3]],
    rng: &mut impl Rng,
) {
    let forest = PkdForest::<3, T>::new(points, rng);

    let tic = Instant::now();
    for needle in simd_needles {
        black_box(forest.might_collide_simd(needle, Simd::splat(0.01f32.powi(2))));
    }
    let toc = Instant::now();
    println!(
        "completed forest (T={T}) in {:?} ({:?}/pt, {:?}/q)",
        toc - tic,
        (toc - tic) / (simd_needles.len() * L) as u32,
        (toc - tic) / simd_needles.len() as u32,
    );
}
