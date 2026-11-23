//! Display blackout brightness dimming for macOS desktop lock.
//!
//! Lowers hardware backlight/luminance to `0` on supported built-in Apple panels
//! (via `DisplayServices`) and external DDC/CI monitors (via `IOAVService`)
//! without putting displays to sleep — so macOS system lock is never triggered.
//!
//! All IOKit / I2C work runs serialized on a dedicated background worker thread
//! (`display-dim`) so the main thread and `CGEventTap` callback never block.

/// Standard DDC/CI 7-bit I2C slave address.
const DDC_I2C_ADDR: u32 = 0x37;
/// DDC/CI register/data sub-address used on Apple Silicon `IOAVService`.
const DDC_DATA_ADDR: u32 = 0x51;
/// MCCS VCP feature code for display luminance (brightness).
const DDC_VCP_LUMINANCE: u8 = 0x10;

/// Builds a DDC/CI Get VCP Feature request packet (4 bytes including checksum).
fn build_ddc_get_vcp_packet(vcp: u8) -> [u8; 4] {
    let checksum = 0x6E ^ (DDC_DATA_ADDR as u8) ^ 0x82 ^ 0x01 ^ vcp;
    [0x82, 0x01, vcp, checksum]
}

/// Builds a DDC/CI Set VCP Feature request packet (6 bytes including checksum).
fn build_ddc_set_vcp_packet(vcp: u8, value: u16) -> [u8; 6] {
    let hi = (value >> 8) as u8;
    let lo = (value & 0xFF) as u8;
    let checksum = 0x6E ^ (DDC_DATA_ADDR as u8) ^ 0x84 ^ 0x03 ^ vcp ^ hi ^ lo;
    [0x84, 0x03, vcp, hi, lo, checksum]
}

