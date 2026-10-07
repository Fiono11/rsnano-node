use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
};

use rsnano_types::Blake2HashBuilder;

/// RAI, report reconciliation: a rateless invertible Bloom lookup table
/// (Yang, Gilad, Alizadeh, "Practical Rateless Set Reconciliation", SIGCOMM
/// 2024). A holder of a set streams coded symbols of it; a requester
/// subtracts the symbols of its own set and peels the difference. Neither
/// side needs a shared root, an estimate of the difference or a retry: the
/// requester asks for more symbols until the difference decodes, about 1.35
/// to 1.72 symbols per differing item.

/// The size of one reconciled item: a certified-state entry or a residual
/// vote record, both 105 bytes
pub const ITEM_SIZE: usize = 105;

pub type Item = [u8; ITEM_SIZE];

/// The upper bound on the symbols one stream may need: a difference of
/// about 100,000 items. A stream that has not decoded by then is given up.
pub const MAX_SYMBOLS: usize = 1 << 18;

/// One coded symbol: how many items it covers (the remote ones counted +1,
/// the local ones −1 once subtracted), the XOR of their bytes and the XOR of
/// a non-linear 64-bit hash of each. The hash has to be non-linear: with a
/// byte-slice check a cell mixing three items looks pure, and peeling loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodedSymbol {
    pub count: i64,
    pub sum: Item,
    pub check: u64,
}

impl CodedSymbol {
    pub const SERIALIZED_SIZE: usize = 8 + ITEM_SIZE + 8;

    pub const ZERO: Self = Self {
        count: 0,
        sum: [0; ITEM_SIZE],
        check: 0,
    };

    fn apply(&mut self, item: &Item, check: u64, direction: i64) {
        self.count += direction;
        for (s, b) in self.sum.iter_mut().zip(item) {
            *s ^= b;
        }
        self.check ^= check;
    }

    /// This symbol minus another of the same index
    pub fn subtract(&self, other: &CodedSymbol) -> CodedSymbol {
        let mut result = *self;
        result.count = self.count.wrapping_sub(other.count);
        for (s, b) in result.sum.iter_mut().zip(&other.sum) {
            *s ^= b;
        }
        result.check ^= other.check;
        result
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0 && self.check == 0 && self.sum.iter().all(|b| *b == 0)
    }

    fn is_pure(&self) -> bool {
        (self.count == 1 || self.count == -1) && item_hash(&self.sum).0 == self.check
    }

    pub fn serialize(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.count.to_le_bytes());
        buffer.extend_from_slice(&self.sum);
        buffer.extend_from_slice(&self.check.to_le_bytes());
    }

    pub fn deserialize(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::SERIALIZED_SIZE {
            return None;
        }
        let mut count = [0; 8];
        count.copy_from_slice(&bytes[..8]);
        let mut sum = [0; ITEM_SIZE];
        sum.copy_from_slice(&bytes[8..8 + ITEM_SIZE]);
        let mut check = [0; 8];
        check.copy_from_slice(&bytes[8 + ITEM_SIZE..]);
        Some(Self {
            count: i64::from_le_bytes(count),
            sum,
            check: u64::from_le_bytes(check),
        })
    }
}

/// The 64-bit check of an item and the seed of its index sequence, both
/// from one Blake2 hash of the item
fn item_hash(item: &Item) -> (u64, u64) {
    let hash = Blake2HashBuilder::new()
        .update(b"RAI rateless item")
        .update(item)
        .build();
    let bytes = hash.as_bytes();
    let mut check = [0; 8];
    check.copy_from_slice(&bytes[..8]);
    let mut seed = [0; 8];
    seed.copy_from_slice(&bytes[8..16]);
    (u64::from_le_bytes(check), u64::from_le_bytes(seed))
}

/// The symbol indices one item is mapped to: 0 first, then gaps drawn so
/// that index i is hit with probability about 1 / (1 + i/2). The draw is a
/// splitmix64 sequence seeded by the item, so every holder maps an item to
/// the same indices.
#[derive(Clone, Copy, Debug)]
struct Mapping {
    prng: u64,
    index: u64,
}

impl Mapping {
    fn new(seed: u64) -> Self {
        Self {
            prng: seed,
            index: 0,
        }
    }

