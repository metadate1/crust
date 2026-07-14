use crust_formats::binary::{Eid, EntryRef};
use crust_formats::stream::{GOOL_PC_NONE, ZoneEntity, ZoneEntityPathPoint, structs::GoolState};
use crust_sim::gool::{
    CollisionObjectReference, HaltReason, Instruction, ObjectHandle as VmObjectHandle,
    RetailPadSnapshot, RetailTransform, VmError, VmObject, VmStateProgram, process_register,
};
use crust_sim::object_arena::{
    NeighborZone, OBJECT_ARENA_CAPACITY, OBJECT_POOL_CAPACITY, ObjectOrigin, TreeParent,
};
use crust_sim::retail_runtime::{
    ProgramBinding, ProgramHost, ProgramOrigin, RetailRuntime, RetailZoneEnvironment, RuntimeError,
    StateProgramBinding,
};

const ZONE_A: Eid = Eid::from_raw(0x1111_1111);
const REG_A: u16 = 0x0e46;
const REG_B: u16 = 0x0e47;
const PARENT_REG0: u16 = 0x0c40;
const CHANGE_TO_STATE_ONE: u32 = 0x8240_0001;
const RETURN: u32 = 0x8289_4000;

fn terminal_state_program(state: u16) -> VmStateProgram {
    VmStateProgram::new(
        state,
        GoolState {
            flags: 0,
            status_c: 0,
            external_index: 0,
            event_pc: GOOL_PC_NONE,
            transition_pc: GOOL_PC_NONE,
            code_pc: 0,
        },
        vec![RETURN],
        Vec::new(),
    )
    .unwrap()
}

fn entity(id: u16, group: u16, executable: u8, subtype: u8) -> ZoneEntity {
    ZoneEntity {
        serialized_parent: EntryRef::from_raw(0),
        spawn_flags: 0,
        group,
        id,
        initializer: [0, 0, 0],
        executable,
        subtype,
        path_points: vec![ZoneEntityPathPoint { x: 0, y: 0, z: 0 }],
    }
}

fn zone_environment() -> RetailZoneEnvironment {
    RetailZoneEnvironment {
        origin: [1_000, -2_000, 3_000],
        object_colors: std::array::from_fn(|index| {
            0x1000 + u16::try_from(index).expect("color index fits u16")
        }),
        player_colors: std::array::from_fn(|index| {
            0x2000 + u16::try_from(index).expect("color index fits u16")
        }),
        graphics_flags: 0,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BindingRecord {
    Entity {
        id: u16,
        executable: u8,
        vm: u16,
    },
    Child {
        executable: u8,
        arguments: Vec<u32>,
        vm: u16,
    },
    State {
        executable: u8,
        state: u16,
        vm: u16,
    },
}

#[derive(Default)]
struct RecordingHost {
    bindings: Vec<BindingRecord>,
}

impl ProgramHost for RecordingHost {
    type Error = &'static str;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        match binding.origin {
            ProgramOrigin::Entity(entity) => self.bindings.push(BindingRecord::Entity {
                id: entity.id,
                executable: binding.executable,
                vm: binding.object.vm().get(),
            }),
            ProgramOrigin::RuntimeChild { arguments } => {
                self.bindings.push(BindingRecord::Child {
                    executable: binding.executable,
                    arguments: arguments.to_vec(),
                    vm: binding.object.vm().get(),
                });
            }
        }

        let code = match binding.executable {
            // Push 0x1234, synchronously spawn executable five, then return.
            1 => vec![Instruction::encode(0x00, REG_A, REG_B), 0x8a10_5001, RETURN],
            // The reclaiming spawn has the same nonfatal null result when no
            // expendable object exists in a full pool.
            12 => vec![Instruction::encode(0x00, REG_A, REG_B), 0x9110_5001, RETURN],
            // Read the creator/parent's register zero, proving the typed link.
            5 => vec![Instruction::encode(0x11, PARENT_REG0, REG_A), RETURN],
            // Yield a retail state change for host-backed state rebinding.
            7 => vec![CHANGE_TO_STATE_ONE],
            // Fail after fetch so a retry would incorrectly skip to RETURN.
            8 => vec![Instruction::encode(0xff, REG_A, REG_B), RETURN],
            // Test the port-zero retail CROSS-tapped control query.
            10 => vec![0x1a00_1040],
            _ => vec![RETURN],
        };
        let mut object = VmObject::new(binding.object.vm(), code).map_err(|_| "VM object")?;
        if matches!(binding.executable, 1 | 12) {
            object.set_register(70, 0x1200).map_err(|_| "register")?;
            object.set_register(71, 0x34).map_err(|_| "register")?;
            object
                .set_register(process_register::MISC_VALUE, 0xdead_beef)
                .map_err(|_| "misc child")?;
        }
        if binding.executable == 6 {
            object
                .set_register(process_register::STATUS_C, 0x1234_5678)
                .map_err(|_| "status c")?;
            object
                .set_register(process_register::STATE_FLAGS, 0x8765_4321)
                .map_err(|_| "state flags")?;
        }
        Ok(object)
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        self.bindings.push(BindingRecord::State {
            executable: binding.executable,
            state: binding.state,
            vm: binding.object.vm().get(),
        });
        Ok(terminal_state_program(binding.state))
    }

    fn zone_environment(
        &mut self,
        _zone: Eid,
    ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
        Ok(Some(zone_environment()))
    }
}

