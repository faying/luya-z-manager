package com.openu60.feature.tools.atterminal

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.openu60.core.network.AgentClient
import com.openu60.core.network.AgentError
import com.openu60.core.network.AuthManager
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.util.UUID
import javax.inject.Inject

data class ATHistoryEntry(
    val id: String = UUID.randomUUID().toString(),
    val command: String,
    val response: String,
    val port: String,
    val elapsedMs: Int,
    val timestamp: Long = System.currentTimeMillis(),
    val isError: Boolean = false,
)

data class ATTerminalState(
    val history: List<ATHistoryEntry> = emptyList(),
    val currentCommand: String = "",
    val timeout: Int = 3,
    val isLoading: Boolean = false,
    val portName: String? = null,
    val portAvailable: Boolean = false,
    val error: String? = null,
    val showDangerConfirm: Boolean = false,
    val pendingDangerousCommand: String? = null,
)

@HiltViewModel
class ATTerminalViewModel @Inject constructor(
    private val agentClient: AgentClient,
    private val authManager: AuthManager,
) : ViewModel() {

    private val _state = MutableStateFlow(ATTerminalState())
    val state: StateFlow<ATTerminalState> = _state.asStateFlow()

    private val dangerousPatterns = listOf("CFUN=0", "CFUN=4", "+CRESET", "&F", "+NVWR", "+QPOWD", "+COPS=")

    init {
        checkPort()
    }

    fun checkPort() {
        viewModelScope.launch {
            try {
                val data = agentClient.getJSON("/api/at/port")
                _state.value = _state.value.copy(
                    portName = data["port"] as? String,
                    portAvailable = data["available"] as? Boolean ?: false,
                )
            } catch (e: Exception) {
                _state.value = _state.value.copy(portAvailable = false, portName = null)
            }
        }
    }

    fun setCommand(cmd: String) {
        _state.value = _state.value.copy(currentCommand = cmd)
    }

    fun setTimeout(t: Int) {
        _state.value = _state.value.copy(timeout = t.coerceIn(1, 30))
    }

    fun send() {
        val cmd = _state.value.currentCommand.trim()
        if (cmd.isEmpty()) return
        if (!cmd.uppercase().startsWith("AT")) {
            addEntry(ATHistoryEntry(command = cmd, response = "Error: Command must start with AT", port = "", elapsedMs = 0, isError = true))
            return
        }
        if (dangerousPatterns.any { cmd.uppercase().contains(it) }) {
            _state.value = _state.value.copy(showDangerConfirm = true, pendingDangerousCommand = cmd)
            return
        }
        executeSend(cmd)
    }

    fun confirmDangerousSend() {
        val cmd = _state.value.pendingDangerousCommand ?: return
        _state.value = _state.value.copy(showDangerConfirm = false, pendingDangerousCommand = null)
        executeSend(cmd)
    }

    fun dismissDangerConfirm() {
        _state.value = _state.value.copy(showDangerConfirm = false, pendingDangerousCommand = null)
    }

    fun clearHistory() {
        _state.value = _state.value.copy(history = emptyList())
    }

    private fun executeSend(cmd: String) {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, currentCommand = "")
            try {
                val body = mapOf("command" to cmd, "timeout" to _state.value.timeout)
                val data = agentClient.postJSON("/api/at/send", body)
                val response = data["response"] as? String ?: ""
                val port = data["port"] as? String ?: ""
                val elapsedMs = (data["elapsed_ms"] as? Number)?.toInt() ?: 0
                addEntry(ATHistoryEntry(command = cmd, response = response, port = port, elapsedMs = elapsedMs))
            } catch (e: AgentError.Unauthorized) {
                authManager.reauthenticate()
                addEntry(ATHistoryEntry(command = cmd, response = "Error: Session expired", port = "", elapsedMs = 0, isError = true))
            } catch (e: Exception) {
                addEntry(ATHistoryEntry(command = cmd, response = "Error: ${e.message}", port = "", elapsedMs = 0, isError = true))
            }
            _state.value = _state.value.copy(isLoading = false)
        }
    }

    private fun addEntry(entry: ATHistoryEntry) {
        _state.value = _state.value.copy(history = listOf(entry) + _state.value.history)
    }
}
