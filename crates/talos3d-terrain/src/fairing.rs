//! Constrained thin-plate (biharmonic) height fairing, shared by the draped
//! terrain render mesh and the [`crate::heightfield::TerrainHeightfield`] grid.
//!
//! Contour reconstruction interpolates heights between surveyed elevation
//! curves (IDW), which terraces: the field plateaus near each curve and steps
//! between them. A membrane relaxation (Laplacian smoothing with data
//! attachment) cannot fix this — the minimiser of the membrane energy is only
//! C0 across point constraints, so the surface stays "tense" over every pinned
//! contour vertex and ripples between curves no matter how the attachment
//! weights are tuned. The thin-plate energy minimises curvature instead of
//! gradient: its minimiser passes C1-smoothly *through* the constraints, which
//! removes both the creases along the curves and the inter-contour terraces
//! while keeping every surveyed contour height exact.

/// Damped Jacobi step size for the bi-Laplacian descent. The mean-valued
/// Laplacian has eigenvalues in `[-2, 0]`, so the bi-Laplacian's lie in
/// `[0, 4]` and the iteration is stable for steps below `0.5`.
const THIN_PLATE_RELAXATION: f32 = 0.45;

/// Iteration count at `smoothing == 1.0`. The ripple wavelength is the contour
/// spacing — a few samples — and constrained curves bound every relaxation
/// span, so the residual decays geometrically and this budget converges
/// visually.
const THIN_PLATE_MAX_ITERATIONS: f32 = 96.0;

/// Fair `heights` toward the constrained thin-plate minimiser.
///
/// `attachment` weights each sample's fidelity to its input height in
/// `0.0..=1.0`: `1.0` marks a surveyed sample (a mesh vertex lying exactly on
/// an elevation curve) that must not move at all; intermediate weights suit
/// samples that only lie *near* survey data (e.g. grid nodes next to a contour
/// crossing) and act as a proportional spring back toward the input height;
/// `0.0` lets the sample relax freely. The `smoothing` strength in `0..=1`
/// controls how far the relaxation is run (`0` = no-op, keep the raw
/// reconstruction).
///
/// `neighbors` yields the adjacent sample indices of a sample; it is borrowed
/// per visit so callers can adapt any topology (triangle-mesh one-rings, grid
/// 4-neighborhoods) without materialising a shared representation.
pub fn fair_heights_thin_plate<N, I>(
    heights: &mut [f32],
    attachment: &[f32],
    smoothing: f32,
    neighbors: N,
) where
    N: Fn(usize) -> I,
    I: IntoIterator<Item = usize>,
{
    let n = heights.len();
    let s = smoothing.clamp(0.0, 1.0);
    if s <= 0.0 || n < 3 || attachment.len() != n {
        return;
    }

    let iterations = (s * THIN_PLATE_MAX_ITERATIONS).round() as usize;
    // Convergence floor: once the largest per-iteration height change drops
    // below a fraction of the data's height range, further iterations are
    // visually inert — stop early (the cost is one-time but synchronous).
    let (min_h, max_h) = heights
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &h| {
            (lo.min(h), hi.max(h))
        });
    let tolerance = (max_h - min_h).max(f32::MIN_POSITIVE) * 1.0e-4;

    let original: Vec<f32> = heights.to_vec();
    let mut laplacian = vec![0.0f32; n];
    for _ in 0..iterations {
        for (index, value) in laplacian.iter_mut().enumerate() {
            *value = match neighbor_mean(index, &neighbors, |j| heights[j]) {
                Some(mean) => mean - heights[index],
                None => 0.0,
            };
        }
        let mut max_delta = 0.0f32;
        for index in 0..n {
            let attach = attachment[index].clamp(0.0, 1.0);
            if attach >= 1.0 {
                continue;
            }
            let Some(mean) = neighbor_mean(index, &neighbors, |j| laplacian[j]) else {
                continue;
            };
            let relaxed = heights[index] - THIN_PLATE_RELAXATION * (mean - laplacian[index]);
            // Soft data spring: blend the relaxed height back toward the input
            // reconstruction in proportion to the sample's survey fidelity.
            let next = relaxed + attach * (original[index] - relaxed);
            max_delta = max_delta.max((next - heights[index]).abs());
            heights[index] = next;
        }
        if max_delta < tolerance {
            break;
        }
    }
}