#[test]
fn browser_pad_snapshot_reaches_retail_gool_before_the_frame_runs() {
    let entities = [entity(34, 3, 10, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let snapshot = RetailPadSnapshot {
        tapped: 0x40,
        held: 0x40,
        held_previous: 0,
        tapped_previous: 0,
        held_previous_2: 0,
    };
    let mut runtime = RetailRuntime::new(0);
    runtime.set_pad_snapshot(0, snapshot).unwrap();

    let report = runtime
        .spawn_and_run_frame(&neighbors, &mut RecordingHost::default(), 1)
        .unwrap();
    let object = report.spawn_attempts[0].result.as_ref().unwrap();

    assert_eq!(report.frame.executions[0].result.as_ref().unwrap().steps, 1);
    assert_eq!(
        runtime
            .machine()
            .object(object.vm())
            .unwrap()
            .stack()
            .last(),
        Some(&0x40),
        "GOOL 0x1a must observe the same CROSS tap as browser UI code"
    );
}

#[test]
fn entity_process_uses_zone_origin_path_rotation_and_object_colors() {
    let mut descriptor = entity(42, 3, 6, 9);
    descriptor.initializer = [0x111, -0x222, 0x333];
    descriptor.path_points = vec![
        ZoneEntityPathPoint {
            x: -7,
            y: 11,
            z: 13,
        },
        ZoneEntityPathPoint { x: 1, y: 2, z: 3 },
    ];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: std::slice::from_ref(&descriptor),
    }];
    let mut runtime = RetailRuntime::new(0);
    let object = runtime
        .spawn_current_zone_neighbors(&neighbors, &mut RecordingHost::default())
        .pop()
        .unwrap()
        .result
        .unwrap();
    let object = runtime.machine().object(object.vm()).unwrap();

    assert_eq!(
        object.retail_transform().unwrap(),
        RetailTransform {
            translation: [248_832, -500_736, 781_312],
            rotation_yxz: [0x111, -0x222, 0x333],
            scale: [0x1000; 3],
        }
    );
    assert_eq!(object.register(process_register::MODE_FLAGS_A), Ok(0x11100));
    assert_eq!(
        object.register(process_register::MODE_FLAGS_B),
        Ok((-0x22200_i32).cast_unsigned())
    );
    assert_eq!(object.register(process_register::MODE_FLAGS_C), Ok(0x33300));
    assert_eq!(object.register(process_register::SUBTYPE), Ok(9));
    assert_eq!(object.register(process_register::PID_FLAGS), Ok(42 << 8));
    assert_eq!(object.register(process_register::PATH_PROGRESS), Ok(0));
    assert_eq!(object.register(process_register::PATH_LENGTH), Ok(2 << 8));
    assert_eq!(object.register(process_register::STATUS_A), Ok(0x0002_0020));
    assert_eq!(object.register(process_register::STATUS_C), Ok(0x1234_5678));
    assert_eq!(
        object.register(process_register::STATE_FLAGS),
        Ok(0x8765_4321)
    );
    assert_eq!(object.register(process_register::STATE_STAMP), Ok(0));
    assert_eq!(
        object.register(process_register::VOICE_ID),
        Ok(u32::MAX - 1)
    );
    assert_eq!(object.register(process_register::NODE), Ok(0xffff));
    assert_eq!(object.retail_colors(), &zone_environment().object_colors);
}

