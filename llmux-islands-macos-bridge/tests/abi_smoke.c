#include "llmux_islands_macos_bridge.h"

#include <stdint.h>

int main(void) {
    static const uint8_t options[] = {'{', '}'};
    LlmuxIslandsBridge *bridge = NULL;
    LlmuxIslandsOwnedBytes error = {0};
    if (llmux_islands_bridge_abi_version() != LLMUX_ISLANDS_BRIDGE_ABI_VERSION) {
        return 1;
    }
    if (llmux_islands_bridge_new(options, sizeof(options), &bridge, &error) !=
        LLMUX_ISLANDS_STATUS_OK) {
        llmux_islands_owned_bytes_free(&error);
        return 2;
    }
    LlmuxIslandsOwnedBytes state = {0};
    if (llmux_islands_bridge_state_json(bridge, &state, &error) !=
        LLMUX_ISLANDS_STATUS_OK) {
        llmux_islands_owned_bytes_free(&error);
        llmux_islands_bridge_free(bridge);
        return 3;
    }
    if (state.ptr == NULL || state.len == 0) {
        llmux_islands_owned_bytes_free(&state);
        llmux_islands_bridge_free(bridge);
        return 4;
    }
    llmux_islands_owned_bytes_free(&state);
    llmux_islands_owned_bytes_free(&state);
    llmux_islands_bridge_free(bridge);
    return 0;
}
