//! Query heaps, backed by host D3D12 query heaps when a host GPU is
//! present. Occlusion queries within which a draw was dropped fall back to
//! reporting visible so untranslated shaders do not cull geometry.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use crate::object::{self, E_INVALIDARG, HRESULT, Iid, Object, QUERY_HEAP, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;

const HEAP_IIDS: [Iid; 4] = [
    guid(
        0x0d9658ae,
        0xed45,
        0x469e,
        [0xa6, 0x1d, 0x97, 0x0e, 0xc5, 0x83, 0xca, 0xb4],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Desc {
    pub(crate) kind: u32,
    pub(crate) count: u32,
    pub(crate) nodes: u32,
}

/// State of one query slot: whether `EndQuery` ran on the host, and whether
/// no draw was dropped while it was open.
#[cfg(windows)]
#[derive(Clone, Copy, Default)]
pub(crate) struct SlotState {
    pub(crate) ended: bool,
    pub(crate) clean: bool,
}

#[cfg(windows)]
pub(crate) type SharedSlots = std::sync::Arc<std::sync::Mutex<Vec<SlotState>>>;

#[repr(C)]
struct Heap {
    object: Object,
    desc: Desc,
    #[cfg(windows)]
    host: Option<windows::Win32::Graphics::Direct3D12::ID3D12QueryHeap>,
    #[cfg(windows)]
    slots: SharedSlots,
}

#[cfg(windows)]
fn make_host(desc: &Desc) -> Option<windows::Win32::Graphics::Direct3D12::ID3D12QueryHeap> {
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_QUERY_HEAP_DESC, D3D12_QUERY_HEAP_TYPE, ID3D12QueryHeap,
    };
    if desc.count == 0 || !matches!(desc.kind, 0..=3 | 7) {
        return None;
    }
    let host = crate::host::host()?;
    let mut heap: Option<ID3D12QueryHeap> = None;
    let host_desc = D3D12_QUERY_HEAP_DESC {
        Type: D3D12_QUERY_HEAP_TYPE(desc.kind as i32),
        Count: desc.count,
        NodeMask: 0,
    };
    // SAFETY: plain query heap creation with a validated description.
    let made = unsafe { host.device.CreateQueryHeap(&host_desc, &mut heap) };
    if let Err(e) = made {
        tracing::debug!(error = %e, ?desc, "host query heap not created; using fallback values");
        return None;
    }
    if let Some(h) = &heap {
        crate::host::name(h, || format!("query heap kind {} count {}", desc.kind, desc.count));
    }
    heap
}

/// Device method.
pub(crate) extern "system" fn create(
    _: This,
    desc: *const Desc,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    if desc.count == 0 {
        return E_INVALIDARG;
    }
    tracing::debug!(?desc, "query heap");
    #[cfg(windows)]
    let host = if !out.is_null() && object::is(iid, &HEAP_IIDS) {
        make_host(&desc)
    } else {
        None
    };
    let heap = Box::new(Heap {
        object: Object::new(&FUNCTIONS),
        desc,
        #[cfg(windows)]
        slots: std::sync::Arc::new(std::sync::Mutex::new(if host.is_some() {
            vec![SlotState::default(); desc.count as usize]
        } else {
            Vec::new()
        })),
        #[cfg(windows)]
        host,
    });
    object::hand_out(QUERY_HEAP, heap, iid, &HEAP_IIDS, out)
}

/// Whether `query_kind` (`D3D12_QUERY_TYPE`) is legal on `heap_kind`
/// (`D3D12_QUERY_HEAP_TYPE`).
#[cfg(windows)]
pub(crate) fn kind_fits_heap(query_kind: u32, heap_kind: u32) -> bool {
    matches!(
        (query_kind, heap_kind),
        (0 | 1, 0) | (2, 1) | (3, 2) | (4..=7, 3) | (10, 7)
    )
}

/// Whether `BeginQuery` is legal for `query_kind` in D3D12 (timestamps and
/// video decode statistics only allow `EndQuery`).
#[cfg(windows)]
pub(crate) fn can_begin(query_kind: u32) -> bool {
    matches!(query_kind, 0 | 1 | 3 | 4..=7)
}

/// Clones the host query heap and shared slot-state table if `this` is valid
/// and `[index, index + count)` lies inside it for `query_kind`.
#[cfg(windows)]
pub(crate) fn heap_handles(
    this: This,
    query_kind: u32,
    index: u32,
    count: u32,
) -> Option<(
    windows::Win32::Graphics::Direct3D12::ID3D12QueryHeap,
    SharedSlots,
)> {
    if this.is_null() {
        return None;
    }
    // SAFETY: callers pass a query heap of ours.
    let heap = unsafe { &*this.cast::<Heap>() };
    let end = index.checked_add(count)?;
    if !kind_fits_heap(query_kind, heap.desc.kind) || end > heap.desc.count {
        return None;
    }
    Some((heap.host.as_ref()?.clone(), std::sync::Arc::clone(&heap.slots)))
}

/// Marks `index` as ended on the host GPU, recording whether no draw was
/// dropped while the query was open.
#[cfg(all(test, windows))]
pub(crate) fn mark_ended(this: This, index: u32, clean: bool) {
    if this.is_null() {
        return;
    }
    // SAFETY: callers pass a query heap of ours.
    let heap = unsafe { &*this.cast::<Heap>() };
    let mut slots = heap.slots.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = slots.get_mut(index as usize) {
        *slot = SlotState { ended: true, clean };
    }
}

/// Snapshot of `[first, first + count)` slot states and the contiguous runs
/// `(start_offset_in_range, len)` of slots that have been ended on the GPU.
#[cfg(all(test, windows))]
pub(crate) fn ended_runs(
    this: This,
    first: u32,
    count: u32,
) -> (Vec<SlotState>, Vec<(u32, u32)>) {
    if this.is_null() || count == 0 {
        return (Vec::new(), Vec::new());
    }
    // SAFETY: callers pass a query heap of ours.
    let heap = unsafe { &*this.cast::<Heap>() };
    let slots = heap.slots.lock().unwrap_or_else(|e| e.into_inner());
    let start = first as usize;
    let end = start.saturating_add(count as usize).min(slots.len());
    if start >= end {
        return (Vec::new(), Vec::new());
    }
    let states = slots[start..end].to_vec();
    let mut runs = Vec::new();
    let mut run_start: Option<u32> = None;
    for (i, s) in states.iter().enumerate() {
        if s.ended {
            run_start.get_or_insert(i as u32);
        } else if let Some(rs) = run_start.take() {
            runs.push((rs, i as u32 - rs));
        }
    }
    if let Some(rs) = run_start {
        runs.push((rs, states.len() as u32 - rs));
    }
    (states, runs)
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    object::answer(
        QUERY_HEAP,
        iid,
        object::is(iid, &HEAP_IIDS).then_some(this),
        out,
    )
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Heap>(this)
}

/// IUnknown 3, object 4, device child 1.
static FUNCTIONS: Table<24> = {
    let mut f = object::d3d12_object(object::missing_table::<QUERY_HEAP, 24>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{E_NOINTERFACE, E_POINTER, S_FALSE, S_OK, slot};

    #[test]
    fn null_descriptions_are_rejected() {
        let mut heap = std::ptr::null();
        assert_eq!(
            create(std::ptr::null(), std::ptr::null(), &HEAP_IIDS[0], &mut heap),
            E_INVALIDARG
        );
        assert!(heap.is_null());
        assert_eq!(
            create(
                std::ptr::null(),
                std::ptr::null(),
                &HEAP_IIDS[0],
                std::ptr::null_mut()
            ),
            E_INVALIDARG
        );
    }

    #[test]
    fn creation_without_an_out_pointer_only_probes() {
        let desc = Desc {
            kind: 1, // timestamps
            count: 64,
            nodes: 0,
        };
        assert_eq!(
            create(std::ptr::null(), &desc, &HEAP_IIDS[0], std::ptr::null_mut()),
            S_FALSE
        );
        let mut heap = std::ptr::null();
        assert_eq!(
            create(std::ptr::null(), &desc, std::ptr::null(), &mut heap),
            E_NOINTERFACE
        );
        assert!(heap.is_null());
    }

    #[test]
    fn heaps_answer_their_interfaces_and_count_references() {
        let desc = Desc {
            kind: 0, // occlusion
            count: 8,
            nodes: 0,
        };
        let null = std::ptr::null();
        let mut heap = null;
        assert_eq!(create(null, &desc, &HEAP_IIDS[0], &mut heap), S_OK);
        assert!(!heap.is_null());
        let query = slot::<extern "system" fn(This, *const Iid, *mut This) -> HRESULT>(heap, 0x0);
        for iid in &HEAP_IIDS {
            let mut out = null;
            assert_eq!(query(heap, iid, &mut out), S_OK);
            assert_eq!(out as usize, heap as usize);
        }
        let unknown = [0u8; 16];
        let mut out = heap;
        assert_eq!(query(heap, &unknown, &mut out), E_NOINTERFACE);
        assert!(out.is_null());
        let mut out = heap;
        assert_eq!(query(heap, std::ptr::null(), &mut out), E_NOINTERFACE);
        assert!(out.is_null());
        assert_eq!(query(heap, &unknown, std::ptr::null_mut()), E_POINTER);
        assert_eq!(slot::<extern "system" fn(This) -> u32>(heap, 0x8)(heap), 6);
        let release = slot::<extern "system" fn(This) -> u32>(heap, 0x10);
        for left in [5, 4, 3, 2, 1, 0] {
            assert_eq!(release(heap), left);
        }
    }

    #[test]
    fn query_heap_validates_and_tracks_ended_runs() {
        let null = std::ptr::null();
        let mut out = null;
        let zero = Desc {
            kind: 0,
            count: 0,
            nodes: 0,
        };
        assert_eq!(create(null, &zero, &HEAP_IIDS[0], &mut out), E_INVALIDARG);
        let desc = Desc {
            kind: 1,
            count: 8,
            nodes: 0,
        };
        assert_eq!(create(null, &desc, &HEAP_IIDS[0], &mut out), S_OK);
        assert!(!out.is_null());
        #[cfg(windows)]
        if heap_handles(out, 2, 0, 8).is_some() {
            assert!(heap_handles(out, 0, 0, 1).is_none(), "wrong query kind");
            assert!(heap_handles(out, 2, 5, 4).is_none(), "past end");
            mark_ended(out, 1, true);
            mark_ended(out, 2, false);
            mark_ended(out, 5, true);
            let (states, runs) = ended_runs(out, 0, 6);
            assert_eq!(runs, vec![(1, 2), (5, 1)]);
            assert!(states[1].clean && !states[2].clean);
        }
        assert_eq!(slot::<extern "system" fn(This) -> u32>(out, 0x10)(out), 0);
    }

    #[cfg(windows)]
    #[test]
    fn gpu_timestamps_and_occlusion_resolve_into_title_buffers() {
        if crate::host::host().is_none() {
            return;
        }
        let null = std::ptr::null();
        let mut ts_heap = null;
        assert_eq!(
            create(
                null,
                &Desc {
                    kind: 1,
                    count: 2,
                    nodes: 0,
                },
                &HEAP_IIDS[0],
                &mut ts_heap
            ),
            S_OK
        );
        let mut occ_heap = null;
        assert_eq!(
            create(
                null,
                &Desc {
                    kind: 0,
                    count: 2,
                    nodes: 0,
                },
                &HEAP_IIDS[0],
                &mut occ_heap
            ),
            S_OK
        );

        let device = crate::device::device();
        let res_iid = guid(
            0x696442be,
            0xa72e,
            0x4059,
            [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad],
        );
        #[repr(C)]
        struct BufDesc {
            dimension: u32,
            alignment: u64,
            width: u64,
            height: u32,
            depth: u16,
            mips: u16,
            format: u32,
            samples: u32,
            quality: u32,
            layout: u32,
            flags: u32,
        }
        let buf_desc = BufDesc {
            dimension: 1,
            alignment: 0,
            width: 256,
            height: 1,
            depth: 1,
            mips: 1,
            format: 0,
            samples: 1,
            quality: 0,
            layout: 1,
            flags: 0,
        };
        let mut memory = [0u64; 32];
        let mut buf = null;
        type CreateAt = extern "system" fn(
            This,
            u64,
            *const BufDesc,
            u32,
            *const std::ffi::c_void,
            *const Iid,
            *mut This,
        ) -> HRESULT;
        assert_eq!(
            slot::<CreateAt>(device, 0x428)(
                device,
                memory.as_mut_ptr() as u64,
                &buf_desc,
                0,
                null.cast(),
                &res_iid,
                &mut buf
            ),
            S_OK
        );

        let queue_iid = guid(
            0x0ec870a6,
            0x5d7e,
            0x4c22,
            [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed],
        );
        let alloc_iid = guid(
            0x6102dee4,
            0xaf59,
            0x4b09,
            [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24],
        );
        let list_iid = guid(
            0x5b160d0f,
            0xac1b,
            0x4185,
            [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55],
        );
        let (mut queue, mut alloc, mut list) = (null, null, null);
        let qdesc = [0u32; 4];
        assert_eq!(
            crate::queue::create(null, qdesc.as_ptr().cast(), &queue_iid, &mut queue),
            S_OK
        );
        assert_eq!(
            crate::list::create_allocator(null, 0, &alloc_iid, &mut alloc),
            S_OK
        );
        assert_eq!(
            crate::list::create_list(null, 0, 0, alloc, null, &list_iid, &mut list),
            S_OK
        );

        let newer = crate::device::newer();
        let off = |s: usize| if newer && s >= 59 { (s + 2) * 8 } else { s * 8 };
        type Mark = extern "system" fn(This, This, u32, u32);
        type Resolve = extern "system" fn(This, This, u32, u32, u32, This, u64);
        let begin = slot::<Mark>(list, off(28));
        let end = slot::<Mark>(list, off(29));
        let resolve = slot::<Resolve>(list, off(30));
        end(list, ts_heap, 2, 0);
        begin(list, occ_heap, 0, 0);
        end(list, occ_heap, 0, 0);
        end(list, ts_heap, 2, 1);
        resolve(list, ts_heap, 2, 0, 2, buf, 0);
        resolve(list, occ_heap, 0, 0, 2, buf, 16);
        // Resolving the same range a second time stays read-only on `occ_heap`.
        resolve(list, occ_heap, 0, 0, 2, buf, 32);

        slot::<extern "system" fn(This) -> HRESULT>(list, 0x48)(list);
        slot::<extern "system" fn(This, u32, *const This)>(queue, 0x50)(queue, 1, &list);
        crate::queue::before_sync();
        crate::host::settle_now(2);

        let m = |i: usize| unsafe { std::ptr::read_volatile(memory.as_ptr().add(i)) };
        assert!(m(1) >= m(0), "monotonic timestamps: [{}, {}]", m(0), m(1));
        assert_eq!(m(2), 0, "empty clean occlusion query measures 0");
        assert_eq!(m(3), 1 << 20, "un-ended occlusion slot stays visible");
        assert_eq!((m(4), m(5)), (0, 1 << 20), "multi-resolve preserves ended state");

        for obj in [list, alloc, queue, buf, occ_heap, ts_heap] {
            slot::<extern "system" fn(This) -> u32>(obj, 0x10)(obj);
        }
    }
}
