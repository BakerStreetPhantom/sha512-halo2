use super::{
    super::DIGEST_SIZE,
    util::{i2lebsp, lebs2ip},
    AssignedBits, BlockWord, SpreadInputs, SpreadVar, Table16Assignment, ROUNDS, STATE,
};

use halo2_proofs::{
    circuit::{Layouter, Value},
    halo2curves::bn256,
    plonk::{Advice, Column, ConstraintSystem, Error, Selector},
    poly::Rotation,
};

use std::convert::TryInto;
use std::ops::Range;

mod compression_gates;
mod compression_util;
mod subregion_digest;
mod subregion_initial;
mod subregion_main;

use compression_gates::CompressionGate;

pub trait UpperSigmaVar<
    const A_LEN: usize,
    const B_LEN: usize,
    const C_LEN: usize,
    const D_LEN: usize,
>
{
    fn spread_a(&self) -> Value<[bool; A_LEN]>;
    fn spread_b(&self) -> Value<[bool; B_LEN]>;
    fn spread_c(&self) -> Value<[bool; C_LEN]>;
    fn spread_d(&self) -> Value<[bool; D_LEN]>;

    fn xor_upper_sigma(&self) -> Value<[bool; 128]> {
        self.spread_a()
            .zip(self.spread_b())
            .zip(self.spread_c())
            .zip(self.spread_d())
            .map(|(((a, b), c), d)| {
                let xor_0 = b
                    .iter()
                    .chain(c.iter())
                    .chain(d.iter())
                    .chain(a.iter())
                    .copied()
                    .collect::<Vec<_>>();
                let xor_1 = c
                    .iter()
                    .chain(d.iter())
                    .chain(a.iter())
                    .chain(b.iter())
                    .copied()
                    .collect::<Vec<_>>();
                let xor_2 = d
                    .iter()
                    .chain(a.iter())
                    .chain(b.iter())
                    .chain(c.iter())
                    .copied()
                    .collect::<Vec<_>>();

                let xor_0 = lebs2ip::<128>(&xor_0.try_into().unwrap());
                let xor_1 = lebs2ip::<128>(&xor_1.try_into().unwrap());
                let xor_2 = lebs2ip::<128>(&xor_2.try_into().unwrap());

                i2lebsp(xor_0 + xor_1 + xor_2)
            })
    }
}

/// A variable that represents the `[A,B,C,D]` words of the SHA-512 internal state.
///
/// The structure of this variable is influenced by the following factors:
/// - In `Σ_0(A)` we need `A` to be split into pieces `(a,b,c,d)` of lengths `(28,6,5,25)`
///   bits respectively (counting from the little end), as well as their spread forms.
/// - `Maj(A,B,C)` requires having the bits of each input in spread form. For `A` we can
///   reuse the pieces from `Σ_0(A)`. Since `B` and `C` are assigned from `A` and `B`
///   respectively in each round, we therefore also have the same pieces in earlier rows.
///   We align the columns to make it efficient to copy-constrain these forms where they
///   are needed.
#[derive(Clone, Debug)]
pub struct AbcdVar {
    a_lo: SpreadVar<14, 28>,
    a_hi: SpreadVar<14, 28>,
    b_lo: SpreadVar<3,6>,
    b_hi: SpreadVar<3,6>,
    c_lo: SpreadVar<2, 4>,
    c_hi: SpreadVar<3, 6>,
    d_lo: SpreadVar<14, 28>,
    d_hi: SpreadVar<11, 22>,
}

impl AbcdVar {
    fn a_lo_range() -> Range<usize> {
        0..14
    }
    fn a_hi_range() -> Range<usize> {
        14..28
    }

    fn b_lo_range() -> Range<usize> {
        28..31
    }

    fn b_hi_range() -> Range<usize> {
        31..34
    }

    fn c_lo_range() -> Range<usize> {
        34..36
    }

    fn c_hi_range() -> Range<usize> {
        36..39
    }

    fn d_lo_range() -> Range<usize> {
        39..53
    }
    fn d_hi_range() -> Range<usize> {
        53..64
    }

    fn pieces(val: u64) -> Vec<Vec<bool>> {
        let val: [bool; 64] = i2lebsp(val.into());
        vec![
            val[Self::a_lo_range()].to_vec(),
            val[Self::a_hi_range()].to_vec(),
            val[Self::b_lo_range()].to_vec(),
            val[Self::b_hi_range()].to_vec(),
            val[Self::c_lo_range()].to_vec(),
            val[Self::c_hi_range()].to_vec(),
            val[Self::d_lo_range()].to_vec(),
            val[Self::d_hi_range()].to_vec(),
        ]
    }
}

impl UpperSigmaVar<56,12,10,50> for AbcdVar {
    fn spread_a(&self) -> Value<[bool; 56]> {
        self.a_lo
        .spread
        .value()
        .zip(self.a_hi.spread.value())
        .map(|(a_lo, a_hi)| {
            a_lo.iter()
                .chain(a_hi.iter())
                .copied()
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        })
    }

    fn spread_b(&self) -> Value<[bool; 12]> {
        self.b_lo
        .spread
        .value()
        .zip(self.b_hi.spread.value())
        .map(|(b_lo, b_hi)| {
            b_lo.iter()
                .chain(b_hi.iter())
                .copied()
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        })
    }

