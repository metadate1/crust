use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::stream::{
    EID_NONE_RAW, INST_ENTRY_TYPE, KNOWN_LEVELS, MIDI_ENTRY_TYPE, parse_instrument_entry,
    parse_nsd, parse_nsf, parse_retail_midi,
};

const ADIO_ENTRY_TYPE: u32 = 12;
const BLOCK_BYTES: usize = 16;

#[derive(Default)]
struct Census {
    occurrences: usize,
    unique: BTreeSet<Vec<u8>>,
    ended_unique: BTreeSet<Vec<u8>>,
    one_shot_unique: BTreeSet<Vec<u8>>,
    repeating_unique: BTreeSet<Vec<u8>>,
    loop_filter_nonzero_unique: BTreeSet<Vec<u8>>,
    repeat_differs_unique: BTreeSet<Vec<u8>>,
    ended: usize,
    repeating: usize,
    loop_filter_nonzero: usize,
    repeat_differs: usize,
    one_shot: usize,
    reserved_shift: usize,
}

impl Census {
    fn inspect(&mut self, encoded: &[u8]) {
        self.occurrences += 1;
        self.unique.insert(encoded.to_vec());
        let blocks = encoded.chunks_exact(BLOCK_BYTES).collect::<Vec<_>>();
        let Some(end_index) = blocks.iter().position(|block| block[1] & 1 != 0) else {
            return;
        };
        self.ended += 1;
        self.ended_unique.insert(encoded.to_vec());
        self.reserved_shift += blocks[..=end_index]
            .iter()
            .filter(|block| block[0] & 0x0f > 12)
            .count();
        let end_flags = blocks[end_index][1];
        if end_flags & 2 == 0 {
            self.one_shot += 1;
            self.one_shot_unique.insert(encoded.to_vec());
            return;
        }
        let Some(loop_start) = blocks[..=end_index]
            .iter()
            .rposition(|block| block[1] & 4 != 0)
        else {
            return;
        };
        self.repeating += 1;
        self.repeating_unique.insert(encoded.to_vec());
        let loop_filter = blocks[loop_start][0] >> 4;
        if loop_filter != 0 {
            self.loop_filter_nonzero += 1;
            self.loop_filter_nonzero_unique.insert(encoded.to_vec());
        }

        let (first, terminal_history) = decode_blocks(&blocks, 0, end_index, [0, 0]);
        let first_loop_start = loop_start * 28;
        let first_loop = &first[first_loop_start..];
        let (second_loop, _) = decode_blocks(&blocks, loop_start, end_index, terminal_history);
        if first_loop != second_loop {
            self.repeat_differs += 1;
            self.repeat_differs_unique.insert(encoded.to_vec());
        }
    }
}

fn decode_blocks(
    blocks: &[&[u8]],
    start: usize,
    end: usize,
    mut history: [i16; 2],
) -> (Vec<i16>, [i16; 2]) {
    const POSITIVE: [i32; 16] = [0, 60, 115, 98, 122, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const NEGATIVE: [i32; 16] = [0, 0, -52, -55, -60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    let mut decoded = Vec::new();
    for block in &blocks[start..=end] {
        let shift = block[0] & 0x0f;
        let filter = usize::from(block[0] >> 4);
        for packed in &block[2..] {
            for nibble in [packed & 0x0f, packed >> 4] {
                let signed = i32::from(i8::try_from(nibble).unwrap()) - i32::from(nibble & 8) * 2;
                let filtered = (i32::from(history[0]) * POSITIVE[filter]
                    + i32::from(history[1]) * NEGATIVE[filter]
                    + 32)
                    >> 6;
                let sample = ((signed << 12) >> shift) + filtered;
                let sample = i16::try_from(sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                    .expect("the clamped predictor sample fits i16");
                history[1] = history[0];
                history[0] = sample;
                decoded.push(sample);
            }
        }
    }
    (decoded, history)
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn census_local_spu_loop_headers() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let mut adio = Census::default();
    let mut vab = Census::default();
    let mut referenced_vab_waves_without_end = 0_usize;

    for known in KNOWN_LEVELS {
        let nsd_bytes = std::fs::read(root.join(known.nsd_filename())).unwrap();
        let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
        let nsf_bytes = std::fs::read(root.join(known.nsf_filename())).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let entries = nsf
            .entries()
            .map(|entry| (entry.eid, entry))
            .collect::<HashMap<Eid, _>>();

        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == ADIO_ENTRY_TYPE)
        {
            if let Some(item) = entry.item(0) {
                adio.inspect(item.bytes(&nsf_bytes).unwrap());
            }
        }

        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == MIDI_ENTRY_TYPE)
        {
            let midi = parse_retail_midi(entry, &nsf_bytes).unwrap();
            let mut fragments = midi
                .header
                .instruments
                .iter()
                .copied()
                .filter(|eid| eid.raw() != EID_NONE_RAW)
                .map(|eid| {
                    let instrument = entries.get(&eid).unwrap();
                    assert_eq!(instrument.entry_type, INST_ENTRY_TYPE);
                    parse_instrument_entry(instrument, &nsf_bytes).unwrap()
                })
                .collect::<Vec<_>>();
            midi.vab.validate_fragments(&fragments).unwrap();
            fragments.sort_by_key(|fragment| fragment.part_index);
            let body = fragments
                .iter()
                .flat_map(|fragment| fragment.body.iter().copied())
                .collect::<Vec<_>>();
            let mut offset = 0;
            for (wave_index, &size) in midi.vab.wave_sizes.iter().enumerate() {
                let end = offset + size;
                let bytes = &body[offset..end];
                if !bytes
                    .chunks_exact(BLOCK_BYTES)
                    .any(|block| block[1] & 1 != 0)
                {
                    let one_based = u16::try_from(wave_index + 1).unwrap();
                    let references = midi
                        .vab
                        .programs
                        .iter()
                        .flat_map(|program| &program.tones)
                        .filter(|tone| tone.wave_index == one_based)
                        .count();
                    referenced_vab_waves_without_end += references;
                    assert!(wave_index + 1 < midi.vab.wave_sizes.len());
                    assert!(
                        body[end..]
                            .chunks_exact(BLOCK_BYTES)
                            .any(|block| block[1] & 1 != 0)
                    );
                }
                vab.inspect(bytes);
                offset = end;
            }
            assert_eq!(offset, body.len());
        }
    }

    assert_eq!(
        [
            adio.occurrences,
            adio.unique.len(),
            adio.ended,
            adio.ended_unique.len(),
            adio.one_shot,
            adio.one_shot_unique.len(),
            adio.repeating,
            adio.repeating_unique.len(),
            adio.loop_filter_nonzero,
            adio.loop_filter_nonzero_unique.len(),
            adio.repeat_differs,
            adio.repeat_differs_unique.len(),
            adio.reserved_shift,
        ],
        [
            1_582, 194, 1_582, 194, 1_520, 180, 62, 14, 60, 13, 60, 13, 0
        ]
    );
    assert_eq!(
        [
            vab.occurrences,
            vab.unique.len(),
            vab.ended,
            vab.ended_unique.len(),
            vab.one_shot,
            vab.one_shot_unique.len(),
            vab.repeating,
            vab.repeating_unique.len(),
            vab.loop_filter_nonzero,
            vab.loop_filter_nonzero_unique.len(),
            vab.repeat_differs,
            vab.repeat_differs_unique.len(),
            vab.reserved_shift,
            referenced_vab_waves_without_end,
        ],
        [758, 296, 756, 295, 748, 291, 8, 4, 8, 4, 8, 4, 0, 2]
    );
}
