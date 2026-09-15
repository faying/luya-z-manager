package com.openu60.feature.bandlock

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.openu60.core.model.BandConfig
import com.openu60.core.network.AgentClient
import com.openu60.core.network.AgentError
import com.openu60.core.network.AuthManager
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

data class BandLockState(
    val selectedNRBands: Set<Int> = emptySet(),
    val selectedLTEBands: Set<Int> = emptySet(),
    val isLoading: Boolean = false,
    val error: String? = null,
    val successMessage: String? = null,
)

@HiltViewModel
class BandLockViewModel @Inject constructor(
    private val agentClient: AgentClient,
    private val authManager: AuthManager,
) : ViewModel() {

    private val _state = MutableStateFlow(BandLockState())
    val state: StateFlow<BandLockState> = _state.asStateFlow()

    fun toggleNRBand(band: Int) {
        val current = _state.value.selectedNRBands.toMutableSet()
        if (band in current) current.remove(band) else current.add(band)
        _state.value = _state.value.copy(selectedNRBands = current, successMessage = null)
    }

    fun toggleLTEBand(band: Int) {
        val current = _state.value.selectedLTEBands.toMutableSet()
        if (band in current) current.remove(band) else current.add(band)
        _state.value = _state.value.copy(selectedLTEBands = current, successMessage = null)
    }

    fun applyNRLock() {
        val bands = _state.value.selectedNRBands.sorted().joinToString(",")
        if (bands.isBlank()) return
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null, successMessage = null)
            try {
                agentClient.postJSON("/api/modem/bands/nr/lock", mapOf(
                    "nr5g_band" to bands,
                    "nr5g_type" to "nsa",
                ))
                agentClient.postJSON("/api/modem/bands/nr/lock", mapOf(
                    "nr5g_band" to bands,
                    "nr5g_type" to "sa",
                ))
                _state.value = _state.value.copy(
                    isLoading = false,
                    successMessage = "NR bands locked to: $bands",
                )
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) applyNRLock()
                else _state.value = _state.value.copy(isLoading = false, error = e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(isLoading = false, error = e.message)
            }
        }
    }

    fun applyLTELock() {
        val bands = _state.value.selectedLTEBands.sorted().joinToString(",")
        if (bands.isBlank()) return
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null, successMessage = null)
            try {
                agentClient.postJSON("/api/modem/bands/lte/lock", mapOf(
                    "lte_band_mask" to bands,
                    "is_lte_band" to "1",
                    "is_gw_band" to "0",
                    "gw_band_mask" to "",
                ))
                _state.value = _state.value.copy(
                    isLoading = false,
                    successMessage = "LTE bands locked to: $bands",
                )
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) applyLTELock()
                else _state.value = _state.value.copy(isLoading = false, error = e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(isLoading = false, error = e.message)
            }
        }
    }

    fun unlockAll() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null, successMessage = null)
            try {
                agentClient.deleteJSON("/api/modem/bands/lock")
                _state.value = _state.value.copy(
                    isLoading = false,
                    selectedNRBands = emptySet(),
                    selectedLTEBands = emptySet(),
                    successMessage = "All bands unlocked",
                )
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) unlockAll()
                else _state.value = _state.value.copy(isLoading = false, error = e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(isLoading = false, error = e.message)
            }
        }
    }
}
