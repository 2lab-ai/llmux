import Foundation

struct SharedUiCoreTransition {
    let state: SharedUiState
    /// Executor-only values. Callers consume and discard these immediately;
    /// they must never be published, persisted, rendered, or logged.
    let effects: [SharedCoreEffect]
}

enum SharedUiCoreError: LocalizedError {
    case incompatibleABI
    case creationFailed
    case invalidAction
    case invalidOutput
    case bridgeFailure(String)

    var errorDescription: String? {
        switch self {
        case .incompatibleABI:
            return "The embedded llmux UI core is incompatible with this app."
        case .creationFailed:
            return "The embedded llmux UI core could not be started."
        case .invalidAction:
            return "The app produced an invalid semantic action."
        case .invalidOutput:
            return "The embedded llmux UI core returned an invalid response."
        case let .bridgeFailure(message):
            return message
        }
    }
}

/// Owning Swift wrapper around the length-delimited C ABI. The model uses one
/// instance from the main actor, so calls and destruction never race.
final class SharedUiCoreRuntime {
    private static let expectedABIVersion: UInt32 = 1
    private var handle: LlmuxIslandsSwiftHandle?

    init(configuration: SharedUiCoreConfiguration) throws {
        guard llmux_islands_bridge_abi_version() == Self.expectedABIVersion else {
            throw SharedUiCoreError.incompatibleABI
        }
        let options = try JSONEncoder().encode(configuration)
        var created: LlmuxIslandsSwiftHandle?
        var error = Self.emptyBytes()
        let status = options.withUnsafeBytes { bytes in
            llmux_islands_swift_new(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &created,
                &error
            )
        }
        let failure = Self.takeBytes(&error)
        guard status == 0, let created else {
            throw Self.bridgeError(from: failure, fallback: .creationFailed)
        }
        handle = created
    }

    deinit {
        if let handle {
            llmux_islands_swift_free(handle)
        }
    }

    func dispatch(_ action: [String: Any]) throws -> SharedUiCoreTransition {
        guard JSONSerialization.isValidJSONObject(action),
              let handle
        else { throw SharedUiCoreError.invalidAction }
        let actionJSON = try JSONSerialization.data(withJSONObject: action)
        var state = Self.emptyBytes()
        var effects = Self.emptyBytes()
        var error = Self.emptyBytes()
        let status = actionJSON.withUnsafeBytes { bytes in
            llmux_islands_swift_dispatch(
                handle,
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &state,
                &effects,
                &error
            )
        }
        return try Self.decodeTransition(
            status: status,
            state: &state,
            effects: &effects,
            error: &error
        )
    }

    func applyDashboard(
        requestID: String,
        dashboardJSON: Data,
        receivedAtMs: UInt64
    ) throws -> SharedUiCoreTransition {
        guard let handle, let requestIDData = requestID.data(using: .utf8) else {
            throw SharedUiCoreError.invalidAction
        }
        var state = Self.emptyBytes()
        var effects = Self.emptyBytes()
        var error = Self.emptyBytes()
        let status = requestIDData.withUnsafeBytes { requestBytes in
            dashboardJSON.withUnsafeBytes { dashboardBytes in
                llmux_islands_swift_apply_dashboard(
                    handle,
                    requestBytes.bindMemory(to: UInt8.self).baseAddress,
                    requestBytes.count,
                    dashboardBytes.bindMemory(to: UInt8.self).baseAddress,
                    dashboardBytes.count,
                    receivedAtMs,
                    &state,
                    &effects,
                    &error
                )
            }
        }
        return try Self.decodeTransition(
            status: status,
            state: &state,
            effects: &effects,
            error: &error
        )
    }

    private static func decodeTransition(
        status: Int32,
        state: inout LlmuxIslandsOwnedBytes,
        effects: inout LlmuxIslandsOwnedBytes,
        error: inout LlmuxIslandsOwnedBytes
    ) throws -> SharedUiCoreTransition {
        let stateData = takeBytes(&state)
        let effectData = takeBytes(&effects)
        let errorData = takeBytes(&error)
        guard status == 0 else {
            throw bridgeError(from: errorData, fallback: .bridgeFailure("The embedded llmux UI core rejected the operation."))
        }
        do {
            return SharedUiCoreTransition(
                state: try JSONDecoder().decode(SharedUiState.self, from: stateData),
                effects: try JSONDecoder().decode([SharedCoreEffect].self, from: effectData)
            )
        } catch {
            throw SharedUiCoreError.invalidOutput
        }
    }

    private static func emptyBytes() -> LlmuxIslandsOwnedBytes {
        LlmuxIslandsOwnedBytes(ptr: nil, len: 0)
    }

    private static func takeBytes(_ bytes: inout LlmuxIslandsOwnedBytes) -> Data {
        defer { llmux_islands_owned_bytes_free(&bytes) }
        guard let pointer = bytes.ptr, bytes.len > 0 else { return Data() }
        return Data(bytes: pointer, count: bytes.len)
    }

    private static func bridgeError(
        from data: Data,
        fallback: SharedUiCoreError
    ) -> SharedUiCoreError {
        struct Payload: Decodable { let message: String }
        guard let payload = try? JSONDecoder().decode(Payload.self, from: data),
              !payload.message.isEmpty
        else { return fallback }
        // Bridge errors are fixed, sanitized strings. Limit their size so an
        // ABI regression still cannot turn UI errors into an unbounded sink.
        return .bridgeFailure(String(payload.message.prefix(240)))
    }
}
