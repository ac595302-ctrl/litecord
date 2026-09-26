//! `discord-ffi`: the C ABI boundary to the native Discord Social SDK.
//!
//! This is the **only** crate in the workspace where a native Discord Social
//! SDK symbol may appear, and the **only** crate allowed `unsafe` code (the
//! workspace lints deny `unsafe_code` everywhere else). Everything above this
//! crate — starting with `discord-adapter` — talks only in plain Rust types
//! ([`LcUser`], [`LcMessage`], [`LcEvent`], ...) and never sees an SDK object.
//!
//! Nothing here links a native library unless the `discord-social-sdk`
//! feature is enabled *and* the SDK has been vendored (see `README.md` and
//! `build.rs`). With the feature off, this crate is plain, safe, always-buildable
//! Rust: [`SDK_LINKED`] is `false`, the `#[repr(C)]` mirror types and
//! [`LcStr`] still exist (so `discord-adapter` can depend on this crate
//! unconditionally and its `convert.rs` module can be always-compiled), but
//! no `extern "C"` declarations and no [`Bridge`] type are compiled in.
#![allow(
    unsafe_code,
    reason = "this is the one crate in the workspace allowed to call into \
              native code; the workspace-wide `unsafe_code = \"deny\"` lint \
              is deliberately overridden here and nowhere else"
)]

/// `true` when this crate was built with the `discord-social-sdk` feature
/// (i.e. the native bridge and its `extern "C"` declarations are compiled
/// in). Does **not** imply the SDK was actually vendored/linked — see
/// `build.rs` for the case where the feature is on but linking will fail.
pub const SDK_LINKED: bool = cfg!(feature = "discord-social-sdk");

/// A borrowed, non-owning string view mirroring the C `LcStr` struct. Valid
/// only for as long as the native side's lifetime contract allows (see
/// `native/discord_bridge.h` — string views live until the next
/// `lc_bridge_run_callbacks` call).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LcStr {
    pub ptr: *const u8,
    pub len: usize,
}

impl LcStr {
    /// An absent/null string view.
    pub const NULL: LcStr = LcStr {
        ptr: std::ptr::null(),
        len: 0,
    };

    /// Builds a borrowed view over a Rust byte slice, for tests and for
    /// constructing values without going through the FFI boundary.
    pub fn from_bytes(bytes: &[u8]) -> LcStr {
        LcStr {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        }
    }

    /// Copies this view into an owned `String`.
    ///
    /// This is a *safe* function — deliberately so, since `discord-ffi` is
    /// the one crate in the workspace allowed `unsafe` code, and every other
    /// crate (including `discord-adapter`, whose `convert.rs` calls this
    /// unconditionally) must be able to call it without an `unsafe` block of
    /// its own. The `unsafe` needed to read through the raw pointer is done
    /// here, once, under the SAFETY contract below.
    ///
    /// # Behavior
    /// * A null pointer (`ptr.is_null()`) is treated as "absent" and yields
    ///   `None`, regardless of `len`.
    /// * A non-null pointer with `len == 0` yields `Some(String::new())`.
    /// * Invalid UTF-8 is replaced lossily (`String::from_utf8_lossy`), never
    ///   an error: native string data must never crash or reject a caller.
    ///
    /// # Safety contract relied on internally
    /// Every `LcStr` value that reaches this method must, if its pointer is
    /// non-null, point to at least `len` initialized, readable bytes that
    /// remain valid (not mutated or freed) for the duration of this call.
    /// `LcStr` values built with [`LcStr::from_bytes`] or [`LcStr::NULL`]
    /// always satisfy this by construction. `LcStr` values that came from the
    /// native bridge satisfy it only before the next `lc_bridge_run_callbacks`
    /// call on the same bridge — see `native/discord_bridge.h`'s "Ownership &
    /// lifetime" section — which is why `Bridge`'s query methods hand out
    /// fresh `LcUser`/`LcMessage` values rather than let callers cache them.
    pub fn to_owned_string(&self) -> Option<String> {
        if self.ptr.is_null() {
            return None;
        }
        // SAFETY: see the safety contract documented above; every `LcStr`
        // this crate hands out (from `Bridge`'s query methods, or built via
        // `LcStr::from_bytes`/`NULL`) upholds it.
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        Some(String::from_utf8_lossy(slice).into_owned())
    }
}

