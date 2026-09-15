package com.openu60.core.model

import kotlin.math.abs
import kotlin.math.pow
import kotlin.math.roundToLong

data class BatteryStatus(
    val capacity: Int = 0,
    val temperature: Double = 0.0,
    val charging: String = "",
    val chargeStatus: Int = 0,
    val timeToFull: Int = -1,
    val timeToEmpty: Int = -1,
    val currentMA: Int? = null,
    val voltageMV: Int? = null,
) {
    companion object {
        val empty = BatteryStatus()
    }
}

data class ThermalStatus(
    val cpuTemp: Double = 0.0,
) {
    companion object {
        val empty = ThermalStatus()
    }
}

data class TrafficStats(
    val rxBytes: Long = 0,
    val txBytes: Long = 0,
    val timestamp: Long = System.currentTimeMillis(),
    val source: String = "",
    val precomputedRxRate: Double? = null,
    val precomputedTxRate: Double? = null,
    val serverRxSpeed: Double? = null,
    val serverTxSpeed: Double? = null,
) {
    companion object {
        val empty = TrafficStats()
    }
}

data class TrafficSpeed(
    val downloadBytesPerSec: Double = 0.0,
    val uploadBytesPerSec: Double = 0.0,
) {
    companion object {
        val zero = TrafficSpeed()
    }
}

data class ConnectedDevice(
    val id: String,
    val name: String,
    val ipAddress: String,
    val ip6Addresses: List<String>,
    val macAddress: String,
    val dhcpHostname: String,
) {
    val displayName: String
        get() = when {
            dhcpHostname.isNotEmpty() -> dhcpHostname
            name.isNotEmpty() -> name
            else -> macAddress
        }
}

data class DeviceIdentity(
    val imei: String = "",
    val simICCID: String = "",
    val simIMSI: String = "",
    val msisdn: String = "",
    val wanIPv4: String = "",
    val wanIPv6: List<String> = emptyList(),
    val lanIP: String = "",
    val spn: String = "",
    val mcc: String = "",
    val mnc: String = "",
    val simStatus: String = "",
) {
    companion object {
        val empty = DeviceIdentity()
    }
}

data class WifiStatus(
    val wifiOn: Boolean = false,
    val ssid2g: String = "",
    val ssid5g: String = "",
    val channel2g: String = "",
    val channel5g: String = "",
    val radio2gDisabled: Boolean = false,
    val radio5gDisabled: Boolean = false,
    val encryption2g: String = "",
    val encryption5g: String = "",
    val hidden2g: Boolean = false,
    val hidden5g: Boolean = false,
    val txPower2g: String = "",
    val txPower5g: String = "",
    val bandwidth2g: String = "",
    val bandwidth5g: String = "",
    val clientsTotal: Int = 0,
    val wifi6: Boolean = false,
    val guestEnabled: Boolean = false,
    val guestSsid: String = "",
) {
    companion object {
        val empty = WifiStatus()
    }
}

data class CpuStatSample(
    val idle: Long,
    val total: Long,
)

data class SystemInfo(
    val cpuUsagePercent: Double = 0.0,
    val cpuUsageIsEstimate: Boolean = true,
    val cpuCores: Int = 1,
    val uptime: Int = 0,
    val memTotal: Long = 0,
    val memFree: Long = 0,
) {
    companion object {
        val empty = SystemInfo()
    }
}

data class USBStatus(
    val mode: String = "",
    val typecCC: String = "no_cc",
    val dataConnected: Boolean = false,
    val powerbankActive: Boolean = false,
) {
    val cableAttached: Boolean get() = typecCC != "no_cc"

    companion object {
        val empty = USBStatus()
    }
}

// MARK: - Parsers

object DeviceParser {

    fun parseBattery(data: Map<String, Any?>): BatteryStatus {
        return BatteryStatus(
            capacity = asInt(data["battery_capacity"]) ?: 0,
            temperature = asDouble(data["battery_temperature"]) ?: 0.0,
            charging = "",
            timeToFull = asInt(data["battery_time_to_full"]) ?: -1,
            timeToEmpty = asInt(data["battery_time_to_empty"]) ?: -1,
        )
    }

    fun parseCharger(data: Map<String, Any?>, battery: BatteryStatus, chargeControl: Map<String, Any?>? = null): BatteryStatus {
        val chargeStatus = asInt(data["charge_status"]) ?: 0
        val chargerConnected = asInt(data["charger_connect"]) == 1
        val chargingStopped = chargeControl?.get("charging_stopped") as? Boolean ?: false
        val charging = when {
            chargerConnected && chargingStopped -> "stopped"
            chargeStatus == 1 -> "charging"
            else -> "discharging"
        }
        return battery.copy(chargeStatus = chargeStatus, charging = charging)
    }

