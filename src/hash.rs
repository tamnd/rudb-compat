//! MD5, because the sqllogictest format is written in it.
//!
//! A `.test` file with more results than its hash threshold does not list them. It says `40 values
//! hashing to 3c13dee48d9356ae19af2515e05e6b54`, and the digest is over the concatenation of every
//! value with a newline after it, in the order the comparison would have read them. That is the
//! format, it is not going to change, and a runner that cannot check those records cannot run most
//! of DuckDB's corpus.
//!
//! It is read and it is never written. Section 9.3.1 of `spec/sql/duckdb/09-the-harness.md` is the
//! decision and the short version is that 231 of the corpus's 34329 query records store a digest,
//! 19 of them in files an ordinary run reads, and the corpus carries no `hash-threshold` line at
//! all. So this exists because 32 of DuckDB's files contain a digest, and for no other reason. The
//! harness never hashes a result it could have compared, no corpus written here stores one, and
//! there is no `hash-threshold` setting on this side. A digest that fails is one bit and there is
//! nothing in it to reduce, which is the whole argument.
//!
//! This is here rather than from a crate because the harness has no dependencies outside the
//! engines it compares, and because MD5 is a hundred lines that have not moved since 1992. Nothing
//! here is security relevant. The digest is a checksum over a list of strings a test file already
//! contains in plain text, and if somebody can choose the query results they can choose the
//! expected results too.

/// The per round left rotation amounts.
const SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// The round constants, which are `floor(2^32 * abs(sin(i + 1)))`.
///
/// Written out rather than computed, because computing them means calling `sin` sixty four times
/// per digest for a table that is the same every time, and because a transcription error in a
/// constant shows up as every digest being wrong rather than as one of them being subtly off.
const CONSTANTS: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// The MD5 of some bytes, lowercase hex, which is the spelling a `.test` file uses.
#[must_use]
pub fn md5_hex(input: &[u8]) -> String {
    let digest = md5(input);
    let mut out = String::with_capacity(32);
    for byte in digest {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// The MD5 of some bytes, as the sixteen bytes themselves.
#[must_use]
pub fn md5(input: &[u8]) -> [u8; 16] {
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

    // The message, padded to a multiple of sixty four bytes with a one bit, then zeros, then the
    // original length in bits as a little endian sixty four bit number. The padding is built into
    // a copy rather than streamed, because a `.test` file's values are already in memory and a
    // streaming interface would be more surface for nothing.
    let mut message = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_le_bytes());

    for block in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let at = index * 4;
            *word = u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]]);
        }

        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (mixed, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let sum = mixed.wrapping_add(a).wrapping_add(CONSTANTS[i]).wrapping_add(words[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(SHIFTS[i]));
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut out = [0u8; 16];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// The digest a `.test` file's `N values hashing to H` record is over.
///
/// Every value, in order, each followed by a newline. The trailing newline on the last value is
/// part of it, which is the sort of detail that makes a digest either match everything or nothing.
#[must_use]
pub fn hash_values(values: &[String]) -> String {
    let mut buffer = String::new();
    for value in values {
        buffer.push_str(value);
        buffer.push('\n');
    }
    md5_hex(buffer.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{hash_values, md5_hex};

    #[test]
    fn the_published_test_vectors_come_out_right() {
        // RFC 1321 appendix A.5, which is the only reason to trust an implementation of this.
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5_hex(b"message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(md5_hex(b"abcdefghijklmnopqrstuvwxyz"), "c3fcd3d76192e4007dfb496cca67e13b");
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn a_message_that_lands_exactly_on_a_block_boundary_still_gets_a_whole_block_of_padding() {
        // Fifty six bytes is the length where the one bit does not fit beside the length field,
        // so the padding runs into a second block. Getting this wrong passes every short vector.
        let input = vec![b'x'; 56];
        assert_eq!(md5_hex(&input), "668a72d5ba17f08e62dabcafad6db14b");
        let input = vec![b'x'; 64];
        assert_eq!(md5_hex(&input), "c1bb4f81d892b2d57947682aeb252456");
    }

    #[test]
    fn the_value_digest_puts_a_newline_after_every_value_including_the_last() {
        let values = vec!["1".to_owned(), "2".to_owned()];
        assert_eq!(hash_values(&values), md5_hex(b"1\n2\n"));
    }
}
