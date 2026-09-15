package com.openu60.feature.router.celllock

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.openu60.core.model.CellLockParser
import com.openu60.core.model.CellLockStatus
import com.openu60.core.model.NeighborCell
import com.openu60.core.network.AgentClient
import com.openu60.core.network.AgentError
import com.openu60.core.network.AuthManager
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

data class CellLockState(
    val status: CellLockStatus = CellLockStatus.empty,
    val neighbors: List<NeighborCell> = emptyList(),
    val isLoading: Boolean = false,
    val message: String? = null,
    val messageIsError: Boolean = false,
    val nrPCI: String = "",
    val nrEARFCN: String = "",
    val nrBand: String = "",
    val ltePCI: String = "",
    val lteEARFCN: String = "",
)

@HiltViewModel
class CellLockViewModel @Inject constructor(
    private val agentClient: AgentClient,
    private val authManager: AuthManager,
) : ViewModel() {

    private val _state = MutableStateFlow(CellLockState())
    val state: StateFlow<CellLockState> = _state.asStateFlow()

    fun updateField(field: String, value: String) {
        _state.value = when (field) {
            "nrPCI" -> _state.value.copy(nrPCI = value)
            "nrEARFCN" -> _state.value.copy(nrEARFCN = value)
            "nrBand" -> _state.value.copy(nrBand = value)
            "ltePCI" -> _state.value.copy(ltePCI = value)
            "lteEARFCN" -> _state.value.copy(lteEARFCN = value)
            else -> _state.value
        }
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, message = null)
            try {
                val data = agentClient.getJSON("/api/modem/cell-lock")
                val status = CellLockParser.parse(data)
                _state.value = _state.value.copy(status = status, isLoading = false)
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) refresh() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    fun lockCell() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, message = null)
            try {
                val params = mutableMapOf<String, Any?>()
                val s = _state.value
                if (s.nrPCI.isNotBlank()) params["nr_pci"] = s.nrPCI
                if (s.nrEARFCN.isNotBlank()) params["nr_earfcn"] = s.nrEARFCN
                if (s.nrBand.isNotBlank()) params["nr_band"] = s.nrBand
                if (s.ltePCI.isNotBlank()) params["lte_pci"] = s.ltePCI
                if (s.lteEARFCN.isNotBlank()) params["lte_earfcn"] = s.lteEARFCN
                agentClient.postJSON("/api/modem/cell-lock", params)
                _state.value = _state.value.copy(
                    isLoading = false,
                    message = "Cell lock applied",
                    messageIsError = false,
                )
                refresh()
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) lockCell() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    fun unlockCell() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, message = null)
            try {
                agentClient.deleteJSON("/api/modem/cell-lock")
                _state.value = _state.value.copy(
                    status = CellLockStatus.empty,
                    isLoading = false,
                    message = "Cell lock removed",
                    messageIsError = false,
                    nrPCI = "", nrEARFCN = "", nrBand = "",
                    ltePCI = "", lteEARFCN = "",
                )
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) unlockCell() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    fun scanNeighbors() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, message = null)
            try {
                agentClient.postJSON("/api/modem/neighbors")
                delay(3000)
                val data = agentClient.getJSON("/api/modem/neighbors")
                val neighbors = CellLockParser.parseNeighbors(data, "neighbor")
                _state.value = _state.value.copy(
                    neighbors = neighbors,
                    isLoading = false,
                    message = "Found ${neighbors.size} neighbor cells",
                    messageIsError = false,
                )
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) scanNeighbors() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    private fun setError(msg: String?) {
        _state.value = _state.value.copy(isLoading = false, message = msg ?: "Unknown error", messageIsError = true)
    }
}