#[test]
fn dedicated_main_entity_uses_player_colors() {
    let entities = [entity(2, 3, 2, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut runtime = RetailRuntime::new(0);
    let object = runtime
        .spawn_current_zone_neighbors(&neighbors, &mut RecordingHost::default())
        .pop()
        .unwrap()
        .result
        .unwrap();

    assert!(object.arena().is_dedicated_main());
    assert_eq!(
        runtime
            .machine()
            .object(object.vm())
            .unwrap()
            .retail_colors(),
        &zone_environment().player_colors
    );
}

#[test]
fn current_zone_entities_and_hosted_children_share_one_runtime_frame() {
    let hidden = [entity(10, 3, 2, 0)];
    let visible = [
        entity(11, 3, 2, 0),
        entity(12, 2, 2, 0),
        entity(13, 3, 1, 0),
    ];
    let neighbors = [
        NeighborZone {
            eid: ZONE_A,
            display_flags: 0,
            entities: &hidden,
        },
        NeighborZone {
            eid: ZONE_A,
            display_flags: 2,
            entities: &visible,
        },
    ];
    let mut host = RecordingHost::default();
    let mut runtime = RetailRuntime::new(0);
    let report = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 3)
        .unwrap();

    assert_eq!(report.spawn_attempts.len(), 2);
    let other = report.spawn_attempts[0].result.as_ref().unwrap();
    let parent = report.spawn_attempts[1].result.as_ref().unwrap();
    assert_eq!(runtime.object_for_arena(other.arena()), Some(*other));
    assert_eq!(runtime.object_for_vm(parent.vm()), Some(*parent));

    let child = report.frame.spawned_children[0];
    // Head insertion reverses scan order. A child created while updating its
    // parent is traversed before the captured top-level sibling.
    assert_eq!(
        report
            .frame
            .executions
            .iter()
            .map(|execution| execution.object)
            .collect::<Vec<_>>(),
        [*parent, child, *other]
    );
    assert_eq!(
        report.frame.executions[0].result.as_ref().unwrap().reason,
        HaltReason::Halted
    );
    assert_eq!(report.frame.frame_index, 0);
    assert_eq!(report.frame.spawned_children.len(), 1);
    assert_eq!(report.frame.effects.len(), 1);
    assert_eq!(
        runtime
            .machine()
            .object(parent.vm())
            .unwrap()
            .register(process_register::MISC_VALUE),
        Ok(CollisionObjectReference::new(child.vm()).to_word()),
        "a successful native spawn leaves misc_child pointing at the child"
    );

    assert_eq!(
        runtime.arena().get(child.arena()).unwrap().parent(),
        TreeParent::Object(parent.arena())
    );
    assert_eq!(
        host.bindings,
        [
            BindingRecord::Entity {
                id: 11,
                executable: 2,
                vm: 0,
            },
            BindingRecord::Entity {
                id: 13,
                executable: 1,
                vm: 1,
            },
            BindingRecord::Child {
                executable: 5,
                arguments: vec![0x1234],
                vm: 2,
            },
        ]
    );

    // The child interprets during frame zero. Its parent link resolves through
    // the VM side of the typed mapping.
    assert_eq!(
        runtime
            .machine()
            .object(child.vm())
            .unwrap()
            .register(70)
            .unwrap(),
        CollisionObjectReference::new(parent.vm()).to_word()
    );
    let parent_vm = runtime.machine().object(parent.vm()).unwrap();
    let child_vm = runtime.machine().object(child.vm()).unwrap();
    assert_eq!(
        child_vm.retail_transform(),
        parent_vm.retail_transform(),
        "GoolObjectInit inherits a runtime child's parent transform"
    );
    assert_eq!(child_vm.retail_colors(), &zone_environment().object_colors);
    assert_eq!(child_vm.register(process_register::SUBTYPE), Ok(0));
    assert_eq!(child_vm.register(process_register::PID_FLAGS), Ok(0));
    assert_eq!(child_vm.register(process_register::PATH_PROGRESS), Ok(0));
    assert_eq!(child_vm.register(process_register::PATH_LENGTH), Ok(0));
    assert_eq!(
        child_vm.register(process_register::STATUS_A),
        Ok(0x0002_0000),
        "the same-frame native update clears GOOL_FLAG_FIRST_FRAME"
    );
    assert_eq!(
        child_vm.register(process_register::VOICE_ID),
        Ok(u32::MAX - 1)
    );
    assert_eq!(child_vm.register(process_register::NODE), Ok(0xffff));
}

#[test]
fn full_native_pool_returns_null_spawns_without_faulting_the_parents() {
    for parent_executable in [1, 12] {
        let mut entities = Vec::with_capacity(OBJECT_ARENA_CAPACITY);
        entities.push(entity(200, 3, 0, 0));
        for offset in 0..OBJECT_POOL_CAPACITY {
            let executable = if offset + 1 == OBJECT_POOL_CAPACITY {
                parent_executable
            } else {
                2
            };
            entities.push(entity(
                10 + u16::try_from(offset).unwrap(),
                3,
                executable,
                0,
            ));
        }
        let neighbors = [NeighborZone {
            eid: ZONE_A,
            display_flags: 2,
            entities: &entities,
        }];
        let mut host = RecordingHost::default();
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        assert_eq!(attempts.len(), OBJECT_ARENA_CAPACITY);
        assert!(attempts.iter().all(|attempt| attempt.result.is_ok()));
        let parent = *attempts
            .last()
            .unwrap()
            .result
            .as_ref()
            .expect("the last pool object is the spawning parent");
        assert_eq!(runtime.arena().len(), OBJECT_ARENA_CAPACITY);
        assert_eq!(runtime.arena().remaining_pool_capacity(), 0);

        let frame = runtime.run_frame(&mut host, 3).unwrap();
        let parent_execution = frame
            .executions
            .iter()
            .find(|execution| execution.object == parent)
            .unwrap();

        assert_eq!(
            parent_execution.result.as_ref().unwrap().reason,
            HaltReason::Halted
        );
        assert!(!runtime.is_object_faulted(parent));
        assert!(frame.spawned_children.is_empty());
        assert_eq!(runtime.arena().len(), OBJECT_ARENA_CAPACITY);
        assert_eq!(
            runtime
                .machine()
                .object(parent.vm())
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0),
            "pool exhaustion is native's null misc_child result"
        );
    }
}

