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
//!
//! ## Borrowed string views are lifetime-checked
//!
//! [`LcStr`] carries a lifetime parameter (`LcStr<'a>`) and its `ptr`/`len`
//! fields are private: safe code can only build one over a Rust byte slice it
//! actually owns/borrows ([`LcStr::from_bytes`]), or receive one that
//! [`Bridge`]'s query methods hand out already tied to the bridge's borrow
//! (see [`Bridge::current_user`] / [`Bridge::get_message`]). There is no safe
//! way to construct an `LcStr` pointing at memory that isn't provably alive
//! for at least `'a`.
#![allow(
    unsafe_code,
    reason = "this is the one crate in the workspace allowed to call into \
              native code; the workspace-wide `unsafe_code = \"deny\"` lint \
              is deliberately overridden here and nowhere else"
)]

use std::marker::PhantomData;

/// `true` when this crate was built with the `discord-social-sdk` feature
/// (i.e. the native bridge and its `extern "C"` declarations are compiled
/// in). Does **not** imply the SDK was actually vendored/linked — see
/// `build.rs` for the case where the feature is on but linking will fail.
pub const SDK_LINKED: bool = cfg!(feature = "discord-social-sdk");

/// A borrowed, non-owning string view mirroring the C `LcStr` struct's ABI
/// layout (a `(ptr, len)` pair). Valid only for as long as the native side's
/// lifetime contract allows (see `native/discord_bridge.h` — string views
/// live until the next `lc_bridge_run_callbacks` call), which the `'a`
/// lifetime parameter tracks: a value of type `LcStr<'a>` asserts that, if
/// its pointer is non-null, it points to at least `len` initialized,
/// readable bytes that remain valid (not mutated or freed) for the entire
/// lifetime `'a`.
///
/// # Why the fields are private
///
/// If `ptr`/`len` were public, safe code could set them independently (e.g.
/// `LcStr { ptr: 0xdead as *const u8, len: 100, .. }`) and produce a value
/// that violates the invariant above regardless of what `'a` says, since
/// nothing checks that `ptr`/`len` actually describe live memory. Keeping
/// them private means the only ways to build an `LcStr<'a>` are
/// [`LcStr::from_bytes`] (which derives `ptr`/`len` from a real `&'a [u8]`,
/// so the invariant holds by construction) and [`LcStr::NULL`]/
/// [`LcStr::from_raw_parts`] (the latter `unsafe` and crate-private, used
/// only at the FFI boundary in `bridge` where the invariant is upheld by a
/// documented `SAFETY` argument instead).
///
/// ```compile_fail
/// // Safe code cannot construct or mutate an `LcStr` by hand: its fields
/// // are private, so this fails to compile ("field `ptr` of struct
/// // `LcStr` is private").
/// let bad = discord_ffi::LcStr::from_bytes(b"ok");
/// let _ = discord_ffi::LcStr { ptr: bad_ptr(), len: 100, _marker: std::marker::PhantomData };
/// fn bad_ptr() -> *const u8 { std::ptr::null() }
/// ```
///
/// ```compile_fail
/// // A view cannot outlive the bytes it borrows: `v` is dropped at the end
/// // of the inner block while `s` (which borrows from it) is used
/// // afterwards, so the borrow checker rejects this.
/// let s;
/// {
///     let v: Vec<u8> = vec![1, 2, 3];
///     s = discord_ffi::LcStr::from_bytes(&v);
/// } // `v` dropped here
/// let _ = s.as_bytes(); // ERROR: `v` does not live long enough
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LcStr<'a> {
    ptr: *const u8,
    len: usize,
    _marker: PhantomData<&'a [u8]>,
}

impl LcStr<'static> {
    /// An absent/null string view. Usable as an `LcStr<'a>` for any `'a`
    /// (via the ordinary lifetime-shortening every `&'static` value gets),
    /// since it borrows nothing.
    pub const NULL: LcStr<'static> = LcStr {
        ptr: std::ptr::null(),
        len: 0,
        _marker: PhantomData,
    };

    /// An empty (but present, non-null) string view. Equivalent to
    /// `LcStr::from_bytes(&[])` except it never needs a real backing slice.
    pub const fn empty() -> LcStr<'static> {
        // A non-null dangling pointer with len 0: reading zero bytes from
        // any non-null, well-aligned pointer is always sound, and `u8` has
        // alignment 1 so `NonNull::dangling`-style pointers are trivially
        // well-aligned.
        LcStr {
            ptr: std::ptr::NonNull::dangling().as_ptr(),
            len: 0,
            _marker: PhantomData,
        }
    }
}

