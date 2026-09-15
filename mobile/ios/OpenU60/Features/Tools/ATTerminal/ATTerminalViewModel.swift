import Foundation

struct ATHistoryEntry: Identifiable {
    let id = UUID()
    let command: String
    let response: String
    let port: String
    let elapsedMs: Int
    let timestamp: Date
    let isError: Bool
}

@Observable
@MainActor
final class ATTerminalViewModel {
    var history: [ATHistoryEntry] = []
    var currentCommand: String = ""
    var timeout: Int = 3
    var isLoading: Bool = false
    var portName: String?
    var portAvailable: Bool = false
    var showDangerConfirm: Bool = false
    var pendingDangerousCommand: String?

    private let client: AgentClient
    private let authManager: AuthManager

    private let dangerousPatterns = ["CFUN=0", "CFUN=4", "+CRESET", "&F", "+NVWR", "+QPOWD", "+COPS="]

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func checkPort() async {
        do {
            let data = try await client.getJSON("/api/at/port")
            portName = data["port"] as? String
            portAvailable = data["available"] as? Bool ?? false
        } catch {
            portAvailable = false
            portName = nil
        }
    }

    func send() {
        let cmd = currentCommand.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cmd.isEmpty else { return }
        guard cmd.uppercased().hasPrefix("AT") else {
            history.insert(ATHistoryEntry(command: cmd, response: "Error: Command must start with AT", port: "", elapsedMs: 0, timestamp: Date(), isError: true), at: 0)
            return
        }

        // Check for dangerous commands
        if dangerousPatterns.contains(where: { cmd.uppercased().contains($0) }) {
            pendingDangerousCommand = cmd
            showDangerConfirm = true
            return
        }

        Task { await executeSend(cmd) }
    }

    func confirmDangerousSend() {
        guard let cmd = pendingDangerousCommand else { return }
        pendingDangerousCommand = nil
        showDangerConfirm = false
        Task { await executeSend(cmd) }
    }

    private func executeSend(_ cmd: String) async {
        isLoading = true
        currentCommand = ""
        do {
            let body: [String: Any] = ["command": cmd, "timeout": timeout]
            let data = try await client.postJSON("/api/at/send", body: body)
            let response = data["response"] as? String ?? ""
            let port = data["port"] as? String ?? ""
            let elapsedMs = (data["elapsed_ms"] as? NSNumber)?.intValue ?? 0
            history.insert(ATHistoryEntry(command: cmd, response: response, port: port, elapsedMs: elapsedMs, timestamp: Date(), isError: false), at: 0)
        } catch {
            history.insert(ATHistoryEntry(command: cmd, response: "Error: \(error.localizedDescription)", port: "", elapsedMs: 0, timestamp: Date(), isError: true), at: 0)
        }
        isLoading = false
    }

    func clearHistory() {
        history.removeAll()
    }

    func insertCommand(_ cmd: String) {
        currentCommand = cmd
    }
}