#[test]
fn retail_state_changes_rebind_before_the_next_cooperative_frame() {
    let entities = [entity(30, 3, 7, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut host = RecordingHost::default();
    let mut runtime = RetailRuntime::new(0);
    let first = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 1)
        .unwrap();
    let object = first.spawn_attempts[0].result.as_ref().unwrap();

    assert_eq!(
        first.frame.executions[0].result.as_ref().unwrap().reason,
        HaltReason::StateChanged(1)
    );
    assert_eq!(runtime.machine().object(object.vm()).unwrap().state(), 1);
    assert_eq!(
        host.bindings.last(),
        Some(&BindingRecord::State {
            executable: 7,
            state: 1,
            vm: object.vm().get(),
        })
    );

    let second = runtime.run_frame(&mut host, 1).unwrap();
    assert_eq!(second.frame_index, 1);
    assert_eq!(
        second.executions[0].result.as_ref().unwrap().reason,
        HaltReason::Halted
    );
}

struct TransitionStateHost;

impl ProgramHost for TransitionStateHost {
    type Error = &'static str;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        let code = match binding.origin {
            ProgramOrigin::Entity(_) => vec![CHANGE_TO_STATE_ONE],
            ProgramOrigin::RuntimeChild { .. } => vec![RETURN],
        };
        VmObject::new(binding.object.vm(), code).map_err(|_| "VM object")
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        VmStateProgram::new(
            binding.state,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: 3,
            },
            vec![
                Instruction::encode(0x11, 0x0801, 0x0e1f),
                0x8a10_5001,
                RETURN,
                RETURN,
            ],
            Vec::new(),
        )
        .map_err(|_| "state program")
    }
}

