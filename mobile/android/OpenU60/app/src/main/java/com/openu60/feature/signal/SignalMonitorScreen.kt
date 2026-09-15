package com.openu60.feature.signal

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.openu60.core.components.AnimatedNumber
import com.openu60.core.network.AuthState

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SignalMonitorScreen(
    viewModel: SignalMonitorViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsState()
    val authState by viewModel.authState.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Signal Monitor") })
        },
    ) { padding ->
        if (authState != AuthState.LOGGED_IN) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
                contentAlignment = Alignment.Center,
            ) {
                Text("Login required to monitor signal")
            }
            return@Scaffold
        }

        PullToRefreshBox(
            isRefreshing = state.isLoading,
            onRefresh = { viewModel.refresh() },
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                if (state.error != null) {
                    Card(
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.errorContainer,
                        ),
                    ) {
                        Text(
                            state.error!!,
                            modifier = Modifier.padding(16.dp),
                            color = MaterialTheme.colorScheme.onErrorContainer,
                        )
                    }
                }

                // Operator info bar
                val op = state.operatorInfo
                if (op.provider.isNotBlank()) {
                    Text(
                        "${op.provider} | ${op.displayNetworkType(state.nr.isConnected, state.lte)} | Signal: ${op.signalBar}/5",
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }

                // NR Signal Panel
                val nr = state.nr
                val nrCaActive = nr.sccCarriers.isNotEmpty()
                if (nr.hasSignal) {
                    SignalPanel(
                        title = "5G NR",
                        badge = if (nrCaActive) "${1 + nr.sccCarriers.size} CC" else null,
                        rows = listOf(
                            SignalRow("RSRP", nr.rsrp?.let { "${it.toInt()} dBm" } ?: "--", rsrpColor(nr.rsrp), numericValue = nr.rsrp?.toInt(), numericSuffix = " dBm"),
                            SignalRow("RSRQ", nr.rsrq?.let { "${it.toInt()} dB" } ?: "--", numericValue = nr.rsrq?.toInt(), numericSuffix = " dB"),
                            SignalRow("SINR", nr.sinr?.let { "${it.toInt()} dB" } ?: "--", sinrColor(nr.sinr), numericValue = nr.sinr?.toInt(), numericSuffix = " dB"),
                            SignalRow("RSSI", nr.rssi?.let { "${it.toInt()} dBm" } ?: "--", numericValue = nr.rssi?.toInt(), numericSuffix = " dBm"),
                            SignalRow("Band", if (nr.band.isNotBlank()) "n${nr.band}${if (nrCaActive) " · PCC" else ""}" else "--"),
                            SignalRow("PCI", nr.pci.ifBlank { "--" }),
                            SignalRow("Cell ID", nr.cellID.ifBlank { "--" }),
                            SignalRow("ARFCN", nr.channel.ifBlank { "--" }),
                            SignalRow("Bandwidth", nr.bandwidth.ifBlank { "--" }),
                            SignalRow("CA", if (nrCaActive) "Active (${1 + nr.sccCarriers.size} CC)" else "Inactive"),
                        ),
                        sccCarriers = nr.sccCarriers,
                    )
                }

                // LTE Signal Panel
                val lte = state.lte
                val lteCaActive = lte.caState != "0" && lte.sccCarriers.isNotEmpty()
                val showNR = nr.hasSignal
                if (lte.hasSignal) {
                    SignalPanel(
                        title = "LTE",
                        badge = if (showNR) "NSA Anchor" else null,
                        badgeColor = if (showNR) Color(0xFFFF9800) else Color(0xFF2196F3),
                        secondBadge = if (lteCaActive) "${1 + lte.sccCarriers.size} CC" else null,
                        rows = listOf(
                            SignalRow("RSRP", lte.rsrp?.let { "${it.toInt()} dBm" } ?: "--", rsrpColor(lte.rsrp), numericValue = lte.rsrp?.toInt(), numericSuffix = " dBm"),
                            SignalRow("RSRQ", lte.rsrq?.let { "${it.toInt()} dB" } ?: "--", numericValue = lte.rsrq?.toInt(), numericSuffix = " dB"),
                            SignalRow("SINR", lte.sinr?.let { "${it.toInt()} dB" } ?: "--", sinrColor(lte.sinr), numericValue = lte.sinr?.toInt(), numericSuffix = " dB"),
                            SignalRow("RSSI", lte.rssi?.let { "${it.toInt()} dBm" } ?: "--", numericValue = lte.rssi?.toInt(), numericSuffix = " dBm"),
                            SignalRow("Band", if (lte.band.isNotBlank()) "B${lte.band}${if (lteCaActive) " · PCC" else ""}" else "--"),
                            SignalRow("PCI", lte.pci.ifBlank { "--" }),
                            SignalRow("Cell ID", lte.cellID.ifBlank { "--" }),
                            SignalRow("EARFCN", lte.earfcn.ifBlank { "--" }),
                            SignalRow("Bandwidth", lte.bandwidth.ifBlank { "--" }),
                            SignalRow("CA", if (lteCaActive) "Active (${1 + lte.sccCarriers.size} CC)" else "Inactive"),
                        ),
                        sccCarriers = lte.sccCarriers,
                    )
                }

                // WCDMA Signal Panel
                val wcdma = state.wcdma
                if (wcdma.rscp != null || wcdma.ecio != null) {
                    SignalPanel(
                        title = "WCDMA",
                        rows = listOf(
                            SignalRow("RSCP", wcdma.rscp?.let { "${it.toInt()} dBm" } ?: "--", numericValue = wcdma.rscp?.toInt(), numericSuffix = " dBm"),
                            SignalRow("Ec/Io", wcdma.ecio?.let { "${it.toInt()} dB" } ?: "--", numericValue = wcdma.ecio?.toInt(), numericSuffix = " dB"),
                        ),
                    )
                }

                // RSRP History chart
                if (state.history.size > 1) {
                    RSRPHistoryCard(state.history)
                }
            }
        }
    }
}