// SAFETY: `LcStr` is a plain (ptr, len) pair with no interior mutability; it
// carries no thread affinity of its own (any affinity comes from the pointee,
// which is documented on the methods that dereference it).
unsafe impl Send for LcStr {}
unsafe impl Sync for LcStr {}

/// Mirrors the C `LcUser` struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LcUser {
    pub id: u64,
    pub username: LcStr,
    pub global_name: LcStr,
    pub avatar_url: LcStr,
    pub is_provisional: u8,
}

/// Mirrors the C `LcMessage` struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LcMessage {
    pub id: u64,
    pub channel_id: u64,
    pub author_id: u64,
    pub sent_at_ms: i64,
    /// `0` means "never edited" (see `native/discord_bridge.h`).
    pub edited_at_ms: i64,
    pub content: LcStr,
}

/// Event kinds. Mirrors the `LC_EVENT_*` C constants.
pub mod event_kind {
    pub const STATUS_CHANGED: u32 = 1;
    pub const MESSAGE_CREATED: u32 = 2;
    pub const MESSAGE_UPDATED: u32 = 3;
    pub const MESSAGE_DELETED: u32 = 4;
    pub const RELATIONSHIP_CHANGED: u32 = 5;
    pub const USER_UPDATED: u32 = 6;
    pub const LOBBY_UPDATED: u32 = 7;
    pub const VOICE_PARTICIPANT_CHANGED: u32 = 8;
    /// Completion of an async `lc_bridge_send_user_message` call.
    pub const SEND_COMPLETED: u32 = 9;
}

/// Token type accepted by `lc_bridge_update_token`.
pub mod token_type {
    pub const ACCESS: i32 = 0;
    pub const REFRESH: i32 = 1;
}

/// Mirrors the C `LcEvent` struct. Ids only — the adapter resolves ids into
/// objects via the query functions, per the V1 design. See
/// `native/discord_bridge.h` for what `id_a`/`id_b`/`status` mean per `kind`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LcEvent {
    pub kind: u32,
    pub id_a: u64,
    pub id_b: u64,
    pub status: i32,
}

/// Declarations of the native bridge functions and the safe [`Bridge`]
/// wrapper around them. Compiled only when the `discord-social-sdk` feature
/// is enabled; linking additionally requires the SDK to be vendored and
/// `LITECORD_DISCORD_SDK_DIR` set (see `build.rs`).
#[cfg(feature = "discord-social-sdk")]
mod bridge {
    use super::{LcEvent, LcMessage, LcUser};
    use std::os::raw::{c_char, c_void};

    pub type LcEventCb = unsafe extern "C" fn(userdata: *mut c_void, ev: *const LcEvent);

    // No `#[link(...)]` attribute here: `cargo:rustc-link-lib` in build.rs
    // supplies the link directive once the SDK is actually vendored. This
    // keeps the declaration itself buildable even when linking is not yet
    // wired up.
    extern "C" {
        pub fn lc_bridge_create(application_id: u64) -> *mut c_void;
        pub fn lc_bridge_destroy(bridge: *mut c_void);
        pub fn lc_bridge_run_callbacks(bridge: *mut c_void);
        pub fn lc_bridge_set_event_callback(
            bridge: *mut c_void,
            cb: Option<LcEventCb>,
            userdata: *mut c_void,
        );
        pub fn lc_bridge_connect(bridge: *mut c_void);
        pub fn lc_bridge_disconnect(bridge: *mut c_void);
        pub fn lc_bridge_update_token(
            bridge: *mut c_void,
            token_type: i32,
            token: *const c_char,
            len: usize,
        );
        pub fn lc_bridge_current_user(bridge: *mut c_void, out: *mut LcUser) -> i32;
        pub fn lc_bridge_get_message(bridge: *mut c_void, id: u64, out: *mut LcMessage) -> i32;
        pub fn lc_bridge_send_user_message(
            bridge: *mut c_void,
            recipient: u64,
            content: *const c_char,
            len: usize,
            request_id: u64,
        );
    }
}

#[cfg(feature = "discord-social-sdk")]
pub use bridge::*;

