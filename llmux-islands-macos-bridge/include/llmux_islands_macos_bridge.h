#ifndef LLMUX_ISLANDS_MACOS_BRIDGE_H
#define LLMUX_ISLANDS_MACOS_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LLMUX_ISLANDS_BRIDGE_ABI_VERSION 1u

typedef struct LlmuxIslandsBridge LlmuxIslandsBridge;

typedef struct LlmuxIslandsOwnedBytes {
    uint8_t *ptr;
    size_t len;
} LlmuxIslandsOwnedBytes;

typedef enum LlmuxIslandsStatus {
    LLMUX_ISLANDS_STATUS_OK = 0,
    LLMUX_ISLANDS_STATUS_INVALID_ARGUMENT = 1,
    LLMUX_ISLANDS_STATUS_INVALID_JSON = 2,
    LLMUX_ISLANDS_STATUS_INVALID_ACTION = 3,
    LLMUX_ISLANDS_STATUS_INTERNAL = 4,
    LLMUX_ISLANDS_STATUS_PANIC = 5
} LlmuxIslandsStatus;

uint32_t llmux_islands_bridge_abi_version(void);

/*
 * All inputs are exact-length UTF-8 JSON; no NUL terminator is read.
 * Every successful state output is the canonical UiState JSON document.
 * Effects are executor-only JSON and must be consumed without rendering,
 * logging, or persistence.  The caller owns every non-empty output and must
 * release it with llmux_islands_owned_bytes_free.
 *
 * Output structs need not be initialized.  A call overwrites them, so an old
 * allocation must be freed before reusing the same output variable.
 */
LlmuxIslandsStatus llmux_islands_bridge_new(
    const uint8_t *options_json,
    size_t options_len,
    LlmuxIslandsBridge **out_bridge,
    LlmuxIslandsOwnedBytes *out_error);

LlmuxIslandsStatus llmux_islands_bridge_dispatch(
    LlmuxIslandsBridge *bridge,
    const uint8_t *action_json,
    size_t action_len,
    LlmuxIslandsOwnedBytes *out_state_json,
    LlmuxIslandsOwnedBytes *out_effects_json,
    LlmuxIslandsOwnedBytes *out_error);

/*
 * request_id must be the exact fetch_dashboard id returned by dispatch.
 * A stale id is intentionally reduced as a no-op by the shared core.
 */
LlmuxIslandsStatus llmux_islands_bridge_apply_dashboard(
    LlmuxIslandsBridge *bridge,
    const uint8_t *request_id,
    size_t request_id_len,
    const uint8_t *dashboard_json,
    size_t dashboard_len,
    uint64_t received_at_ms,
    LlmuxIslandsOwnedBytes *out_state_json,
    LlmuxIslandsOwnedBytes *out_effects_json,
    LlmuxIslandsOwnedBytes *out_error);

LlmuxIslandsStatus llmux_islands_bridge_state_json(
    LlmuxIslandsBridge *bridge,
    LlmuxIslandsOwnedBytes *out_state_json,
    LlmuxIslandsOwnedBytes *out_error);

/* Null-safe. Concurrent use and destruction of the same handle is forbidden. */
void llmux_islands_bridge_free(LlmuxIslandsBridge *bridge);

/* Null-safe, clears the struct, and is safe to call twice on that struct. */
void llmux_islands_owned_bytes_free(LlmuxIslandsOwnedBytes *bytes);

#ifdef __cplusplus
}
#endif

#endif
