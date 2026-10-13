//! PTT via the classic OmniRig freeware (VE3NEA, dxatlas.com).
//!
//! OmniRig owns the radio's serial port and shares it with other programs
//! through a COM object. Clients attach to the running instance and poll the
//! `Tx` property of the selected rig slot (`Rig1` or `Rig2` — the type
//! library exposes exactly two): it reads `PM_RX` while the rig is receiving
//! and `PM_TX` while it is transmitting.
//!
//! The interface layout (GUIDs, vtable order, `PM_*`/`ST_*` values) comes
//! from OmniRig's own type library (`OmniRig.ridl` / `OmniRig_TLB.pas` in
//! the upstream source). The COM calls are hand-rolled FFI — no crate — and
//! are only available on Windows; elsewhere `connect` reports the platform
//! is unsupported.

/// COM GUID. Also used to build the registry path for the install check.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows client uses it
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self { data1, data2, data3, data4 }
    }
}

/// The registry form of a GUID (uppercase, braces): the key name the installer
/// writes under `SOFTWARE\Classes\CLSID`.
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows client uses it
fn guid_string(g: &Guid) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}

#[cfg(windows)]
mod ffi {
    use std::os::raw::{c_char, c_int, c_void};

    use super::Guid;

    /// COM HRESULT.
    pub type HResult = c_int;

    /// Registry status code (`LSTATUS`).
    pub type LStatus = c_int;

    /// OmniRigX coclass (the running OmniRig instance).
    pub const CLSID_OMNIRIGX: Guid =
        Guid::new(0x0839_E8C6, 0xED30, 0x4950, [0x80, 0x87, 0x96, 0x6F, 0x97, 0x0F, 0x0C, 0xAE]);
    /// IOmniRigX, the top-level interface (exposes the `Rig1` property).
    pub const IID_IOMNIRIGX: Guid =
        Guid::new(0x501A_2858, 0x3331, 0x467A, [0x83, 0x7A, 0x98, 0x9F, 0xDE, 0xDA, 0xCC, 0x7D]);

    pub const S_OK: HResult = 0;
    // Negative HRESULTs; the literals only fit in i32 through a u32 cast.
    // REGDB_E_CLASSNOTREG (the class is not registered in the registry view
    // this process sees) — the value previously written here, 0x800401F3, is
    // REGDB_E_READREGDB.
    pub const REGDB_E_CLASSNOTREG: HResult = 0x8004_0154u32 as i32;
    pub const COINIT_MULTITHREADED: u32 = 0;
    // CLSCTX_LOCAL_SERVER is 0x4. It was previously written as 2, which is
    // CLSCTX_INPROC_HANDLER — that is what made activation fail on a machine
    // where the CLSID is registered correctly: OmniRig's class has only a
    // LocalServer32 key, so requesting an in-process handler gives
    // REGDB_E_CLASSNOTREG.
    pub const CLSCTX_LOCAL_SERVER: u32 = 0x4;

    // Registry access, used only to work out *why* the CLSID lookup failed.
    pub const ERROR_SUCCESS: LStatus = 0;
    pub const HKEY_LOCAL_MACHINE: *mut c_void = 0x8000_0002 as *mut c_void;
    pub const KEY_QUERY_VALUE: u32 = 0x0001;
    pub const KEY_WOW64_64KEY: u32 = 0x0100;
    pub const KEY_WOW64_32KEY: u32 = 0x0200;