private data class SignalRow(
    val label: String,
    val value: String,
    val color: Color = Color.Unspecified,
    val numericValue: Int? = null,
    val numericSuffix: String = "",
)

@Composable
private fun SignalPanel(
    title: String,
    rows: List<SignalRow>,
    badge: String? = null,
    badgeColor: Color = Color(0xFF2196F3),
    secondBadge: String? = null,
    sccCarriers: List<com.openu60.core.model.LTECarrier> = emptyList(),
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    title,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                )
                if (badge != null) {
                    Spacer(modifier = Modifier.width(6.dp))
                    Text(
                        badge,
                        style = MaterialTheme.typography.labelSmall,
                        fontWeight = FontWeight.Bold,
                        color = badgeColor,
                        modifier = Modifier
                            .background(badgeColor.copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                            .padding(horizontal = 6.dp, vertical = 2.dp),
                    )
                }
                if (secondBadge != null) {
                    Spacer(modifier = Modifier.width(4.dp))
                    Text(
                        secondBadge,
                        style = MaterialTheme.typography.labelSmall,
                        fontWeight = FontWeight.Bold,
                        color = Color(0xFFFF9800),
                        modifier = Modifier
                            .background(Color(0xFFFF9800).copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                            .padding(horizontal = 6.dp, vertical = 2.dp),
                    )
                }
            }
            Spacer(modifier = Modifier.height(8.dp))
            rows.forEach { row ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 2.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        row.label,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    if (row.numericValue != null) {
                        AnimatedNumber(
                            value = row.numericValue,
                            suffix = row.numericSuffix,
                            style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.Medium),
                            color = if (row.color != Color.Unspecified) row.color else MaterialTheme.colorScheme.onSurface,
                        )
                    } else {
                        Text(
                            row.value,
                            style = MaterialTheme.typography.bodyMedium,
                            fontWeight = FontWeight.Medium,
                            color = if (row.color != Color.Unspecified) row.color else MaterialTheme.colorScheme.onSurface,
                        )
                    }
                }
            }
            if (sccCarriers.isNotEmpty()) {
                HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
                sccCarriers.forEach { carrier ->
                    SCCCarrierSection(carrier)
                }
            }
        }
    }
}