    fn next(&mut self) -> u64 {
        self.prng = self.prng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.prng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let r = (z >> 11) as f64 / (1u64 << 53) as f64;
        let gap = ((self.index as f64 + 1.5) * (1.0 / (1.0 - r).sqrt() - 1.0)).ceil();
        let gap = if gap.is_finite() && gap >= 1.0 {
            gap.min(u32::MAX as f64) as u64
        } else {
            1
        };
        self.index = self.index.saturating_add(gap);
        self.index
    }

    /// Whether the item is mapped to this index
    fn maps_to(seed: u64, index: u64) -> bool {
        let mut mapping = Self::new(seed);
        while mapping.index < index {
            mapping.next();
        }
        mapping.index == index
    }
}

/// Items with their current index, in index order: what the symbol at the
/// next index is made of. Shared by the encoder and the decoder, which both
/// have to add items to symbols they have not seen yet.
#[derive(Default)]
struct Window {
    items: Vec<(Item, u64, i64)>,
    mappings: Vec<Mapping>,
    due: BinaryHeap<Reverse<(u64, usize)>>,
}

impl Window {
    fn push(&mut self, item: Item, direction: i64, mapping: Mapping) {
        let (check, _) = item_hash(&item);
        let index = self.items.len();
        self.items.push((item, check, direction));
        self.due.push(Reverse((mapping.index, index)));
        self.mappings.push(mapping);
    }

    /// Adds every item due at this index to the symbol, `sign` times its
    /// direction, and moves each to its next index
    fn apply(&mut self, symbol: &mut CodedSymbol, index: u64, sign: i64) {
        while let Some(Reverse((due, k))) = self.due.peek().copied() {
            if due != index {
                break;
            }
            self.due.pop();
            let (item, check, direction) = self.items[k];
            symbol.apply(&item, check, sign * direction);
            let next = self.mappings[k].next();
            self.due.push(Reverse((next, k)));
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }
}

/// The coded symbols of one set, produced on demand and cached: any holder
/// of the set answers any range of indices, and a second requester costs
/// nothing.
pub struct Encoder {
    window: Window,
    symbols: Vec<CodedSymbol>,
}

impl Encoder {
    pub fn new(items: impl IntoIterator<Item = Item>) -> Self {
        let mut window = Window::default();
        for item in items {
            let (_, seed) = item_hash(&item);
            window.push(item, 1, Mapping::new(seed));
        }
        Self {
            window,
            symbols: Vec::new(),
        }
    }

    /// The items encoded
    pub fn len(&self) -> usize {
        self.window.len()
    }

    /// The symbols at indices `from .. from + count`, at most up to
    /// `MAX_SYMBOLS`
    pub fn symbols(&mut self, from: usize, count: usize) -> &[CodedSymbol] {
        let end = from.saturating_add(count).min(MAX_SYMBOLS);
        while self.symbols.len() < end {
            let index = self.symbols.len() as u64;
            let mut symbol = CodedSymbol::ZERO;
            self.window.apply(&mut symbol, index, 1);
            self.symbols.push(symbol);
        }
        &self.symbols[from.min(end)..end]
    }
}

/// One item of the difference: held only by the remote set (to insert into
/// the local one) or only by the local set (to delete from it)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovered {
    Remote(Item),
    Local(Item),
}

/// Decodes the difference between a remote set, received as coded symbols,
/// and a local base set. The base is fixed when the decoder is created: the
/// local symbols subtracted from symbol i must be those of the same set for
/// every i, so a base that moves under a running stream breaks the peel.
pub struct Decoder {
    local: Encoder,
    /// Remote minus local, minus every item recovered so far
    cells: Vec<CodedSymbol>,
    recovered: Window,
    seen: HashSet<Item>,
    pure: Vec<usize>,
    failed: bool,
}

impl Decoder {
    pub fn new(local: impl IntoIterator<Item = Item>) -> Self {
        Self {
            local: Encoder::new(local),
            cells: Vec::new(),
            recovered: Window::default(),
            seen: HashSet::new(),
            pure: Vec::new(),
            failed: false,
        }
    }

    /// The symbols received so far: the index the next one must have
    pub fn received(&self) -> usize {
        self.cells.len()
    }