    fn spread_c(&self) -> Value<[bool; 10]> {
        self.c_lo
        .spread
        .value()
        .zip(self.c_hi.spread.value())
        .map(|(c_lo, c_hi)| {
            c_lo.iter()
                .chain(c_hi.iter())
                .copied()
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        })
    }

    fn spread_d(&self) -> Value<[bool; 50]> {
        self.d_lo
        .spread
        .value()
        .zip(self.d_hi.spread.value())
        .map(|(d_lo, d_hi)| {
            d_lo.iter()
                .chain(d_hi.iter())
                .copied()
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        })
    }
}

/// A variable that represents the `[E,F,G,H]` words of the SHA-512 internal state.
///
/// The structure of this variable is influenced by the following factors:
/// - In `Σ_1(E)` we need `E` to be split into pieces `(a,b,c,d)` of lengths `(14,4,23,23)`
///   bits respectively (counting from the little end), as well as their spread forms.
/// - `Ch(E,F,G)` requires having the bits of each input in spread form. For `E` we can
///   reuse the pieces from `Σ_1(E)`. Since `F` and `G` are assigned from `E` and `F`
///   respectively in each round, we therefore also have the same pieces in earlier rows.
///   We align the columns to make it efficient to copy-constrain these forms where they
///   are needed.
#[derive(Clone, Debug)]
pub struct EfghVar {
    a: SpreadVar<14, 28>,
    b_lo: SpreadVar<2, 4>,
    b_hi: SpreadVar<2, 4>,
    c_lo: SpreadVar<13, 26>,
    c_hi: SpreadVar<10, 20>,
    d_lo: SpreadVar<13, 26>,
    d_hi: SpreadVar<10, 20>,
}

impl EfghVar {
    fn a_range() -> Range<usize> {
        0..14
    }

    fn b_lo_range() -> Range<usize> {
        14..16
    }

    fn b_hi_range() -> Range<usize> {
        16..18
    }

    fn c_lo_range() -> Range<usize> {
        18..31
    }
    fn c_hi_range() -> Range<usize> {
        31..41
    }

    fn d_lo_range() -> Range<usize> {
        41..54
    }
    fn d_hi_range() -> Range<usize> {
        54..64
    }

    fn pieces(val: u64) -> Vec<Vec<bool>> {
        let val: [bool; 64] = i2lebsp(val.into());
        vec![
            val[Self::a_range()].to_vec(),
            val[Self::b_lo_range()].to_vec(),
            val[Self::b_hi_range()].to_vec(),
            val[Self::c_lo_range()].to_vec(),
            val[Self::c_hi_range()].to_vec(),
            val[Self::d_lo_range()].to_vec(),
            val[Self::d_hi_range()].to_vec(),
        ]
    }
}
impl UpperSigmaVar<28, 8, 46, 46> for EfghVar {
    fn spread_a(&self) -> Value<[bool; 28]> {
        self.a.spread.value().map(|v| v.0)
    }

