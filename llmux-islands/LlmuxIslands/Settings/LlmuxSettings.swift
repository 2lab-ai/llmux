import Foundation

/// User-editable connection settings for the llmux daemon, backed by
/// UserDefaults. The Settings window writes these; `LlmuxClient.current()`
/// reads them. Defaults target the local loopback daemon.
enum LlmuxSettings {
    private static let defaults = UserDefaults.standard

    static var host: String {
        get { defaults.string(forKey: "llmux.host") ?? "127.0.0.1" }
        set { defaults.set(newValue, forKey: "llmux.host") }
    }

    static var port: Int {
        get {
            let v = defaults.integer(forKey: "llmux.port")
            return v == 0 ? 3456 : v
        }
        set { defaults.set(newValue, forKey: "llmux.port") }
    }

    static var apiKey: String {
        get { defaults.string(forKey: "llmux.apiKey") ?? "" }
        set { defaults.set(newValue, forKey: "llmux.apiKey") }
    }
}
