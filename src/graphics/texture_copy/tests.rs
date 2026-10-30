use super::*;
use crate::graphics::{
    completion::{self, Completion, Flight},
    fault::Fault,
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

fn tiled_surface(size: [u32; 2]) -> Surface {
    Surface {
        size,
        stride: size[0].next_multiple_of(32) as usize * 4,
        storage: TargetStorage::Tiled,
    }
}

#[test]
fn tiled_strips_rebase_zero_origins_and_copy_only_requested_logical_pixels() {
    for (source, source_origin, destination, destination_origin, size) in [
        ([96, 96], [0, 0], [96, 96], [0, 0], [96, 96]),
        ([137, 93], [32, 32], [64, 40], [0, 0], [64, 40]),
        ([2048, 96], [32, 0], [1536, 96], [0, 0], [1536, 96]),
        ([1024, 1088], [0, 0], [1024, 1088], [0, 0], [1024, 1088]),
        ([512, 128], [0, 0], [137, 93], [0, 0], [137, 93]),
        ([128, 128], [0, 0], [128, 128], [32, 32], [32, 32]),
    ] {
        let source = tiled_surface(source);
        let destination = tiled_surface(destination);
        let plan = Plan::new(source, source_origin, destination, destination_origin, size).unwrap();
        let src: Vec<u32> = (0..source.stride / 4 * source.size[1].next_multiple_of(32) as usize)
            .map(|i| i as u32)
            .collect();
        let mut dst = vec![
            u32::MAX;
            destination.stride / 4
                * destination.size[1].next_multiple_of(32) as usize
        ];
        let mut calls = 0;
        let mut finals = 0;
        plan.submit(
            Vec::<()>::new(),
            |_| {},
            |tile| {
                calls += 1;
                finals += usize::from(tile.last);
                assert!(tile.size.iter().all(|n| *n <= 1024 && n.is_multiple_of(32)));
                let [s, d] = plan.addresses(tile);
                assert_eq!(s.origin, [0, 0]);
                assert_eq!(d.origin, [0, 0]);
                assert_eq!(s.stride, tile.size[0] as i32 * 4);
                assert_eq!(d.stride, s.stride);
                // Model the SDK's local tiled view independently from its rebased
                // physical surface: each command's tile-row width is its own width.
                for y in 0..tile.size[1] as usize {
                    for x in 0..tile.size[0] as usize {
                        let local = ((y / 32) * (tile.size[0] as usize / 32) + x / 32) * 1024
                            + (y % 32) * 32
                            + x % 32;
                        dst[d.offset / 4 + local] = src[s.offset / 4 + local];
                    }
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(finals, 1);
        if size == [96, 96] {
            assert_eq!(calls, 1);
        }
        if size == [1024, 1088] {
            assert_eq!(calls, 2);
        }
        for y in 0..destination.size[1] as usize {
            for x in 0..destination.size[0] as usize {
                let actual = dst[TargetStorage::Tiled.byte_offset(x, y, destination.stride, 4) / 4];
                let inside = x >= destination_origin[0] as usize
                    && y >= destination_origin[1] as usize
                    && x < (destination_origin[0] + size[0]) as usize
                    && y < (destination_origin[1] + size[1]) as usize;
                let expected = if inside {
                    src[TargetStorage::Tiled.byte_offset(
                        source_origin[0] as usize + x - destination_origin[0] as usize,
                        source_origin[1] as usize + y - destination_origin[1] as usize,
                        source.stride,
                        4,
                    ) / 4]
                } else {
                    u32::MAX
                };
                assert_eq!(
                    actual, expected,
                    "destination={:?} pixel=({x},{y})",
                    destination.size
                );
            }
        }
    }
}

#[test]
fn tiled_admission_rejects_unaligned_origins_mixed_layouts_and_interior_padding() {
    let s = tiled_surface([128, 128]);
    assert!(Plan::new(s, [1, 0], s, [0, 0], [32, 32]).is_err());
    assert!(Plan::new(s, [0, 0], s, [0, 1], [32, 32]).is_err());
    assert!(Plan::new(s, [0, 0], s, [0, 0], [33, 32]).is_err());
    assert!(
        Plan::new(
            s,
            [0, 0],
            Surface {
                storage: TargetStorage::Linear,
                ..s
            },
            [0, 0],
            [32, 32]
        )
        .is_err()
    );
}

fn plan() -> Plan {
    Plan::new(
        Surface {
            size: [2072, 1050],
            stride: 2072 * 4,
            storage: TargetStorage::Linear,
        },
        [17, 11],
        Surface {
            size: [2064, 1040],
            stride: 2064 * 4,
            storage: TargetStorage::Linear,
        },
        [3, 9],
        [2049, 1025],
    )
    .unwrap()
}

#[test]
fn subrectangle_crosses_both_1024_boundaries_without_changing_padding() {
    let plan = plan();
    let source_stride = plan.source_stride as usize / 4;
    let destination_stride = plan.destination_stride as usize / 4;
    let source: Vec<u32> = (0..1050)
        .flat_map(|y| (0..source_stride).map(move |x| (y << 16) as u32 | x as u32))
        .collect();
    let mut destination = std::vec![u32::MAX; destination_stride * 1040];
    let mut calls = 0;
    let mut notifications = Vec::new();
    plan.submit(
        Vec::<()>::new(),
        |_| {},
        |tile| {
            calls += 1;
            assert!(tile.size.into_iter().all(|size| (1..=1024).contains(&size)));
            for y in 0..tile.size[1] as usize {
                let from = (tile.source[1] as usize + y) * source_stride + tile.source[0] as usize;
                let to = (tile.destination[1] as usize + y) * destination_stride
                    + tile.destination[0] as usize;
                destination[to..to + tile.size[0] as usize]
                    .copy_from_slice(&source[from..from + tile.size[0] as usize]);
            }
            if tile.last {
                notifications.push(calls);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(calls, 6);
    assert_eq!(notifications, [6]);
    for y in 0..1040 {
        for x in 0..destination_stride {
            let expected = if (3..2052).contains(&x) && (9..1034).contains(&y) {
                source[(y - 9 + 11) * source_stride + x - 3 + 17]
            } else {
                u32::MAX
            };
            assert_eq!(
                destination[y * destination_stride + x],
                expected,
                "at {x},{y}"
            );
        }
    }
}

fn retain<T>(owners: Vec<T>, notification: &Cell<u32>, flights: &RefCell<VecDeque<Flight<T>>>) {
    flights.borrow_mut().push_back(Flight {
        slot: 7,
        token: Rc::new(Completion {
            address: notification.as_ptr(),
            value: 43,
            done: Cell::new(false),
            leases: RefCell::new(owners),
            staging: 0,
            #[cfg(feature = "profiling")]
            profile: Default::default(),
        }),
    });
}

#[test]
fn last_notification_retires_both_allocations_after_all_tiles() {
    let source = Rc::new(1);
    let destination = Rc::new(2);
    let weak_source = Rc::downgrade(&source);
    let weak_destination = Rc::downgrade(&destination);
    let notification = Cell::new(0);
    let flights = RefCell::new(VecDeque::new());
    let mut calls = 0;
    plan()
        .submit(
            std::vec![source, destination],
            |owners| retain(owners, &notification, &flights),
            |tile| {
                calls += 1;
                assert!(weak_source.upgrade().is_some());
                assert!(weak_destination.upgrade().is_some());
                let token = flights.borrow()[0].token.clone();
                assert_eq!(token.leases.borrow().len(), 2);
                assert!(
                    !token.poll(),
                    "earlier tiles have no completion notification"
                );
                if tile.last {
                    notification.set(43);
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(calls, 6);
    assert!(weak_source.upgrade().is_some());
    assert!(weak_destination.upgrade().is_some());
    let mut slots = Vec::new();
    completion::collect(&mut flights.borrow_mut(), &mut slots, || Ok(())).unwrap();
    assert_eq!(slots, [7]);
    assert!(weak_source.upgrade().is_none());
    assert!(weak_destination.upgrade().is_none());
}

#[test]
fn partial_submission_fault_retains_both_allocations_and_original_error() {
    let source = Rc::new(1);
    let destination = Rc::new(2);
    let weak_source = Rc::downgrade(&source);
    let weak_destination = Rc::downgrade(&destination);
    let notification = Cell::new(0);
    let flights = RefCell::new(VecDeque::new());
    let fault = Fault::default();
    let mut calls = 0;
    let error = plan()
        .submit(
            std::vec![source, destination],
            |owners| retain(owners, &notification, &flights),
            |_| {
                calls += 1;
                assert!(weak_source.upgrade().is_some());
                assert!(weak_destination.upgrade().is_some());
                if calls == 2 {
                    Err(fault.record("sceGxmTransferCopy(texture)", -37))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
    assert_eq!(calls, 2);
    assert!(matches!(error, Error::Terminal(ref f)
        if f.operation == "sceGxmTransferCopy(texture)" && f.code == -37));
    // Even an observed notification cannot make a faulted queue release storage.
    notification.set(43);
    let mut slots = Vec::new();
    assert!(completion::collect(&mut flights.borrow_mut(), &mut slots, || fault.check()).is_err());
    assert!(slots.is_empty());
    assert_eq!(flights.borrow()[0].token.leases.borrow().len(), 2);
    assert!(weak_source.upgrade().is_some());
    assert!(weak_destination.upgrade().is_some());
}
