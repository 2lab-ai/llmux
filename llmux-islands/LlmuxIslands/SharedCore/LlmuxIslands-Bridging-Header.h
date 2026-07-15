#ifndef LLMUX_ISLANDS_BRIDGING_HEADER_H
#define LLMUX_ISLANDS_BRIDGING_HEADER_H

#include "llmux_islands_macos_bridge.h"

/*
 * Swift imports pointers to incomplete C structs inconsistently across Xcode
 * releases. Keep that type out of the Swift surface and expose the bridge as
 * a plain opaque pointer. These wrappers are header-only and preserve the
 * versioned C ABI exported by the Rust archive.
 */
typedef void *LlmuxIslandsSwiftHandle;

static inline int32_t llmux_islands_swift_new(
    const uint8_t *options_json,
    size_t options_len,
    LlmuxIslandsSwiftHandle *out_handle,
    LlmuxIslandsOwnedBytes *out_error
) {
    LlmuxIslandsBridge *bridge = NULL;
    LlmuxIslandsStatus status = llmux_islands_bridge_new(
        options_json, options_len, &bridge, out_error
    );
    *out_handle = bridge;
    return (int32_t)status;
}

static inline int32_t llmux_islands_swift_dispatch(
    LlmuxIslandsSwiftHandle handle,
    const uint8_t *action_json,
    size_t action_len,
    LlmuxIslandsOwnedBytes *out_state_json,
    LlmuxIslandsOwnedBytes *out_effects_json,
    LlmuxIslandsOwnedBytes *out_error
) {
    return (int32_t)llmux_islands_bridge_dispatch(
        (LlmuxIslandsBridge *)handle,
        action_json,
        action_len,
        out_state_json,
        out_effects_json,
        out_error
    );
}

static inline int32_t llmux_islands_swift_apply_dashboard(
    LlmuxIslandsSwiftHandle handle,
    const uint8_t *request_id,
    size_t request_id_len,
    const uint8_t *dashboard_json,
    size_t dashboard_len,
    uint64_t received_at_ms,
    LlmuxIslandsOwnedBytes *out_state_json,
    LlmuxIslandsOwnedBytes *out_effects_json,
    LlmuxIslandsOwnedBytes *out_error
) {
    return (int32_t)llmux_islands_bridge_apply_dashboard(
        (LlmuxIslandsBridge *)handle,
        request_id,
        request_id_len,
        dashboard_json,
        dashboard_len,
        received_at_ms,
        out_state_json,
        out_effects_json,
        out_error
    );
}

static inline void llmux_islands_swift_free(LlmuxIslandsSwiftHandle handle) {
    llmux_islands_bridge_free((LlmuxIslandsBridge *)handle);
}

#endif