    /// The next symbols of the remote stream, in index order
    pub fn add_symbols(&mut self, symbols: &[CodedSymbol]) {
        for remote in symbols {
            if self.cells.len() >= MAX_SYMBOLS {
                self.failed = true;
                return;
            }
            let index = self.cells.len();
            let local = self.local.symbols(index, 1)[0];
            let mut cell = remote.subtract(&local);
            // A recovered item is gone from the difference at every index
            self.recovered.apply(&mut cell, index as u64, -1);
            if cell.is_pure() {
                self.pure.push(index);
            }
            self.cells.push(cell);
        }
        self.peel();
    }

    fn peel(&mut self) {
        while let Some(index) = self.pure.pop() {
            let cell = self.cells[index];
            if !cell.is_pure() {
                continue;
            }
            let item = cell.sum;
            let direction = cell.count;
            let (check, seed) = item_hash(&item);
            // A pure-looking cell whose item does not map here is a
            // collision, not an item of the difference
            if !Mapping::maps_to(seed, index as u64) {
                continue;
            }
            // An item recovered twice, or more items than symbols: the
            // stream is not an encoding of any set
            if !self.seen.insert(item) || self.seen.len() > 2 * self.cells.len() + 64 {
                self.failed = true;
                return;
            }
            let mut mapping = Mapping::new(seed);
            while (mapping.index as usize) < self.cells.len() {
                let at = mapping.index as usize;
                self.cells[at].apply(&item, check, -direction);
                if self.cells[at].is_pure() {
                    self.pure.push(at);
                }
                mapping.next();
            }
            self.recovered.push(item, direction, mapping);
        }
    }

    /// Every item of the difference is recovered: symbol 0 covers every
    /// item, and nothing is left in it
    pub fn is_done(&self) -> bool {
        !self.failed && self.cells.first().is_some_and(CodedSymbol::is_empty)
    }

