//! Geometry of a chunked array: the chunk grid, edge chunks, and the
//! contiguous runs that gathering and slicing copy.
//!
//! Chunks are numbered in C order over the chunk grid. An edge chunk is
//! smaller than the nominal chunk shape when the array does not divide evenly,
//! and it is stored at its real size.

/// Shape, chunk shape and element size of an array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub shape: Vec<u64>,
    pub chunk: Vec<u64>,
    pub elem: usize,
}

impl Geometry {
    /// Number of chunks along each axis.
    pub fn grid(&self) -> Vec<u64> {
        self.shape
            .iter()
            .zip(&self.chunk)
            .map(|(&s, &c)| s.div_ceil(c))
            .collect()
    }

    /// Total number of chunks.
    pub fn chunk_count(&self) -> u64 {
        self.grid().iter().product()
    }

    /// Start and extent of chunk number `linear`, in C order over the grid.
    pub fn chunk_box(&self, linear: u64) -> (Vec<u64>, Vec<u64>) {
        let grid = self.grid();
        let mut rest = linear;
        let mut index = vec![0u64; grid.len()];
        for d in (0..grid.len()).rev() {
            index[d] = rest % grid[d];
            rest /= grid[d];
        }
        let start: Vec<u64> = index
            .iter()
            .zip(&self.chunk)
            .map(|(&i, &c)| i * c)
            .collect();
        let extent: Vec<u64> = start
            .iter()
            .zip(&self.shape)
            .zip(&self.chunk)
            .map(|((&s, &len), &c)| c.min(len - s))
            .collect();
        (start, extent)
    }

    /// Number of elements in the whole array.
    pub fn elements(&self) -> u64 {
        self.shape.iter().product()
    }
}

/// C-order byte offset and length of each contiguous run of a box inside an
/// array of `shape`. Each run is the box's last axis, so the list has one
/// entry per index of the other axes.
pub(crate) fn runs(
    shape: &[u64],
    start: &[u64],
    extent: &[u64],
    elem: usize,
) -> Vec<(usize, usize)> {
    if extent.contains(&0) || shape.is_empty() {
        return Vec::new();
    }
    let last = shape.len() - 1;
    // Byte stride of each axis in the full array.
    let mut strides = vec![elem as u64; shape.len()];
    for d in (0..last).rev() {
        strides[d] = strides[d + 1] * shape[d + 1];
    }

    let run_len = (extent[last] as usize) * elem;
    let count: u64 = extent[..last].iter().product();
    let mut out = Vec::with_capacity(count as usize);
    let mut index = vec![0u64; last];
    for _ in 0..count {
        let mut offset = start[last] * strides[last];
        for d in 0..last {
            offset += (start[d] + index[d]) * strides[d];
        }
        out.push((offset as usize, run_len));
        // Advance the odometer over the leading axes.
        for d in (0..last).rev() {
            index[d] += 1;
            if index[d] < extent[d] {
                break;
            }
            index[d] = 0;
        }
    }
    out
}

/// The part of two boxes that they share, if any.
pub(crate) fn overlap(
    a_start: &[u64],
    a_extent: &[u64],
    b_start: &[u64],
    b_extent: &[u64],
) -> Option<(Vec<u64>, Vec<u64>)> {
    let mut start = Vec::with_capacity(a_start.len());
    let mut extent = Vec::with_capacity(a_start.len());
    for d in 0..a_start.len() {
        let lo = a_start[d].max(b_start[d]);
        let hi = (a_start[d] + a_extent[d]).min(b_start[d] + b_extent[d]);
        if hi <= lo {
            return None;
        }
        start.push(lo);
        extent.push(hi - lo);
    }
    Some((start, extent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_chunks_are_smaller() {
        let g = Geometry {
            shape: vec![5, 7],
            chunk: vec![2, 3],
            elem: 1,
        };
        assert_eq!(g.grid(), vec![3, 3]);
        assert_eq!(g.chunk_count(), 9);
        assert_eq!(g.chunk_box(0), (vec![0, 0], vec![2, 3]));
        assert_eq!(g.chunk_box(8), (vec![4, 6], vec![1, 1]));
        assert_eq!(g.chunk_box(2), (vec![0, 6], vec![2, 1]));
    }

    #[test]
    fn runs_cover_a_box_in_c_order() {
        // A 3x4 array of single bytes, and its 2x2 box at (1, 1).
        let runs = runs(&[3, 4], &[1, 1], &[2, 2], 1);
        assert_eq!(runs, vec![(5, 2), (9, 2)]);
    }

    #[test]
    fn overlap_is_the_shared_box() {
        assert_eq!(
            overlap(&[0, 0], &[4, 4], &[2, 3], &[4, 4]),
            Some((vec![2, 3], vec![2, 1]))
        );
        assert_eq!(overlap(&[0], &[2], &[2], &[2]), None);
    }
}