impl<'a> LcStr<'a> {
    /// Builds a borrowed view over a Rust byte slice. The invariant holds by
    /// construction: `ptr`/`len` are derived directly from `bytes`, which is
    /// guaranteed live for `'a`.
    pub fn from_bytes(bytes: &'a [u8]) -> LcStr<'a> {
        LcStr {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
            _marker: PhantomData,
        }
    }

    /// Builds a view directly from a raw pointer and length, for use only at
    /// the FFI boundary (see `bridge::to_lc_user`/`to_lc_message`).
    ///
    /// # Safety
    /// If `ptr` is non-null, it must point to at least `len` initialized,
    /// readable bytes that remain valid (not mutated or freed) for the
    /// entire lifetime `'a` chosen by the caller.
    #[cfg_attr(
        not(feature = "discord-social-sdk"),
        allow(
            dead_code,
            reason = "only called from the `bridge` module, which is compiled \
                      out entirely without the `discord-social-sdk` feature"
        )
    )]
    pub(crate) unsafe fn from_raw_parts(ptr: *const u8, len: usize) -> LcStr<'a> {
        LcStr {
            ptr,
            len,
            _marker: PhantomData,
        }
    }

    /// `true` for an absent view (null pointer), regardless of `len`.
    pub const fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// Borrows this view's bytes as a `&'a [u8]`. A null pointer (absent) is
    /// treated the same as an empty slice — use [`LcStr::is_null`] first if
    /// the absent/empty distinction matters.
    pub fn as_bytes(&self) -> &'a [u8] {
        if self.ptr.is_null() {
            return &[];
        }
        // SAFETY: every `LcStr<'a>` upholds the invariant documented on the
        // struct: a non-null `ptr` points to at least `len` initialized,
        // readable bytes valid for `'a`. Note this reads `self.ptr`/
        // `self.len` (plain `Copy` fields) to build a slice borrowed for
        // `'a`, not for `&self`'s (shorter) lifetime — that is exactly what
        // the struct's lifetime parameter licenses.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Interprets this view's bytes as UTF-8, replacing invalid sequences
    /// lossily. A null pointer yields an empty (but `Borrowed`) string —
    /// use [`LcStr::to_owned_string`] if the absent/empty distinction
    /// matters.
    pub fn to_str_lossy(&self) -> std::borrow::Cow<'a, str> {
        String::from_utf8_lossy(self.as_bytes())
    }

    /// Copies this view into an owned `String`.
    ///
    /// # Behavior
    /// * A null pointer (`is_null()`) is treated as "absent" and yields
    ///   `None`, regardless of `len`.
    /// * A non-null pointer with `len == 0` yields `Some(String::new())`.
    /// * Invalid UTF-8 is replaced lossily (`String::from_utf8_lossy`), never
    ///   an error: native string data must never crash or reject a caller.
    pub fn to_owned_string(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        Some(self.to_str_lossy().into_owned())
    }
}

// Deliberately no `unsafe impl Send/Sync for LcStr`. `LcStr` holds a raw
// `*const u8`, so it is `!Send`/`!Sync` by default (auto traits require
// every field to be `Send`/`Sync`, and raw pointers are neither). That is
// the correct, conservative default here: an `LcStr` handed out by the
// native bridge is only valid on the bridge's pump thread until the next
// `lc_bridge_run_callbacks` call (see `native/discord_bridge.h`'s
// "Threading" section), so letting it cross threads would be unsound
// regardless of the lifetime parameter, which only tracks *how long*, not
// *from which thread*, the view may be read.

/// Mirrors the C `LcUser` struct. Carries the same lifetime as the
/// [`LcStr`] views inside it (see [`LcStr`]'s docs for what that lifetime
/// guarantees).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LcUser<'a> {
    pub id: u64,
    pub username: LcStr<'a>,
    pub global_name: LcStr<'a>,
    pub avatar_url: LcStr<'a>,
    pub is_provisional: u8,
}

/// Mirrors the C `LcMessage` struct. Carries the same lifetime as the
/// [`LcStr`] view inside it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LcMessage<'a> {
    pub id: u64,
    pub channel_id: u64,
    pub author_id: u64,
    pub sent_at_ms: i64,
    /// `0` means "never edited" (see `native/discord_bridge.h`).
    pub edited_at_ms: i64,
    pub content: LcStr<'a>,
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
/// Carries no `LcStr`, so it needs no lifetime parameter and is freely
/// `Send`/`Sync` (all fields are plain integers).
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
///
/// The `extern "C"` declarations here use lifetime-free `Raw*` mirror
/// structs (exactly matching the C ABI layout) rather than the public,
/// lifetime-carrying [`LcStr`]/[`LcUser`]/[`LcMessage`] types: an `extern
/// "C"` out-parameter has no meaningful Rust lifetime of its own (the
/// callee is C++, which knows nothing about Rust borrows), so the safe
/// [`Bridge`] methods below fill a `Raw*` value, then attach whatever
/// lifetime is actually appropriate (tied to `&self`) when converting it
/// into the public type — see `bridge::to_lc_user`/`bridge::to_lc_message`.
#[cfg(feature = "discord-social-sdk")]
mod bridge {
    use super::{LcEvent, LcMessage, LcStr, LcUser};
    use std::os::raw::{c_char, c_void};