    /// The stream can not be decoded: give it up and start another
    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn recovered(&self) -> impl Iterator<Item = Recovered> + '_ {
        self.recovered.items.iter().map(|(item, _, direction)| {
            if *direction > 0 {
                Recovered::Remote(*item)
            } else {
                Recovered::Local(*item)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_differences_of_every_size_within_twice_the_difference() {
        for d in [1, 4, 50, 1000, 5000] {
            let shared: Vec<Item> = (0..2000).map(item).collect();
            let remote_only: Vec<Item> = (10_000..10_000 + d / 2 + d % 2).map(item).collect();
            let local_only: Vec<Item> = (20_000..20_000 + d / 2).map(item).collect();
            let mut encoder = Encoder::new(shared.iter().chain(&remote_only).copied());
            let mut decoder = Decoder::new(shared.iter().chain(&local_only).copied());
            let used = decode(&mut encoder, &mut decoder, 16);
            assert!(decoder.is_done(), "d = {d}");
            assert!(used <= 2 * d as usize + 16, "d = {d}: {used} symbols");
            let mut remote: Vec<Item> = Vec::new();
            let mut local: Vec<Item> = Vec::new();
            for recovered in decoder.recovered() {
                match recovered {
                    Recovered::Remote(item) => remote.push(item),
                    Recovered::Local(item) => local.push(item),
                }
            }
            remote.sort();
            local.sort();
            let mut expected_remote = remote_only.clone();
            expected_remote.sort();
            let mut expected_local = local_only.clone();
            expected_local.sort();
            assert_eq!(remote, expected_remote);
            assert_eq!(local, expected_local);
        }
    }

    #[test]
    fn identical_sets_decode_on_the_first_symbol() {
        let set: Vec<Item> = (0..500).map(item).collect();
        let mut encoder = Encoder::new(set.clone());
        let mut decoder = Decoder::new(set);
        decoder.add_symbols(encoder.symbols(0, 1));
        assert!(decoder.is_done());
        assert_eq!(decoder.recovered().count(), 0);
    }

    #[test]
    fn coding_is_linear() {
        let a: Vec<Item> = (0..300).map(item).collect();
        let b: Vec<Item> = (200..450).map(item).collect();
        let only_a: Vec<Item> = (0..200).map(item).collect();
        let only_b: Vec<Item> = (300..450).map(item).collect();
        let mut enc_a = Encoder::new(a);
        let mut enc_b = Encoder::new(b);
        let mut enc_only_a = Encoder::new(only_a);
        let mut enc_only_b = Encoder::new(only_b);
        for i in 0..200 {
            let difference = enc_a.symbols(i, 1)[0].subtract(&enc_b.symbols(i, 1)[0]);
            let expected = enc_only_a.symbols(i, 1)[0].subtract(&enc_only_b.symbols(i, 1)[0]);
            assert_eq!(difference, expected, "symbol {i}");
        }
    }

    #[test]
    fn independent_encoders_of_one_set_agree() {
        let set: Vec<Item> = (0..1000).map(item).collect();
        let mut reversed = set.clone();
        reversed.reverse();
        let mut one = Encoder::new(set);
        let mut other = Encoder::new(reversed);
        assert_eq!(one.symbols(0, 400), other.symbols(0, 400));
        // A range asked for later is the same as part of a longer prefix
        assert_eq!(one.symbols(100, 50).to_vec(), other.symbols(0, 150)[100..]);
    }

    /// Garbage that is no encoding of any set never decodes, and decoding
    /// it ends
    #[test]
    fn garbage_input_does_not_decode() {
        let mut decoder = Decoder::new((0..100).map(item));
        let garbage: Vec<CodedSymbol> = (0..2000u64)
            .map(|i| CodedSymbol {
                count: (i % 3) as i64 - 1,
                sum: item(i * 7 + 1),
                check: i.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            })
            .collect();
        decoder.add_symbols(&garbage);
        assert!(!decoder.is_done());
    }

    /// The regression of 15 September: with a linear check, a cell mixing
    /// three items looked pure. The check is a hash of the item, so a mixed
    /// cell is not mistaken for one.
    #[test]
    fn a_mixed_cell_is_not_pure() {
        let mut cell = CodedSymbol::ZERO;
        for i in 0..3 {
            let it = item(i);
            cell.apply(&it, item_hash(&it).0, 1);
        }
        cell.count = 1;
        assert!(!cell.is_pure());
    }

    #[test]
    fn index_zero_holds_every_item_and_density_falls() {
        let items: Vec<Item> = (0..4000).map(item).collect();
        let mut encoder = Encoder::new(items);
        let symbols = encoder.symbols(0, 101).to_vec();
        assert_eq!(symbols[0].count, 4000);
        // ρ(i) = 1 / (1 + i/2): about 2000 at index 2, about 78 at index 100
        assert!(
            (1700..2300).contains(&symbols[2].count),
            "{}",
            symbols[2].count
        );
        assert!(
            (40..130).contains(&symbols[100].count),
            "{}",
            symbols[100].count
        );
    }

    #[test]
    fn symbols_serialize() {
        let mut encoder = Encoder::new((0..10).map(item));
        let symbol = encoder.symbols(0, 1)[0];
        let mut bytes = Vec::new();
        symbol.serialize(&mut bytes);
        assert_eq!(bytes.len(), CodedSymbol::SERIALIZED_SIZE);
        assert_eq!(CodedSymbol::deserialize(&bytes), Some(symbol));
        assert_eq!(CodedSymbol::deserialize(&bytes[1..]), None);
    }

    /*
     * Test helpers
     */

    fn item(i: u64) -> Item {
        let mut item = [0; ITEM_SIZE];
        item[..8].copy_from_slice(&i.to_le_bytes());
        item[ITEM_SIZE - 1] = (i % 251) as u8;
        item
    }

    /// Streams symbols in growing batches until the decoder is done or
    /// fails; returns the symbols used
    fn decode(encoder: &mut Encoder, decoder: &mut Decoder, first: usize) -> usize {
        let mut batch = first;
        while !decoder.is_done() && !decoder.failed() && decoder.received() < MAX_SYMBOLS {
            let from = decoder.received();
            let symbols = encoder.symbols(from, batch).to_vec();
            for symbol in symbols {
                decoder.add_symbols(&[symbol]);
                if decoder.is_done() {
                    break;
                }
            }
            batch = (batch * 2).min(500);
        }
        decoder.received()
    }
}
