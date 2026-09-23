use rsnano_types::{Blake2HashBuilder, BlockHash};

/// RAI: one cell of an invertible Bloom lookup table over 32-byte keys: how
/// many keys were added to it, the XOR of those keys, and the XOR of their
/// checksums. A cell holding exactly one key is pure: its count is one and
/// its check is the checksum of its key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SketchCell {
    pub count: i32,
    pub key: [u8; 32],
    pub check: u32,
}

impl SketchCell {
    fn is_empty(&self) -> bool {
        self.count == 0 && self.key == [0; 32] && self.check == 0
    }

    fn pure_key(&self) -> Option<(BlockHash, i32)> {
        if self.count != 1 && self.count != -1 {
            return None;
        }
        let key = BlockHash::from_bytes(self.key);
        (Sketch::checksum(&key) == self.check).then_some((key, self.count))
    }
}

/// RAI: a set sketch, for reconciling a reporter's residual object against
/// the one a validator derived. Subtracting the sketch of one set from the
/// sketch of another leaves a sketch of their symmetric difference, which
/// peels out key by key when it is small enough for the cells; the cost is
/// the cells, not the sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sketch {
    cells: Vec<SketchCell>,
}

/// The symmetric difference a sketch peeled out
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Peeled {
    /// Keys in the sketch the other was subtracted from, and not in the other
    pub ours: Vec<BlockHash>,
    /// Keys in the subtracted sketch only
    pub theirs: Vec<BlockHash>,
}

impl Sketch {
    /// Cells a key is added to
    pub const HASHES: usize = 3;
    /// The smallest sketch: decodes a difference of about forty keys, which
    /// is far more than a lost vote or two
    pub const MIN_CELLS: usize = 64;
    /// The largest: 40 KiB on the wire
    pub const MAX_CELLS: usize = 1024;

    pub fn new(cells: usize) -> Self {
        Self {
            cells: vec![SketchCell::default(); cells.clamp(Self::HASHES, Self::MAX_CELLS)],
        }
    }

    pub fn from_cells(cells: Vec<SketchCell>) -> Self {
        Self { cells }
    }

    /// The sketch of a set of keys
    pub fn over(keys: impl IntoIterator<Item = BlockHash>, cells: usize) -> Self {
        let mut sketch = Self::new(cells);
        for key in keys {
            sketch.insert(&key);
        }
        sketch
    }

