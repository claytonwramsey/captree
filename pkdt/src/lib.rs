#![feature(portable_simd)]
#![feature(new_uninit)]
#![warn(clippy::pedantic)]

use std::{
    hint::unreachable_unchecked,
    simd::{LaneCount, Mask, Simd, SimdConstPtr, SimdPartialOrd, SupportedLaneCount},
};

mod forest;

pub use forest::PkdForest;

#[derive(Clone, Debug, PartialEq)]
/// A power-of-two KD-tree.
///
/// # Generic parameters
///
/// - `D`: The dimension of the space.
pub struct PkdTree<const D: usize> {
    /// The test values for determining which part of the tree to enter.
    ///
    /// The first element of `tests` should be the first value to test against.
    /// If we are less than `tests[0]`, we move on to `tests[1]`; if not, we move on to `tests[2]`.
    /// At the `i`-th test performed in sequence of the traversal, if we are less than `tests[idx]`,
    /// we advance to `2 * idx + 1`; otherwise, we go to `2 * idx + 2`.
    ///
    /// The length of `tests` must be `N`, rounded up to the next power of 2, minus one.
    tests: Box<[f32]>,
    /// The relevant points at the center of each volume divided by `tests`.
    ///
    /// If there are `N` points in the tree, let `N2` be `N` rounded up to the next power of 2.
    /// Then `points` has length `N2 * D`.
    points: Box<[f32]>,
}

impl<const D: usize> PkdTree<D> {
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    /// Construct a new `PkdTree` containing all the points in `points`.
    /// For performance, this function changes the ordering of `points`, but does not affect the
    /// set of points inside it.
    ///
    /// # Panics
    ///
    /// This function will panic if `D` is greater than or equal to 255.
    ///
    /// TODO: do all our sorting on the allocation that we return?
    pub fn new(points: &[[f32; D]]) -> Self {
        /// Recursive helper function to sort the points for the KD tree and generate the tests.
        fn recur_sort_points<const D: usize>(
            points: &mut [[f32; D]],
            tests: &mut [f32],
            d: u8,
            i: usize,
        ) {
            // TODO make this algorithm O(n log n) instead of O(n log^2 n)
            if points.len() > 1 {
                let halflen = points.len() / 2;
                points.sort_unstable_by(|a, b| a[d as usize].partial_cmp(&b[d as usize]).unwrap());
                let median = (points[halflen - 1][d as usize]
                    + points[halflen][d as usize])
                    / 2.0;
                tests[i] = median;
                let next_dim = (d + 1) % D as u8;
                let (lhs, rhs) = points.split_at_mut(halflen);
                recur_sort_points(lhs, tests, next_dim, i + 1);
                recur_sort_points(rhs, tests, next_dim, i + halflen);
            }
        }

        assert!(D < u8::MAX as usize);

        let n2 = points.len().next_power_of_two();

        let mut tests = vec![f32::INFINITY; n2 - 1].into_boxed_slice();

        // hack: just pad with infinity to make it a power of 2
        let mut new_points = vec![[f32::INFINITY; D]; n2];
        new_points[..points.len()].copy_from_slice(points);
        recur_sort_points(new_points.as_mut(), tests.as_mut(), 0, 0);

        let mut my_points = vec![f32::NAN; n2 * D].into_boxed_slice();
        for (i, pt) in new_points.iter().enumerate() {
            for (d, value) in (*pt).into_iter().enumerate() {
                my_points[d * n2 + i] = value;
            }
        }

        PkdTree {
            tests,
            points: my_points,
        }
    }

    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    /// Get the indices of points which are closest to `needles`.
    ///
    /// TODO: refactor this to use `needles` as an out parameter as well, and shove the nearest
    /// points in there?
    pub fn query<const L: usize>(&self, needles: &[[f32; L]; D]) -> [usize; L]
    where
        LaneCount<L>: SupportedLaneCount,
    {
        let mut test_idxs: Simd<usize, L> = Simd::splat(0);
        let n2 = self.tests.len() + 1;
        let mut increment = n2 / 2;
        debug_assert!(n2.is_power_of_two());

        // in release mode, tell the compiler about this invariant
        if !n2.is_power_of_two() {
            unsafe { unreachable_unchecked() };
        }

        // Advance the tests forward
        for i in 0..n2.ilog2() as usize {
            let test_ptrs = Simd::splat((self.tests.as_ref() as *const [f32]).cast::<f32>())
                .wrapping_add(test_idxs);
            let relevant_tests: Simd<f32, L> = unsafe { Simd::gather_ptr(test_ptrs) };
            let needle_values = Simd::from_array(needles[i % D]);
            let cmp_results: Mask<isize, L> = needle_values.simd_lt(relevant_tests).into();

            // TODO is there a faster way than using a conditional select?
            test_idxs += cmp_results.select(Simd::splat(1), Simd::splat(increment));
            increment >>= 1;
        }

        (test_idxs - Simd::splat(self.tests.len())).into()
    }

    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    /// Get the access index of the point closest to `needle`
    pub fn query1(&self, needle: [f32; D]) -> usize {
        let n2 = self.tests.len() + 1;
        assert!(n2.is_power_of_two());

        let mut test_idx = 0;
        let mut increment = n2 / 2;
        for i in 0..n2.ilog2() as usize {
            // println!("current idx: {test_idx}");
            if needle[i % D] < self.tests[test_idx] {
                test_idx += 1;
            } else {
                test_idx += increment;
            };
            increment >>= 1;
        }

        test_idx - self.tests.len()
    }