/// A thin, safe wrapper owning one native bridge handle.
///
/// # Thread affinity
/// The native bridge requires [`Bridge::run_callbacks`] to be pumped
/// periodically from **one consistent thread** (see
/// `native/discord_bridge.h`'s "Threading" section); string views produced by
/// query methods are only valid until the next such pump, on that same
/// thread. `Bridge` is therefore deliberately `!Sync`: sharing a `&Bridge`
/// across threads would let two threads race to pump callbacks or to read
/// string views concurrently with a pump invalidating them. It is `Send` so
/// ownership can move to whichever thread will own the pump loop, and the
/// SDK itself accepts `Connect`/`Disconnect`/`SendUserMessage` calls from
/// other threads by internally marshaling them.
#[cfg(feature = "discord-social-sdk")]
use std::os::raw::c_char;

#[cfg(feature = "discord-social-sdk")]
#[derive(Debug)]
pub struct Bridge {
    ptr: *mut std::os::raw::c_void,
    // Force `!Sync` (raw pointers are already `!Sync`, but this documents
    // the intent explicitly and survives future field changes).
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

// SAFETY: the underlying native handle has no thread-affinity requirement for
// simply being *moved*; only pumping callbacks and reading string views must
// stay on one thread, which `Bridge`'s `!Sync` (via `PhantomData<Cell<_>>`)
// enforces at compile time by preventing concurrent `&Bridge` access from
// multiple threads.
#[cfg(feature = "discord-social-sdk")]
unsafe impl Send for Bridge {}

#[cfg(feature = "discord-social-sdk")]
impl Bridge {
    /// Creates a new bridge for the given Discord application id. Returns
    /// `None` if the native side failed to initialize.
    pub fn new(application_id: u64) -> Option<Self> {
        // SAFETY: `lc_bridge_create` is documented (native/discord_bridge.h)
        // to either return a valid, owned pointer or NULL; no preconditions
        // on the caller beyond that the SDK library is correctly linked.
        let ptr = unsafe { bridge::lc_bridge_create(application_id) };
        if ptr.is_null() {
            None
        } else {
            Some(Self {
                ptr,
                _not_sync: std::marker::PhantomData,
            })
        }
    }

    /// Pumps the native event loop, delivering any queued events to `on_event`.
    /// Must be called periodically from the same thread every time (see
    /// struct docs).
    pub fn run_callbacks(&mut self, mut on_event: impl FnMut(LcEvent)) {
        // A small bit of C-callable glue: we stash a fat closure pointer
        // behind a thin `*mut c_void` using a boxed trait object, matching
        // the pattern documented in native/discord_bridge.h (`userdata` is
        // passed through unchanged).
        struct Ctx<'a> {
            f: &'a mut dyn FnMut(LcEvent),
        }

        unsafe extern "C" fn trampoline(userdata: *mut std::os::raw::c_void, ev: *const LcEvent) {
            // SAFETY: `userdata` was set to a valid `*mut Ctx` immediately
            // before this call, for the duration of this call only, and
            // `ev` is documented by native/discord_bridge.h to be a valid,
            // readable `LcEvent` for the duration of the callback.
            unsafe {
                if userdata.is_null() || ev.is_null() {
                    return;
                }
                let ctx = &mut *(userdata as *mut Ctx<'_>);
                (ctx.f)(*ev);
            }
        }

        let mut ctx = Ctx { f: &mut on_event };
        let ctx_ptr = &mut ctx as *mut Ctx<'_> as *mut std::os::raw::c_void;

        // SAFETY: `self.ptr` is a valid bridge handle for the lifetime of
        // `self`; `trampoline` and `ctx_ptr` satisfy
        // `lc_bridge_set_event_callback`'s contract (callback invoked
        // synchronously and only during `lc_bridge_run_callbacks`, on this
        // thread), so `ctx` outliving the two calls below is sufficient.
        unsafe {
            bridge::lc_bridge_set_event_callback(self.ptr, Some(trampoline), ctx_ptr);
            bridge::lc_bridge_run_callbacks(self.ptr);
            // Clear the callback so `ctx` (a local about to go out of scope)
            // can never be invoked again after this function returns.
            bridge::lc_bridge_set_event_callback(self.ptr, None, std::ptr::null_mut());
        }
    }

    pub fn connect(&self) {
        // SAFETY: `self.ptr` is a valid bridge handle.
        unsafe { bridge::lc_bridge_connect(self.ptr) }
    }

    pub fn disconnect(&self) {
        // SAFETY: `self.ptr` is a valid bridge handle.
        unsafe { bridge::lc_bridge_disconnect(self.ptr) }
    }