    pub fn cells(&self) -> &[SketchCell] {
        &self.cells
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn insert(&mut self, key: &BlockHash) {
        self.toggle(key, 1);
    }

    fn toggle(&mut self, key: &BlockHash, sign: i32) {
        let check = Self::checksum(key);
        for position in self.positions(key) {
            let cell = &mut self.cells[position];
            cell.count += sign;
            for (c, k) in cell.key.iter_mut().zip(key.as_bytes()) {
                *c ^= k;
            }
            cell.check ^= check;
        }
    }

    /// The distinct cells a key goes to, the same on every replica
    fn positions(&self, key: &BlockHash) -> [usize; Self::HASHES] {
        let n = self.cells.len();
        let bytes = key.as_bytes();
        let mut positions = [0usize; Self::HASHES];
        for i in 0..Self::HASHES {
            let mut word = [0u8; 8];
            word.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            let mut candidate = (u64::from_le_bytes(word) % n as u64) as usize;
            // Distinct positions, so that a key never cancels itself
            while positions[..i].contains(&candidate) {
                candidate = (candidate + 1) % n;
            }
            positions[i] = candidate;
        }
        positions
    }

    fn checksum(key: &BlockHash) -> u32 {
        let hash = Blake2HashBuilder::new()
            .update(b"RAI sketch check")
            .update(key.as_bytes())
            .build();
        let mut word = [0u8; 4];
        word.copy_from_slice(&hash.as_bytes()[..4]);
        u32::from_le_bytes(word)
    }

    /// Subtracts the other sketch, leaving the sketch of the symmetric
    /// difference. The sketches have to be of one size.
    pub fn subtract(&mut self, other: &Sketch) -> bool {
        if self.cells.len() != other.cells.len() {
            return false;
        }
        for (cell, theirs) in self.cells.iter_mut().zip(&other.cells) {
            cell.count -= theirs.count;
            for (c, k) in cell.key.iter_mut().zip(&theirs.key) {
                *c ^= k;
            }
            cell.check ^= theirs.check;
        }
        true
    }

    /// Peels the difference out of a subtracted sketch: a pure cell gives a
    /// key, removing the key frees other cells, until every cell is empty.
    /// None when the difference is too large for the cells.
    pub fn peel(mut self) -> Option<Peeled> {
        let mut peeled = Peeled::default();
        loop {
            let Some((key, sign)) = self.cells.iter().find_map(SketchCell::pure_key) else {
                break;
            };
            if sign > 0 {
                peeled.ours.push(key);
            } else {
                peeled.theirs.push(key);
            }
            self.toggle(&key, -sign);
        }
        self.cells
            .iter()
            .all(SketchCell::is_empty)
            .then_some(peeled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u64) -> BlockHash {
        Blake2HashBuilder::new().update(i.to_le_bytes()).build()
    }

    #[test]
    fn identical_sets_have_no_difference() {
        let mut ours = Sketch::over((0..500).map(key), Sketch::MIN_CELLS);
        let theirs = Sketch::over((0..500).rev().map(key), Sketch::MIN_CELLS);
        assert!(ours.subtract(&theirs));
        assert_eq!(ours.peel(), Some(Peeled::default()));
    }

    #[test]
    fn a_small_difference_peels_out_of_a_small_sketch() {
        // Ours lacks 3, theirs lacks 500..502
        let mut ours = Sketch::over((0..500).filter(|i| *i != 3).map(key), Sketch::MIN_CELLS);
        let theirs = Sketch::over((0..503).filter(|i| *i != 3).map(key), Sketch::MIN_CELLS);
        assert!(ours.subtract(&theirs));
        let mut peeled = ours.peel().expect("three keys fit sixty-four cells");
        peeled.ours.sort();
        peeled.theirs.sort();
        assert!(peeled.ours.is_empty());
        let mut expected = vec![key(500), key(501), key(502)];
        expected.sort();
        assert_eq!(peeled.theirs, expected);

        // The other way round
        let mut theirs = Sketch::over((0..503).filter(|i| *i != 3).map(key), Sketch::MIN_CELLS);
        let ours = Sketch::over((0..500).filter(|i| *i != 3).map(key), Sketch::MIN_CELLS);
        assert!(theirs.subtract(&ours));
        let peeled = theirs.peel().unwrap();
        assert_eq!(peeled.ours.len(), 3);
        assert!(peeled.theirs.is_empty());
    }

    #[test]
    fn a_difference_too_large_for_the_cells_does_not_peel() {
        let mut ours = Sketch::over((0..10).map(key), 16);
        let theirs = Sketch::over((100..200).map(key), 16);
        assert!(ours.subtract(&theirs));
        assert_eq!(ours.peel(), None);
        // With enough cells it does
        let mut ours = Sketch::over((0..10).map(key), Sketch::MAX_CELLS);
        let theirs = Sketch::over((100..200).map(key), Sketch::MAX_CELLS);
        assert!(ours.subtract(&theirs));
        let peeled = ours.peel().unwrap();
        assert_eq!(peeled.ours.len(), 10);
        assert_eq!(peeled.theirs.len(), 100);
    }

    #[test]
    fn sketches_of_different_sizes_do_not_subtract() {
        let mut ours = Sketch::new(64);
        assert!(!ours.subtract(&Sketch::new(128)));
    }

    #[test]
    fn a_key_goes_to_distinct_cells() {
        // No fewer cells than positions, so that they can be distinct
        let sketch = Sketch::new(2);
        assert_eq!(sketch.len(), Sketch::HASHES);
        for i in 0..50 {
            let p = sketch.positions(&key(i));
            assert!(p[0] != p[1] && p[1] != p[2] && p[0] != p[2]);
        }
        let sketch = Sketch::new(64);
        for i in 0..200 {
            let p = sketch.positions(&key(i));
            assert!(p[0] != p[1] && p[1] != p[2] && p[0] != p[2]);
        }
    }
}