    fn spread_b(&self) -> Value<[bool; 8]> {
        self.b_lo
            .spread
            .value()
            .zip(self.b_hi.spread.value())
            .map(|(b_lo, b_hi)| {
                b_lo.iter()
                    .chain(b_hi.iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap()
            })
    }

    fn spread_c(&self) -> Value<[bool; 46]> {
        self.c_lo
            .spread
            .value()
            .zip(self.c_hi.spread.value())
            .map(|(c_lo, c_hi)| {
                c_lo.iter()
                    .chain(c_hi.iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap()
            })
    }

    fn spread_d(&self) -> Value<[bool; 46]> {
        self.d_lo
            .spread
            .value()
            .zip(self.d_hi.spread.value())
            .map(|(d_lo, d_hi)| {
                d_lo.iter()
                    .chain(d_hi.iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap()
            })
    }
}

#[derive(Clone, Debug)]
pub struct RoundWordDense(AssignedBits<32>, AssignedBits<32>);

impl From<(AssignedBits<32>, AssignedBits<32>)> for RoundWordDense {
    fn from(halves: (AssignedBits<32>, AssignedBits<32>)) -> Self {
        Self(halves.0, halves.1)
    }
}

impl RoundWordDense {
    pub fn value(&self) -> Value<u64> {
        self.0
            .value_u32()
            .zip(self.1.value_u32())
            .map(|(lo, hi)| lo as u64 + (1 << 32) * hi as u64)
    }
}

#[derive(Clone, Debug)]
pub struct RoundWordSpread(AssignedBits<64>, AssignedBits<64>);

impl From<(AssignedBits<64>, AssignedBits<64>)> for RoundWordSpread {
    fn from(halves: (AssignedBits<64>, AssignedBits<64>)) -> Self {
        Self(halves.0, halves.1)
    }
}

impl RoundWordSpread {
    pub fn value(&self) -> Value<u128> {
        self.0
            .value_u64()
            .zip(self.1.value_u64())
            .map(|(lo, hi)| lo as u128 + (1 << 64) * hi as u128)
    }
}


#[derive(Clone, Debug)]
pub struct RoundWordA {
    pieces: Option<AbcdVar>,
    dense_halves: RoundWordDense,
    spread_halves: Option<RoundWordSpread>,
}

impl RoundWordA {
    pub fn new(
        pieces: AbcdVar,
        dense_halves: RoundWordDense,
        spread_halves: RoundWordSpread,
    ) -> Self {
        RoundWordA {
            pieces: Some(pieces),
            dense_halves,
            spread_halves: Some(spread_halves),
        }
    }

    pub fn new_dense(dense_halves: RoundWordDense) -> Self {
        RoundWordA {
            pieces: None,
            dense_halves,
            spread_halves: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RoundWordE {
    pieces: Option<EfghVar>,
    dense_halves: RoundWordDense,
    spread_halves: Option<RoundWordSpread>,
}

impl RoundWordE {
    pub fn new(
        pieces: EfghVar,
        dense_halves: RoundWordDense,
        spread_halves: RoundWordSpread,
    ) -> Self {
        RoundWordE {
            pieces: Some(pieces),
            dense_halves,
            spread_halves: Some(spread_halves),
        }
    }

    pub fn new_dense(dense_halves: RoundWordDense) -> Self {
        RoundWordE {
            pieces: None,
            dense_halves,
            spread_halves: None,
        }
    }
}


#[derive(Clone, Debug)]
pub struct RoundWord {
    dense_halves: RoundWordDense,
    spread_halves: RoundWordSpread,
}

impl RoundWord {
    pub fn new(dense_halves: RoundWordDense, spread_halves: RoundWordSpread) -> Self {
        RoundWord {
            dense_halves,
            spread_halves,
        }
    }
}

/// The internal state for SHA-512.
#[derive(Clone, Debug)]
pub struct State {
    a: Option<StateWord>,
    b: Option<StateWord>,
    c: Option<StateWord>,
    d: Option<StateWord>,
    e: Option<StateWord>,
    f: Option<StateWord>,
    g: Option<StateWord>,
    h: Option<StateWord>,
}

impl State {
    #[allow(clippy::many_single_char_names)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        a: StateWord,
        b: StateWord,
        c: StateWord,
        d: StateWord,
        e: StateWord,
        f: StateWord,
        g: StateWord,
        h: StateWord,
    ) -> Self {
        State {
            a: Some(a),
            b: Some(b),
            c: Some(c),
            d: Some(d),
            e: Some(e),
            f: Some(f),
            g: Some(g),
            h: Some(h),
        }
    }

    pub fn empty_state() -> Self {
        State {
            a: None,
            b: None,
            c: None,
            d: None,
            e: None,
            f: None,
            g: None,
            h: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum StateWord {
    A(RoundWordA),
    B(RoundWord),
    C(RoundWord),
    D(RoundWordDense),
    E(RoundWordE),
    F(RoundWord),
    G(RoundWord),
    H(RoundWordDense),
}


#[derive(Clone, Debug)]
pub(super) struct CompressionConfig {
    lookup: SpreadInputs,
    message_schedule: Column<Advice>,
    extras: [Column<Advice>; 6],

    s_ch: Selector,
    s_ch_neg: Selector,
    s_maj: Selector,
    s_h_prime: Selector,
    s_a_new: Selector,
    s_e_new: Selector,

    s_upper_sigma_0: Selector,
    s_upper_sigma_1: Selector,

    // Decomposition gate for AbcdVar
    s_decompose_abcd: Selector,
    // Decomposition gate for EfghVar
    s_decompose_efgh: Selector,

    s_digest: Selector,
}

impl Table16Assignment for CompressionConfig {}

impl CompressionConfig {
    pub(super) fn configure(
        meta: &mut ConstraintSystem<bn256::Fr>,
        lookup: SpreadInputs,
        message_schedule: Column<Advice>,
        extras: [Column<Advice>; 6],
    ) -> Self {
        let s_ch = meta.selector();
        let s_ch_neg = meta.selector();
        let s_maj = meta.selector();
        let s_h_prime = meta.selector();
        let s_a_new = meta.selector();
        let s_e_new = meta.selector();

        let s_upper_sigma_0 = meta.selector();
        let s_upper_sigma_1 = meta.selector();

        // Decomposition gate for AbcdVar
        let s_decompose_abcd = meta.selector();
        // Decomposition gate for EfghVar
        let s_decompose_efgh = meta.selector();

        let s_digest = meta.selector();

        // Rename these here for ease of matching the gates to the specification.
        let a_0 = lookup.tag;
        let a_1 = lookup.dense;
        let a_2 = lookup.spread;
        let a_3 = extras[0];
        let a_4 = extras[1];
        let a_5 = message_schedule;
        let a_6 = extras[2];
        let a_7 = extras[3];
        let a_8 = extras[4];
        let a_9 = extras[5];
        // Decompose `A,B,C,D` words into (28, 6, 5, 25)-bit chunks.
        // `b` is split into (3,3)-bit b_lo and b_hi.
        // `c` is split into (2,3)-bit c_lo and c_hi.
        meta.create_gate("decompose ABCD", |meta| {
            let s_decompose_abcd = meta.query_selector(s_decompose_abcd);
            let a_lo = meta.query_advice(a_1, Rotation::cur()); // 14-bit chunk
            let spread_a_lo = meta.query_advice(a_2, Rotation::cur());
            let tag_a_lo = meta.query_advice(a_0, Rotation::cur());
            let a_hi = meta.query_advice(a_1, Rotation::next()); // 14-bit chunk
            let spread_a_hi = meta.query_advice(a_2, Rotation::next());
            let tag_a_hi = meta.query_advice(a_0, Rotation::next());
            let b_lo = meta.query_advice(a_3, Rotation::cur()); // 3-bit chunk
            let spread_b_lo = meta.query_advice(a_4, Rotation::cur());
            let b_hi = meta.query_advice(a_3, Rotation::next()); //3-bit chunk
            let spread_b_hi = meta.query_advice(a_4,Rotation::next());
            let c_lo = meta.query_advice(a_3, Rotation(2)); // 2-bit chunk
            let spread_c_lo = meta.query_advice(a_4, Rotation(2));
            // let c_mid = meta.query_advice(a_5, Rotation::cur()); // 3-bit chunk
            // let spread_c_mid = meta.query_advice(a_6, Rotation::cur());
            let c_hi = meta.query_advice(a_3, Rotation(3)); // 3-bit chunk
            let spread_c_hi = meta.query_advice(a_4, Rotation(3));
            let d_lo = meta.query_advice(a_1, Rotation(2)); // 14-bit chunk
            let spread_d_lo = meta.query_advice(a_2, Rotation(2));
            let tag_d_lo = meta.query_advice(a_0, Rotation(2));
            let d_hi = meta.query_advice(a_1, Rotation(3)); // 11-bit chunk
            let spread_d_hi = meta.query_advice(a_2, Rotation(3));
            let tag_d_hi = meta.query_advice(a_0, Rotation(3));
            let word_lo = meta.query_advice(a_7, Rotation::cur());
            let spread_word_lo = meta.query_advice(a_8, Rotation::cur());
            let word_hi = meta.query_advice(a_7, Rotation::next());
            let spread_word_hi = meta.query_advice(a_8, Rotation::next());

            CompressionGate::s_decompose_abcd(
                s_decompose_abcd,
                a_lo,
                spread_a_lo,
                tag_a_lo,
                a_hi,
                spread_a_hi,
                tag_a_hi,
                b_lo,
                spread_b_lo,
                b_hi,
                spread_b_hi,
                c_lo,
                spread_c_lo,
                c_hi,
                spread_c_hi,
                d_lo,
                spread_d_lo,
                tag_d_lo,
                d_hi,
                spread_d_hi,
                tag_d_hi,
                word_lo,
                spread_word_lo,
                word_hi,
                spread_word_hi,
            )
        });
        // Decompose `E,F,G,H` words into (14, 4, 23, 23)-bit chunks.
        // `b` is split into (2, 2)-bit b_lo, b_hi
        meta.create_gate("Decompose EFGH", |meta| {
            let s_decompose_efgh = meta.query_selector(s_decompose_efgh);
            let a = meta.query_advice(a_1, Rotation::cur()); // 14-bit chunk
            let spread_a = meta.query_advice(a_2, Rotation::cur());
            let tag_a = meta.query_advice(a_0,Rotation::cur());
            // let a_hi = meta.query_advice(a_5, Rotation::next()); // 3-bit chunk
            // let spread_a_hi = meta.query_advice(a_6, Rotation::next());
            let b_lo = meta.query_advice(a_3, Rotation::cur()); // 2-bit chunk
            let spread_b_lo = meta.query_advice(a_4, Rotation::cur());
            let b_hi = meta.query_advice(a_3, Rotation::next()); // 2-bit chunk
            let spread_b_hi = meta.query_advice(a_4, Rotation::next());
            let c_lo = meta.query_advice(a_1, Rotation::next()); // 13-bit chunk
            let spread_c_lo = meta.query_advice(a_2, Rotation::next());
            let tag_c_lo = meta.query_advice(a_0, Rotation::next());
            let c_hi = meta.query_advice(a_1, Rotation(2)); // 10-bit chunk
            let spread_c_hi = meta.query_advice(a_2, Rotation(2));
            let tag_c_hi = meta.query_advice(a_0, Rotation(2));
            let d_lo = meta.query_advice(a_1, Rotation(3)); // 13-bit chunk
            let spread_d_lo = meta.query_advice(a_2, Rotation(3));
            let tag_d_lo = meta.query_advice(a_0, Rotation(3));
            let d_hi = meta.query_advice(a_1, Rotation(4)); // 10-bit chunk
            let spread_d_hi = meta.query_advice(a_2, Rotation(4));
            let tag_d_hi = meta.query_advice(a_0, Rotation(4));
            let word_lo = meta.query_advice(a_7, Rotation::cur());
            let spread_word_lo = meta.query_advice(a_8, Rotation::cur());
            let word_hi = meta.query_advice(a_7, Rotation::next());
            let spread_word_hi = meta.query_advice(a_8, Rotation::next());

            CompressionGate::s_decompose_efgh(
                s_decompose_efgh,
                a,
                spread_a,
                tag_a,
                b_lo,
                spread_b_lo,
                b_hi,
                spread_b_hi,
                c_lo,
                spread_c_lo,
                tag_c_lo,
                c_hi,
                spread_c_hi,
                tag_c_hi,
                d_lo,
                spread_d_lo,
                tag_d_lo,
                d_hi,
                spread_d_hi,
                tag_d_hi,
                word_lo,
                spread_word_lo,
                word_hi,
                spread_word_hi,
            )
        });

        // s_upper_sigma_0 on abcd words
        // (28, 6, 5, 25)-bit chunks
        meta.create_gate("s_upper_sigma_0", |meta| {
            let s_upper_sigma_0 = meta.query_selector(s_upper_sigma_0);
            let spread_r0_even_lo = meta.query_advice(a_2, Rotation::prev()); // spread_r0_even_lo
            let spread_r0_even_hi = meta.query_advice(a_2, Rotation::cur()); // spread_r0_even_hi
            let spread_r0_odd_lo = meta.query_advice(a_2, Rotation::next());  // spread_r0_odd_lo
            let spread_r0_odd_hi = meta.query_advice(a_2, Rotation(2)); // spread_r0_odd_hi
            let spread_r1_even_lo = meta.query_advice(a_2, Rotation(3)); // spread_r1_even_lo
            let spread_r1_even_hi = meta.query_advice(a_2, Rotation(4)); // spread_r1_even_hi
            let spread_r1_odd_lo = meta.query_advice(a_2, Rotation(5));  // spread_r1_odd_lo
            let spread_r1_odd_hi = meta.query_advice(a_2, Rotation(6));  // spread_r1_odd_hi

            let spread_a_lo = meta.query_advice(a_3, Rotation::next());
            let spread_a_hi = meta.query_advice(a_4, Rotation::prev());
            let spread_b_lo = meta.query_advice(a_4, Rotation::cur());
            let spread_b_hi = meta.query_advice(a_4, Rotation::next());
            let spread_c_lo = meta.query_advice(a_5, Rotation::prev());
            // let spread_c_mid = meta.query_advice(a_4, Rotation::prev());
            let spread_c_hi = meta.query_advice(a_5, Rotation::cur());
            let spread_d_lo = meta.query_advice(a_5, Rotation::next());
            let spread_d_hi = meta.query_advice(a_3, Rotation::prev());

            CompressionGate::s_upper_sigma_0(
                s_upper_sigma_0,
                spread_r0_even_lo,
                spread_r0_even_hi,
                spread_r0_odd_lo,
                spread_r0_odd_hi,
                spread_r1_even_lo,
                spread_r1_even_hi,
                spread_r1_odd_lo,
                spread_r1_odd_hi,
                spread_a_lo,
                spread_a_hi,
                spread_b_lo,
                spread_b_hi,
                spread_c_lo,
                spread_c_hi,
                spread_d_lo,
                spread_d_hi,
            )
        });

        // s_upper_sigma_1 on efgh words
        // (14, 4, 23, 23)-bit chunks
        meta.create_gate("s_upper_sigma_1", |meta| {
            let s_upper_sigma_1 = meta.query_selector(s_upper_sigma_1);
            let spread_r0_even_lo = meta.query_advice(a_2, Rotation::prev()); // spread_r0_even_lo
            let spread_r0_even_hi = meta.query_advice(a_2, Rotation::cur()); // spread_r0_even_hi
            let spread_r0_odd_lo = meta.query_advice(a_2, Rotation::next());  // spread_r0_odd_lo
            let spread_r0_odd_hi = meta.query_advice(a_2, Rotation(2)); // spread_r0_odd_hi
            let spread_r1_even_lo = meta.query_advice(a_2, Rotation(3)); // spread_r1_even_lo
            let spread_r1_even_hi = meta.query_advice(a_2, Rotation(4)); // spread_r1_even_hi
            let spread_r1_odd_lo = meta.query_advice(a_2, Rotation(5));  // spread_r1_odd_lo
            let spread_r1_odd_hi = meta.query_advice(a_2, Rotation(6));  // spread_r1_odd_hi

            let spread_a = meta.query_advice(a_3, Rotation::next());
            // let spread_a_hi = meta.query_advice(a_4, Rotation::next());
            let spread_b_lo = meta.query_advice(a_4, Rotation::prev());
            let spread_b_hi = meta.query_advice(a_4, Rotation::cur());
            let spread_c_lo = meta.query_advice(a_4, Rotation::next());
            let spread_c_hi = meta.query_advice(a_5, Rotation::prev());
            let spread_d_lo = meta.query_advice(a_5, Rotation::cur());
            let spread_d_hi = meta.query_advice(a_5, Rotation::next());

            CompressionGate::s_upper_sigma_1(
                s_upper_sigma_1,
                spread_r0_even_lo,
                spread_r0_even_hi,
                spread_r0_odd_lo,
                spread_r0_odd_hi,
                spread_r1_even_lo,
                spread_r1_even_hi,
                spread_r1_odd_lo,
                spread_r1_odd_hi,
                spread_a,
                spread_b_lo,
                spread_b_hi,
                spread_c_lo,
                spread_c_hi,
                spread_d_lo,
                spread_d_hi,
            )
        });

        // s_ch on efgh words (14,4,23,23)
        // First part of choice gate on (E, F, G), E ∧ F
        meta.create_gate("s_ch", |meta| {
            let s_ch = meta.query_selector(s_ch);
            let spread_p0_even_lo = meta.query_advice(a_2, Rotation::prev()); // spread_r0_even_lo
            let spread_p0_even_hi = meta.query_advice(a_2, Rotation::cur()); // spread_r0_even_hi
            let spread_p0_odd_lo = meta.query_advice(a_2, Rotation::next());  // spread_r0_odd_lo
            let spread_p0_odd_hi = meta.query_advice(a_2, Rotation(2)); // spread_r0_odd_hi
            let spread_p1_even_lo = meta.query_advice(a_2, Rotation(3)); // spread_r1_even_lo
            let spread_p1_even_hi = meta.query_advice(a_2, Rotation(4)); // spread_r1_even_hi
            let spread_p1_odd_lo = meta.query_advice(a_2, Rotation(5));  // spread_r1_odd_lo
            let spread_p1_odd_hi = meta.query_advice(a_2, Rotation(6));  // spread_r1_odd_hi

            let spread_e_lo = meta.query_advice(a_3, Rotation::prev());
            let spread_e_hi = meta.query_advice(a_4, Rotation::prev());
            let spread_f_lo = meta.query_advice(a_3, Rotation::next());
            let spread_f_hi = meta.query_advice(a_4, Rotation::next());

            CompressionGate::s_ch(
                s_ch,
                spread_p0_even_lo,
                spread_p0_even_hi,
                spread_p0_odd_lo,
                spread_p0_odd_hi,
                spread_p1_even_lo,
                spread_p1_even_hi,
                spread_p1_odd_lo,
                spread_p1_odd_hi,
                spread_e_lo,
                spread_e_hi,
                spread_f_lo,
                spread_f_hi,
            )
        });

        // s_ch_neg on efgh words
        // Second part of Choice gate on (E, F, G), ¬E ∧ G
        meta.create_gate("s_ch_neg", |meta| {
            let s_ch_neg = meta.query_selector(s_ch_neg);
            let spread_q0_even_lo = meta.query_advice(a_2, Rotation::prev()); // spread_r0_even_lo
            let spread_q0_even_hi = meta.query_advice(a_2, Rotation::cur()); // spread_r0_even_hi
            let spread_q0_odd_lo = meta.query_advice(a_2, Rotation::next());  // spread_r0_odd_lo
            let spread_q0_odd_hi = meta.query_advice(a_2, Rotation(2)); // spread_r0_odd_hi
            let spread_q1_even_lo = meta.query_advice(a_2, Rotation(3)); // spread_r1_even_lo
            let spread_q1_even_hi = meta.query_advice(a_2, Rotation(4)); // spread_r1_even_hi
            let spread_q1_odd_lo = meta.query_advice(a_2, Rotation(5));  // spread_r1_odd_lo
            let spread_q1_odd_hi = meta.query_advice(a_2, Rotation(6));  // spread_r1_odd_hi
            let spread_e_lo = meta.query_advice(a_5, Rotation::prev());
            let spread_e_hi = meta.query_advice(a_5, Rotation::cur());
            let spread_e_neg_lo = meta.query_advice(a_3, Rotation::prev());
            let spread_e_neg_hi = meta.query_advice(a_4, Rotation::prev());
            let spread_g_lo = meta.query_advice(a_3, Rotation::next());
            let spread_g_hi = meta.query_advice(a_4, Rotation::next());

            CompressionGate::s_ch_neg(
                s_ch_neg,
                spread_q0_even_lo,
                spread_q0_even_hi,
                spread_q0_odd_lo,
                spread_q0_odd_hi,
                spread_q1_even_lo,
                spread_q1_even_hi,
                spread_q1_odd_lo,
                spread_q1_odd_hi,
                spread_e_lo,
                spread_e_hi,
                spread_e_neg_lo,
                spread_e_neg_hi,
                spread_g_lo,
                spread_g_hi,
            )
        });

        // s_maj on abcd words
        meta.create_gate("s_maj", |meta| {
            let s_maj = meta.query_selector(s_maj);
            let spread_q0_even_lo = meta.query_advice(a_2, Rotation::prev()); // spread_r0_even_lo
            let spread_q0_even_hi = meta.query_advice(a_2, Rotation::cur()); // spread_r0_even_hi
            let spread_q0_odd_lo = meta.query_advice(a_2, Rotation::next());  // spread_r0_odd_lo
            let spread_q0_odd_hi = meta.query_advice(a_2, Rotation(2)); // spread_r0_odd_hi
            let spread_q1_even_lo = meta.query_advice(a_2, Rotation(3)); // spread_r1_even_lo
            let spread_q1_even_hi = meta.query_advice(a_2, Rotation(4)); // spread_r1_even_hi
            let spread_q1_odd_lo = meta.query_advice(a_2, Rotation(5));  // spread_r1_odd_lo
            let spread_q1_odd_hi = meta.query_advice(a_2, Rotation(6));  // spread_r1_odd_hi
            let spread_a_lo = meta.query_advice(a_4, Rotation::prev());
            let spread_a_hi = meta.query_advice(a_5, Rotation::prev());
            let spread_b_lo = meta.query_advice(a_4, Rotation::cur());
            let spread_b_hi = meta.query_advice(a_5, Rotation::cur());
            let spread_c_lo = meta.query_advice(a_4, Rotation::next());
            let spread_c_hi = meta.query_advice(a_5, Rotation::next());

            CompressionGate::s_maj(
                s_maj,
                spread_q0_even_lo,
                spread_q0_even_hi,
                spread_q0_odd_lo,
                spread_q0_odd_hi,
                spread_q1_even_lo,
                spread_q1_even_hi,
                spread_q1_odd_lo,
                spread_q1_odd_hi,
                spread_a_lo,
                spread_a_hi,
                spread_b_lo,
                spread_b_hi,
                spread_c_lo,
                spread_c_hi,
            )
        });

        // s_h_prime to compute H' = H + Ch(E, F, G) + s_upper_sigma_1(E) + K + W
        meta.create_gate("s_h_prime", |meta| {
            let s_h_prime = meta.query_selector(s_h_prime);
            let h_prime_lo = meta.query_advice(a_7, Rotation::next());
            let h_prime_hi = meta.query_advice(a_8, Rotation::next());
            let h_prime_carry = meta.query_advice(a_9, Rotation::next());
            let sigma_e_lo = meta.query_advice(a_4, Rotation::cur());
            let sigma_e_hi = meta.query_advice(a_5, Rotation::cur());
            let ch_lo = meta.query_advice(a_3, Rotation(3)); //changed
            let ch_hi = meta.query_advice(a_6, Rotation::next());
            let ch_neg_lo = meta.query_advice(a_5, Rotation::prev());
            let ch_neg_hi = meta.query_advice(a_5, Rotation::next());
            let h_lo = meta.query_advice(a_7, Rotation::prev());
            let h_hi = meta.query_advice(a_7, Rotation::cur());
            let k_lo = meta.query_advice(a_6, Rotation::prev());
            let k_hi = meta.query_advice(a_6, Rotation::cur());
            let w_lo = meta.query_advice(a_8, Rotation::prev());
            let w_hi = meta.query_advice(a_8, Rotation::cur());

            CompressionGate::s_h_prime(
                s_h_prime,
                h_prime_lo,
                h_prime_hi,
                h_prime_carry,
                sigma_e_lo,
                sigma_e_hi,
                ch_lo,
                ch_hi,
                ch_neg_lo,
                ch_neg_hi,
                h_lo,
                h_hi,
                k_lo,
                k_hi,
                w_lo,
                w_hi,
            )
        });

        // s_a_new
        meta.create_gate("s_a_new", |meta| {
            let s_a_new = meta.query_selector(s_a_new);
            let a_new_lo = meta.query_advice(a_8, Rotation::cur());
            let a_new_hi = meta.query_advice(a_8, Rotation::next());
            let a_new_carry = meta.query_advice(a_9, Rotation::cur());
            let sigma_a_lo = meta.query_advice(a_6, Rotation::cur());
            let sigma_a_hi = meta.query_advice(a_6, Rotation::next());
            let maj_abc_lo = meta.query_advice(a_3, Rotation(3)); //changed
            let maj_abc_hi = meta.query_advice(a_3, Rotation::prev());
            let h_prime_lo = meta.query_advice(a_7, Rotation::prev());
            let h_prime_hi = meta.query_advice(a_8, Rotation::prev());

            CompressionGate::s_a_new(
                s_a_new,
                a_new_lo,
                a_new_hi,
                a_new_carry,
                sigma_a_lo,
                sigma_a_hi,
                maj_abc_lo,
                maj_abc_hi,
                h_prime_lo,
                h_prime_hi,
            )
        });

        // s_e_new
        meta.create_gate("s_e_new", |meta| {
            let s_e_new = meta.query_selector(s_e_new);
            let e_new_lo = meta.query_advice(a_8, Rotation::cur());
            let e_new_hi = meta.query_advice(a_8, Rotation::next());
            let e_new_carry = meta.query_advice(a_9, Rotation::next());
            let d_lo = meta.query_advice(a_7, Rotation::cur());
            let d_hi = meta.query_advice(a_7, Rotation::next());
            let h_prime_lo = meta.query_advice(a_7, Rotation::prev());
            let h_prime_hi = meta.query_advice(a_8, Rotation::prev());

            CompressionGate::s_e_new(
                s_e_new,
                e_new_lo,
                e_new_hi,
                e_new_carry,
                d_lo,
                d_hi,
                h_prime_lo,
                h_prime_hi,
            )
        });

        // s_digest for final round
        meta.create_gate("s_digest", |meta| {
            let s_digest = meta.query_selector(s_digest);
            let lo_0 = meta.query_advice(a_3, Rotation::cur());
            let hi_0 = meta.query_advice(a_4, Rotation::cur());
            let word_0 = meta.query_advice(a_5, Rotation::cur());
            let lo_1 = meta.query_advice(a_6, Rotation::cur());
            let hi_1 = meta.query_advice(a_7, Rotation::cur());
            let word_1 = meta.query_advice(a_8, Rotation::cur());
            let lo_2 = meta.query_advice(a_3, Rotation::next());
            let hi_2 = meta.query_advice(a_4, Rotation::next());
            let word_2 = meta.query_advice(a_5, Rotation::next());
            let lo_3 = meta.query_advice(a_6, Rotation::next());
            let hi_3 = meta.query_advice(a_7, Rotation::next());
            let word_3 = meta.query_advice(a_8, Rotation::next());

            CompressionGate::s_digest(
                s_digest, lo_0, hi_0, word_0, lo_1, hi_1, word_1, lo_2, hi_2, word_2, lo_3, hi_3,
                word_3,
            )
        });

        CompressionConfig {
            lookup,
            message_schedule,
            extras,
            s_ch,
            s_ch_neg,
            s_maj,
            s_h_prime,
            s_a_new,
            s_e_new,
            s_upper_sigma_0,
            s_upper_sigma_1,
            s_decompose_abcd,
            s_decompose_efgh,
            s_digest,
        }
    }

    /// Initialize compression with a constant Initialization Vector of 64-byte words.
    /// Returns an initialized state.
    pub(super) fn initialize_with_iv(
        &self,
        layouter: &mut impl Layouter<bn256::Fr>,
        init_state: [u64; STATE],
    ) -> Result<State, Error> {
        let mut new_state = State::empty_state();
        layouter.assign_region(
            || "initialize_with_iv",
            |mut region| {
                new_state = self.initialize_iv(&mut region, init_state)?;
                Ok(())
            },
        )?;
        Ok(new_state)
    }

    /// Initialize compression with some initialized state. This could be a state
    /// output from a previous compression round.
    pub(super) fn initialize_with_state(
        &self,
        layouter: &mut impl Layouter<bn256::Fr>,
        init_state: State,
    ) -> Result<State, Error> {
        let mut new_state = State::empty_state();
        layouter.assign_region(
            || "initialize_with_state",
            |mut region| {
                new_state = self.initialize_state(&mut region, init_state.clone())?;
                Ok(())
            },
        )?;
        Ok(new_state)
    }

    /// Given an initialized state and a message schedule, perform 80 compression rounds.
    pub(super) fn compress(
        &self,
        layouter: &mut impl Layouter<bn256::Fr>,
        initialized_state: State,
        w_halves: [(AssignedBits<32>, AssignedBits<32>); ROUNDS],
    ) -> Result<State, Error> {
        let mut state = State::empty_state();
        layouter.assign_region(
            || "compress",
            |mut region| {
                state = initialized_state.clone();
                for (idx, w_halves) in w_halves.iter().enumerate() {
                    state = self.assign_round(&mut region, idx.into(), state.clone(), w_halves)?;
                }
                Ok(())
            },
        )?;
        Ok(state)
    }

    /// After the final round, convert the state into the final digest.
    pub(super) fn digest(
        &self,
        layouter: &mut impl Layouter<bn256::Fr>,
        state: State,
    ) -> Result<[BlockWord; DIGEST_SIZE], Error> {
        let mut digest = [BlockWord(Value::known(0)); DIGEST_SIZE];
        layouter.assign_region(
            || "digest",
            |mut region| {
                digest = self.assign_digest(&mut region, state.clone())?;
                
                Ok(())
            },
        )?;
        Ok(digest)
    }
}
#[cfg(test)]
mod tests {
    use super::super::{
        super::BLOCK_SIZE, msg_schedule_test_input, BlockWord, Table16Chip, Table16Config, IV,
    };
    use halo2_proofs::{
        circuit::{Layouter, SimpleFloorPlanner},
        dev::MockProver,
        plonk::{Circuit, ConstraintSystem, Error},
    };
    use halo2_proofs::halo2curves::bn256;

    #[test]
    fn compress() {
        struct MyCircuit {}

        impl Circuit<bn256::Fr> for MyCircuit {
            type Config = Table16Config;
            type FloorPlanner = SimpleFloorPlanner;

            fn without_witnesses(&self) -> Self {
                MyCircuit {}
            }

            fn configure(meta: &mut ConstraintSystem<bn256::Fr>) -> Self::Config {
                Table16Chip::configure(meta)
            }

            fn synthesize(
                &self,
                config: Self::Config,
                mut layouter: impl Layouter<bn256::Fr>,
            ) -> Result<(), Error> {
                Table16Chip::load(config.clone(), &mut layouter)?;

                // Test vector: "abc"
                let input: [BlockWord; BLOCK_SIZE] = msg_schedule_test_input();

                let (_, w_halves) = config.message_schedule.process(&mut layouter, input)?;

                let compression = config.compression.clone();
                let initial_state = compression.initialize_with_iv(&mut layouter, IV)?;

                let state = config
                    .compression
                    .compress(&mut layouter, initial_state, w_halves)?;

                let digest = config.compression.digest(&mut layouter, state)?;
                println!("{:?}",digest);
                for (idx, digest_word) in digest.iter().enumerate() {
                    digest_word.0.assert_if_known(|digest_word| {
                        println!("{:?},  {:?}",*digest_word,IV[idx]);
                        (*digest_word as u128 + IV[idx] as u128) as u64
                            == super::compression_util::COMPRESSION_OUTPUT[idx]
                    });
                }

                Ok(())
            }
        }

        let circuit: MyCircuit = MyCircuit {};

        let prover = match MockProver::<bn256::Fr>::run(19, &circuit, vec![]) {
            Ok(prover) => prover,
            Err(e) => panic!("{:?}", e),
        };
        assert_eq!(prover.verify(), Ok(()));
    }
}