    #[link(name = "ole32")]
    extern "system" {
        pub fn CoInitializeEx(pvReserved: *mut c_void, dwCoInit: u32) -> HResult;
        pub fn CoCreateInstance(
            rclsid: *const Guid,
            punkOuter: *mut c_void,
            dwClsContext: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> HResult;
        /// Returns a running object registered via `RegisterActiveObject`.
        pub fn GetActiveObject(
            rclsid: *const Guid,
            riid: *const Guid,
            ppvObj: *mut *mut c_void,
        ) -> HResult;
    }

    #[link(name = "advapi32")]
    extern "system" {
        pub fn RegOpenKeyExA(
            hKey: *mut c_void,
            subKey: *const c_char,
            options: u32,
            samDesired: u32,
            phkResult: *mut *mut c_void,
        ) -> LStatus;
        pub fn RegCloseKey(hKey: *mut c_void) -> LStatus;
    }
}

/// `IRigX.Tx` values (RigParamX bits).
#[cfg(windows)]
const PM_RX: i32 = 0x0020_0000;
#[cfg(windows)]
const PM_TX: i32 = 0x0040_0000;

/// OmniRig rig link status (RigStatusX).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows client builds it
pub enum Status {
    NotConfigured,
    Disabled,
    PortBusy,
    NotResponding,
    Online,
}

impl Status {
    #[cfg_attr(not(windows), allow(dead_code))]
    fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Status::NotConfigured),
            1 => Some(Status::Disabled),
            2 => Some(Status::PortBusy),
            3 => Some(Status::NotResponding),
            4 => Some(Status::Online),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Status::NotConfigured => "not configured",
            Status::Disabled => "disabled",
            Status::PortBusy => "port busy",
            Status::NotResponding => "not responding",
            Status::Online => "online",
        }
    }
}

/// Read a vtable slot of a COM object as raw function-pointer bits.
///
/// The caller transmutes the result to the concrete `unsafe extern "system"
/// fn` type of the slot. A vtable entry and a function pointer are both
/// pointer-sized, so the transmute is well-defined.
///
/// # Safety
/// `obj` must be a live COM interface pointer and `index` a valid slot of
/// that interface's vtable.
#[cfg(windows)]
unsafe fn slot_bits(obj: *mut std::os::raw::c_void, index: usize) -> *const u8 {
    // The first pointer-sized word of the object is the vtable pointer
    // (4 bytes in the 32-bit build, 8 in a 64-bit one).
    let vtable: *const u8 = unsafe { *(obj as *const *const u8) };
    // The vtable is an array of function pointers; advance by `index` of
    // them (not bytes) and read the slot.
    unsafe { *((vtable as *const *const u8).add(index)) }
}

/// Is `CLSID_OMNIRIGX` registered in the given registry view?
///
/// `sam_desired` carries the WOW64 view flag. A 64-bit process reads the
/// native view by default and the 32-bit view only with `KEY_WOW64_32KEY`; a
/// 32-bit process always sees the 32-bit view (the flags are ignored there),
/// so both probes return the same answer in the 32-bit build.
///
/// This runs on the poller thread, not in an audio callback, so the allocation
/// is fine.
///
/// # Safety
/// `sam_desired` must be one of the `KEY_WOW64_*` constants.
#[cfg(windows)]
unsafe fn clsid_registered(sam_desired: u32) -> bool {
    use std::ptr;

    let path = match std::ffi::CString::new(format!(
        "SOFTWARE\\Classes\\CLSID\\{}",
        guid_string(&ffi::CLSID_OMNIRIGX)
    )) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let mut key: *mut std::os::raw::c_void = ptr::null_mut();
    let status = unsafe {
        ffi::RegOpenKeyExA(
            ffi::HKEY_LOCAL_MACHINE,
            path.as_ptr(),
            0,
            ffi::KEY_QUERY_VALUE | sam_desired,
            &mut key,
        )
    };
    if status == ffi::ERROR_SUCCESS {
        unsafe { ffi::RegCloseKey(key) };
        true
    } else {
        false
    }
}

/// Vtable slot of `Get_RigN` on IOmniRigX: `Get_Rig1` is 9, `Get_Rig2` is 10
/// (IUnknown + IDispatch prefix = 7 slots, then the type-library order).
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows client uses it
pub fn rig_slot(rig: u8) -> usize {
    8 + rig as usize
}

/// A connection to one of OmniRig's rig slots (`Rig1` or `Rig2`).
///
/// The COM vtable layout (IUnknown + IDispatch prefix = 7 slots):
/// IOmniRigX `Get_Rig1` at slot 9, `Get_Rig2` at 10; IRigX `Get_Status` at
/// 12, `Get_Tx` at 32. Property getters use the automation convention —
/// `HRESULT` return with the value in an out parameter — because cross-process
/// calls go through the proxy stub generated from the type library.
#[cfg(windows)]
pub struct Client {
    /// IOmniRigX (the OmniRigX coclass object).
    omni: *mut std::os::raw::c_void,
    /// IRigX (the selected rig slot; null if that rig is not configured).
    rig: *mut std::os::raw::c_void,
}

