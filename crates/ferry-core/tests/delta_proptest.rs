//! Property test: applying an encoded delta always reconstructs `new`.

use ferry_core::delta::{apply, encode};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn apply_encode_roundtrips(
        old in proptest::collection::vec(any::<u8>(), 0..4096),
        new in proptest::collection::vec(any::<u8>(), 0..4096),
        block in prop_oneof![Just(None), (1usize..2048).prop_map(Some)],
    ) {
        let delta = encode(&old, &new, block);
        let rebuilt = apply(&old, &delta).expect("apply must succeed for self-produced delta");
        prop_assert_eq!(rebuilt, new);
    }

    /// `new` derived from `old` with realistic mutations (the rsync sweet spot).
    #[test]
    fn mutated_roundtrips(
        old in proptest::collection::vec(any::<u8>(), 0..8192),
        cut in 0usize..8192,
        insert in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let cut = cut.min(old.len());
        let mut new = old[..cut].to_vec();
        new.extend_from_slice(&insert);
        if cut < old.len() {
            new.extend_from_slice(&old[cut..]);
        }
        let delta = encode(&old, &new, None);
        let rebuilt = apply(&old, &delta).expect("apply");
        prop_assert_eq!(rebuilt, new);
    }
}
