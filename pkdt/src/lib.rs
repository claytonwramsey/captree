#![feature(portable_simd)]
#![feature(new_uninit)]
#![warn(clippy::pedantic)]

use std::{
    hint::unreachable_unchecked,
    simd::{LaneCount, Mask, Simd, SimdConstPtr, SimdPartialOrd, SupportedLaneCount},
};

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
    /// Construct a new `PkdTree` containing all the points in `points`.
    /// For performance, this function changes the ordering of `points`, but does not affect the
    /// set of points inside it.
    ///
    /// TODO: do all our sorting on the allocation that we return?
    pub fn new(points: &[[f32; D]]) -> Self {
        /// Recursive helper function to sort the points for the KD tree and generate the tests.
        fn recur_sort_points<const D: usize>(
            points: &mut [[f32; D]],
            tests: &mut [f32],
            d: usize,
            i: usize,
        ) {
            // TODO make this algorithm O(n log n) instead of O(n log^2 n)
            if points.len() > 1 {
                points.sort_unstable_by(|a, b| a[d].partial_cmp(&b[d]).unwrap());
                let median = (points[points.len() / 2 - 1][d] + points[points.len() / 2][d]) / 2.0;
                tests[i] = median;
                let next_dim = (d + 1) % D;
                let (lhs, rhs) = points.split_at_mut(points.len() / 2);
                recur_sort_points(lhs, tests, next_dim, 2 * i + 1);
                recur_sort_points(rhs, tests, next_dim, 2 * i + 2);
            }
        }

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
            test_idxs <<= Simd::splat(1);
            test_idxs += cmp_results.select(Simd::splat(1), Simd::splat(2));
        }

        (test_idxs - Simd::splat(self.tests.len())).into()
    }

    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::similar_names)]
    /// Query in parallel, bailing at height `bail_height` and then linearly searching all
    /// candidates for the nearest neighbors below `bail_height`.
    ///
    /// # Panics
    ///
    /// This function will panic f `bail_height` is greater than the depth of the search tree.
    pub fn query_bail<const L: usize>(&self, needles: &[[f32; L]; D], bail_height: u8) -> [usize; L]
    where
        LaneCount<L>: SupportedLaneCount,
    {
        let n2 = self.tests.len() + 1;
        debug_assert!(n2.is_power_of_two());

        // in release mode, tell the compiler about this invariant
        if !n2.is_power_of_two() {
            unsafe { unreachable_unchecked() };
        }

        assert!(bail_height <= n2.ilog2() as u8);
        let mut test_idxs: Simd<usize, L> = Simd::splat(0);

        // Advance the tests forward
        for i in 0..n2.ilog2() as u8 - bail_height {
            let test_ptrs = Simd::splat((self.tests.as_ref() as *const [f32]).cast::<f32>())
                .wrapping_add(test_idxs);
            let relevant_tests: Simd<f32, L> = unsafe { Simd::gather_ptr(test_ptrs) };
            let needle_values = Simd::from_array(needles[i as usize % D]);
            let cmp_results: Mask<isize, L> = needle_values.simd_lt(relevant_tests).into();

            // TODO is there a faster way than using a conditional select?
            test_idxs <<= Simd::splat(1);
            test_idxs += cmp_results.select(Simd::splat(1), Simd::splat(2));
        }

        // linear search for the nearest one
        let mut point_idxs = (test_idxs << Simd::splat(bail_height as usize))
            + Simd::splat((1 << bail_height) - 1)
            - Simd::splat(self.tests.len());

        let mut best_dists = Simd::splat(0.0);
        let mut best_idxs = point_idxs;
        for d in 0..D {
            unsafe {
                let point_base = (self.points.as_ref() as *const [f32])
                    .cast::<f32>()
                    .add(d * n2);
                let point_ptrs = Simd::splat(point_base).wrapping_add(point_idxs);

                let point_values = Simd::gather_ptr(point_ptrs);
                let diffs = Simd::from_array(needles[d % D]) - point_values;
                best_dists += diffs * diffs;
            }
        }

        for _ in 1..1 << bail_height {
            point_idxs += Simd::splat(1);
            let mut dists = Simd::splat(0.0);
            for d in 0..D {
                unsafe {
                    let point_base = (self.points.as_ref() as *const [f32])
                        .cast::<f32>()
                        .add(d * n2);
                    let point_ptrs = Simd::splat(point_base).wrapping_add(point_idxs);

                    let point_values = Simd::gather_ptr(point_ptrs);
                    let diffs = Simd::from_array(needles[d % D]) - point_values;
                    dists += diffs * diffs;
                }
                // println!("{best_dists:?} vs {dists:?}");
            }

            let was_dists_lower = dists.simd_lt(best_dists);
            best_dists = was_dists_lower.select(dists, best_dists);
            let wdl_isize: Mask<isize, L> = was_dists_lower.into();
            best_idxs = wdl_isize.select(point_idxs, best_idxs);
        }

        best_idxs.into()
    }

    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    /// Get the access index of the point closest to `needle`
    pub fn query1(&self, needle: [f32; D]) -> usize {
        let n2 = self.tests.len() + 1;
        assert!(n2.is_power_of_two());

        let mut test_idx = 0;
        for i in 0..n2.ilog2() as usize {
            // println!("current idx: {test_idx}");
            let add = if needle[i % D] < (self.tests[test_idx]) {
                1
            } else {
                2
            };
            test_idx <<= 1;
            test_idx += add;
        }

        test_idx - self.tests.len()
    }

    #[must_use]
    /// Query for one point in this tree, returning an exact answer.
    pub fn query1_exact(&self, needle: [f32; D]) -> usize {
        let n2 = self.tests.len() + 1;
        let mut guess = self.query1(needle);
        let mut test_idx = guess + self.tests.len();

        let mut distance = dist(self.get_point(guess), needle);
        let mut i = 0;

        while test_idx != 0 {
            let last_test_idx = test_idx;
            test_idx = (test_idx + 1) / 2 - 1;
            assert!(2 * test_idx + 1 == last_test_idx || 2 * test_idx + 2 == last_test_idx);
            let d = i % D;

            if (needle[d] - self.tests[test_idx]).abs() < distance {
                // needle is close enough to test plane to justify re-searching

                // new_test_idx starts on the opposite side of the test plane
                let mut new_test_idx = 2 * test_idx + 1 + last_test_idx % 2;
                assert_ne!(new_test_idx, last_test_idx);
                for j in (n2.ilog2() as usize - i)..n2.ilog2() as usize {
                    let add = if needle[j % D] < (self.tests[new_test_idx]) {
                        1
                    } else {
                        2
                    };
                    new_test_idx <<= 1;
                    new_test_idx += add;
                }

                let new_guess = new_test_idx - self.tests.len();
                let new_distance = dist(self.get_point(new_guess), needle);

                if new_distance < distance {
                    distance = new_distance;
                    guess = new_guess;
                    test_idx = new_test_idx;
                    i = 0;
                    continue;
                }
            }
            i += 1;
        }

        guess
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

fn dist<const D: usize>(a: [f32; D], b: [f32; D]) -> f32 {
    a.into_iter()
        .zip(b)
        .map(|(x1, x2)| (x1 - x2).powi(2))
        .sum::<f32>()
        .sqrt()
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
    fn bailing() {
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
        assert_eq!(kdt.query_bail(&needles, 1), [0, points.len() - 1]);
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