#[cfg(windows)]
impl Client {
    /// Attach to a running OmniRig instance, or start one, and grab the
    /// selected rig slot (`rig` is 1 or 2).
    pub fn connect(rig: u8) -> Result<Self, String> {
        use std::os::raw::c_void;
        use std::ptr;

        // Initialize COM for this thread; idempotent if the poller already
        // did (e.g. after a reconnect).
        let hr = unsafe { ffi::CoInitializeEx(ptr::null_mut(), ffi::COINIT_MULTITHREADED) };
        if hr != ffi::S_OK && hr != 1 /* S_FALSE */ {
            return Err(format!("COM initialization failed (0x{:08X})", hr as u32));
        }

        let mut omni: *mut c_void = ptr::null_mut();
        let hr =
            unsafe { ffi::GetActiveObject(&ffi::CLSID_OMNIRIGX, &ffi::IID_IOMNIRIGX, &mut omni) };
        if hr != ffi::S_OK {
            // Not running (or not registered as a running object): ask the
            // COM runtime to start the local server. This matches OmniRig's
            // own C++ sample client.
            let hr2 = unsafe {
                ffi::CoCreateInstance(
                    &ffi::CLSID_OMNIRIGX,
                    ptr::null_mut(),
                    ffi::CLSCTX_LOCAL_SERVER,
                    &ffi::IID_IOMNIRIGX,
                    &mut omni,
                )
            };
            if hr2 != ffi::S_OK {
                return Err(match hr2 {
                    ffi::REGDB_E_CLASSNOTREG => {
                        // The CLSID lookup failed for this process. Check which
                        // registry views can see it, so the message says whether
                        // OmniRig is missing or merely invisible to this bitness.
                        let in32 = unsafe { clsid_registered(ffi::KEY_WOW64_32KEY) };
                        let in64 = unsafe { clsid_registered(ffi::KEY_WOW64_64KEY) };
                        // The view this process actually reads. A 32-bit process
                        // ignores the WOW64 flags, so both probes agree there.
                        let own = if cfg!(target_pointer_width = "64") { in64 } else { in32 };
                        if own {
                            // Visible to this process, so the failure is the kind
                            // of server requested, not the bitness.
                            format!(
                                "OmniRig's COM class is registered in this process's registry view but not as a local server it can start (0x{:08X}); start OmniRig and connect to the running instance",
                                hr2 as u32
                            )
                        } else if in32 || in64 {
                            // A 64-bit process can still attach to an already
                            // running OmniRig through the class object table
                            // (Delphi's CoRegisterClassObject): the registry is
                            // only consulted when the server has to be started.
                            format!(
                                "OmniRig is installed but its COM class is registered only in the 32-bit registry view, so this process cannot start it (0x{:08X}); start OmniRig and connect to the running instance",
                                hr2 as u32
                            )
                        } else {
                            format!(
                                "OmniRig is not installed on this machine (0x{:08X}); its installer registers the COM class",
                                hr2 as u32
                            )
                        }
                    }
                    other => format!("OmniRig could not be started (0x{:08X})", other as u32),
                });
            }
        }

        // Get_RigN (IOmniRigX slots 9/10). Property getters use the automation
        // convention: HRESULT return, value in an out parameter. Cross-process
        // calls always go through the MIDL-generated proxy stub, which follows
        // that convention — calling it as a direct-return function makes the
        // stub write through an out-parameter pointer that was never passed
        // (access violation in RPCRT4).
        type GetRig = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
        type Release = unsafe extern "system" fn(*mut c_void) -> u32;
        let get_rig: GetRig = unsafe { std::mem::transmute(slot_bits(omni, rig_slot(rig))) };
        let release: Release = unsafe { std::mem::transmute(slot_bits(omni, 2)) };

        let mut rig_obj: *mut c_void = ptr::null_mut();
        let hr3 = unsafe { get_rig(omni, &mut rig_obj) };
        if hr3 != ffi::S_OK {
            unsafe { release(omni) };
            return Err(format!(
                "OmniRig rig {rig} could not be read (0x{:08X})",
                hr3 as u32
            ));
        }
        if rig_obj.is_null() {
            unsafe { release(omni) };
            return Err(format!("OmniRig is running but rig {rig} is not configured"));
        }
        Ok(Client {
            omni,
            rig: rig_obj,
        })
    }