fn neighbor_mean<N, I>(index: usize, neighbors: &N, value: impl Fn(usize) -> f32) -> Option<f32>
where
    N: Fn(usize) -> I,
    I: IntoIterator<Item = usize>,
{
    let mut sum = 0.0f32;
    let mut count = 0u32;
    for j in neighbors(index) {
        sum += value(j);
        count += 1;
    }
    (count > 0).then(|| sum / count as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1D chain topology: neighbors are index ± 1.
    fn chain_neighbors(n: usize) -> impl Fn(usize) -> Vec<usize> {
        move |i| {
            let mut adj = Vec::with_capacity(2);
            if i > 0 {
                adj.push(i - 1);
            }
            if i + 1 < n {
                adj.push(i + 1);
            }
            adj
        }
    }

    /// A terraced ramp: contour samples every 4th node carry the surveyed
    /// height; between them the raw reconstruction plateaus (IDW terracing).
    fn terraced_chain(n: usize) -> (Vec<f32>, Vec<f32>) {
        // Plateau at the last contour height; survey samples every 4th node.
        let heights = (0..n).map(|i| (i / 4) as f32).collect();
        let attachment = (0..n).map(|i| if i % 4 == 0 { 1.0 } else { 0.0 }).collect();
        (heights, attachment)
    }

    #[test]
    fn zero_smoothing_is_a_no_op() {
        let (mut heights, attachment) = terraced_chain(33);
        let before = heights.clone();
        fair_heights_thin_plate(&mut heights, &attachment, 0.0, chain_neighbors(33));
        assert_eq!(heights, before);
    }

    #[test]
    fn fully_attached_heights_stay_exact() {
        let (mut heights, attachment) = terraced_chain(33);
        let before = heights.clone();
        fair_heights_thin_plate(&mut heights, &attachment, 1.0, chain_neighbors(33));
        for ((&height, &was), &attach) in heights.iter().zip(&before).zip(&attachment) {
            if attach >= 1.0 {
                assert_eq!(height, was, "surveyed sample moved");
            }
        }
    }

    #[test]
    fn terraces_relax_toward_uniform_slope() {
        let n = 41;
        let (mut heights, attachment) = terraced_chain(n);
        fair_heights_thin_plate(&mut heights, &attachment, 1.0, chain_neighbors(n));
        // On an even contour ladder the thin-plate minimiser is the straight
        // ramp h = i/4. Interior nodes (away from the free ends) must be close.
        for (i, &height) in heights.iter().enumerate().take(n - 8).skip(8) {
            let ramp = i as f32 / 4.0;
            assert!(
                (height - ramp).abs() < 0.05,
                "node {i}: {height} vs ramp {ramp}"
            );
        }
        // And the slope must not oscillate: strictly monotone interior.
        for i in 8..n - 9 {
            assert!(
                heights[i + 1] > heights[i],
                "terrace step survived at {i}: {} -> {}",
                heights[i],
                heights[i + 1]
            );
        }
    }

    #[test]
    fn no_crease_at_surveyed_interior_samples() {
        let n = 41;
        let (mut heights, attachment) = terraced_chain(n);
        fair_heights_thin_plate(&mut heights, &attachment, 1.0, chain_neighbors(n));
        // C1 through constraints: the one-sided slopes on either side of a
        // surveyed sample agree, instead of kinking like the membrane solution.
        for i in (12..n - 12).filter(|&i| attachment[i] >= 1.0) {
            let left = heights[i] - heights[i - 1];
            let right = heights[i + 1] - heights[i];
            assert!(
                (left - right).abs() < 0.02,
                "crease at surveyed sample {i}: left {left} right {right}"
            );
        }
    }

    #[test]
    fn partial_attachment_tracks_data_without_rigidity() {
        // Same ladder, but the survey samples are soft (0.5): they may flex a
        // little, yet must stay near their data while terraces still relax.
        let n = 41;
        let (mut heights, mut attachment) = terraced_chain(n);
        for weight in attachment.iter_mut() {
            if *weight >= 1.0 {
                *weight = 0.5;
            }
        }
        let before = heights.clone();
        fair_heights_thin_plate(&mut heights, &attachment, 1.0, chain_neighbors(n));
        for i in 8..n - 8 {
            if attachment[i] > 0.0 {
                assert!(
                    (heights[i] - before[i]).abs() < 0.2,
                    "soft sample {i} drifted: {} -> {}",
                    before[i],
                    heights[i]
                );
            }
            let ramp = i as f32 / 4.0;
            assert!(
                (heights[i] - ramp).abs() < 0.15,
                "node {i}: {} vs ramp {ramp}",
                heights[i]
            );
        }
    }
}
