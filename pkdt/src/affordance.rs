use std::{
    hint::unreachable_unchecked,
    mem::size_of,
    simd::{LaneCount, Mask, Simd, SimdConstPtr, SimdPartialOrd, SupportedLaneCount},
};

use rand::Rng;

use crate::{distsq, median_partition};

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::module_name_repetitions)]
/// An affordance tree, which allows for efficient nearest-neighbor-within-a-radius queries.
///
/// # Generic parameters
///
/// - `D`: The dimension of the space.
pub struct AffordanceTree<const D: usize> {
    /// The test values for determining which part of the tree to enter.
    ///
    /// The first element of `tests` should be the first value to test against.
    /// If we are less than `tests[0]`, we move on to `tests[1]`; if not, we move on to `tests[2]`.
    /// At the `i`-th test performed in sequence of the traversal, if we are less than `tests[idx]`,
    /// we advance to `2 * idx + 1`; otherwise, we go to `2 * idx + 2`.
    ///
    /// The length of `tests` must be `N`, rounded up to the next power of 2, minus one.
    tests: Box<[f32]>,
    /// The range of radii which are legal for queries on this tree.
    rsq_range: (f32, f32),
    /// Indexes for the starts of the affordance buffer subsequence of `points` corresponding to
    /// each leaf cell in the tree.
    aff_starts: Box<[usize]>,
    /// The relevant points which may collide with the outcome of some test.
    /// The affordance buffer for a point of index `i`
    points: Box<[[f32; D]]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// A prismatic bounding volume.
struct Volume<const D: usize> {
    lower: [f32; D],
    upper: [f32; D],
}

impl<const D: usize> AffordanceTree<D> {
    #[must_use]
    #[allow(clippy::too_many_lines)]
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
    pub fn new(points: &[[f32; D]], rsq_range: (f32, f32), rng: &mut impl Rng) -> Self {
        #[allow(clippy::float_cmp)]
        #[allow(clippy::too_many_arguments)]
        /// Recursive helper function to sort the points for the KD tree and generate the tests.
        /// Runs in O(n log n)
        fn build_tree<const D: usize>(
            points: &mut [[f32; D]],
            tests: &mut [f32],
            d: u8,
            i: usize,
            mut possible_collisions: Vec<[f32; D]>,
            volume: Volume<D>,
            ranges: &mut Vec<usize>,
            affordances: &mut Vec<[f32; D]>,
            rsq_range: (f32, f32),
            rng: &mut impl Rng,
        ) {
            if points.len() <= 1 {
                let cell_center = points[0];

                let (rsq_min, rsq_max) = rsq_range;

                possible_collisions.retain(|pt| {
                    let closest = volume.closest_point(pt);
                    let center_dist = distsq(cell_center, closest);
                    let closest_dist = distsq(*pt, closest);
                    cell_center != *pt
                        && closest_dist < rsq_max
                        && closest_dist < volume.furthest_distsq_to(&cell_center)
                        && rsq_min < center_dist
                });
                possible_collisions.push(cell_center);
                let l = possible_collisions.len();
                possible_collisions.swap(0, l - 1); // put the center at the front
                ranges.push(affordances.len());
                affordances.extend(possible_collisions);
            } else {
                tests[i] = median_partition(points, d as usize, rng);
                let next_dim = (d + 1) % D as u8;
                let (lhs, rhs) = points.split_at_mut(points.len() / 2);
                let (low_vol, hi_vol) = volume.split(tests[i], d as usize);
                let mut lo_afford = possible_collisions.clone();
                let mut hi_afford = possible_collisions;
                lo_afford.retain(|pt| {
                    rsq_range.0 < low_vol.furthest_distsq_to(pt)
                        && low_vol.distsq_to(pt) < rsq_range.1
                });
                hi_afford.retain(|pt| {
                    rsq_range.0 < hi_vol.furthest_distsq_to(pt)
                        && hi_vol.distsq_to(pt) < rsq_range.1
                });
                build_tree(
                    lhs,
                    tests,
                    next_dim,
                    2 * i + 1,
                    lo_afford,
                    low_vol,
                    ranges,
                    affordances,
                    rsq_range,
                    rng,
                );
                build_tree(
                    rhs,
                    tests,
                    next_dim,
                    2 * i + 2,
                    hi_afford,
                    hi_vol,
                    ranges,
                    affordances,
                    rsq_range,
                    rng,
                );
            }
        }

        assert!(D < u8::MAX as usize);

        let n2 = points.len().next_power_of_two();

        let mut tests = vec![f32::INFINITY; n2 - 1].into_boxed_slice();

        // hack: just pad with infinity to make it a power of 2
        let mut new_points = vec![[f32::INFINITY; D]; n2].into_boxed_slice();
        new_points[..points.len()].copy_from_slice(points);
        let mut points = Vec::with_capacity(n2);
        let possible_collisions = new_points.clone().to_vec();
        let mut ranges = Vec::new();
        build_tree(
            new_points.as_mut(),
            tests.as_mut(),
            0,
            0,
            possible_collisions,
            Volume {
                lower: [-f32::INFINITY; D],
                upper: [f32::INFINITY; D],
            },
            &mut ranges,
            &mut points,
            rsq_range,
            rng,
        );
        ranges.push(points.len());

        AffordanceTree {
            tests,
            rsq_range,
            aff_starts: ranges.into_boxed_slice(),
            points: points.into_boxed_slice(),
        }
    }