/// Parses an 11-byte DDC/CI VCP Feature reply into `(current, max)`.
fn parse_ddc_vcp_reply(reply: &[u8; 11], expected_vcp: u8) -> Option<(u16, u16)> {
    if reply[0] != 0x6E
        || (reply[1] & 0x7F) < 8
        || reply[2] != 0x02
        || reply[3] != 0x00
        || reply[4] != expected_vcp
    {
        return None;
    }
    let checksum = reply[..10].iter().fold(0x50_u8, |acc, &b| acc ^ b);
    if checksum != reply[10] {
        return None;
    }
    let max = u16::from_be_bytes([reply[6], reply[7]]);
    let cur = u16::from_be_bytes([reply[8], reply[9]]);
    if max == 0 || cur > max {
        return None;
    }
    Some((cur, max))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        DDC_DATA_ADDR, DDC_I2C_ADDR, DDC_VCP_LUMINANCE, build_ddc_get_vcp_packet,
        build_ddc_set_vcp_packet, parse_ddc_vcp_reply,
    };
    use objc2_core_foundation::CFString;
    use std::collections::HashMap;
    use std::ffi::{CStr, c_void};
    use std::ptr;
    use std::sync::{Condvar, Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant};

    type IoObject = u32;
    type IoIterator = u32;
    type IoService = u32;
    type KernReturn = i32;
    type IoReturn = i32;
    type IoAvServiceRef = *mut c_void;

    type DsCanChangeFn = unsafe extern "C" fn(u32) -> bool;
    type DsGetBrightnessFn = unsafe extern "C" fn(u32, *mut f32) -> i32;
    type DsSetBrightnessFn = unsafe extern "C" fn(u32, f32) -> i32;

    type AvCreateWithServiceFn = unsafe extern "C" fn(*const c_void, IoService) -> IoAvServiceRef;
    type AvWriteI2cFn =
        unsafe extern "C" fn(IoAvServiceRef, u32, u32, *const c_void, u32) -> IoReturn;
    type AvReadI2cFn = unsafe extern "C" fn(IoAvServiceRef, u32, u32, *mut c_void, u32) -> IoReturn;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const libc::c_char) -> *mut c_void;
        fn IOServiceGetMatchingServices(
            main_port: u32,
            matching: *mut c_void,
            existing: *mut IoIterator,
        ) -> KernReturn;
        fn IOIteratorNext(iterator: IoIterator) -> IoObject;
        fn IOObjectRelease(object: IoObject) -> KernReturn;
        fn IORegistryEntryGetRegistryEntryID(entry: IoObject, entry_id: *mut u64) -> KernReturn;
        fn IORegistryEntryCreateCFProperty(
            entry: IoObject,
            key: *const c_void,
            allocator: *const c_void,
            options: u32,
        ) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFEqual(cf1: *const c_void, cf2: *const c_void) -> u8;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut u32,
            display_count: *mut u32,
        ) -> i32;
    }

    #[derive(Clone, Copy)]
    struct DisplayServicesApi {
        can_change: DsCanChangeFn,
        get: DsGetBrightnessFn,
        set: DsSetBrightnessFn,
    }

    #[derive(Clone, Copy)]
    struct IoAvServiceApi {
        create_with_service: AvCreateWithServiceFn,
        write_i2c: AvWriteI2cFn,
        read_i2c: AvReadI2cFn,
    }

    unsafe fn lookup_symbol(handle: *mut c_void, name: &CStr) -> Option<*mut c_void> {
        // SAFETY: `handle` is a live `dlopen` handle and `name` is NUL-terminated.
        let sym = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if sym.is_null() { None } else { Some(sym) }
    }

    fn display_services() -> Option<DisplayServicesApi> {
        static API: OnceLock<Option<DisplayServicesApi>> = OnceLock::new();
        *API.get_or_init(|| {
            // SAFETY: constant NUL-terminated private framework path; handle kept process-wide.
            let handle = unsafe {
                libc::dlopen(
                    c"/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices"
                        .as_ptr(),
                    libc::RTLD_LAZY | libc::RTLD_LOCAL,
                )
            };
            if handle.is_null() {
                return None;
            }
            // SAFETY: function pointers match DisplayServices.framework signatures.
            unsafe {
                let can_change = lookup_symbol(handle, c"DisplayServicesCanChangeBrightness")?;
                let get = lookup_symbol(handle, c"DisplayServicesGetBrightness")?;
                let set = lookup_symbol(handle, c"DisplayServicesSetBrightness")?;
                Some(DisplayServicesApi {
                    can_change: std::mem::transmute::<*mut c_void, DsCanChangeFn>(can_change),
                    get: std::mem::transmute::<*mut c_void, DsGetBrightnessFn>(get),
                    set: std::mem::transmute::<*mut c_void, DsSetBrightnessFn>(set),
                })
            }
        })
    }

    fn io_av_service() -> Option<IoAvServiceApi> {
        static API: OnceLock<Option<IoAvServiceApi>> = OnceLock::new();
        *API.get_or_init(|| {
            // SAFETY: constant NUL-terminated framework path; handle kept process-wide.
            let handle = unsafe {
                libc::dlopen(
                    c"/System/Library/Frameworks/IOKit.framework/IOKit".as_ptr(),
                    libc::RTLD_LAZY | libc::RTLD_LOCAL,
                )
            };
            if handle.is_null() {
                return None;
            }
            // SAFETY: function pointers match IOKit IOAVService signatures.
            unsafe {
                let create_with_service = lookup_symbol(handle, c"IOAVServiceCreateWithService")?;
                let write_i2c = lookup_symbol(handle, c"IOAVServiceWriteI2C")?;
                let read_i2c = lookup_symbol(handle, c"IOAVServiceReadI2C")?;
                Some(IoAvServiceApi {
                    create_with_service: std::mem::transmute::<*mut c_void, AvCreateWithServiceFn>(
                        create_with_service,
                    ),
                    write_i2c: std::mem::transmute::<*mut c_void, AvWriteI2cFn>(write_i2c),
                    read_i2c: std::mem::transmute::<*mut c_void, AvReadI2cFn>(read_i2c),
                })
            }
        })
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Target {
        Released,
        Awake,
        Dimmed,
    }

    #[derive(Clone, Default)]
    struct SavedLevels {
        builtin: Vec<(u32, f32)>,
        ddc: Vec<(u64, u16)>,
    }

    struct WorkerState {
        target: Target,
        currently_dimmed: bool,
        busy: bool,
        saved: Option<SavedLevels>,
    }

    struct Dimmer {
        state: Mutex<WorkerState>,
        cvar: Condvar,
    }

    static DIMMER: OnceLock<Dimmer> = OnceLock::new();

    fn ensure_dimmer() -> &'static Dimmer {
        DIMMER.get_or_init(|| {
            let dimmer = Dimmer {
                state: Mutex::new(WorkerState {
                    target: Target::Released,
                    currently_dimmed: false,
                    busy: false,
                    saved: None,
                }),
                cvar: Condvar::new(),
            };
            if let Err(e) = thread::Builder::new()
                .name("display-dim".into())
                .spawn(worker_loop)
            {
                tracing::warn!("could not spawn display-dim worker thread: {e}");
            }
            dimmer
        })
    }

    pub fn dim_async() {
        let dimmer = ensure_dimmer();
        let mut guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.target = Target::Dimmed;
        dimmer.cvar.notify_all();
    }

    pub fn wake_async() {
        let Some(dimmer) = DIMMER.get() else {
            return;
        };
        let mut guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.target = Target::Awake;
        dimmer.cvar.notify_all();
    }

    pub fn release_async() {
        let Some(dimmer) = DIMMER.get() else {
            return;
        };
        let mut guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.target = Target::Released;
        dimmer.cvar.notify_all();
    }

    pub fn release_sync() {
        let Some(dimmer) = DIMMER.get() else {
            return;
        };
        let mut guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.busy
            && !guard.currently_dimmed
            && guard.saved.is_none()
            && guard.target == Target::Released
        {
            return;
        }
        guard.target = Target::Released;
        dimmer.cvar.notify_all();

        let deadline = Instant::now() + Duration::from_secs(2);
        while guard.busy
            || guard.currently_dimmed
            || guard.saved.is_some()
            || guard.target != Target::Released
        {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, wait_res) = dimmer
                .cvar
                .wait_timeout(guard, deadline.saturating_duration_since(now))
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
            if wait_res.timed_out() {
                break;
            }
        }
    }

    fn worker_loop() {
        let Some(dimmer) = DIMMER.get() else {
            return;
        };
        let mut guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match guard.target {
                Target::Dimmed if !guard.currently_dimmed => {
                    guard.busy = true;
                    let cached = guard.saved.clone();
                    drop(guard);

                    let levels = match cached {
                        Some(existing) => existing,
                        None => probe_all_displays(),
                    };

                    guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
                    if guard.target != Target::Dimmed {
                        // Interrupted before applying dim: keep/clear state on next pass.
                        continue;
                    }
                    if guard.saved.is_none() {
                        guard.saved = Some(levels.clone());
                    }
                    let apply_levels = guard.saved.clone().unwrap_or(levels);
                    guard.currently_dimmed = true;
                    drop(guard);

                    apply_dim_all(&apply_levels);
                    guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
                }
                Target::Awake if guard.currently_dimmed => {
                    guard.busy = true;
                    let saved = guard.saved.clone();
                    drop(guard);

                    if let Some(levels) = saved {
                        restore_all(&levels);
                    }

                    guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.currently_dimmed = false;
                }
                Target::Released if guard.currently_dimmed || guard.saved.is_some() => {
                    guard.busy = true;
                    let was_dimmed = guard.currently_dimmed;
                    let saved = guard.saved.take();
                    guard.currently_dimmed = false;
                    drop(guard);

                    if was_dimmed && let Some(levels) = saved {
                        restore_all(&levels);
                    }

                    guard = dimmer.state.lock().unwrap_or_else(|e| e.into_inner());
                }
                _ => {
                    guard.busy = false;
                    dimmer.cvar.notify_all();
                    guard = dimmer.cvar.wait(guard).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
    }

    fn active_display_ids() -> Vec<u32> {
        let mut count = 0_u32;
        // SAFETY: NULL display buffer queries the count of active displays.
        if unsafe { CGGetActiveDisplayList(0, ptr::null_mut(), &mut count) } != 0 || count == 0 {
            return Vec::new();
        }
        let mut ids = vec![0_u32; count as usize];
        let mut actual = 0_u32;
        // SAFETY: `ids` has capacity for `count` `CGDirectDisplayID` values.
        if unsafe { CGGetActiveDisplayList(count, ids.as_mut_ptr(), &mut actual) } != 0 {
            return Vec::new();
        }
        ids.truncate(actual as usize);
        ids
    }

    fn probe_builtin_displays() -> Vec<(u32, f32)> {
        let Some(api) = display_services() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for id in active_display_ids() {
            // SAFETY: valid display ID and valid out-pointer.
            let can = unsafe { (api.can_change)(id) };
            if !can {
                continue;
            }
            let mut val = 0.0_f32;
            // SAFETY: valid display ID and initialized stack float.
            let rc = unsafe { (api.get)(id, &mut val) };
            if rc == 0 && val.is_finite() && val > 0.0 {
                out.push((id, val.min(1.0)));
            }
        }
        out
    }

    struct ExternalAvService {
        entry_id: u64,
        av: IoAvServiceRef,
    }

    impl Drop for ExternalAvService {
        fn drop(&mut self) {
            if !self.av.is_null() {
                // SAFETY: `self.av` was returned by `IOAVServiceCreateWithService`.
                unsafe { CFRelease(self.av.cast()) };
            }
        }
    }

    fn enumerate_external_av_services(api: IoAvServiceApi) -> Vec<ExternalAvService> {
        // SAFETY: C string literal matches the IORegistry service class name.
        let matching = unsafe { IOServiceMatching(c"DCPAVServiceProxy".as_ptr()) };
        if matching.is_null() {
            return Vec::new();
        }
        let mut iter = 0_u32;
        // SAFETY: `IOServiceGetMatchingServices` consumes one reference to `matching`.
        let kr = unsafe { IOServiceGetMatchingServices(0, matching, &mut iter) };
        if kr != 0 || iter == 0 {
            return Vec::new();
        }

        let location_key = CFString::from_static_str("Location");
        let external_val = CFString::from_static_str("External");
        let location_ptr = (&*location_key as *const CFString).cast::<c_void>();
        let external_ptr = (&*external_val as *const CFString).cast::<c_void>();

        let mut services = Vec::new();
        loop {
            // SAFETY: `iter` is a live `io_iterator_t`.
            let svc = unsafe { IOIteratorNext(iter) };
            if svc == 0 {
                break;
            }
            // SAFETY: `svc` is a live `io_object_t`; `location_ptr` is a static `CFString`.
            let prop =
                unsafe { IORegistryEntryCreateCFProperty(svc, location_ptr, ptr::null(), 0) };
            let is_external = if !prop.is_null() {
                // SAFETY: both arguments are non-null `CFTypeRef`s; release `prop` immediately.
                let eq = unsafe { CFEqual(prop, external_ptr) } != 0;
                unsafe { CFRelease(prop) };
                eq
            } else {
                false
            };

            if is_external {
                let mut entry_id = 0_u64;
                // SAFETY: `svc` is a valid IORegistryEntry and `entry_id` is writable.
                let id_rc = unsafe { IORegistryEntryGetRegistryEntryID(svc, &mut entry_id) };
                if id_rc == 0 {
                    // SAFETY: `svc` is an active external `DCPAVServiceProxy`.
                    let av = unsafe { (api.create_with_service)(ptr::null(), svc) };
                    if !av.is_null() {
                        services.push(ExternalAvService { entry_id, av });
                    }
                }
            }
            // SAFETY: `svc` was returned by `IOIteratorNext`.
            unsafe { IOObjectRelease(svc) };
        }
        // SAFETY: `iter` was returned by `IOServiceGetMatchingServices`.
        unsafe { IOObjectRelease(iter) };
        services
    }

    fn read_ddc_luminance(api: IoAvServiceApi, av: IoAvServiceRef) -> Option<u16> {
        let pkt = build_ddc_get_vcp_packet(DDC_VCP_LUMINANCE);
        for _ in 0..3 {
            // SAFETY: `pkt` is a 4-byte stack buffer valid for `IOAVServiceWriteI2C`.
            let w1 = unsafe {
                (api.write_i2c)(
                    av,
                    DDC_I2C_ADDR,
                    DDC_DATA_ADDR,
                    pkt.as_ptr().cast(),
                    pkt.len() as u32,
                )
            };
            thread::sleep(Duration::from_millis(10));
            let w2 = unsafe {
                (api.write_i2c)(
                    av,
                    DDC_I2C_ADDR,
                    DDC_DATA_ADDR,
                    pkt.as_ptr().cast(),
                    pkt.len() as u32,
                )
            };
            thread::sleep(Duration::from_millis(50));
            let mut reply = [0_u8; 11];
            // SAFETY: `reply` is an 11-byte mutable stack buffer.
            let rrc = unsafe {
                (api.read_i2c)(
                    av,
                    DDC_I2C_ADDR,
                    0x00,
                    reply.as_mut_ptr().cast(),
                    reply.len() as u32,
                )
            };
            if w1 == 0
                && w2 == 0
                && rrc == 0
                && let Some((cur, _max)) = parse_ddc_vcp_reply(&reply, DDC_VCP_LUMINANCE)
            {
                return Some(cur);
            }
            thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn write_ddc_luminance(api: IoAvServiceApi, av: IoAvServiceRef, value: u16) -> bool {
        let pkt = build_ddc_set_vcp_packet(DDC_VCP_LUMINANCE, value);
        // SAFETY: `pkt` is a 6-byte stack buffer valid for `IOAVServiceWriteI2C`.
        let w1 = unsafe {
            (api.write_i2c)(
                av,
                DDC_I2C_ADDR,
                DDC_DATA_ADDR,
                pkt.as_ptr().cast(),
                pkt.len() as u32,
            )
        };
        thread::sleep(Duration::from_millis(10));
        let w2 = unsafe {
            (api.write_i2c)(
                av,
                DDC_I2C_ADDR,
                DDC_DATA_ADDR,
                pkt.as_ptr().cast(),
                pkt.len() as u32,
            )
        };
        w1 == 0 || w2 == 0
    }

    fn probe_ddc_displays() -> Vec<(u64, u16)> {
        let Some(api) = io_av_service() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for svc in enumerate_external_av_services(api) {
            if let Some(cur) = read_ddc_luminance(api, svc.av)
                && cur > 0
            {
                out.push((svc.entry_id, cur));
            }
        }
        out
    }

    fn probe_all_displays() -> SavedLevels {
        SavedLevels {
            builtin: probe_builtin_displays(),
            ddc: probe_ddc_displays(),
        }
    }

    fn apply_dim_all(levels: &SavedLevels) {
        if let Some(api) = display_services() {
            for &(id, _) in &levels.builtin {
                // SAFETY: `id` was probed via `DisplayServicesCanChangeBrightness`.
                let _ = unsafe { (api.set)(id, 0.0) };
            }
        }
        if !levels.ddc.is_empty()
            && let Some(api) = io_av_service()
        {
            let targets: HashMap<u64, u16> = levels.ddc.iter().copied().collect();
            for svc in enumerate_external_av_services(api) {
                if targets.contains_key(&svc.entry_id) {
                    let _ = write_ddc_luminance(api, svc.av, 0);
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        tracing::info!(
            builtin = levels.builtin.len(),
            ddc = levels.ddc.len(),
            "display brightness dimmed for lock"
        );
    }

    fn restore_all(levels: &SavedLevels) {
        if let Some(api) = display_services() {
            for &(id, val) in &levels.builtin {
                // SAFETY: `id` and `val` were saved from `DisplayServicesGetBrightness`.
                let _ = unsafe { (api.set)(id, val) };
            }
        }
        if !levels.ddc.is_empty()
            && let Some(api) = io_av_service()
        {
            let targets: HashMap<u64, u16> = levels.ddc.iter().copied().collect();
            for svc in enumerate_external_av_services(api) {
                if let Some(&saved) = targets.get(&svc.entry_id) {
                    let _ = write_ddc_luminance(api, svc.av, saved);
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        tracing::info!(
            builtin = levels.builtin.len(),
            ddc = levels.ddc.len(),
            "display brightness restored"
        );
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn dim_async() {}
    pub fn wake_async() {}
    pub fn release_async() {}
    pub fn release_sync() {}
}

/// Lowers supported display brightnesses to `0` in the background while locked.
pub fn dim_async() {
    imp::dim_async();
}

/// Restores saved display brightnesses in the background on user wake input
/// without discarding the saved levels (so re-dimming preserves originals).
pub fn wake_async() {
    imp::wake_async();
}

/// Restores saved display brightnesses in the background on unlock and forgets
/// the saved levels.
pub fn release_async() {
    imp::release_async();
}

/// Synchronously waits (up to 2s) for any pending brightness restore to finish
/// before process exit.
pub fn release_sync() {
    imp::release_sync();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ddc_get_vcp_packet_with_checksum() {
        let pkt = build_ddc_get_vcp_packet(DDC_VCP_LUMINANCE);
        assert_eq!(pkt, [0x82, 0x01, 0x10, 0x6E ^ 0x51 ^ 0x82 ^ 0x01 ^ 0x10]);
    }

    #[test]
    fn builds_ddc_set_vcp_packet_with_checksum() {
        assert_eq!(
            build_ddc_set_vcp_packet(DDC_VCP_LUMINANCE, 0),
            [
                0x84,
                0x03,
                0x10,
                0x00,
                0x00,
                0x6E ^ 0x51 ^ 0x84 ^ 0x03 ^ 0x10
            ]
        );
        assert_eq!(
            build_ddc_set_vcp_packet(DDC_VCP_LUMINANCE, 70),
            [
                0x84,
                0x03,
                0x10,
                0x00,
                70,
                0x6E ^ 0x51 ^ 0x84 ^ 0x03 ^ 0x10 ^ 70
            ]
        );
    }

    #[test]
    fn parses_real_ddc_vcp_replies_and_rejects_corrupt_packets() {
        let dell: [u8; 11] = [
            0x6e, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x46, 0x86,
        ];
        assert_eq!(
            parse_ddc_vcp_reply(&dell, DDC_VCP_LUMINANCE),
            Some((70, 100))
        );

        let mi: [u8; 11] = [
            0x6e, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x22, 0xe2,
        ];
        assert_eq!(parse_ddc_vcp_reply(&mi, DDC_VCP_LUMINANCE), Some((34, 100)));

        // Bad checksum.
        let mut bad_ck = dell;
        bad_ck[10] ^= 0x01;
        assert_eq!(parse_ddc_vcp_reply(&bad_ck, DDC_VCP_LUMINANCE), None);

        // Unsupported VCP result code (`reply[3] != 0`).
        let mut err = dell;
        err[3] = 0x01;
        err[10] ^= 0x01;
        assert_eq!(parse_ddc_vcp_reply(&err, DDC_VCP_LUMINANCE), None);

        // Wrong VCP opcode.
        assert_eq!(parse_ddc_vcp_reply(&dell, 0x12), None);

        // Current exceeds max.
        let mut over = dell;
        over[8] = 0x01;
        over[10] ^= 0x01;
        assert_eq!(parse_ddc_vcp_reply(&over, DDC_VCP_LUMINANCE), None);
    }
}