#[test]
fn production_rebind_runs_transition_block_and_its_host_effect_synchronously() {
    let entities = [entity(35, 3, 11, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut runtime = RetailRuntime::new(0);
    let report = runtime
        .spawn_and_run_frame(&neighbors, &mut TransitionStateHost, 1)
        .unwrap();
    let object = *report.spawn_attempts[0].result.as_ref().unwrap();

    assert_eq!(
        report.frame.executions[0].result.as_ref().unwrap().reason,
        HaltReason::StateChanged(1)
    );
    assert_eq!(runtime.machine().object(object.vm()).unwrap().state(), 1);
    assert!(!runtime.is_object_faulted(object));
    assert_eq!(report.frame.spawned_children.len(), 1);
    let child = report.frame.spawned_children[0];
    assert_eq!(
        runtime.arena().get(child.arena()).unwrap().origin(),
        ObjectOrigin::Runtime {
            executable: 5,
            subtype: 0,
        }
    );
    assert!(report.frame.effects.iter().any(|effect| matches!(
        effect,
        crust_sim::gool::VmEffect::SpawnChildren {
            parent,
            executable: 5,
            arguments,
            ..
        } if *parent == object.vm() && arguments == &[0x100]
    )));
}

#[derive(Default)]
struct TransitionChainHost {
    rebound_states: Vec<u16>,
}

impl ProgramHost for TransitionChainHost {
    type Error = &'static str;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        VmObject::new(binding.object.vm(), vec![CHANGE_TO_STATE_ONE]).map_err(|_| "VM object")
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        self.rebound_states.push(binding.state);
        let transition = if binding.state == 1 {
            0x8240_0002
        } else {
            RETURN
        };
        VmStateProgram::new(
            binding.state,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: 1,
            },
            vec![transition, RETURN],
            Vec::new(),
        )
        .map_err(|_| "state program")
    }
}