    /// Read the rig's transmit state: `Ok(0)` = RX, `Ok(1)` = TX.
    pub fn tx(&self) -> Result<u8, String> {
        use std::os::raw::c_void;
        // Get_Tx (IRigX slot 32): automation convention — HRESULT return, the
        // value in an out parameter.
        type GetTx = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
        let get_tx: GetTx = unsafe { std::mem::transmute(slot_bits(self.rig, 32)) };
        let mut value = 0i32;
        let hr = unsafe { get_tx(self.rig, &mut value) };
        if hr != ffi::S_OK {
            return Err(format!("OmniRig Tx read failed (0x{:08X})", hr as u32));
        }
        match value {
            PM_TX => Ok(1),
            PM_RX => Ok(0),
            other => Err(format!("unexpected OmniRig Tx value {other:#010X}")),
        }
    }

    /// The rig link status, if known.
    pub fn status(&self) -> Option<Status> {
        use std::os::raw::c_void;
        type GetStatus = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
        let get_status: GetStatus = unsafe { std::mem::transmute(slot_bits(self.rig, 12)) };
        let mut code = 0i32;
        if unsafe { get_status(self.rig, &mut code) } != ffi::S_OK {
            return None;
        }
        Status::from_code(code)
    }

}

#[cfg(windows)]
impl Drop for Client {
    fn drop(&mut self) {
        use std::os::raw::c_void;
        type Release = unsafe extern "system" fn(*mut c_void) -> u32;
        unsafe {
            let release: Release = std::mem::transmute(slot_bits(self.rig, 2));
            release(self.rig);
            let release: Release = std::mem::transmute(slot_bits(self.omni, 2));
            release(self.omni);
        }
    }
}

// ---------------------------------------------------------------------------
// Non-Windows stub: the app is a Windows app, so the real client only exists
// there; everywhere else the OmniRig method reports an unsupported platform.
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
pub struct Client {
    _unconstructible: (),
}

#[cfg(not(windows))]
impl Client {
    pub fn connect(_rig: u8) -> Result<Self, String> {
        Err("OmniRig is only available on Windows".to_string())
    }

    pub fn tx(&self) -> Result<u8, String> {
        Err("OmniRig is only available on Windows".to_string())
    }

    pub fn status(&self) -> Option<Status> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn status_codes() {
        assert_eq!(Status::from_code(0), Some(Status::NotConfigured));
        assert_eq!(Status::from_code(1), Some(Status::Disabled));
        assert_eq!(Status::from_code(2), Some(Status::PortBusy));
        assert_eq!(Status::from_code(3), Some(Status::NotResponding));
        assert_eq!(Status::from_code(4), Some(Status::Online));
        assert_eq!(Status::from_code(5), None);
    }

    #[test]
    fn status_names() {
        assert_eq!(Status::Online.as_str(), "online");
        assert_eq!(Status::NotResponding.as_str(), "not responding");
    }

    #[test]
    fn rig_slots_match_the_type_library() {
        assert_eq!(super::rig_slot(1), 9); // Get_Rig1
        assert_eq!(super::rig_slot(2), 10); // Get_Rig2
    }

    #[test]
    fn guid_string_is_the_registry_form() {
        // The install check builds the registry path from this.
        assert_eq!(
            super::guid_string(&super::Guid::new(
                0x0839_E8C6,
                0xED30,
                0x4950,
                [0x80, 0x87, 0x96, 0x6F, 0x97, 0x0F, 0x0C, 0xAE]
            )),
            "{0839E8C6-ED30-4950-8087-966F970F0CAE}"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_connect_fails() {
        assert!(super::Client::connect(1).is_err());
    }
}
