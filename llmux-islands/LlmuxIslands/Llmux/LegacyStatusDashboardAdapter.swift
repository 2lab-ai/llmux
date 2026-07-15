import Foundation

/// Converts the old `/llmux/status` document into the dashboard wire shape
/// expected by the embedded Rust core. This is a compatibility transport
/// adapter only: Rust still performs account aliasing, handle allocation,
/// privacy filtering, health derivation, and every mutation lookup.
///
/// A previously accepted dashboard may be supplied as a base. Status-owned
/// account fields replace its account snapshot while analytics-only fields are
/// retained, so a temporary downgrade to an old daemon does not erase the last
/// good statistics view.
enum LegacyStatusDashboardAdapter {
    enum AdapterError: LocalizedError {
        case invalidStatus

        var errorDescription: String? {
            "The legacy llmux status response was invalid."
        }
    }

    static func dashboardData(
        from statusData: Data,
        previousDashboardData: Data?,
        endpointPort: Int?,
        receivedAtMs: UInt64,
        emailAnonymous: Bool,
        showFableWeekly: Bool
    ) throws -> Data {
        let decoded: LlmuxStatus
        let rawStatus: [String: Any]
        do {
            decoded = try JSONDecoder().decode(LlmuxStatus.self, from: statusData)
            guard let object = try JSONSerialization.jsonObject(with: statusData) as? [String: Any]
            else { throw AdapterError.invalidStatus }
            rawStatus = object
        } catch {
            throw AdapterError.invalidStatus
        }

        guard let rawAccounts = rawStatus["accounts"] as? [[String: Any]],
              rawAccounts.count == decoded.accounts.count
        else { throw AdapterError.invalidStatus }

        var dashboard = previousDashboardData
            .flatMap { Self.jsonObject($0) }
            ?? Self.emptyDashboard(
                port: decoded.port ?? endpointPort,
                emailAnonymous: emailAnonymous,
                showFableWeekly: showFableWeekly
            )

        let previousAccounts = Self.accountsByName(dashboard["accounts"])
        let accounts = zip(decoded.accounts, rawAccounts).enumerated().map { index, pair in
            Self.account(
                decoded: pair.0,
                raw: pair.1,
                previous: previousAccounts[pair.0.name],
                order: index + 1,
                receivedAtMs: receivedAtMs
            )
        }

        dashboard["version"] = Self.nonemptyString(rawStatus["version"])
            ?? Self.nonemptyString(dashboard["version"])
            ?? "llmux legacy"
        dashboard["pid"] = min(
            Self.unsigned(rawStatus["pid"])
                ?? Self.unsigned(dashboard["pid"])
                ?? 0,
            UInt64(UInt32.max)
        )
        dashboard["uptime_secs"] = Self.unsigned(rawStatus["uptime_secs"])
            ?? Self.unsigned(dashboard["uptime_secs"])
            ?? 0
        dashboard["port"] = Self.validPort(rawStatus["port"])
            ?? Self.validPort(dashboard["port"])
            ?? Self.validPort(endpointPort)
            ?? 3_456
        dashboard["current"] = decoded.current ?? NSNull()
        dashboard["current_by_group"] = Self.currentByGroup(
            status: rawStatus,
            current: decoded.current,
            accounts: rawAccounts
        )
        dashboard["accounts"] = accounts
        dashboard["email_anonymous"] = decoded.emailAnonymous ?? emailAnonymous
        dashboard["show_fable_weekly"] = Self.bool(rawStatus["show_fable_weekly"])
            ?? showFableWeekly

        // `/status` has no global activity counters. Preserve the accepted
        // totals, but make the live in-flight number agree with its account
        // snapshot instead of retaining a stale value.
        var totals = dashboard["totals"] as? [String: Any] ?? Self.emptyTotals()
        let liveInFlight = accounts.reduce(UInt64(0)) { partial, account in
            let count = Self.unsigned(account["in_flight"]) ?? 0
            let (sum, overflow) = partial.addingReportingOverflow(count)
            return overflow ? UInt64.max : sum
        }
        totals["in_flight"] = min(liveInFlight, UInt64(UInt32.max))
        dashboard["totals"] = totals

        guard JSONSerialization.isValidJSONObject(dashboard) else {
            throw AdapterError.invalidStatus
        }
        do {
            return try JSONSerialization.data(withJSONObject: dashboard, options: [.sortedKeys])
        } catch {
            throw AdapterError.invalidStatus
        }
    }

