//
//  LlmuxEvents.swift
//  LlmuxIslands
//
//  The llmux events list (the daemon-side replacement for the single event
//  banner). Wire contract:
//
//    - `GET /llmux/dashboard` carries `events: [{id, from, to, content}]`
//      (additive — omitted entirely when empty/absent, decodes as []).
//    - `POST /llmux/events` with ONE event object `{id, from, to, content}` =
//      idempotent upsert by id; the SAME endpoint with `{"remove": "<id>"}` =
//      idempotent remove (absent id still 200). Both return
//      `200 {"ok": true, "events": [<stored list>]}`; validation failures are
//      400 (non-empty id/content, parseable from/to, from < to).
//    - `from`/`to` are either RFC3339-with-offset ("2026-07-12T23:30:00+09:00")
//      or compact local time `YYYYMMDDHHMM` ("202607080000").
//
//  Everything in this file is pure Foundation (no networking, no UI) so the
//  parsing/formatting/active-window logic is unit-testable with canned strings
//  — the same split as CLIRunner's CLIParse.
//

import Foundation

// MARK: - LlmuxEvent

/// One event on the wire. Decoded tolerantly: a partial object (missing keys)
/// decodes with empty-string defaults rather than failing the whole document —
/// same additive-field rule as the rest of the dashboard doc.
struct LlmuxEvent: Decodable, Identifiable, Equatable {
    let id: String
    let from: String
    let to: String
    let content: String

    init(id: String, from: String, to: String, content: String) {
        self.id = id
        self.from = from
        self.to = to
        self.content = content
    }

    enum CodingKeys: String, CodingKey { case id, from, to, content }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        from = try c.decodeIfPresent(String.self, forKey: .from) ?? ""
        to = try c.decodeIfPresent(String.self, forKey: .to) ?? ""
        content = try c.decodeIfPresent(String.self, forKey: .content) ?? ""
    }

    /// The JSON body for the `POST /llmux/events` upsert.
    var jsonObject: [String: Any] {
        ["id": id, "from": from, "to": to, "content": content]
    }
}

// MARK: - LlmuxEventList (echo decoding)

/// Decodes the stored list `POST /llmux/events` echoes back. The canonical
/// shape is `{"ok": true, "events": [...]}`; a bare array `[...]` is also
/// tolerated. An `{"ok": true}` echo with the `events` key omitted (the
/// empty-list case, mirroring the dashboard doc's omit-when-empty rule)
/// decodes as []. Returns nil for anything else — callers then keep their
/// local state and refresh via the dashboard.
enum LlmuxEventList {
    static func decode(_ data: Data) -> [LlmuxEvent]? {
        if let bare = try? JSONDecoder().decode([LlmuxEvent].self, from: data) {
            return bare
        }
        struct Wrapped: Decodable {
            let ok: Bool?
            let events: [LlmuxEvent]?
        }
        if let wrapped = try? JSONDecoder().decode(Wrapped.self, from: data) {
            if let events = wrapped.events { return events }
            // ok-but-no-events echo = the stored list is empty.
            if wrapped.ok == true { return [] }
        }
        return nil
    }
}

// MARK: - LlmuxEventTime (pure time parsing / rendering)

/// Parses and renders the two event timestamp formats. All functions take an
/// explicit `timeZone` (defaulting to the device's) so tests pin a fixed zone
/// and assert exact strings.
enum LlmuxEventTime {
    /// Parse an event timestamp: compact `YYYYMMDDHHMM` (interpreted in
    /// `timeZone`, i.e. local wall-clock) or RFC3339-with-offset (the offset
    /// wins; `timeZone` is irrelevant to the instant). nil for anything else.
    static func parse(_ raw: String, timeZone: TimeZone = .current) -> Date? {
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.count == 12, s.allSatisfy(\.isNumber) {
            return parseCompact(s, timeZone: timeZone)
        }
        return parseRFC3339(s)
    }

    /// Compact `YYYYMMDDHHMM` in the given zone. Rejects out-of-range
    /// components (month 13, hour 25, …) via strict calendar validation.
    private static func parseCompact(_ s: String, timeZone: TimeZone) -> Date? {
        func int(_ range: Range<Int>) -> Int? {
            let start = s.index(s.startIndex, offsetBy: range.lowerBound)
            let end = s.index(s.startIndex, offsetBy: range.upperBound)
            return Int(s[start..<end])
        }
        guard let year = int(0..<4), let month = int(4..<6), let day = int(6..<8),
              let hour = int(8..<10), let minute = int(10..<12) else { return nil }
        guard (1...12).contains(month), (1...31).contains(day),
              (0...23).contains(hour), (0...59).contains(minute) else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let comps = DateComponents(year: year, month: month, day: day, hour: hour, minute: minute)
        guard let date = calendar.date(from: comps),
              // Round-trip check catches non-existent wall-clock values
              // (e.g. Feb 30 normalizing to Mar 2).
              calendar.component(.day, from: date) == day,
              calendar.component(.month, from: date) == month else { return nil }
        return date
    }

    /// RFC3339 with offset, with or without fractional seconds.
    private static func parseRFC3339(_ s: String) -> Date? {
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        if let date = plain.date(from: s) { return date }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: s)
    }

    /// Render a timestamp for the events list in LOCAL time as `M/d HH:mm`
    /// (e.g. "7/8 00:00"). Garbage in, verbatim out: an unparseable string is
    /// returned unchanged, never mangled or hidden.
    static func display(_ raw: String, timeZone: TimeZone = .current) -> String {
        guard let date = parse(raw, timeZone: timeZone) else { return raw }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = timeZone
        formatter.dateFormat = "M/d HH:mm"
        return formatter.string(from: date)
    }

    /// The `from → to` range label for one event row.
    static func displayRange(from: String, to: String, timeZone: TimeZone = .current) -> String {
        "\(display(from, timeZone: timeZone)) → \(display(to, timeZone: timeZone))"
    }

    /// Whether an event window is active now: `from <= now < to` (from is
    /// inclusive, to is exclusive). Unparseable bounds are never active.
    static func isActive(from: String, to: String, now: Date, timeZone: TimeZone = .current) -> Bool {
        guard let start = parse(from, timeZone: timeZone),
              let end = parse(to, timeZone: timeZone) else { return false }
        return start <= now && now < end
    }
}