#[test]
fn transition_state_link_rebinds_the_next_state_before_the_frame_returns() {
    let entities = [entity(36, 3, 11, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut host = TransitionChainHost::default();
    let mut runtime = RetailRuntime::new(0);
    let report = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 1)
        .unwrap();
    let object = *report.spawn_attempts[0].result.as_ref().unwrap();

    assert_eq!(host.rebound_states, [1, 2]);
    assert_eq!(runtime.machine().object(object.vm()).unwrap().state(), 2);
    assert!(!runtime.is_object_faulted(object));
    assert_eq!(
        report.frame.executions[0].result.as_ref().unwrap().reason,
        HaltReason::StateChanged(1)
    );
    assert!(report.frame.effects.iter().any(|effect| matches!(
        effect,
        crust_sim::gool::VmEffect::StateChanged { object: vm, state: 2 }
            if *vm == object.vm()
    )));
}

#[test]
fn vm_failure_quarantines_only_that_object_without_skipping_the_opcode_next_frame() {
    let entities = [entity(31, 3, 2, 0), entity(32, 3, 8, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut host = RecordingHost::default();
    let mut runtime = RetailRuntime::new(0);
    let first = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 3)
        .unwrap();
    let faulted = *first.spawn_attempts[1].result.as_ref().unwrap();
    let healthy = *first.spawn_attempts[0].result.as_ref().unwrap();

    assert!(first.frame.executions.iter().any(|execution| {
        execution.object == faulted
            && matches!(
                execution.result,
                Err(RuntimeError::Vm(VmError::UnknownOpcode(0xff)))
            )
    }));
    assert_eq!(runtime.machine().object(faulted.vm()).unwrap().pc(), 1);
    assert!(runtime.is_object_faulted(faulted));
    assert_eq!(runtime.faulted_object_count(), 1);
    assert_eq!(runtime.faulted_objects().collect::<Vec<_>>(), [faulted]);

    let second = runtime.run_frame(&mut host, 3).unwrap();
    assert_eq!(
        second
            .executions
            .iter()
            .map(|execution| execution.object)
            .collect::<Vec<_>>(),
        [healthy],
        "healthy siblings remain scheduled while the failed object is omitted"
    );
    assert_eq!(runtime.machine().object(faulted.vm()).unwrap().pc(), 1);

    let third = runtime.run_frame(&mut host, 3).unwrap();
    assert!(
        third
            .executions
            .iter()
            .all(|execution| execution.object != faulted)
    );
    assert_eq!(runtime.machine().object(faulted.vm()).unwrap().pc(), 1);
}

#[derive(Default)]
struct RejectingStateHost {
    state_binding_attempts: usize,
}

impl ProgramHost for RejectingStateHost {
    type Error = &'static str;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        VmObject::new(binding.object.vm(), vec![CHANGE_TO_STATE_ONE]).map_err(|_| "VM object")
    }

    fn bind_state_program(
        &mut self,
        _binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        self.state_binding_attempts += 1;
        Err("state program unavailable")
    }
}

#[test]
fn program_failure_during_state_rebind_is_not_retried_after_quarantine() {
    let entities = [entity(33, 3, 9, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut host = RejectingStateHost::default();
    let mut runtime = RetailRuntime::new(0);
    let first = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 1)
        .unwrap();
    let faulted = *first.spawn_attempts[0].result.as_ref().unwrap();

    assert_eq!(
        first.frame.executions[0].result,
        Err(RuntimeError::Program("state program unavailable"))
    );
    assert_eq!(host.state_binding_attempts, 1);
    assert!(runtime.is_object_faulted(faulted));

    let second = runtime.run_frame(&mut host, 1).unwrap();
    assert!(second.executions.is_empty());
    assert_eq!(
        host.state_binding_attempts, 1,
        "a quarantined state transition must not call the host again"
    );
}

struct WrongHandleHost;

impl ProgramHost for WrongHandleHost {
    type Error = &'static str;

    fn bind_program(&mut self, _binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        Ok(VmObject::new(VmObjectHandle::new(1).unwrap(), vec![RETURN]).unwrap())
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        Ok(terminal_state_program(binding.state))
    }
}

#[test]
fn mismatched_host_handle_rolls_back_object_and_persistent_spawn_bit() {
    let entities = [entity(25, 3, 2, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut runtime = RetailRuntime::new(0);
    let attempt = runtime
        .spawn_current_zone_neighbors(&neighbors, &mut WrongHandleHost)
        .pop()
        .unwrap();

    assert!(matches!(
        attempt.result,
        Err(RuntimeError::HostObjectHandleMismatch {
            expected,
            actual,
        }) if expected.get() == 0 && actual.get() == 1
    ));
    assert!(runtime.arena().is_empty());
    assert_eq!(runtime.arena().spawn_table().flags(25), Some(0));
    assert!(
        runtime
            .object_for_vm(VmObjectHandle::new(0).unwrap())
            .is_none()
    );
}

struct RejectingProgramHost;

impl ProgramHost for RejectingProgramHost {
    type Error = &'static str;

    fn bind_program(&mut self, _binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        Err("invalid retail subtype")
    }

    fn bind_state_program(
        &mut self,
        _binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        Err("invalid retail state")
    }
}

#[test]
fn rejected_retail_program_keeps_spawned_bit_without_an_orphan_object() {
    let entities = [entity(26, 3, 2, 0)];
    let neighbors = [NeighborZone {
        eid: ZONE_A,
        display_flags: 2,
        entities: &entities,
    }];
    let mut runtime = RetailRuntime::new(0);
    let attempt = runtime
        .spawn_current_zone_neighbors(&neighbors, &mut RejectingProgramHost)
        .pop()
        .unwrap();

    assert_eq!(
        attempt.result,
        Err(RuntimeError::Program("invalid retail subtype"))
    );
    assert!(runtime.arena().is_empty());
    assert_eq!(runtime.arena().spawn_table().flags(26), Some(1));
}