    #[must_use]
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    /// Query for one point in this tree, returning an exact answer.
    pub fn query1_exact(&self, needle: [f32; D]) -> usize {
        let mut id = usize::MAX;
        let mut best_distsq = f32::INFINITY;
        self.exact_help(
            0,
            0,
            &[[-f32::INFINITY, f32::INFINITY]; D],
            needle,
            &mut id,
            &mut best_distsq,
        );
        id
    }

    #[allow(clippy::cast_possible_truncation)]
    fn exact_help(
        &self,
        test_idx: usize,
        d: u8,
        bounding_box: &[[f32; 2]; D],
        point: [f32; D],
        best_id: &mut usize,
        best_distsq: &mut f32,
    ) {
        if bb_distsq(point, bounding_box) > *best_distsq {
            return;
        }

        if self.tests.len() <= test_idx {
            let id = test_idx - self.tests.len();
            let new_distsq = distsq(point, self.get_point(id));
            if new_distsq < *best_distsq {
                *best_id = id;
                *best_distsq = new_distsq;
            }

            return;
        }

        let test = self.tests[test_idx];

        let mut bb_below = *bounding_box;
        bb_below[d as usize][1] = test;
        let mut bb_above = *bounding_box;
        bb_above[d as usize][0] = test;

        let next_d = (d + 1) % D as u8;
        if point[d as usize] < test {
            self.exact_help(
                test_idx + 1,
                next_d,
                &bb_below,
                point,
                best_id,
                best_distsq,
            );
            self.exact_help(
                2 * test_idx + 2,
                next_d,
                &bb_above,
                point,
                best_id,
                best_distsq,
            );
        } else {
            self.exact_help(
                2 * test_idx + 2,
                next_d,
                &bb_above,
                point,
                best_id,
                best_distsq,
            );
            self.exact_help(
                2 * test_idx + 1,
                next_d,
                &bb_below,
                point,
                best_id,
                best_distsq,
            );
        }
    }

    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn get_point(&self, id: usize) -> [f32; D] {
        let mut point = [0.0; D];
        let n2 = self.tests.len() + 1;
        assert!(n2.is_power_of_two());
        for (d, value) in point.iter_mut().enumerate() {
            *value = self.points[d * n2 + id];
        }

        point
    }
}

fn bb_distsq<const D: usize>(point: [f32; D], bb: &[[f32; 2]; D]) -> f32 {
    point
        .into_iter()
        .zip(bb.iter())
        .map(|(x, [lower, upper])| {
            (if x < *lower {
                *lower - x
            } else if *upper < x {
                x - *upper
            } else {
                0.0
            })
            .powi(2)
        })
        .sum()
}

fn distsq<const D: usize>(a: [f32; D], b: [f32; D]) -> f32 {
    a.into_iter()
        .zip(b)
        .map(|(x1, x2)| (x1 - x2).powi(2))
        .sum::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_query() {
        let points = vec![
            [0.1, 0.1],
            [0.1, 0.2],
            [0.5, 0.0],
            [0.3, 0.9],
            [1.0, 1.0],
            [0.35, 0.75],
            [0.6, 0.2],
            [0.7, 0.8],
        ];
        let kdt = PkdTree::new(&points);

        println!("testing for correctness...");

        let neg1 = [-1.0, -1.0];
        let neg1_idx = kdt.query1(neg1);
        assert_eq!(neg1_idx, 0);

        let pos1 = [1.0, 1.0];
        let pos1_idx = kdt.query1(pos1);
        assert_eq!(pos1_idx, points.len() - 1);
    }

    #[test]
    fn multi_query() {
        let points = vec![
            [0.1, 0.1],
            [0.1, 0.2],
            [0.5, 0.0],
            [0.3, 0.9],
            [1.0, 1.0],
            [0.35, 0.75],
            [0.6, 0.2],
            [0.7, 0.8],
        ];
        let kdt = PkdTree::new(&points);

        let needles = [[-1.0, 2.0], [-1.0, 2.0]];
        assert_eq!(kdt.query(&needles), [0, points.len() - 1]);
    }

    #[test]
    fn not_a_power_of_two() {
        let points = vec![[0.0], [2.0], [4.0]];
        let kdt = PkdTree::new(&points);

        println!("{kdt:?}");

        assert_eq!(kdt.query1([-1.0]), 0);
        assert_eq!(kdt.query1([0.5]), 0);
        assert_eq!(kdt.query1([1.5]), 1);
        assert_eq!(kdt.query1([2.5]), 1);
        assert_eq!(kdt.query1([3.5]), 2);
        assert_eq!(kdt.query1([4.5]), 2);
    }

    #[test]
    fn a_power_of_two() {
        let points = vec![[0.0], [2.0], [4.0], [6.0]];
        let kdt = PkdTree::new(&points);

        println!("{kdt:?}");

        assert_eq!(kdt.query1([-1.0]), 0);
        assert_eq!(kdt.query1([0.5]), 0);
        assert_eq!(kdt.query1([1.5]), 1);
        assert_eq!(kdt.query1([2.5]), 1);
        assert_eq!(kdt.query1([3.5]), 2);
        assert_eq!(kdt.query1([4.5]), 2);
    }
}
