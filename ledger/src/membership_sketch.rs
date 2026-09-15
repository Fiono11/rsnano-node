use std::collections::BTreeSet;

use rsnano_types::BlockHash;

/// Hash functions of the table; each indexes its own third of the cells so one
/// member never lands in the same cell twice.
const HASHES: usize = 3;
/// Cells per table: about 100 differing members decode from one sketch.
pub const SKETCH_CELLS: usize = 192;
const CELL_BYTES: usize = 32 + 8 + 4;
/// Serialized size of a sketch.
pub const SKETCH_BYTES: usize = SKETCH_CELLS * CELL_BYTES;

/// Invertible Bloom lookup table over member hashes. Two replicas subtract
/// their sketches and peel the difference in one message, at a cost
/// proportional to the difference rather than to the membership. Members are
/// uniformly random hashes, so their own bytes serve as cell indices; the check
/// value is a non-linear mix of the member so a mixed cell is never taken for
/// a pure one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipSketch {
    cells: Vec<Cell>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cell {
    key_sum: [u8; 32],
    check_sum: u64,
    count: i32,
}

impl Cell {
    fn is_pure(&self) -> bool {
        (self.count == 1 || self.count == -1)
            && check_of(&BlockHash::from_bytes(self.key_sum)) == self.check_sum
    }
    fn is_empty(&self) -> bool {
        self.count == 0 && self.check_sum == 0 && self.key_sum == [0u8; 32]
    }
    fn apply(&mut self, member: &BlockHash, delta: i32) {
        for (sum, byte) in self.key_sum.iter_mut().zip(member.as_bytes()) {
            *sum ^= byte;
        }
        self.check_sum ^= check_of(member);
        self.count += delta;
    }
}

/// A non-linear 64-bit check of a member: XOR-ing two members' checks never
/// equals the check of their XOR, so a cell holding several members is never
/// mistaken for a pure one.
fn check_of(member: &BlockHash) -> u64 {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for word in member.as_bytes().chunks(8) {
        x = (x ^ u64::from_le_bytes(word.try_into().unwrap())).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
    }
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn cell_indices(member: &BlockHash) -> [usize; HASHES] {
    let bytes = member.as_bytes();
    let stride = SKETCH_CELLS / HASHES;
    let mut indices = [0usize; HASHES];
    for (i, index) in indices.iter_mut().enumerate() {
        let window = u32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().unwrap()) as usize;
        *index = i * stride + window % stride;
    }
    indices
}

impl Default for MembershipSketch {
    fn default() -> Self {
        Self::new()
    }
}

impl MembershipSketch {
    pub fn new() -> Self {
        Self {
            cells: vec![Cell::default(); SKETCH_CELLS],
        }
    }

    pub fn insert(&mut self, member: &BlockHash) {
        for index in cell_indices(member) {
            self.cells[index].apply(member, 1);
        }
    }

    pub fn remove(&mut self, member: &BlockHash) {
        for index in cell_indices(member) {
            self.cells[index].apply(member, -1);
        }
    }