    fun parseThermal(data: Map<String, Any?>): ThermalStatus {
        return ThermalStatus(cpuTemp = asDouble(data["cpuss_temp"]) ?: 0.0)
    }

    fun parseTraffic(data: Map<String, Any?>): TrafficStats {
        val stats = data["statistics"] as? Map<*, *> ?: emptyMap<String, Any?>()
        return TrafficStats(
            rxBytes = asLong(stats["rx_bytes"]) ?: 0,
            txBytes = asLong(stats["tx_bytes"]) ?: 0,
            timestamp = System.currentTimeMillis(),
        )
    }

    fun parseWwandstTraffic(data: Map<String, Any?>): TrafficStats? {
        val rx = asLong(data["real_rx_bytes"]) ?: return null
        val tx = asLong(data["real_tx_bytes"]) ?: 0
        val rxRate = asDouble(data["real_rx_speed"])
        val txRate = asDouble(data["real_tx_speed"])
        return TrafficStats(
            rxBytes = rx,
            txBytes = tx,
            timestamp = System.currentTimeMillis(),
            source = "wwandst",
            precomputedRxRate = if (rxRate != null && txRate != null && (rxRate > 0 || txRate > 0)) rxRate else null,
            precomputedTxRate = if (rxRate != null && txRate != null && (rxRate > 0 || txRate > 0)) txRate else null,
        )
    }

    fun computeSpeed(previous: TrafficStats, current: TrafficStats): TrafficSpeed {
        // Priority 1: server-computed speeds from zte-agent
        val sRx = current.serverRxSpeed
        val sTx = current.serverTxSpeed
        if (sRx != null && sTx != null) {
            return TrafficSpeed(downloadBytesPerSec = sRx, uploadBytesPerSec = sTx)
        }
        // Priority 2: pre-computed rates from ZTE daemon
        val pRx = current.precomputedRxRate
        val pTx = current.precomputedTxRate
        if (pRx != null && pTx != null) {
            return TrafficSpeed(downloadBytesPerSec = pRx, uploadBytesPerSec = pTx)
        }
        // Priority 3: client-side delta (skip when source changes)
        if (previous.source.isNotEmpty() && current.source.isNotEmpty() && previous.source != current.source) {
            return TrafficSpeed.zero
        }
        val elapsed = (current.timestamp - previous.timestamp) / 1000.0
        if (elapsed <= 0) return TrafficSpeed.zero
        val rxDelta = if (current.rxBytes > previous.rxBytes) current.rxBytes - previous.rxBytes else 0
        val txDelta = if (current.txBytes > previous.txBytes) current.txBytes - previous.txBytes else 0
        return TrafficSpeed(
            downloadBytesPerSec = rxDelta / elapsed,
            uploadBytesPerSec = txDelta / elapsed,
        )
    }

    fun parseHostHints(data: Map<String, Any?>): List<ConnectedDevice> {
        val devices = mutableListOf<ConnectedDevice>()
        for ((mac, value) in data) {
            val info = value as? Map<*, *> ?: continue
            val name = info["name"] as? String ?: ""
            val ipAddrs = info["ipaddrs"] as? List<*> ?: emptyList<String>()
            val ip6Addrs = info["ip6addrs"] as? List<*> ?: emptyList<String>()
            val ip = ipAddrs.firstOrNull()?.toString() ?: ""
            devices.add(ConnectedDevice(
                id = mac,
                name = name,
                ipAddress = ip,
                ip6Addresses = ip6Addrs.mapNotNull { it?.toString() },
                macAddress = mac,
                dhcpHostname = "",
            ))
        }
        return devices.sortedWith(compareBy { it.ipAddress })
    }

    fun enrichWithDHCP(devices: List<ConnectedDevice>, leases: List<Map<String, Any?>>): List<ConnectedDevice> {
        val leaseMap = mutableMapOf<String, String>()
        for (lease in leases) {
            val mac = (lease["macaddr"] as? String)?.uppercase() ?: continue
            val hostname = lease["hostname"] as? String ?: continue
            leaseMap[mac] = hostname
        }
        return devices.map { device ->
            val hostname = leaseMap[device.macAddress.uppercase()]
            if (hostname != null) device.copy(dhcpHostname = hostname) else device
        }
    }