    private static func emptyDashboard(
        port: Int?,
        emailAnonymous: Bool,
        showFableWeekly: Bool
    ) -> [String: Any] {
        [
            "version": "llmux legacy",
            "pid": UInt64(0),
            "uptime_secs": UInt64(0),
            "port": validPort(port) ?? 3_456,
            "current": NSNull(),
            "current_by_group": [String: String](),
            "upstream": "",
            "config_path": NSNull(),
            "select_params": [
                "five_hour_max": 0.90,
                "seven_day_max": 0.99,
                "fable_weekly_max": 0.98,
                "mode": "default",
                "usage_max_age_secs": UInt64(600),
            ],
            "refresh_ahead_secs": UInt64(0),
            "evaluate_tick_secs": UInt64(60),
            "accounts": [[String: Any]](),
            "scheduler": [
                "last_switch": NSNull(),
                "next_in_line": NSNull(),
                "next_eval_in_secs": UInt64(60),
            ],
            "poller": [[String: Any]](),
            "totals": emptyTotals(),
            "model_usage": [[String: Any]](),
            "client_usage": [[String: Any]](),
            "windowed": [[String: Any]](),
            "activity": [
                "in_flight": [[String: Any]](),
                "completed": [[String: Any]](),
            ],
            "logs": [[String: Any]](),
            "codex": [
                "available": false,
                "fast": false,
                "model": "",
            ],
            "email_anonymous": emailAnonymous,
            "show_fable_weekly": showFableWeekly,
            "data_quality": [
                "model_usage": "hydrated activity/runtime",
                "windowed": "best effort",
                "cost": "API-equivalent estimate",
                "cache": "missing fields shown as unavailable",
            ],
            "events": [[String: Any]](),
        ]
    }

    private static func emptyTotals() -> [String: Any] {
        [
            "requests": UInt64(0),
            "ok": UInt64(0),
            "errors": UInt64(0),
            "tokens_in": UInt64(0),
            "tokens_out": UInt64(0),
            "rpm_5m": 0.0,
            "in_flight": UInt64(0),
            "cost_usd": 0.0,
        ]
    }

    private static func account(
        decoded: LlmuxAccountRecord,
        raw: [String: Any],
        previous: [String: Any]?,
        order: Int,
        receivedAtMs: UInt64
    ) -> [String: Any] {
        var result = previous ?? [:]
        let status = nonemptyString(raw["status"]) ?? decoded.status ?? "unknown"
        let paused = bool(raw["paused"]) ?? decoded.paused ?? false

        result["name"] = decoded.name
        result["type"] = decoded.type
        if let group = nonemptyString(raw["group"]) ?? decoded.group {
            // `group` is ignored by DashboardDoc itself but is useful when a
            // status-only daemon exposes only the scalar `current` value.
            result["group"] = group
        }
        result["status"] = status
        result["order"] = unsigned(raw["order"]) ?? UInt64(order)
        result["blocked"] = nullableString(raw["blocked"]) ?? NSNull()
        result["healthy"] = bool(raw["healthy"]) ?? (status != "auth_failed")
        result["five_hour"] = window(
            raw["five_hour"],
            previous: previous?["five_hour"],
            receivedAtMs: receivedAtMs
        )
        result["seven_day"] = window(
            raw["seven_day"],
            previous: previous?["seven_day"],
            receivedAtMs: receivedAtMs
        )
        result["fable_weekly"] = scopedWindow(raw["fable_weekly"], receivedAtMs: receivedAtMs)
        result["scoped_limits"] = raw["scoped_limits"] as? [[String: Any]] ?? []
        if let cooldownUntil = unsigned(raw["cooldown_until"])
            ?? (status == "cooldown" ? unsigned(previous?["cooldown_until"]) : nil) {
            result["cooldown_until"] = cooldownUntil
        } else {
            result["cooldown_until"] = NSNull()
        }
        if let source = nullableString(raw["cooldown_source"]) {
            result["cooldown_source"] = source
        } else if status == "cooldown",
                  let source = nullableString(previous?["cooldown_source"]) {
            result["cooldown_source"] = source
        } else {
            result["cooldown_source"] = NSNull()
        }
        result["in_flight"] = min(
            unsigned(raw["in_flight"]) ?? UInt64(max(0, decoded.inFlight ?? 0)),
            UInt64(UInt32.max)
        )
        if let tokenExpiresAtMs = unsigned(raw["token_expires_at_ms"])
            ?? decoded.tokenExpiresAtMs {
            result["token_expires_at_ms"] = tokenExpiresAtMs
        } else {
            result["token_expires_at_ms"] = NSNull()
        }
        if let lastRefreshMs = unsigned(raw["last_refresh_ms"])
            ?? unsigned(previous?["last_refresh_ms"]) {
            result["last_refresh_ms"] = lastRefreshMs
        } else {
            result["last_refresh_ms"] = NSNull()
        }
        result["paused"] = paused
        if let limits = raw["limits"] as? [String: Any] {
            result["limits"] = limits
        }
        result["totals"] = normalizedLifetimeTotals(
            raw["totals"] as? [String: Any],
            previous: previous?["totals"] as? [String: Any]
        )
        result["session"] = normalizedSessionTotals(
            raw["session"] as? [String: Any],
            previous: previous?["session"] as? [String: Any]
        )
        return result
    }

