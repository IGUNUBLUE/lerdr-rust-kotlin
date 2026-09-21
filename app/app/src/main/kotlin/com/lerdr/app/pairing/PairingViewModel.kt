package com.lerdr.app.pairing

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import java.net.URLEncoder
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import lerdr.core.data.InvitePayload
import com.lerdr.navigation.SetupLink as NavSetupLink

/** What the pairing screen renders — one phase, one optional error detail. */
data class PairingUiState(
    val phase: Phase = Phase.IDLE,
    val error: Error? = null,
) {
    enum class Phase { IDLE, CONNECTING, SUCCESS }

    /** FailureVector-mapped user-visible buckets. */
    enum class Error(val message: String) {
        INVALID_LINK("This setup link is not valid"),
        INVITATION_EXPIRED("This invitation has expired — ask for a new link"),
        REJECTED("The relay rejected this device — the link may already be used"),
        UNREACHABLE("Could not reach the relay — check it is running and retry"),
        FAILED("Pairing failed — try again"),
    }
}

/**
 * Owns the pairing mutation point: link → [InvitePayload] → [PairingManager]
 * → UI state. Success is signaled once; the screen navigates Home on it.
 */
class PairingViewModel(
    private val pairingManager: PairingManager,
) : ViewModel() {

    private val _uiState = MutableStateFlow(PairingUiState())
    val uiState: StateFlow<PairingUiState> = _uiState.asStateFlow()

    /** Deep-link path — the nav key's SetupLink re-validated strictly. */
    fun connect(link: NavSetupLink) {
        val payload = link.toInvitePayload()
        if (payload == null) {
            _uiState.value = PairingUiState(error = PairingUiState.Error.INVALID_LINK)
            return
        }
        pair(payload)
    }

    /** Scan path — QR-decoded text through the same strict parser. */
    fun connectScanned(text: String) = connectPasted(text)

    /** Paste path — raw link text through the strict parser directly. */
    fun connectPasted(text: String) {
        when (val result = lerdr.core.data.SetupLink.parse(text)) {
            is lerdr.core.data.SetupLinkResult.Parsed -> pair(result.payload)
            lerdr.core.data.SetupLinkResult.Empty ->
                _uiState.value = PairingUiState(error = PairingUiState.Error.INVALID_LINK)
            else -> _uiState.value = PairingUiState(error = PairingUiState.Error.INVALID_LINK)
        }
    }

    fun pair(payload: InvitePayload) {
        if (_uiState.value.phase == PairingUiState.Phase.CONNECTING) return
        _uiState.value = PairingUiState(phase = PairingUiState.Phase.CONNECTING)
        viewModelScope.launch {
            _uiState.value = when (val outcome = pairingManager.pair(payload)) {
                is PairingOutcome.Success -> PairingUiState(phase = PairingUiState.Phase.SUCCESS)
                PairingOutcome.InvalidLink ->
                    PairingUiState(error = PairingUiState.Error.INVALID_LINK)
                PairingOutcome.InvitationExpired ->
                    PairingUiState(error = PairingUiState.Error.INVITATION_EXPIRED)
                is PairingOutcome.Rejected ->
                    PairingUiState(error = PairingUiState.Error.REJECTED)
                PairingOutcome.TimedOut ->
                    PairingUiState(error = PairingUiState.Error.UNREACHABLE)
                is PairingOutcome.Failed ->
                    PairingUiState(error = PairingUiState.Error.FAILED)
            }
        }
    }
}

/**
 * Rebuilds a `lerdr://pair#…` fragment from the navigation key's decoded
 * fields and re-parses it through the strict data-layer validator — retired
 * `gateways` links and unsafe origins die here, not in the registry.
 */
fun NavSetupLink.toInvitePayload(): InvitePayload? {
    if (gateways.isNotEmpty()) return null
    val fragment = buildString {
        append("setup=").append(urlEncode(setup))
        label?.let { append("&label=").append(urlEncode(it)) }
        relay?.let { append("&relay=").append(urlEncode(it)) }
        invite?.let { append("&invite=").append(urlEncode(it)) }
        inviteVersion?.let { append("&invite_version=").append(it) }
        inviteExpires?.let { append("&invite_expires=").append(it) }
        relayId?.let { append("&relay_id=").append(urlEncode(it)) }
        rendezvous?.let { append("&rendezvous=").append(urlEncode(it)) }
    }
    return lerdr.core.data.SetupLink.parseOrNull("lerdr://pair#$fragment")
}

private fun urlEncode(value: String): String =
    URLEncoder.encode(value, Charsets.UTF_8.name())
