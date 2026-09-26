// discord_bridge.h — C ABI boundary between Rust (discord-ffi) and the
// official Discord Social SDK C++ headers (discordpp.h).
//
// NOT compiled in CI. Written against Discord Social SDK 1.x headers; the
// implementation (discord_bridge.cpp) must be verified against whatever SDK
// version is actually vendored into `vendor/discord-sdk/` before this is
// trusted. See crates/discord-ffi/README.md.
//
// ---------------------------------------------------------------------------
// Design (V1 hackathon plan §"native bridge"):
//
//   * The bridge exposes *ids only* for events (LcEvent). It deliberately does
//     not hand the Rust side full SDK objects: the adapter (discord-adapter)
//     resolves ids into Litecord domain objects via the query functions below,
//     on its own schedule, keeping the SDK object model entirely on the C++
//     side of this boundary.
//   * All Discord Social SDK types stay inside discord_bridge.cpp. Nothing in
//     this header names an SDK type; everything is plain C.
//
// Ownership & lifetime
// ---------------------------------------------------------------------------
//   * `LcBridge*` is an opaque handle. It is created by `lc_bridge_create` and
//     MUST be destroyed exactly once with `lc_bridge_destroy`. Using a handle
//     after destroying it is undefined behavior.
//   * String views (`LcStr`) returned by the query functions below (via
//     `LcUser`/`LcMessage` out-params) point into buffers owned by the bridge.
//     They are valid ONLY until the next call to `lc_bridge_run_callbacks` on
//     the same bridge, or until the bridge is destroyed, whichever comes
//     first. Callers must copy any string they need to keep (discord-ffi's
//     `LcStr::to_owned_string` does this).
//   * `lc_bridge_send_user_message` is asynchronous: it returns immediately,
//     and completion is reported later as an `LcEvent` of kind
//     `LC_EVENT_SEND_COMPLETED` with `id_a` set to the `request_id` the caller
//     passed in, `id_b` set to the resulting message id (0 on failure), and
//     `status` set to a nonzero SDK error code on failure (0 on success).
//
// Threading
// ---------------------------------------------------------------------------
//   * `lc_bridge_run_callbacks` MUST be pumped periodically (e.g. every
//     10-50ms) from exactly one thread — the Discord Social SDK's own
//     callback/event loop is not internally thread-safe and everything below
//     is only as thread-safe as that pump discipline. That thread is also the
//     only thread from which query functions and the string views they
//     produce may safely be used, since `lc_bridge_run_callbacks` invalidates
//     previously-returned string views.
//   * `lc_bridge_set_event_callback` registers a callback invoked synchronously
//     from within `lc_bridge_run_callbacks`, on that same thread. The callback
//     must not block and must not call back into the bridge reentrantly.
//   * `lc_bridge_connect`, `lc_bridge_disconnect`, `lc_bridge_update_token`,
//     and `lc_bridge_send_user_message` may be called from any thread; the SDK
//     internally marshals the request onto its own thread. Their *effects*
//     (state changes, message delivery) still only become visible through
//     events delivered during `lc_bridge_run_callbacks`.
//
// Error handling
// ---------------------------------------------------------------------------
//   * Query functions return 0 on success, nonzero if the requested object is
//     not currently known to the SDK's local cache. They never allocate on
//     the Rust side and never throw across the ABI boundary (all C++
//     exceptions are caught internally by discord_bridge.cpp).

#ifndef LITECORD_DISCORD_BRIDGE_H
#define LITECORD_DISCORD_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LcBridge LcBridge;

// A borrowed, non-owning string view. See "Ownership & lifetime" above.
typedef struct LcStr {
    const char* ptr; // may be NULL for "absent"
    size_t len;
} LcStr;

typedef struct LcUser {
    uint64_t id;
    LcStr username;
    LcStr global_name;  // ptr == NULL if unset
    LcStr avatar_url;   // ptr == NULL if unset
    uint8_t is_provisional;
} LcUser;

typedef struct LcMessage {
    uint64_t id;
    uint64_t channel_id;
    uint64_t author_id;
    int64_t sent_at_ms;
    int64_t edited_at_ms; // 0 means "never edited"
    LcStr content;
} LcMessage;

// LcEvent kinds. Carries ids only — the adapter resolves ids into full
// objects via the query functions, per the V1 design.
enum {
    LC_EVENT_STATUS_CHANGED = 1,
    LC_EVENT_MESSAGE_CREATED = 2,
    LC_EVENT_MESSAGE_UPDATED = 3,
    LC_EVENT_MESSAGE_DELETED = 4,
    LC_EVENT_RELATIONSHIP_CHANGED = 5,
    LC_EVENT_USER_UPDATED = 6,
    LC_EVENT_LOBBY_UPDATED = 7,
    LC_EVENT_VOICE_PARTICIPANT_CHANGED = 8,
    // Completion of an async lc_bridge_send_user_message call.
    LC_EVENT_SEND_COMPLETED = 9,
};