    private static func window(
        _ rawValue: Any?,
        previous: Any?,
        receivedAtMs: UInt64
    ) -> Any {
        guard let raw = rawValue as? [String: Any],
              let utilization = finiteDouble(raw["utilization"])
        else { return NSNull() }

        let old = previous as? [String: Any]
        let reportedResetsIn = unsigned(raw["resets_in_secs"])
        let resetsIn = reportedResetsIn ?? 0
        let nowSeconds = receivedAtMs / 1_000
        let resetsAt = unsigned(raw["resets_at"])
            ?? reportedResetsIn.map { nowSeconds.saturatingAdding($0) }
            ?? unsigned(old?["resets_at"])
            ?? nowSeconds
        return [
            "utilization": utilization,
            "resets_at": resetsAt,
            "resets_in_secs": resetsIn,
            "fetched_at_ms": unsigned(raw["fetched_at_ms"])
                ?? receivedAtMs,
            "source": nonemptyString(raw["source"])
                ?? "poll",
        ]
    }

    private static func scopedWindow(_ rawValue: Any?, receivedAtMs: UInt64) -> Any {
        guard let raw = rawValue as? [String: Any],
              let utilization = finiteDouble(raw["utilization"])
        else { return NSNull() }
        let resetsIn = unsigned(raw["resets_in_secs"]) ?? 0
        return [
            "utilization": utilization,
            "resets_at": unsigned(raw["resets_at"])
                ?? (receivedAtMs / 1_000).saturatingAdding(resetsIn),
            "resets_in_secs": resetsIn,
            "severity": nonemptyString(raw["severity"]) ?? "normal",
            "is_active": bool(raw["is_active"]) ?? false,
            "constraining": bool(raw["constraining"]) ?? false,
        ]
    }

    private static func normalizedLifetimeTotals(
        _ raw: [String: Any]?,
        previous: [String: Any]?
    ) -> [String: Any] {
        let source = raw ?? previous ?? [:]
        return [
            "requests": unsigned(source["requests"]) ?? 0,
            "input_tokens": unsigned(source["input_tokens"]) ?? 0,
            "output_tokens": unsigned(source["output_tokens"]) ?? 0,
        ]
    }

    private static func normalizedSessionTotals(
        _ raw: [String: Any]?,
        previous: [String: Any]?
    ) -> [String: Any] {
        let source = raw ?? previous ?? [:]
        return [
            "requests": unsigned(source["requests"]) ?? 0,
            "ok": unsigned(source["ok"]) ?? 0,
            "errors": unsigned(source["errors"]) ?? 0,
            "tokens_in": unsigned(source["tokens_in"]) ?? 0,
            "tokens_out": unsigned(source["tokens_out"]) ?? 0,
        ]
    }

    private static func currentByGroup(
        status: [String: Any],
        current: String?,
        accounts: [[String: Any]]
    ) -> [String: String] {
        if let explicit = status["current_by_group"] as? [String: Any] {
            let values = explicit.reduce(into: [String: String]()) { result, pair in
                guard let value = nonemptyString(pair.value) else { return }
                result[pair.key] = value
            }
            if !values.isEmpty || current == nil { return values }
        }
        guard let current,
              let account = accounts.first(where: { nonemptyString($0["name"]) == current })
        else { return [:] }
        let group = nonemptyString(account["group"]) ?? providerGroup(for: account["type"])
        return [group: current]
    }

    private static func providerGroup(for rawKind: Any?) -> String {
        switch nonemptyString(rawKind)?.lowercased() {
        case "codex": return "codex"
        case "grok": return "grok"
        default: return "claude"
        }
    }

    private static func accountsByName(_ value: Any?) -> [String: [String: Any]] {
        guard let accounts = value as? [[String: Any]] else { return [:] }
        return accounts.reduce(into: [:]) { result, account in
            guard let name = nonemptyString(account["name"]) else { return }
            result[name] = account
        }
    }

    private static func jsonObject(_ data: Data) -> [String: Any]? {
        (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }

    private static func nullableString(_ value: Any?) -> Any? {
        if value is NSNull { return NSNull() }
        return nonemptyString(value)
    }

    private static func nonemptyString(_ value: Any?) -> String? {
        guard let value = value as? String, !value.isEmpty else { return nil }
        return value
    }

    private static func bool(_ value: Any?) -> Bool? {
        value as? Bool
    }

    private static func finiteDouble(_ value: Any?) -> Double? {
        guard !(value is Bool), let number = value as? NSNumber else { return nil }
        let result = number.doubleValue
        return result.isFinite ? result : nil
    }

    private static func unsigned(_ value: Any?) -> UInt64? {
        guard !(value is Bool), let number = value as? NSNumber else { return nil }
        let result = number.doubleValue
        guard result.isFinite, result >= 0, result.rounded(.towardZero) == result else { return nil }
        return result >= Double(UInt64.max) ? UInt64.max : UInt64(result)
    }

    private static func validPort(_ value: Any?) -> Int? {
        guard let value = unsigned(value), (1...65_535).contains(value) else { return nil }
        return Int(value)
    }
}

private extension UInt64 {
    func saturatingAdding(_ other: UInt64) -> UInt64 {
        let (value, overflow) = addingReportingOverflow(other)
        return overflow ? .max : value
    }
}