    pub type LcEventCb = unsafe extern "C" fn(userdata: *mut c_void, ev: *const LcEvent);

    /// Lifetime-free ABI mirror of the C `LcStr` — used only to cross the
    /// `extern "C"` boundary itself; see the module docs.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct RawLcStr {
        pub ptr: *const u8,
        pub len: usize,
    }

    impl RawLcStr {
        const NULL: RawLcStr = RawLcStr {
            ptr: std::ptr::null(),
            len: 0,
        };
    }

    /// Lifetime-free ABI mirror of the C `LcUser`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct RawLcUser {
        pub id: u64,
        pub username: RawLcStr,
        pub global_name: RawLcStr,
        pub avatar_url: RawLcStr,
        pub is_provisional: u8,
    }

    impl RawLcUser {
        pub(crate) const fn empty() -> Self {
            RawLcUser {
                id: 0,
                username: RawLcStr::NULL,
                global_name: RawLcStr::NULL,
                avatar_url: RawLcStr::NULL,
                is_provisional: 0,
            }
        }
    }

    /// Lifetime-free ABI mirror of the C `LcMessage`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct RawLcMessage {
        pub id: u64,
        pub channel_id: u64,
        pub author_id: u64,
        pub sent_at_ms: i64,
        pub edited_at_ms: i64,
        pub content: RawLcStr,
    }

    impl RawLcMessage {
        pub(crate) const fn empty() -> Self {
            RawLcMessage {
                id: 0,
                channel_id: 0,
                author_id: 0,
                sent_at_ms: 0,
                edited_at_ms: 0,
                content: RawLcStr::NULL,
            }
        }
    }

    /// Converts a freshly-filled [`RawLcUser`] into the public, borrowing
    /// [`LcUser<'a>`], attaching whatever lifetime `'a` the caller picks.
    ///
    /// # Safety
    /// The `RawLcStr` fields of `raw`, if non-null, must point to bytes that
    /// remain valid, readable, and unmutated for the entire lifetime `'a`
    /// the caller chooses for the result.
    pub(crate) unsafe fn to_lc_user<'a>(raw: RawLcUser) -> LcUser<'a> {
        // SAFETY: forwarded from this function's own contract, per field.
        unsafe {
            LcUser {
                id: raw.id,
                username: LcStr::from_raw_parts(raw.username.ptr, raw.username.len),
                global_name: LcStr::from_raw_parts(raw.global_name.ptr, raw.global_name.len),
                avatar_url: LcStr::from_raw_parts(raw.avatar_url.ptr, raw.avatar_url.len),
                is_provisional: raw.is_provisional,
            }
        }
    }

    /// Converts a freshly-filled [`RawLcMessage`] into the public, borrowing
    /// [`LcMessage<'a>`]. See [`to_lc_user`]'s safety contract; identical,
    /// applied to `raw.content`.
    ///
    /// # Safety
    /// See [`to_lc_user`].
    pub(crate) unsafe fn to_lc_message<'a>(raw: RawLcMessage) -> LcMessage<'a> {
        // SAFETY: forwarded from this function's own contract.
        unsafe {
            LcMessage {
                id: raw.id,
                channel_id: raw.channel_id,
                author_id: raw.author_id,
                sent_at_ms: raw.sent_at_ms,
                edited_at_ms: raw.edited_at_ms,
                content: LcStr::from_raw_parts(raw.content.ptr, raw.content.len),
            }
        }
    }

    // No `#[link(...)]` attribute here: `cargo:rustc-link-lib` in build.rs
    // supplies the link directive once the SDK is actually vendored. This
    // keeps the declaration itself buildable even when linking is not yet
    // wired up.
    //
    // `pub(crate)`, not `pub`: these raw extern functions (and the `Raw*`
    // structs above) are an implementation detail of `Bridge`; nothing
    // outside this crate should call them directly, since doing so would
    // bypass the lifetime tracking `Bridge`'s safe methods provide.
    extern "C" {
        pub(crate) fn lc_bridge_create(application_id: u64) -> *mut c_void;
        pub(crate) fn lc_bridge_destroy(bridge: *mut c_void);
        pub(crate) fn lc_bridge_run_callbacks(bridge: *mut c_void);
        pub(crate) fn lc_bridge_set_event_callback(
            bridge: *mut c_void,
            cb: Option<LcEventCb>,
            userdata: *mut c_void,
        );
        pub(crate) fn lc_bridge_connect(bridge: *mut c_void);
        pub(crate) fn lc_bridge_disconnect(bridge: *mut c_void);
        pub(crate) fn lc_bridge_update_token(
            bridge: *mut c_void,
            token_type: i32,
            token: *const c_char,
            len: usize,
        );
        pub(crate) fn lc_bridge_current_user(bridge: *mut c_void, out: *mut RawLcUser) -> i32;
        pub(crate) fn lc_bridge_get_message(
            bridge: *mut c_void,
            id: u64,
            out: *mut RawLcMessage,
        ) -> i32;
        pub(crate) fn lc_bridge_send_user_message(
            bridge: *mut c_void,
            recipient: u64,
            content: *const c_char,
            len: usize,
            request_id: u64,
        );
    }
}

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
///
/// # Why query methods take `&mut self`
/// The native bridge writes every query's strings into per-bridge scratch
/// buffers (`scratch_username`, `scratch_content`, ...), so *any* later
/// query, not only [`Bridge::run_callbacks`], can overwrite the bytes an
/// earlier [`LcUser`]/[`LcMessage`] points at. [`Bridge::current_user`] and
/// [`Bridge::get_message`] therefore borrow the bridge mutably: while a
/// result is alive, the borrow checker rejects another query, a pump
/// (`run_callbacks` also takes `&mut self`) and dropping the bridge. Copy
/// out what you need (`user.username.to_owned_string()`) before the next
/// call, as `discord-adapter`'s `convert` module does.
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

    /// Reads the current user from the bridge's local cache, if known. The
    /// returned [`LcUser`] borrows the bridge mutably; see "Why query
    /// methods take `&mut self`" on [`Bridge`].
    pub fn current_user(&mut self) -> Option<LcUser<'_>> {
        let mut raw = bridge::RawLcUser::empty();
        // SAFETY: `self.ptr` is valid; `&mut raw` is a valid, writable
        // `RawLcUser` for the duration of this call, per
        // `lc_bridge_current_user`'s contract.
        let rc = unsafe { bridge::lc_bridge_current_user(self.ptr, &mut raw) };
        if rc != 0 {
            return None;
        }
        // SAFETY: the string views written into `raw` point into the
        // bridge's scratch buffers, which stay unchanged until the next
        // query, `lc_bridge_run_callbacks` or destruction. Every one of
        // those needs `&mut self` or ownership, and the `LcUser<'_>`
        // returned here holds the `&mut self` borrow, so none can run while
        // it is alive.
        Some(unsafe { bridge::to_lc_user(raw) })
    }

    /// Reads a message by id from the bridge's local cache, if known. See
    /// [`Bridge::current_user`] for why the result borrows `&mut self`.
    pub fn get_message(&mut self, id: u64) -> Option<LcMessage<'_>> {
        let mut raw = bridge::RawLcMessage::empty();
        // SAFETY: as above, for `lc_bridge_get_message`.
        let rc = unsafe { bridge::lc_bridge_get_message(self.ptr, id, &mut raw) };
        if rc != 0 {
            return None;
        }
        // SAFETY: see `current_user`'s SAFETY comment; identical reasoning.
        Some(unsafe { bridge::to_lc_message(raw) })
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
        // `LcStr::empty()` is exactly this case: a non-null, dangling
        // pointer with `len == 0`, as a real empty (but present) native
        // string would be represented. Reading zero bytes from any non-null
        // pointer is always sound.
        let s = LcStr::empty();
        assert!(!s.is_null());
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
    fn as_bytes_borrows_the_backing_slice() {
        let buf = b"litecord".to_vec();
        let s = LcStr::from_bytes(&buf);
        assert_eq!(s.as_bytes(), &buf[..]);
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

    /// ABI regression test: `LcStr` must stay exactly a `(ptr, len)` pair —
    /// the `PhantomData` marker that carries its lifetime must add no size
    /// or alignment, since native code writes into memory shaped like this
    /// struct without knowing anything about Rust lifetimes.
    #[test]
    fn lc_str_layout_matches_c_abi() {
        use std::mem::{align_of, size_of};
        assert_eq!(size_of::<LcStr<'static>>(), 2 * size_of::<usize>());
        assert_eq!(align_of::<LcStr<'static>>(), align_of::<usize>());
    }

    /// The `PhantomData<&'a [u8]>` marker used to give `LcStr`/`LcUser`/
    /// `LcMessage` their lifetime parameter is zero-sized, so it never
    /// contributes to any of those `#[repr(C)]` types' layout.
    #[test]
    fn phantom_lifetime_marker_is_zero_sized() {
        assert_eq!(std::mem::size_of::<PhantomData<&'static [u8]>>(), 0);
    }
}