    fun parseIdentity(
        simInfo: Map<String, Any?>,
        imeiData: Map<String, Any?>,
        wanStatus: Map<String, Any?>,
        wan6Status: Map<String, Any?>,
        lanStatus: Map<String, Any?>,
    ): DeviceIdentity {
        val wanIPv4 = (wanStatus["ipv4-address"] as? List<*>)
            ?.firstOrNull()?.let { (it as? Map<*, *>)?.get("address") as? String } ?: ""

        val wanIPv6 = (wan6Status["ipv6-address"] as? List<*>)
            ?.mapNotNull { entry ->
                val addr = (entry as? Map<*, *>)?.get("address") as? String
                addr?.takeIf { !it.startsWith("fe80") }
            } ?: emptyList()

        val lanIP = (lanStatus["ipv4-address"] as? List<*>)
            ?.firstOrNull()?.let { (it as? Map<*, *>)?.get("address") as? String } ?: ""

        val spnHex = simInfo["spn_name_data"] as? String
        val spn = if (spnHex != null) decodeSpn(spnHex) else ""

        return DeviceIdentity(
            imei = imeiData["imei"] as? String ?: "",
            simICCID = simInfo["sim_iccid"] as? String ?: "",
            simIMSI = simInfo["sim_imsi"] as? String ?: "",
            msisdn = simInfo["msisdn"] as? String ?: "",
            wanIPv4 = wanIPv4,
            wanIPv6 = wanIPv6,
            lanIP = lanIP,
            spn = spn,
            mcc = simInfo["mdm_mcc"] as? String ?: "",
            mnc = simInfo["mdm_mnc"] as? String ?: "",
            simStatus = simInfo["sim_states"] as? String ?: "",
        )
    }

    // MARK: - SPN Decoder

    fun decodeSpn(hex: String): String {
        val trimmed = hex.trim()
        if (trimmed.isEmpty() || trimmed.length % 4 != 0) return ""
        val sb = StringBuilder()
        var i = 0
        while (i + 3 < trimmed.length) {
            val code = trimmed.substring(i, i + 4).toIntOrNull(16)
            if (code != null && code != 0) {
                sb.append(code.toChar())
            }
            i += 4
        }
        return sb.toString()
    }

    // MARK: - USB Parser

    fun parseUSBStatus(usbData: Map<String, Any?>, chargerData: Map<String, Any?>?): USBStatus {
        return USBStatus(
            mode = usbData["mode"] as? String ?: "",
            typecCC = usbData["typec_cc"] as? String ?: "no_cc",
            dataConnected = asInt(usbData["connect"]) == 1,
            powerbankActive = asInt(chargerData?.get("otg_powerbank_state")) == 1,
        )
    }

    // MARK: - WiFi Parser

    fun parseWifiStatus(data: Map<String, Any?>): WifiStatus {
        return WifiStatus(
            wifiOn = (data["wifi_onoff"] as? String) == "1",
            ssid2g = data["main2g_ssid"] as? String ?: "",
            ssid5g = data["main5g_ssid"] as? String ?: "",
            radio2gDisabled = (data["radio2_disabled"] as? String) == "1",
            radio5gDisabled = (data["radio5_disabled"] as? String) == "1",
        )
    }

    fun formatEncryption(raw: String): String = when (raw.lowercase()) {
        "psk2", "psk2+ccmp" -> "WPA2"
        "sae" -> "WPA3"
        "sae-mixed", "sae+psk2" -> "WPA2/3"
        "psk-mixed", "psk+psk2" -> "WPA/2"
        "psk" -> "WPA"
        "none", "" -> "Open"
        else -> raw.uppercase()
    }

    // MARK: - System Parser

    fun parseSystemInfo(data: Map<String, Any?>, cpuCores: Int = 1): SystemInfo {
        var cpuUsage = 0.0
        val load = data["load"] as? List<*>
        if (load != null && load.isNotEmpty()) {
            val load1 = asDouble(load[0]) ?: 0.0
            val loadAvg = load1 / 65536.0
            cpuUsage = minOf(loadAvg / maxOf(cpuCores, 1).toDouble() * 100.0, 100.0)
        }
        val uptime = asInt(data["uptime"]) ?: 0
        val memoryMap = data["memory"] as? Map<*, *>
        val memTotal = asLong(data["memory_total"]) ?: asLong(memoryMap?.get("total")) ?: 0
        val memFree = asLong(data["memory_free"]) ?: asLong(memoryMap?.get("free")) ?: 0
        return SystemInfo(
            cpuUsagePercent = cpuUsage,
            cpuUsageIsEstimate = true,
            cpuCores = cpuCores,
            uptime = uptime,
            memTotal = memTotal,
            memFree = memFree,
        )
    }

    // MARK: - WAN Parser

