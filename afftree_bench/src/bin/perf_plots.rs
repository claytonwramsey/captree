#![feature(portable_simd)]

use std::cmp::min;
use std::env::args;
use std::error::Error;
use std::fs::File;
use std::hint::black_box;
use std::io::Write;

use afftree::{AffordanceTree, PkdTree};
use afftree_bench::{
    fuzz_pointcloud, parse_pointcloud_csv, parse_trace_csv, simd_trace_new, stopwatch, SimdTrace,
    Trace,
};
#[allow(unused_imports)]
use kiddo::SquaredEuclidean;
use morton_filter::morton_filter;
use rand::seq::SliceRandom;
use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha20Rng;

const N_TRIALS: usize = 100_000;
const L: usize = 8;

const QUERY_RADIUS: f32 = 0.05;

struct Benchmark<'a> {
    seq: &'a Trace,
    simd: &'a SimdTrace<L>,
    f_query: File,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut f_construct = File::create("construct_time.csv")?;
    let mut f_mem = File::create("mem.csv")?;

    let args: Vec<String> = args().collect();

    let mut rng = ChaCha20Rng::seed_from_u64(2707);
    let points: Box<[[f32; 3]]> = if args.len() < 2 {
        (0..1 << 16)
            .map(|_| {
                [
                    rng.gen_range::<f32, _>(0.0..1.0),
                    rng.gen_range::<f32, _>(0.0..1.0),
                    rng.gen_range::<f32, _>(0.0..1.0),
                ]
            })
            .collect()
    } else {
        let mut p = parse_pointcloud_csv(&args[1])?.to_vec();
        fuzz_pointcloud(&mut p, 0.001, &mut rng);
        p.shuffle(&mut rng);
        p.truncate(1 << 16);
        p.into_boxed_slice()
    };

    let all_trace: Box<[([f32; 3], f32)]> = if args.len() < 3 {
        (0..N_TRIALS)
            .map(|_| {
                (
                    [
                        rng.gen_range(0.0..1.0),
                        rng.gen_range(0.0..1.0),
                        rng.gen_range(0.0..1.0),
                    ],
                    rng.gen_range(0.0..=QUERY_RADIUS),
                )
            })
            .collect()
    } else {
        parse_trace_csv(&args[2])?
    };

    // let rsq_range = (
    //     all_trace
    //         .iter()
    //         .map(|x| x.1)
    //         .min_by(|a, b| a.partial_cmp(b).unwrap())
    //         .ok_or("no points")?
    //         .powi(2),
    //     all_trace
    //         .iter()
    //         .map(|x| x.1)
    //         .max_by(|a, b| a.partial_cmp(b).unwrap())
    //         .ok_or("no points")?
    //         .powi(2),
    // );
    let rsq_range = (0.01 * 0.01, 0.08 * 0.08);

    println!("number of points: {}", points.len());
    println!("number of tests: {}", all_trace.len());
    println!("radius-squared range: {rsq_range:?}");

    let afftree = AffordanceTree::<3>::new(&points, rsq_range).unwrap();

    let collide_trace: Box<Trace> = all_trace
        .iter()
        .filter(|(center, r)| afftree.collides(center, *r))
        .copied()
        .collect();

    let no_collide_trace: Box<Trace> = all_trace
        .iter()
        .filter(|(center, r)| !afftree.collides(center, *r))
        .copied()
        .collect();

    let all_simd_trace = simd_trace_new(&all_trace);
    let collide_simd_trace = simd_trace_new(&collide_trace);
    let no_collide_simd_trace = simd_trace_new(&no_collide_trace);

    let mut benchmarks = [
        Benchmark {
            seq: &all_trace,
            simd: &all_simd_trace,
            f_query: File::create("mixed.csv")?,
        },
        Benchmark {
            seq: &collide_trace,
            simd: &collide_simd_trace,
            f_query: File::create("collides.csv")?,
        },
        Benchmark {
            seq: &no_collide_trace,
            simd: &no_collide_simd_trace,
            f_query: File::create("no_collides.csv")?,
        },
    ];

    let mut r_filter = 0.001;
    loop {
        let mut new_points = points.to_vec();
        morton_filter(&mut new_points, r_filter);
        do_row(
            &new_points,
            &mut benchmarks,
            rsq_range,
            &mut f_construct,
            &mut f_mem,
        )?;
        r_filter *= 1.03;
        if new_points.len() < 500 {
            break;
        }
    }

    Ok(())
}

fn do_row(
    points: &[[f32; 3]],
    benchmarks: &mut [Benchmark],
    rsq_range: (f32, f32),
    f_construct: &mut File,
    f_mem: &mut File,
) -> Result<(), Box<dyn Error>> {
    let (kdt, kdt_time) = stopwatch(|| kiddo::ImmutableKdTree::new_from_slice(points));

    let (pkdt, pkdt_time) = stopwatch(|| PkdTree::new(points));

    let (afftree, afftree_time) =
        stopwatch(|| AffordanceTree::<3>::new(points, rsq_range).unwrap());
    writeln!(
        f_construct,
        "{},{},{},{}",
        points.len(),
        kdt_time.as_secs_f64(),
        pkdt_time.as_secs_f64(),
        afftree_time.as_secs_f64(),
    )?;

    writeln!(
        f_mem,
        "{},{},{}",
        points.len(),
        pkdt.memory_used(),
        afftree.memory_used()
    )?;

    for Benchmark {
        seq: trace,
        simd: simd_trace,
        f_query,
    } in benchmarks
    {
        let (_, kdt_within_q_time) = stopwatch(|| {
            for (center, radius) in trace.iter() {
                black_box(
                    kdt.within_unsorted::<SquaredEuclidean>(center, radius.powi(2))
                        .is_empty(),
                );
            }
        });
        let (_, kdt_nearest_q_time) = stopwatch(|| {
            for (center, radius) in trace.iter() {
                black_box(kdt.nearest_one::<SquaredEuclidean>(center).distance <= radius.powi(2));
            }
        });
        let kdt_total_q_time = min(kdt_within_q_time, kdt_nearest_q_time);

        let (_, pkdt_total_seq_q_time) = stopwatch(|| {
            for (center, radius) in trace.iter() {
                black_box(pkdt.might_collide(*center, radius.powi(2)));
            }
        });
        let (_, pkdt_total_simd_q_time) = stopwatch(|| {
            for (centers, radii) in simd_trace.iter() {
                black_box(pkdt.might_collide_simd(centers, radii * radii));
            }
        });
        let (_, afftree_total_seq_q_time) = stopwatch(|| {
            for (center, radius) in trace.iter() {
                black_box(afftree.collides(center, radius.powi(2)));
            }
        });
        let (_, afftree_total_simd_q_time) = stopwatch(|| {
            for (centers, radii) in simd_trace.iter() {
                black_box(afftree.collides_simd(centers, radii * radii));
            }
        });

        let trace_len = trace.len() as f64;
        writeln!(
            f_query,
            "{},{},{},{},{},{},{}",
            points.len(),
            trace.len(),
            kdt_total_q_time.as_secs_f64() / trace_len,
            pkdt_total_seq_q_time.as_secs_f64() / trace_len,
            pkdt_total_simd_q_time.as_secs_f64() / trace_len,
            afftree_total_seq_q_time.as_secs_f64() / trace_len,
            afftree_total_simd_q_time.as_secs_f64() / trace_len,
        )?;
    }

    Ok(())
}