@Composable
private fun RSRPHistoryCard(history: List<com.openu60.core.model.SignalSnapshot>) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                "RSRP History",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
            )
            Spacer(modifier = Modifier.height(8.dp))

            val nrValues = history.mapNotNull { it.nrRSRP }
            val lteValues = history.mapNotNull { it.lteRSRP }

            if (nrValues.isNotEmpty()) {
                Text(
                    "NR: min ${nrValues.min().toInt()} / avg ${nrValues.average().toInt()} / max ${nrValues.max().toInt()} dBm",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            if (lteValues.isNotEmpty()) {
                Text(
                    "LTE: min ${lteValues.min().toInt()} / avg ${lteValues.average().toInt()} / max ${lteValues.max().toInt()} dBm",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Spacer(modifier = Modifier.height(8.dp))

            // Simple text-based sparkline for RSRP history
            val values = nrValues.ifEmpty { lteValues }
            if (values.size >= 2) {
                val barChars = "\u2581\u2582\u2583\u2584\u2585\u2586\u2587\u2588"
                val minVal = values.min().toFloat()
                val maxVal = values.max().toFloat()
                val range = (maxVal - minVal).coerceAtLeast(1f)
                val sparkline = values.joinToString("") { v ->
                    val idx = ((v.toFloat() - minVal) / range * (barChars.length - 1)).toInt()
                        .coerceIn(0, barChars.length - 1)
                    barChars[idx].toString()
                }
                Text(
                    sparkline,
                    style = MaterialTheme.typography.headlineSmall,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }
    }
}

@Composable
private fun SCCCarrierSection(carrier: com.openu60.core.model.LTECarrier) {
    Column(modifier = Modifier.padding(vertical = 4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                carrier.label,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.width(6.dp))
            Text(
                "SCC",
                style = MaterialTheme.typography.labelSmall,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .background(
                        MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.12f),
                        shape = MaterialTheme.shapes.small,
                    )
                    .padding(horizontal = 5.dp, vertical = 1.dp),
            )
        }
        Text(
            buildString {
                if (carrier.band.isNotBlank()) append("B${carrier.band}")
                if (carrier.pci.isNotBlank()) append(" · PCI ${carrier.pci}")
                if (carrier.bandwidth.isNotBlank()) append(" · BW ${carrier.bandwidth}")
            }.ifBlank { "--" },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(
            modifier = Modifier.fillMaxWidth().padding(top = 2.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text("RSRP ${carrier.rsrp?.let { "${it.toInt()}" } ?: "--"}", style = MaterialTheme.typography.bodySmall, color = rsrpColor(carrier.rsrp))
            Text("RSRQ ${carrier.rsrq?.let { "${it.toInt()}" } ?: "--"}", style = MaterialTheme.typography.bodySmall)
            Text("SINR ${carrier.sinr?.let { "${it.toInt()}" } ?: "--"}", style = MaterialTheme.typography.bodySmall, color = sinrColor(carrier.sinr))
            Text("RSSI ${carrier.rssi?.let { "${it.toInt()}" } ?: "--"}", style = MaterialTheme.typography.bodySmall)
        }
    }
}

private fun rsrpColor(rsrp: Double?): Color {
    if (rsrp == null) return Color.Unspecified
    return when {
        rsrp >= -80 -> Color(0xFF4CAF50)
        rsrp >= -100 -> Color(0xFFFFEB3B)
        rsrp >= -110 -> Color(0xFFFF9800)
        else -> Color(0xFFF44336)
    }
}

private fun sinrColor(sinr: Double?): Color {
    if (sinr == null) return Color.Unspecified
    return when {
        sinr >= 20 -> Color(0xFF4CAF50)
        sinr >= 10 -> Color(0xFFFFEB3B)
        sinr >= 0 -> Color(0xFFFF9800)
        else -> Color(0xFFF44336)
    }
}
