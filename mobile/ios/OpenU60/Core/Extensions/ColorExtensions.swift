import SwiftUI

extension Color {
    /// Color based on RSRP signal strength thresholds.
    /// >= -80: green (excellent), >= -100: yellow (good), >= -110: orange (fair), < -110: red (poor)
    static func rsrpColor(_ rsrp: Double?) -> Color {
        guard let rsrp = rsrp else { return .gray }
        if rsrp >= -80 { return .green }
        if rsrp >= -100 { return .yellow }
        if rsrp >= -110 { return .orange }
        return .red
    }

    /// Color based on SINR/SNR thresholds.
    /// >= 20: green, >= 10: yellow, >= 0: orange, < 0: red
    static func sinrColor(_ sinr: Double?) -> Color {
        guard let sinr = sinr else { return .gray }
        if sinr >= 20 { return .green }
        if sinr >= 10 { return .yellow }
        if sinr >= 0 { return .orange }
        return .red
    }

    /// Color based on RSRQ thresholds.
    /// >= -10: green, >= -15: yellow, >= -20: orange, < -20: red
    static func rsrqColor(_ rsrq: Double?) -> Color {
        guard let rsrq = rsrq else { return .gray }
        if rsrq >= -10 { return .green }
        if rsrq >= -15 { return .yellow }
        if rsrq >= -20 { return .orange }
        return .red
    }

    /// Color for battery percentage.
    static func batteryColor(_ percent: Int) -> Color {
        if percent >= 50 { return .green }
        if percent >= 20 { return .yellow }
        return .red
    }

    /// Color for CPU usage percentage (0-100%).
    static func cpuUsageColor(_ percent: Double) -> Color {
        if percent < 50 { return .green }
        if percent < 80 { return .yellow }
        return .red
    }

    /// Signal quality label for RSRP.
    static func rsrpQuality(_ rsrp: Double?) -> String {
        guard let rsrp = rsrp else { return "No Signal" }
        if rsrp >= -80 { return "Excellent" }
        if rsrp >= -100 { return "Good" }
        if rsrp >= -110 { return "Fair" }
        return "Poor"
    }

    /// Signal bars (0-4) from RSRP.
    static func signalBars(_ rsrp: Double?) -> Int {
        guard let rsrp = rsrp else { return 0 }
        if rsrp >= -80 { return 4 }
        if rsrp >= -90 { return 3 }
        if rsrp >= -100 { return 2 }
        if rsrp >= -110 { return 1 }
        return 0
    }

    /// Color based on RSCP (3G) signal strength thresholds.
    static func rscpColor(_ rscp: Double?) -> Color {
        guard let rscp = rscp else { return .gray }
        if rscp >= -75 { return .green }
        if rscp >= -85 { return .yellow }
        if rscp >= -95 { return .orange }
        return .red
    }

    /// Signal quality label for RSCP (3G).
    static func rscpQuality(_ rscp: Double?) -> String {
        guard let rscp = rscp else { return "No Signal" }
        if rscp >= -75 { return "Excellent" }
        if rscp >= -85 { return "Good" }
        if rscp >= -95 { return "Fair" }
        return "Poor"
    }

    /// Signal bars (0-4) from RSCP (3G).
    static func rscpBars(_ rscp: Double?) -> Int {
        guard let rscp = rscp else { return 0 }
        if rscp >= -75 { return 4 }
        if rscp >= -85 { return 3 }
        if rscp >= -95 { return 2 }
        if rscp >= -105 { return 1 }
        return 0
    }

    /// Color based on Ec/Io (3G) thresholds.
    static func ecioColor(_ ecio: Double?) -> Color {
        guard let ecio = ecio else { return .gray }
        if ecio >= -6 { return .green }
        if ecio >= -10 { return .yellow }
        if ecio >= -15 { return .orange }
        return .red
    }
}