    /// The members of `self` not in `other` and of `other` not in `self`, or
    /// `None` when the difference is too large for the table.
    pub fn difference(&self, other: &Self) -> Option<(Vec<BlockHash>, Vec<BlockHash>)> {
        let mut cells = self.cells.clone();
        for (cell, theirs) in cells.iter_mut().zip(&other.cells) {
            for (sum, byte) in cell.key_sum.iter_mut().zip(theirs.key_sum) {
                *sum ^= byte;
            }
            cell.check_sum ^= theirs.check_sum;
            cell.count -= theirs.count;
        }
        let mut only_mine = BTreeSet::new();
        let mut only_theirs = BTreeSet::new();
        let mut pure: Vec<usize> = (0..SKETCH_CELLS).filter(|i| cells[*i].is_pure()).collect();
        // Every peel empties the member's cells, so a decodable difference
        // needs at most one peel per member per cell; anything more is a
        // check collision and the sketch is treated as undecodable.
        let mut peels = 0;
        while let Some(index) = pure.pop() {
            let cell = cells[index];
            if !cell.is_pure() {
                continue;
            }
            peels += 1;
            if peels > SKETCH_CELLS {
                return None;
            }
            let member = BlockHash::from_bytes(cell.key_sum);
            let delta = cell.count;
            if delta == 1 {
                only_mine.insert(member);
            } else {
                only_theirs.insert(member);
            }
            for peeled in cell_indices(&member) {
                cells[peeled].apply(&member, -delta);
                if cells[peeled].is_pure() {
                    pure.push(peeled);
                }
            }
        }
        cells.iter().all(Cell::is_empty).then(|| {
            (
                only_mine.into_iter().collect(),
                only_theirs.into_iter().collect(),
            )
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SKETCH_BYTES);
        for cell in &self.cells {
            bytes.extend_from_slice(&cell.key_sum);
            bytes.extend_from_slice(&cell.check_sum.to_le_bytes());
            bytes.extend_from_slice(&cell.count.to_le_bytes());
        }
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SKETCH_BYTES {
            return None;
        }
        let cells = bytes
            .chunks(CELL_BYTES)
            .map(|chunk| Cell {
                key_sum: chunk[..32].try_into().unwrap(),
                check_sum: u64::from_le_bytes(chunk[32..40].try_into().unwrap()),
                count: i32::from_le_bytes(chunk[40..44].try_into().unwrap()),
            })
            .collect();
        Some(Self { cells })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difference_names_the_members_of_each_side() {
        let shared: Vec<_> = (0..10_000u64).map(member).collect();
        let mut mine = MembershipSketch::new();
        let mut theirs = MembershipSketch::new();
        for m in &shared {
            mine.insert(m);
            theirs.insert(m);
        }
        let only_mine: Vec<_> = (20_000..20_040u64).map(member).collect();
        let only_theirs: Vec<_> = (30_000..30_050u64).map(member).collect();
        for m in &only_mine {
            mine.insert(m);
        }
        for m in &only_theirs {
            theirs.insert(m);
        }
        let (a, b) = mine.difference(&theirs).expect("decodes");
        assert_eq!(a, sorted(&only_mine));
        assert_eq!(b, sorted(&only_theirs));
        let (b, a) = theirs.difference(&mine).expect("decodes both ways");
        assert_eq!(a, sorted(&only_mine));
        assert_eq!(b, sorted(&only_theirs));
        assert_eq!(mine.difference(&mine), Some((vec![], vec![])));
    }

    #[test]
    fn removal_and_serialization_round_trip() {
        let mut sketch = MembershipSketch::new();
        for i in 0..100u64 {
            sketch.insert(&member(i));
        }
        let bytes = sketch.to_bytes();
        assert_eq!(bytes.len(), SKETCH_BYTES);
        assert_eq!(MembershipSketch::from_bytes(&bytes), Some(sketch.clone()));
        assert!(MembershipSketch::from_bytes(&bytes[1..]).is_none());
        for i in 0..100u64 {
            sketch.remove(&member(i));
        }
        assert_eq!(sketch, MembershipSketch::new());
    }

    #[test]
    fn an_oversized_difference_is_reported_as_undecodable() {
        let mut mine = MembershipSketch::new();
        let theirs = MembershipSketch::new();
        for i in 0..1_000u64 {
            mine.insert(&member(i));
        }
        assert_eq!(mine.difference(&theirs), None);
    }

    fn member(i: u64) -> BlockHash {
        rsnano_types::Blake2HashBuilder::new()
            .update(i.to_le_bytes())
            .build()
    }

    fn sorted(members: &[BlockHash]) -> Vec<BlockHash> {
        let mut sorted = members.to_vec();
        sorted.sort();
        sorted
    }
}