typedef struct LcEvent {
    uint32_t kind; // one of LC_EVENT_*
    uint64_t id_a; // meaning depends on `kind` (see field docs below)
    uint64_t id_b; // meaning depends on `kind`
    int32_t status; // SDK status/error code; 0 == ok. Meaningful for
                     // STATUS_CHANGED (session status) and SEND_COMPLETED
                     // (send error code).
} LcEvent;

// Field meanings by kind:
//   STATUS_CHANGED:            id_a/id_b unused; status = session status code.
//   MESSAGE_CREATED/UPDATED:   id_a = message id, id_b = channel id.
//   MESSAGE_DELETED:           id_a = message id, id_b = channel id.
//   RELATIONSHIP_CHANGED:      id_a = user id.
//   USER_UPDATED:              id_a = user id.
//   LOBBY_UPDATED:             id_a = lobby id.
//   VOICE_PARTICIPANT_CHANGED: id_a = lobby id, id_b = user id.
//   SEND_COMPLETED:            id_a = request_id (caller-supplied),
//                              id_b = new message id (0 on failure),
//                              status = 0 on success, nonzero SDK error code
//                              on failure.

typedef void (*lc_event_cb)(void* userdata, const LcEvent* ev);

// Token types accepted by lc_bridge_update_token.
enum {
    LC_TOKEN_ACCESS = 0,
    LC_TOKEN_REFRESH = 1,
};

// Creates a bridge bound to the given Discord application id. Returns NULL on
// failure (e.g. SDK initialization failure). The returned pointer must be
// destroyed with lc_bridge_destroy.
LcBridge* lc_bridge_create(uint64_t application_id);

// Destroys a bridge created by lc_bridge_create. `bridge` may be NULL (no-op).
// Must be called from the same thread that pumps lc_bridge_run_callbacks.
void lc_bridge_destroy(LcBridge* bridge);

// Pumps the SDK's internal event loop and delivers any queued events via the
// callback registered with lc_bridge_set_event_callback. Must be called
// periodically (see "Threading" above) from one consistent thread.
void lc_bridge_run_callbacks(LcBridge* bridge);

// Registers (replacing any previous) the callback invoked during
// lc_bridge_run_callbacks for each SDK event. `userdata` is passed through
// unchanged; the bridge does not take ownership of it.
void lc_bridge_set_event_callback(LcBridge* bridge, lc_event_cb cb, void* userdata);

// Begins connecting to Discord. Asynchronous; progress and completion are
// reported as LC_EVENT_STATUS_CHANGED events.
void lc_bridge_connect(LcBridge* bridge);

// Disconnects. Asynchronous; reported as LC_EVENT_STATUS_CHANGED.
void lc_bridge_disconnect(LcBridge* bridge);

// Supplies or refreshes an OAuth2 token. `token` need not be NUL-terminated;
// exactly `len` bytes are read and copied internally before this call
// returns.
void lc_bridge_update_token(LcBridge* bridge, int32_t token_type, const char* token, size_t len);

// Fills `out` with the currently authenticated user. Returns 0 on success,
// nonzero if no current user is known yet (e.g. not connected). String views
// in `out` are valid until the next lc_bridge_run_callbacks call.
int32_t lc_bridge_current_user(LcBridge* bridge, LcUser* out);

// Fills `out` with a message by id from the SDK's local cache. Returns 0 on
// success, nonzero if the message is not cached (the caller should treat this
// as "needs hydration", not as a hard error). String views in `out` are valid
// until the next lc_bridge_run_callbacks call.
int32_t lc_bridge_get_message(LcBridge* bridge, uint64_t id, LcMessage* out);

// Sends a direct message to `recipient`. `content` need not be
// NUL-terminated; exactly `len` bytes are read and copied internally before
// this call returns. Asynchronous: completion is reported as an
// LC_EVENT_SEND_COMPLETED event carrying the given `request_id` back in
// `id_a` so the caller can correlate it.
void lc_bridge_send_user_message(
    LcBridge* bridge,
    uint64_t recipient,
    const char* content,
    size_t len,
    uint64_t request_id
);

#ifdef __cplusplus
} // extern "C"
#endif

#endif // LITECORD_DISCORD_BRIDGE_H