    pub fn update_token(&self, token_type: i32, token: &str) {
        // SAFETY: `self.ptr` is valid; `token.as_ptr()` is valid for
        // `token.len()` bytes for the duration of this call, matching
        // `lc_bridge_update_token`'s contract that the bytes are copied
        // internally before returning.
        unsafe {
            bridge::lc_bridge_update_token(
                self.ptr,
                token_type,
                token.as_ptr() as *const c_char,
                token.len(),
            )
        }
    }

    /// Reads the current user from the bridge's local cache, if known.
    pub fn current_user(&self) -> Option<LcUser> {
        let mut out = LcUser {
            id: 0,
            username: LcStr::NULL,
            global_name: LcStr::NULL,
            avatar_url: LcStr::NULL,
            is_provisional: 0,
        };
        // SAFETY: `self.ptr` is valid; `&mut out` is a valid, writable
        // `LcUser` for the duration of this call, per
        // `lc_bridge_current_user`'s contract.
        let rc = unsafe { bridge::lc_bridge_current_user(self.ptr, &mut out) };
        if rc == 0 {
            Some(out)
        } else {
            None
        }
    }

    /// Reads a message by id from the bridge's local cache, if known.
    pub fn get_message(&self, id: u64) -> Option<LcMessage> {
        let mut out = LcMessage {
            id: 0,
            channel_id: 0,
            author_id: 0,
            sent_at_ms: 0,
            edited_at_ms: 0,
            content: LcStr::NULL,
        };
        // SAFETY: as above, for `lc_bridge_get_message`.
        let rc = unsafe { bridge::lc_bridge_get_message(self.ptr, id, &mut out) };
        if rc == 0 {
            Some(out)
        } else {
            None
        }
    }

    /// Sends a DM. Asynchronous: completion arrives as a later
    /// `LC_EVENT_SEND_COMPLETED` event carrying `request_id` back in `id_a`.
    pub fn send_user_message(&self, recipient: u64, content: &str, request_id: u64) {
        // SAFETY: `self.ptr` is valid; `content.as_ptr()` is valid for
        // `content.len()` bytes for the duration of this call, matching
        // `lc_bridge_send_user_message`'s contract that the bytes are copied
        // internally before returning.
        unsafe {
            bridge::lc_bridge_send_user_message(
                self.ptr,
                recipient,
                content.as_ptr() as *const c_char,
                content.len(),
                request_id,
            )
        }
    }
}

#[cfg(feature = "discord-social-sdk")]
impl Drop for Bridge {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` is a valid bridge handle owned solely by this
        // `Bridge`, and `Drop` runs at most once.
        unsafe { bridge::lc_bridge_destroy(self.ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_linked_matches_feature() {
        assert_eq!(SDK_LINKED, cfg!(feature = "discord-social-sdk"));
    }

    #[test]
    fn null_ptr_is_absent() {
        let s = LcStr::NULL;
        assert_eq!(s.to_owned_string(), None);
    }

    #[test]
    fn valid_utf8_roundtrips() {
        let buf = "hello, litecord".as_bytes();
        let s = LcStr::from_bytes(buf);
        assert_eq!(s.to_owned_string(), Some("hello, litecord".to_string()));
    }

    #[test]
    fn empty_non_null_is_empty_string() {
        let buf: &[u8] = &[];
        // Use a non-null dangling pointer with len 0, as a real empty (but
        // present) native string would be represented. Reading zero bytes
        // from any non-null pointer is always sound.
        let s = LcStr {
            ptr: buf.as_ptr().wrapping_add(1),
            len: 0,
        };
        assert_eq!(s.to_owned_string(), Some(String::new()));
    }

    #[test]
    fn invalid_utf8_is_replaced_lossily() {
        let buf: &[u8] = &[0x68, 0x69, 0xff, 0xfe];
        let s = LcStr::from_bytes(buf);
        let owned = s.to_owned_string().unwrap();
        assert!(owned.starts_with("hi"));
        assert!(owned.contains('\u{FFFD}'));
    }

    #[test]
    fn event_kind_constants_are_distinct() {
        let kinds = [
            event_kind::STATUS_CHANGED,
            event_kind::MESSAGE_CREATED,
            event_kind::MESSAGE_UPDATED,
            event_kind::MESSAGE_DELETED,
            event_kind::RELATIONSHIP_CHANGED,
            event_kind::USER_UPDATED,
            event_kind::LOBBY_UPDATED,
            event_kind::VOICE_PARTICIPANT_CHANGED,
            event_kind::SEND_COMPLETED,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }
}
