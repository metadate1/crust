//! Bounds and malformed-input checks for LEA-created animation descriptors.

use crust_sim::gool::{
    AnimationSource, ObjectHandle, ProcessAnimationKind, REGISTER_COUNT, StorageReference,
    StorageRegion, VmError, VmObject, process_register,
};
use proptest::prelude::*;

fn object_with_register_tail(words: &[u32]) -> (VmObject, StorageReference) {
    assert!(!words.is_empty() && words.len() < REGISTER_COUNT - 64);
    let handle = ObjectHandle::new(0).unwrap();
    let start = REGISTER_COUNT - words.len();
    let reference = StorageReference::checked(handle, StorageRegion::Register, start).unwrap();
    let mut object = VmObject::new(handle, vec![0]).unwrap();
    for (offset, word) in words.iter().copied().enumerate() {
        object.set_register(start + offset, word).unwrap();
    }
    object
        .set_register(process_register::ANIMATION_SEQUENCE, reference.to_word())
        .unwrap();
    (object, reference)
}

#[test]
fn known_process_descriptors_reject_each_truncated_consumed_layout() {
    for words in [vec![1], vec![2], vec![4], vec![0x0001_0005, 0, u32::MAX]] {
        let (object, reference) = object_with_register_tail(&words);
        assert_eq!(
            object.animation_source(),
            Err(VmError::InvalidAnimationReference(reference.to_word()))
        );
    }
}

#[test]
fn no_draw_and_font_only_consume_the_header_native_reads() {
    let (object, _) = object_with_register_tail(&[0x1122_3373]);
    let AnimationSource::Process(process) = object.animation_source().unwrap().unwrap() else {
        panic!("expected process no-draw source");
    };
    assert_eq!(*process.kind(), ProcessAnimationKind::NoDraw);

    let (object, _) = object_with_register_tail(&[0xbb5f_aa03]);
    let AnimationSource::Process(process) = object.animation_source().unwrap().unwrap() else {
        panic!("expected process font source");
    };
    let ProcessAnimationKind::Font(header) = process.kind() else {
        panic!("expected a font header");
    };
    assert_eq!(header.length, 0x5f);
    assert_eq!(header.reserved_1, 0xaa);
    assert_eq!(header.reserved_3, 0xbb);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn arbitrary_bounded_process_words_never_panic(words in proptest::collection::vec(any::<u32>(), 1..64)) {
        let (object, _) = object_with_register_tail(&words);
        let _ = object.animation_source();
    }
}
