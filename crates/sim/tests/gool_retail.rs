use crust_formats::binary::Eid;
use crust_formats::stream::{
    ENTRY_MAGIC, GOOL_PC_NONE, LevelId, NSF_PAGE_SIZE, load_gool_program, parse_nsd, parse_nsf,
};
use crust_sim::gool::{HaltReason, Instruction, Machine, ObjectHandle, VmObject};

const MODERN_HEADER_SIZE: usize = 0x520;
const LDAT_PREFIX_SIZE: usize = 0x118;

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn entry(eid: Eid, items: &[Vec<u8>]) -> Vec<u8> {
    let table_end = 16 + (items.len() + 1) * 4;
    let total = table_end + items.iter().map(Vec::len).sum::<usize>();
    assert!(total.is_multiple_of(4));
    let mut bytes = vec![0_u8; table_end];
    put_u32(&mut bytes, 0, ENTRY_MAGIC);
    put_u32(&mut bytes, 4, eid.raw());
    put_u32(&mut bytes, 8, 2);
    put_u32(&mut bytes, 12, u32::try_from(items.len()).unwrap());
    let mut cursor = table_end;
    for (index, item) in items.iter().enumerate() {
        put_u32(&mut bytes, 16 + index * 4, u32::try_from(cursor).unwrap());
        bytes.extend_from_slice(item);
        cursor += item.len();
    }
    put_u32(
        &mut bytes,
        16 + items.len() * 4,
        u32::try_from(cursor).unwrap(),
    );
    bytes
}

fn fixture() -> (Vec<u8>, Vec<u8>, Eid) {
    let global_eid = Eid::from_name("glob1").unwrap();
    let external_eid = Eid::from_name("code1").unwrap();
    let ldat_offset = MODERN_HEADER_SIZE + 16;
    let mut nsd_bytes = vec![0_u8; ldat_offset + LDAT_PREFIX_SIZE];
    put_u32(&mut nsd_bytes, 0x400, 1);
    put_u32(&mut nsd_bytes, 0x404, 2);
    put_u32(&mut nsd_bytes, MODERN_HEADER_SIZE, 1);
    put_u32(&mut nsd_bytes, MODERN_HEADER_SIZE + 4, global_eid.raw());
    put_u32(&mut nsd_bytes, MODERN_HEADER_SIZE + 8, 1);
    put_u32(&mut nsd_bytes, MODERN_HEADER_SIZE + 12, external_eid.raw());
    put_u32(&mut nsd_bytes, ldat_offset, 1);
    put_u32(&mut nsd_bytes, ldat_offset + 4, LevelId::TITLE.get());

    let mut header = Vec::new();
    for word in [1, 0x100, 0, 32, 1, 0] {
        push_u32(&mut header, word);
    }
    let mut shared_code = Vec::new();
    push_u32(&mut shared_code, 0x8200_0000);
    let mut internal = Vec::new();
    push_u32(&mut internal, external_eid.raw());
    push_u32(&mut internal, 2);
    let mut maps = Vec::new();
    for value in [0xffff, 0x00ff, 0x00ff, 0] {
        push_u16(&mut maps, value);
    }
    let mut states = Vec::new();
    push_u32(&mut states, 0x1122_3344);
    push_u32(&mut states, 0x5566_7788);
    push_u16(&mut states, 0);
    push_u16(&mut states, GOOL_PC_NONE);
    push_u16(&mut states, 4);
    push_u16(&mut states, 0);
    let global = entry(global_eid, &[header, shared_code, internal, maps, states]);

    let add = Instruction::encode(0x00, 1, 0x400);
    let branch = (0x82_u32 << 24) | (1 << 10) | 1;
    let mut code = Vec::new();
    for word in [add, add, branch, 0xff00_0000, add] {
        push_u32(&mut code, word);
    }
    let mut external_data = Vec::new();
    push_u32(&mut external_data, 3);
    let external = entry(external_eid, &[Vec::new(), code, external_data]);

    let table_end = 16 + 3 * 4;
    let global_start = table_end;
    let external_start = global_start + global.len();
    let end = external_start + external.len();
    let mut nsf_bytes = vec![0_u8; NSF_PAGE_SIZE];
    put_u16(&mut nsf_bytes, 0, 0x1234);
    put_u16(&mut nsf_bytes, 2, 0);
    put_u32(&mut nsf_bytes, 4, 1);
    put_u32(&mut nsf_bytes, 8, 2);
    put_u32(&mut nsf_bytes, 16, u32::try_from(global_start).unwrap());
    put_u32(&mut nsf_bytes, 20, u32::try_from(external_start).unwrap());
    put_u32(&mut nsf_bytes, 24, u32::try_from(end).unwrap());
    nsf_bytes[global_start..external_start].copy_from_slice(&global);
    nsf_bytes[external_start..end].copy_from_slice(&external);
    (nsd_bytes, nsf_bytes, global_eid)
}

#[test]
fn parsed_retail_entry_executes_state_code_and_packed_branch() {
    let (nsd_bytes, nsf_bytes, global_eid) = fixture();
    let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let program = load_gool_program(&metadata, &nsf, &nsf_bytes, global_eid, 2).unwrap();
    let handle = ObjectHandle::new(0).unwrap();
    let object = VmObject::from_gool_program(handle, &program).unwrap();

    assert_eq!(object.initial_stack_pointer(), 32);
    assert_eq!(object.state_flags(), 0x1122_3344);
    assert_eq!(object.status_c(), 0x5566_7788);
    assert_eq!(object.transition_pc(), Some(4));
    assert_eq!(object.global_code(), &[0x8200_0000]);
    // GoolObjectChangeState places the initial frame directly in the shared
    // process/register union at header.init_sp. The fourth word is the
    // synthetic zero wait consumed before code interpretation begins.
    assert_eq!(object.register(32), Ok(0xffff));
    assert_eq!(object.register(33), Ok(0xa600_0000));
    assert_eq!(object.register(34), Ok(32 * 4));
    assert_eq!(object.register(35), Ok(0));

    let mut machine = Machine::new(0);
    machine.insert_object(object).unwrap();
    let execution = machine.run(handle, 4).unwrap();
    assert_eq!(execution.reason, HaltReason::BudgetExhausted);
    assert_eq!(machine.object(handle).unwrap().pc(), 5);
    assert_eq!(
        machine.object(handle).unwrap().stack(),
        &[0xffff, 0xa600_0000, 32 * 4, 5, 5]
    );
    assert_eq!(machine.object(handle).unwrap().register(35), Ok(5));
    assert_eq!(machine.object(handle).unwrap().register(36), Ok(5));
}
