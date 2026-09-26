use crate::{registry, Axis, LayoutError, LayoutNode, LayoutResult, Orientation};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimumSize {
    pub width: f32,
    pub height: f32,
}

impl LayoutNode {
    pub fn minimum_size(&self) -> MinimumSize {
        match self {
            Self::Panel {
                panel,
                placement,
                orientation,
                ..
            } => {
                let Some(d) = registry::descriptor(panel) else {
                    return MinimumSize {
                        width: 0.0,
                        height: 0.0,
                    };
                };
                if registry::orientation(panel, *placement, *orientation)
                    == Some(Orientation::Horizontal)
                    && d.orientations.len() > 1
                {
                    MinimumSize {
                        width: d.minimum_height,
                        height: d.minimum_width,
                    }
                } else {
                    MinimumSize {
                        width: d.minimum_width,
                        height: d.minimum_height,
                    }
                }
            }
            Self::Split { axis, children, .. } => {
                let sizes: Vec<_> = children.iter().map(|c| c.node.minimum_size()).collect();
                match axis {
                    Axis::Horizontal => MinimumSize {
                        width: sizes.iter().map(|s| s.width).sum(),
                        height: sizes.iter().map(|s| s.height).fold(0.0, f32::max),
                    },
                    Axis::Vertical => MinimumSize {
                        width: sizes.iter().map(|s| s.width).fold(0.0, f32::max),
                        height: sizes.iter().map(|s| s.height).sum(),
                    },
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SplitAllocation {
    pub sizes: Vec<f32>,
    /// Renderer must use a narrow-window projection/scroll policy when true.
    pub below_minimum: bool,
}

/// Allocate proportional space while preserving all satisfiable minimum sizes.
/// The result is transient geometry, never persisted as screen coordinates.
pub fn allocate_split(
    total: f32,
    weights: &[f32],
    minima: &[f32],
) -> LayoutResult<SplitAllocation> {
    if !total.is_finite()
        || total < 0.0
        || weights.is_empty()
        || weights.len() != minima.len()
        || weights
            .iter()
            .any(|w| !w.is_finite() || *w <= 0.0 || *w > 1_000_000.0)
        || minima
            .iter()
            .any(|m| !m.is_finite() || *m < 0.0 || *m > 1_000_000.0)
    {
        return Err(LayoutError("invalid allocation inputs".into()));
    }
    let min_sum: f32 = minima.iter().sum();
    if min_sum > total {
        return Ok(SplitAllocation {
            sizes: minima.iter().map(|m| total * m / min_sum).collect(),
            below_minimum: true,
        });
    }
    let mut sizes = vec![0.0; weights.len()];
    let mut fixed = vec![false; weights.len()];
    let mut remaining = total;
    for _ in 0..=weights.len() {
        let sum: f32 = weights
            .iter()
            .zip(&fixed)
            .filter(|(_, f)| !**f)
            .map(|(w, _)| *w)
            .sum();
        if sum == 0.0 {
            break;
        }
        let mut newly_fixed = false;
        for i in 0..weights.len() {
            if !fixed[i] && remaining * weights[i] / sum < minima[i] {
                sizes[i] = minima[i];
                fixed[i] = true;
                newly_fixed = true;
            }
        }
        if !newly_fixed {
            for i in 0..weights.len() {
                if !fixed[i] {
                    sizes[i] = remaining * weights[i] / sum;
                }
            }
            break;
        }
        remaining = (total
            - sizes
                .iter()
                .zip(&fixed)
                .filter(|(_, f)| **f)
                .map(|(s, _)| *s)
                .sum::<f32>())
        .max(0.0);
    }
    Ok(SplitAllocation {
        sizes,
        below_minimum: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_minimum_without_distorting_unconstrained_weights() {
        let a = allocate_split(100.0, &[1.0, 3.0], &[10.0, 10.0]).unwrap();
        assert_eq!(a.sizes, vec![25.0, 75.0]);
        let b = allocate_split(100.0, &[1.0, 3.0], &[40.0, 10.0]).unwrap();
        assert_eq!(b.sizes, vec![40.0, 60.0]);
        let c = allocate_split(30.0, &[1.0, 3.0], &[40.0, 20.0]).unwrap();
        assert!(c.below_minimum);
        assert_eq!(c.sizes, vec![20.0, 10.0]);
    }
}
