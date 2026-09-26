// discord_bridge.cpp — implementation sketch of the C ABI declared in
// discord_bridge.h, against the official Discord Social SDK C++ API
// (discordpp.h).
//
// *** NOT compiled in CI. ***
// This file is only compiled when the `discord-social-sdk` feature is
// enabled AND `LITECORD_DISCORD_SDK_DIR` points at a real vendored copy of
// the SDK (see crates/discord-ffi/build.rs and README.md). Neither is true on
// CI, by design: the SDK is proprietary and is never redistributed with this
// repository (see crates/discord-ffi/README.md and vendor/discord-sdk/README.md).
//
// It is written against Discord Social SDK 1.x headers (discordpp::Client,
// SetStatusChangedCallback, SetMessageCreatedCallback, GetMessageHandle,
// GetCurrentUser, SendUserMessage, RunCallbacks) as documented at the time
// this was written. It has NOT been compiled or run against a real SDK
// checkout. Before relying on it:
//   1. Vendor the actual SDK into vendor/discord-sdk/ (see README.md).
//   2. Diff this file's API usage against the vendored discordpp.h — method
//      names, signatures and callback shapes may have moved between SDK
//      releases.
//   3. Build with LITECORD_DISCORD_SDK_DIR set and fix whatever the compiler
//      flags.
//   4. Exercise it against a real Discord application before trusting it in
//      `SocialSdkBackend` (crates/discord-adapter/src/social_sdk.rs), which
//      today is a deliberate skeleton that never fakes data.
//
// This sketch intentionally keeps all Social SDK types on this side of the
// boundary: discord_bridge.h exposes ids and plain structs only.

#include "discord_bridge.h"

#include <cstring>
#include <exception>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

// Vendored SDK header. Path is supplied via `-I$LITECORD_DISCORD_SDK_DIR/include`
// (see build.rs).
#include <discordpp.h>

namespace {

// A pending, not-yet-delivered event plus the string bytes it may reference,
// so LcEvent (ids only, per header docs) never needs to own strings itself.
struct PendingEvent {
    LcEvent event;
};

} // namespace

struct LcBridge {
    explicit LcBridge(uint64_t application_id)
        : client(std::make_shared<discordpp::Client>()) {
        (void)application_id;
        // NOTE: verify against the vendored SDK whether the application id is
        // passed to the Client constructor, to Connect(), or to a separate
        // SetApplicationId() call — this varies across 1.x point releases.
    }

    std::shared_ptr<discordpp::Client> client;

    lc_event_cb callback = nullptr;
    void* userdata = nullptr;

    // Buffers backing the LcStr views handed out by the most recent query
    // call; cleared/replaced on every lc_bridge_run_callbacks so views from
    // an older pump are (by contract) no longer valid.
    std::string scratch_username;
    std::string scratch_global_name;
    std::string scratch_avatar_url;
    std::string scratch_content;

    // Events queued by SDK callbacks, drained on the next RunCallbacks pump
    // (SDK callbacks and RunCallbacks both run on the pump thread per the
    // Threading contract in discord_bridge.h, but we still guard with a
    // mutex since Connect/Disconnect/SendUserMessage may be invoked from
    // other threads and could, depending on SDK internals, complete
    // synchronously).
    std::mutex pending_mutex;
    std::vector<PendingEvent> pending;

    void queue(LcEvent ev) {
        std::lock_guard<std::mutex> guard(pending_mutex);
        pending.push_back(PendingEvent{ev});
    }
};

namespace {

LcStr view_of(const std::string& s) {
    if (s.empty()) {
        return LcStr{nullptr, 0};
    }
    return LcStr{s.data(), s.size()};
}

} // namespace

