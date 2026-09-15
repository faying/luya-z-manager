package com.openu60.feature.tools.atterminal

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.widget.Toast
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Timer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

private val quickCommands = listOf("AT", "ATI", "AT+CSQ", "AT+COPS?", "AT+CPIN?", "AT+CGDCONT?", "AT+CGSN", "AT+CLCC")

@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
fun ATTerminalScreen(
    onBack: () -> Unit,
    viewModel: ATTerminalViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsState()
    val context = LocalContext.current
    val listState = rememberLazyListState()
    var showTimeoutMenu by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("AT Terminal")
                        if (state.portName != null) {
                            Text(
                                state.portName!!,
                                style = MaterialTheme.typography.bodySmall,
                                color = if (state.portAvailable) {
                                    MaterialTheme.colorScheme.primary
                                } else {
                                    MaterialTheme.colorScheme.error
                                },
                            )
                        }
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    if (state.history.isNotEmpty()) {
                        IconButton(onClick = { viewModel.clearHistory() }) {
                            Icon(Icons.Default.Delete, contentDescription = "Clear")
                        }
                    }
                },
            )
        },
        bottomBar = {
            Surface(tonalElevation = 3.dp) {
                Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
                    // Quick-insert chips
                    LazyRow(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        modifier = Modifier.padding(bottom = 8.dp),
                    ) {
                        items(quickCommands) { cmd ->
                            AssistChip(
                                onClick = {
                                    viewModel.setCommand(cmd)
                                },
                                label = {
                                    Text(
                                        cmd,
                                        style = MaterialTheme.typography.labelSmall,
                                        fontFamily = FontFamily.Monospace,
                                    )
                                },
                            )
                        }
                    }
                    // Input row
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        OutlinedTextField(
                            value = state.currentCommand,
                            onValueChange = { viewModel.setCommand(it) },
                            placeholder = { Text("AT command...") },
                            singleLine = true,
                            textStyle = LocalTextStyle.current.copy(fontFamily = FontFamily.Monospace),
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        // Timeout selector
                        Box {
                            IconButton(onClick = { showTimeoutMenu = true }) {
                                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                    Icon(
                                        Icons.Default.Timer,
                                        contentDescription = "Timeout",
                                        modifier = Modifier.size(16.dp),
                                    )
                                    Text(
                                        "${state.timeout}s",
                                        style = MaterialTheme.typography.labelSmall,
                                    )
                                }
                            }
                            DropdownMenu(
                                expanded = showTimeoutMenu,
                                onDismissRequest = { showTimeoutMenu = false },
                            ) {
                                listOf(1, 3, 5, 10, 15, 30).forEach { t ->
                                    DropdownMenuItem(
                                        text = { Text("${t}s") },
                                        onClick = {
                                            viewModel.setTimeout(t)
                                            showTimeoutMenu = false
                                        },
                                    )
                                }
                            }
                        }
                        // Send button
                        IconButton(
                            onClick = { viewModel.send() },
                            enabled = state.currentCommand.isNotBlank() && !state.isLoading,
                        ) {
                            if (state.isLoading) {
                                CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                            } else {
                                Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
                            }
                        }
                    }
                }
            }
        },
    ) { padding ->
        if (state.history.isEmpty()) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
                contentAlignment = Alignment.Center,
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(
                        "No commands sent yet",
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        "Tap a quick command or type one below",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        } else {
            LazyColumn(
                state = listState,
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
                contentPadding = PaddingValues(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(state.history, key = { it.id }) { entry ->
                    HistoryEntryCard(
                        entry = entry,
                        onLongPress = {
                            val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                            clipboard.setPrimaryClip(ClipData.newPlainText("AT Response", entry.response))
                            Toast.makeText(context, "Response copied", Toast.LENGTH_SHORT).show()
                        },
                    )
                }
            }
        }
    }

    // Dangerous command confirmation dialog
    if (state.showDangerConfirm) {
        AlertDialog(
            onDismissRequest = { viewModel.dismissDangerConfirm() },
            title = { Text("Dangerous Command") },
            text = {
                Text("\"${state.pendingDangerousCommand}\" may disable the modem, reset settings, or disconnect the network. Are you sure?")
            },
            confirmButton = {
                TextButton(
                    onClick = { viewModel.confirmDangerousSend() },
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) {
                    Text("Send Anyway")
                }
            },
            dismissButton = {
                TextButton(onClick = { viewModel.dismissDangerConfirm() }) {
                    Text("Cancel")
                }
            },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun HistoryEntryCard(
    entry: ATHistoryEntry,
    onLongPress: () -> Unit,
) {
    val timeFormat = remember { SimpleDateFormat("HH:mm:ss", Locale.getDefault()) }

    Card(
        colors = if (entry.isError) {
            CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.errorContainer.copy(alpha = 0.3f),
            )
        } else {
            CardDefaults.cardColors()
        },
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(
                onClick = {},
                onLongClick = onLongPress,
            ),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            // Command line
            Text(
                "> ${entry.command}",
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Bold,
                color = if (entry.isError) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.primary
                },
            )
            Spacer(modifier = Modifier.height(4.dp))
            // Response
            Text(
                entry.response,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                color = if (entry.isError) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurface
                },
                lineHeight = 18.sp,
            )
            Spacer(modifier = Modifier.height(6.dp))
            // Caption: timestamp, port, elapsed
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    timeFormat.format(Date(entry.timestamp)),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (entry.port.isNotEmpty()) {
                    Text(
                        "${entry.port} \u00B7 ${entry.elapsedMs}ms",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}