    #[must_use]
    /// Determine whether a point in this tree collides with a ball with radius squared `r_squared`.
    ///
    /// # Panics
    ///
    /// This function will panic if `r_squared` is outside the range of squared radii passed to the
    /// construction of the tree.
    /// TODO: implement real error handling.
    pub fn collides(&self, center: &[f32; D], r_squared: f32) -> bool {
        assert!(self.rsq_range.0 <= r_squared);
        assert!(r_squared <= self.rsq_range.1);

        let n2 = self.tests.len() + 1;
        assert!(n2.is_power_of_two());

        let mut test_idx = 0;
        for i in 0..n2.trailing_zeros() as usize {
            // println!("current idx: {test_idx}");
            let add = if center[i % D] < (self.tests[test_idx]) {
                1
            } else {
                2
            };
            test_idx <<= 1;
            test_idx += add;
        }

        let i = test_idx - self.tests.len();
        let range = self.aff_starts[i]..self.aff_starts[i + 1];

        self.points[range]
            .iter()
            .any(|pt| distsq(*pt, *center) <= r_squared)
    }

    #[must_use]
    /// Determine whether any sphere in the list of provided spheres intersects a point in this
    /// tree.
    pub fn collides_simd<const L: usize>(
        &self,
        centers: &[Simd<f32, L>],
        radii_squared: Simd<f32, L>,
    ) -> bool
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
        for i in 0..n2.trailing_zeros() as usize {
            let test_ptrs = Simd::splat((self.tests.as_ref() as *const [f32]).cast::<f32>())
                .wrapping_add(test_idxs);
            let relevant_tests: Simd<f32, L> = unsafe { Simd::gather_ptr(test_ptrs) };
            let cmp_results: Mask<isize, L> = centers[i % D].simd_lt(relevant_tests).into();

            // TODO is there a faster way than using a conditional select?
            test_idxs <<= Simd::splat(1);
            test_idxs += cmp_results.select(Simd::splat(1), Simd::splat(2));
        }

        let start_ptrs = Simd::splat((self.aff_starts.as_ref() as *const [usize]).cast::<usize>())
            .wrapping_add(test_idxs)
            .wrapping_sub(Simd::splat(self.tests.len()));
        let starts = unsafe { Simd::gather_ptr(start_ptrs) } * Simd::splat(D);
        let ends =
            unsafe { Simd::gather_ptr(start_ptrs.wrapping_add(Simd::splat(1))) } * Simd::splat(D);

        let points_base = Simd::splat((self.points.as_ref() as *const [[f32; D]]).cast::<f32>());
        let mut aff_ptrs = points_base.wrapping_add(starts);
        let end_ptrs = points_base.wrapping_add(ends);
        let mut inbounds = Mask::from_int(Simd::splat(-1));
        while inbounds.any() {
            let mut dists_sq = Simd::splat(0.0);
            for center_set in centers {
                let vals = unsafe { Simd::gather_select_ptr(aff_ptrs, inbounds, *center_set) };
                let diffs = center_set - vals;
                dists_sq += diffs * diffs;
                aff_ptrs = aff_ptrs.wrapping_add(Simd::splat(1));
            }
            if dists_sq.simd_lt(radii_squared).any() {
                return true;
            }

            inbounds &= aff_ptrs.simd_lt(end_ptrs);
        }

        false
    }

    #[must_use]
    /// Return the total memory used (stack + heap) by this structure.
    pub fn memory_used(&self) -> usize {
        size_of::<AffordanceTree<D>>()
            + (self.points.len() * D + self.tests.len()) * size_of::<f32>()
            + self.aff_starts.len() * size_of::<usize>()
    }

    #[must_use]
    /// Get the average number of affordances per point.
    pub fn affordance_size(&self) -> usize {
        self.points.len() / (self.tests.len() + 1)
    }
}

impl<const D: usize> Volume<D> {
    pub fn distsq_to(&self, point: &[f32; D]) -> f32 {
        let mut p2 = [0.0; D];

        point
            .iter()
            .zip(self.lower)
            .zip(self.upper)
            .map(|((p, l), u)| clamp(*p, l, u))
            .zip(p2.iter_mut())
            .for_each(|(clamped, coord)| *coord = clamped);

        distsq(p2, *point)
    }

    #[allow(clippy::needless_range_loop)]
    pub fn furthest_distsq_to(&self, point: &[f32; D]) -> f32 {
        let mut dist = 0.0;

        for d in 0..D {
            let lo_diff = (self.lower[d] - point[d]).abs();
            let hi_diff = (self.upper[d] - point[d]).abs();

            dist += if lo_diff < hi_diff { hi_diff } else { lo_diff }.powi(2);
        }

        dist
    }

    pub fn split(mut self, test: f32, dim: usize) -> (Self, Self) {
        let mut rhs = self;
        self.upper[dim] = test;
        rhs.lower[dim] = test;

        (self, rhs)
    }

    pub fn closest_point(&self, query: &[f32; D]) -> [f32; D] {
        let mut closest = [f32::NAN; D];
        for d in 0..D {
            closest[d] = clamp(query[d], self.lower[d], self.upper[d]);
        }
        closest
    }
}

fn clamp(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use rand::thread_rng;

    use crate::AffordanceTree;

    #[test]
    fn build_simple() {
        let points = [[0.0, 0.1], [0.4, -0.2], [-0.2, -0.1]];
        let t = AffordanceTree::new(&points, (0.0, 0.04), &mut thread_rng());
        println!("{t:?}");
    }

    #[test]
    fn exact_query_single() {
        let points = [[0.0, 0.1], [0.4, -0.2], [-0.2, -0.1]];
        let t = AffordanceTree::new(&points, (0.0, 0.04), &mut thread_rng());

        let q0 = [0.0, -0.01];
        assert!(t.collides(&q0, (0.12f32).powi(2)));
    }
}