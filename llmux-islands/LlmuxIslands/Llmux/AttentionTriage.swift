import CryptoKit
import Foundation

/// What the owner did with the NEEDS ATTENTION rows: sent them to the bottom
/// of the Usage panel (`_`) or dismissed them (`x`).
///
/// A row is named by its account and condition (`GlanceAttention.conditionKey`),
/// never by its numbers, so a minimized "7d 8% left" row stays at the bottom
/// when it becomes "7d 6% left". A row that is not in the triage is new and
/// shows at the top. A row whose condition has been missing from
/// `forgetAfterAbsentPolls` online polls in a row (about 15 minutes at the
/// 10-second poll) is forgotten, so the same problem coming back later is new
/// again. Counting observed polls rather than clock time means sleep, a quit
/// app or an offline daemon never counts as "the problem cleared", and a row
/// that flickers between polls (an account briefly cold after a restart)
/// does not jump back to the top.
///
/// Persisted keys are SHA-256 digests, so no account id is written to disk.
/// Foundation + CryptoKit only: the logic test target exercises it directly.
struct AttentionTriage: Codable, Equatable {
    enum Placement: String, Codable {
        case bottom
        case dismissed
    }

    struct Entry: Codable, Equatable {
        var placement: Placement
        /// Consecutive online polls without this row; 0 while it is present.
        var absentPolls: Int = 0
    }

    static let forgetAfterAbsentPolls = 90

    private(set) var entries: [String: Entry] = [:]

    /// The rows split for display: `top` holds what the owner has not acted
    /// on yet, `bottom` what was minimized. Dismissed rows are in neither.
    /// Each side keeps the resolver's order (worst first).
    func split(_ items: [GlanceAttention]) -> (top: [GlanceAttention], bottom: [GlanceAttention]) {
        var top: [GlanceAttention] = []
        var bottom: [GlanceAttention] = []
        for item in items {
            switch entries[Self.digest(item)]?.placement {
            case .dismissed?: continue
            case .bottom?: bottom.append(item)
            case nil: top.append(item)
            }
        }
        return (top, bottom)
    }

    /// `_`: these rows move to the bottom and stay there.
    mutating func minimize(_ items: [GlanceAttention]) {
        place(items, .bottom)
    }

    /// `x`: these rows disappear until their condition clears and returns.
    mutating func dismiss(_ items: [GlanceAttention]) {
        place(items, .dismissed)
    }

    /// Record one poll's rows. An offline poll changes nothing: it has no
    /// rows, and counting it would let a daemon outage undo the owner's
    /// choices. Returns whether anything changed, so the caller saves only
    /// then (an empty triage — the usual case — never writes).
    @discardableResult
    mutating func observe(_ items: [GlanceAttention], online: Bool) -> Bool {
        guard online else { return false }
        let present = Set(items.map(Self.digest))
        var changed = false
        for (key, entry) in entries {
            if present.contains(key) {
                if entry.absentPolls != 0 {
                    entries[key]?.absentPolls = 0
                    changed = true
                }
            } else if entry.absentPolls + 1 >= Self.forgetAfterAbsentPolls {
                entries[key] = nil
                changed = true
            } else {
                entries[key]?.absentPolls = entry.absentPolls + 1
                changed = true
            }
        }
        return changed
    }

    private mutating func place(_ items: [GlanceAttention], _ placement: Placement) {
        for item in items {
            entries[Self.digest(item)] = Entry(placement: placement)
        }
    }

    static func digest(_ item: GlanceAttention) -> String {
        SHA256.hash(data: Data(item.conditionKey.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
    }

    // MARK: - Persistence

    static let defaultsKey = "attentionTriage"

    static func load(from defaults: UserDefaults = .standard) -> AttentionTriage {
        guard let data = defaults.data(forKey: defaultsKey),
              let triage = try? JSONDecoder().decode(AttentionTriage.self, from: data)
        else { return AttentionTriage() }
        return triage
    }

    func save(to defaults: UserDefaults = .standard) {
        guard let data = try? JSONEncoder().encode(self) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }
}