extern "C" {

LcBridge* lc_bridge_create(uint64_t application_id) {
    try {
        auto* bridge = new LcBridge(application_id);

        // Wire SDK callbacks to enqueue ids-only LcEvents. Exact callback
        // registration names/signatures must be checked against the
        // vendored discordpp.h.
        bridge->client->SetStatusChangedCallback(
            [bridge](discordpp::Client::Status status, discordpp::Client::Error /*error*/, int32_t errorDetail) {
                LcEvent ev{};
                ev.kind = LC_EVENT_STATUS_CHANGED;
                ev.status = static_cast<int32_t>(status);
                ev.id_a = 0;
                ev.id_b = static_cast<uint64_t>(errorDetail);
                bridge->queue(ev);
            });

        bridge->client->SetMessageCreatedCallback(
            [bridge](uint64_t messageId) {
                auto handle = bridge->client->GetMessageHandle(messageId);
                LcEvent ev{};
                ev.kind = LC_EVENT_MESSAGE_CREATED;
                ev.id_a = messageId;
                ev.id_b = handle.has_value() ? handle->ChannelId() : 0;
                ev.status = 0;
                bridge->queue(ev);
            });

        // TODO(verify against SDK): SetMessageUpdatedCallback,
        // SetMessageDeletedCallback, SetRelationshipCreatedCallback /
        // SetRelationshipDeletedCallback, SetUserUpdatedCallback,
        // SetLobbyUpdatedCallback / SetLobbyMemberUpdatedCallback,
        // SetVoiceParticipantStatusChangedCallback. Each should push exactly
        // one LcEvent of the matching kind with ids only.

        return bridge;
    } catch (const std::exception&) {
        return nullptr;
    } catch (...) {
        return nullptr;
    }
}

void lc_bridge_destroy(LcBridge* bridge) {
    delete bridge;
}

void lc_bridge_run_callbacks(LcBridge* bridge) {
    if (bridge == nullptr) {
        return;
    }
    try {
        bridge->client->RunCallbacks();
    } catch (...) {
        // The SDK boundary must never throw across the C ABI.
    }

    std::vector<PendingEvent> drained;
    {
        std::lock_guard<std::mutex> guard(bridge->pending_mutex);
        drained.swap(bridge->pending);
    }
    if (bridge->callback == nullptr) {
        return;
    }
    for (const auto& pe : drained) {
        bridge->callback(bridge->userdata, &pe.event);
    }
}

void lc_bridge_set_event_callback(LcBridge* bridge, lc_event_cb cb, void* userdata) {
    if (bridge == nullptr) {
        return;
    }
    bridge->callback = cb;
    bridge->userdata = userdata;
}

void lc_bridge_connect(LcBridge* bridge) {
    if (bridge == nullptr) {
        return;
    }
    try {
        // NOTE: verify exact Connect() overload/signature against the
        // vendored SDK version (may require scopes, redirect URI, etc.).
        bridge->client->Connect();
    } catch (...) {
    }
}

void lc_bridge_disconnect(LcBridge* bridge) {
    if (bridge == nullptr) {
        return;
    }
    try {
        bridge->client->Disconnect();
    } catch (...) {
    }
}

void lc_bridge_update_token(LcBridge* bridge, int32_t token_type, const char* token, size_t len) {
    if (bridge == nullptr || token == nullptr) {
        return;
    }
    try {
        std::string value(token, len);
        if (token_type == LC_TOKEN_ACCESS) {
            bridge->client->UpdateToken(discordpp::AuthorizationTokenType::BearerToken, value, /*callback=*/nullptr);
        } else {
            bridge->client->UpdateToken(discordpp::AuthorizationTokenType::RefreshToken, value, /*callback=*/nullptr);
        }
    } catch (...) {
    }
}

int32_t lc_bridge_current_user(LcBridge* bridge, LcUser* out) {
    if (bridge == nullptr || out == nullptr) {
        return -1;
    }
    try {
        auto user = bridge->client->GetCurrentUser();
        if (!user.has_value()) {
            return 1;
        }
        bridge->scratch_username = user->Username();
        bridge->scratch_global_name = user->GlobalName();
        bridge->scratch_avatar_url = user->AvatarUrl(discordpp::AvatarType::Png, 128);

        out->id = user->Id();
        out->username = view_of(bridge->scratch_username);
        out->global_name = view_of(bridge->scratch_global_name);
        out->avatar_url = view_of(bridge->scratch_avatar_url);
        out->is_provisional = user->IsProvisionalAccount() ? 1 : 0;
        return 0;
    } catch (...) {
        return -1;
    }
}

int32_t lc_bridge_get_message(LcBridge* bridge, uint64_t id, LcMessage* out) {
    if (bridge == nullptr || out == nullptr) {
        return -1;
    }
    try {
        auto handle = bridge->client->GetMessageHandle(id);
        if (!handle.has_value()) {
            return 1;
        }
        bridge->scratch_content = handle->Content();

        out->id = id;
        out->channel_id = handle->ChannelId();
        out->author_id = handle->AuthorId();
        out->sent_at_ms = handle->SentAt();
        out->edited_at_ms = handle->IsEdited() ? handle->EditedAt() : 0;
        out->content = view_of(bridge->scratch_content);
        return 0;
    } catch (...) {
        return -1;
    }
}

void lc_bridge_send_user_message(
    LcBridge* bridge,
    uint64_t recipient,
    const char* content,
    size_t len,
    uint64_t request_id
) {
    if (bridge == nullptr || content == nullptr) {
        return;
    }
    try {
        std::string body(content, len);
        bridge->client->SendUserMessage(
            recipient,
            body,
            [bridge, request_id](discordpp::ClientResult result, uint64_t messageId) {
                LcEvent ev{};
                ev.kind = LC_EVENT_SEND_COMPLETED;
                ev.id_a = request_id;
                ev.id_b = messageId;
                ev.status = result.Successful() ? 0 : static_cast<int32_t>(result.Error());
                bridge->queue(ev);
            });
    } catch (...) {
        LcEvent ev{};
        ev.kind = LC_EVENT_SEND_COMPLETED;
        ev.id_a = request_id;
        ev.id_b = 0;
        ev.status = -1;
        bridge->queue(ev);
    }
}

} // extern "C"
