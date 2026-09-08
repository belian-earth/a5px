//! Cell-level area-of-interest mask.
//!
//! R computes the AOI as a compacted A5 cell set (`a5R::a5_polygon_to_cells`
//! with the chosen containment) and hands over the compacted ids. Membership
//! of a target-resolution cell is tested by walking its ancestors: a cell is
//! in the AOI iff it, or one of its parents, is in the compacted set. The
//! compacted set stays small for large AOIs, unlike the uncompacted set at
//! fine resolutions, and `cell_to_parent` is a few bit operations.

use ahash::AHashSet;

pub(crate) struct CellMask {
    /// (resolution, cells at that resolution), ascending by resolution.
    levels: Vec<(i32, AHashSet<u64>)>,
}

impl CellMask {
    /// `None` when `cells` is empty (no mask).
    pub(crate) fn from_cells(cells: &[u64]) -> Option<Self> {
        if cells.is_empty() {
            return None;
        }
        let mut levels: Vec<(i32, AHashSet<u64>)> = Vec::new();
        for &c in cells {
            let r = a5::get_resolution(c);
            match levels.iter_mut().find(|(lr, _)| *lr == r) {
                Some((_, set)) => {
                    set.insert(c);
                }
                None => {
                    let mut set = AHashSet::new();
                    set.insert(c);
                    levels.push((r, set));
                }
            }
        }
        levels.sort_by_key(|(r, _)| *r);
        Some(Self { levels })
    }

    /// Is `cell` (at `resolution`) inside the AOI?
    #[inline]
    pub(crate) fn contains(&self, cell: u64, resolution: i32) -> bool {
        for (r, set) in &self.levels {
            if *r == resolution {
                if set.contains(&cell) {
                    return true;
                }
            } else if *r < resolution {
                if let Ok(p) = a5::cell_to_parent(cell, Some(*r)) {
                    if set.contains(&p) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Per-worker memo of the last mask decision: adjacent pixels almost always
/// share a cell, so the ancestor walk runs once per cell change.
pub(crate) struct MaskCache {
    last_cell: u64,
    last_in: bool,
}

impl MaskCache {
    pub(crate) fn new() -> Self {
        Self { last_cell: u64::MAX, last_in: false }
    }

    #[inline]
    pub(crate) fn allows(&mut self, mask: Option<&CellMask>, cell: u64, resolution: i32) -> bool {
        let Some(m) = mask else { return true };
        if cell != self.last_cell {
            self.last_cell = cell;
            self.last_in = m.contains(cell, resolution);
        }
        self.last_in
    }
}