    fun parseWanIPv4(data: Map<String, Any?>): String {
        val ipv4Arr = data["ipv4-address"] as? List<*> ?: return ""
        val first = ipv4Arr.firstOrNull() as? Map<*, *> ?: return ""
        return first["address"] as? String ?: ""
    }

    fun parseWanIPv6(data: Map<String, Any?>): String {
        val ipv6Arr = data["ipv6-address"] as? List<*>
        if (ipv6Arr != null) {
            for (entry in ipv6Arr) {
                val addr = (entry as? Map<*, *>)?.get("address") as? String
                if (addr != null && !addr.startsWith("fe80")) return addr
            }
        }
        val ipv6Prefix = data["ipv6-prefix-assignment"] as? List<*>
        if (ipv6Prefix != null) {
            for (entry in ipv6Prefix) {
                val addr = (entry as? Map<*, *>)?.get("address") as? String
                if (addr != null && !addr.startsWith("fe80")) return addr
            }
        }
        return ""
    }

    // MARK: - Formatting

    data class FormattedValue(val number: Double, val unit: String, val decimalPlaces: Int)

    private fun adaptiveDecimals(value: Double): Int {
        val a = abs(value)
        return when {
            a < 10 -> 2
            a < 100 -> 1
            else -> 0
        }
    }

    private fun roundTo(v: Double, decimals: Int): Double {
        val factor = 10.0.pow(decimals)
        return (v * factor).roundToLong() / factor
    }

    fun speedComponents(bytesPerSec: Double): FormattedValue {
        val bits = bytesPerSec * 8.0
        val gb = 1_000_000_000.0
        val mb = 1_000_000.0
        val kb = 1_000.0
        val (raw, unit) = when {
            bits >= gb -> bits / gb to " Gb/s"
            bits >= mb -> bits / mb to " Mb/s"
            bits >= kb -> bits / kb to " Kb/s"
            else -> bits to " b/s"
        }
        return FormattedValue(number = roundTo(raw, 1), unit = unit, decimalPlaces = 1)
    }

    fun bytesComponents(bytes: Long): FormattedValue {
        val b = bytes.toDouble()
        val tb = 1024.0 * 1024.0 * 1024.0 * 1024.0
        val gb = 1024.0 * 1024.0 * 1024.0
        val mb = 1024.0 * 1024.0
        val kb = 1024.0
        val (raw, unit) = when {
            b >= tb -> b / tb to " TB"
            b >= gb -> b / gb to " GB"
            b >= mb -> b / mb to " MB"
            b >= kb -> b / kb to " KB"
            else -> b to " B"
        }
        val dp = adaptiveDecimals(raw)
        return FormattedValue(number = roundTo(raw, dp), unit = unit, decimalPlaces = dp)
    }

    fun formatSpeed(bytesPerSec: Double): String {
        val c = speedComponents(bytesPerSec)
        return "%.${c.decimalPlaces}f${c.unit.trim()}".format(c.number)
    }

    fun formatBytes(bytes: Long): String {
        val c = bytesComponents(bytes)
        return "%.${c.decimalPlaces}f${c.unit.trim()}".format(c.number)
    }

    // MARK: - Helpers

    fun asInt(value: Any?): Int? = when (value) {
        is Int -> value
        is Long -> value.toInt()
        is Double -> value.toInt()
        is String -> value.toIntOrNull()
        else -> null
    }

    fun asDouble(value: Any?): Double? = when (value) {
        is Double -> value
        is Int -> value.toDouble()
        is Long -> value.toDouble()
        is String -> value.toDoubleOrNull()
        else -> null
    }

    fun asLong(value: Any?): Long? = when (value) {
        is Long -> value
        is Int -> value.toLong()
        is Double -> value.toLong()
        is String -> value.toLongOrNull()
        else -> null
    }

    fun asBool(value: Any?): Boolean = when (value) {
        is Boolean -> value
        is String -> value == "1" || value.lowercase() == "true" || value.lowercase() == "on"
        is Int -> value != 0
        else -> false
    }
}

// MARK: - Process Monitor

data class ProcessInfo(
    val pid: Int,
    val name: String,
    val cpuPct: Double,
    val rssKb: Long,
    val state: String,
    val isBloat: Boolean,
)

data class ProcessListResponse(
    val processes: List<ProcessInfo>,
    val totalCount: Int,
    val bloatCount: Int,
    val bloatCpuPct: Double,
    val bloatRssKb: Long,
)

data class KilledProcess(
    val pid: Int,
    val name: String,
)

data class KillBloatResponse(
    val killed: List<KilledProcess>,
    val skipped: List<KilledProcess>,
    val freedRssKb: Long,
)
