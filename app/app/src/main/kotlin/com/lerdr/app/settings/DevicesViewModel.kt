package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import java.net.URLEncoder
import java.time.Instant
import java.time.OffsetDateTime
import java.time.format.DateTimeFormatter
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull
import lerdr.core.data.DeviceRole
import lerdr.core.model.Inbound
import lerdr.core.store.RelayStatus
import lerdr.core.transport.RelaySession

/**
 * One paired device row — the oracle's `DeviceSummary`
 * (`device-auth.ts:38-46`) plus the wire `revoked` flag so a tombstone
 * that slips through the relay's filter still renders honestly.
 */
@Immutable
data class DeviceUi(
    val deviceId: String,
    val credentialId: String,
    val name: String,
    val role: DeviceRole,
    val pairedAtEpochMs: Long,
    /** null or ≤0 → "Never" (the wire emits Go's zero time for unseen). */
    val lastSeenAtEpochMs: Long?,
    /** The relay marked this row as the caller's own credential. */
    val current: Boolean,
    val revoked: Boolean,
)

/**
 * A minted one-use invitation rendered as a shareable link — the oracle
 * builds `location.href#setup=…&invite=…`; here the equivalent
 * `lerdr://pair#…` deep link that `SetupLink.parse` round-trips.
 */
@Immutable
data class InvitationUi(
    val link: String,
    val deviceName: String,
    val role: DeviceRole,
    val expiresAtEpochMs: Long,
    /** Decoded `qr_code` modules — null until the relay answers (or forever when it can't). */
    val qr: QrBitmapUi?,
)

/** `qrBitmap` (`frontend/src/lib/qr.ts`) — the relay's packed module matrix, unpacked. */
@Immutable
data class QrBitmapUi(
    /** QR width in modules (21..177). */
    val size: Int,
    /** `size*size` row-major dark-module flags (base64 bitfield, decoded). */
    val darkModules: List<Boolean>,
)

/** Everything the Devices card renders — one immutable snapshot. */
@Immutable
data class DevicesUiState(
    val relayLabel: String = "",
    val devices: List<DeviceUi> = emptyList(),
    /** Caller's device id (session handshake first, `device_list` answer refines). */
    val currentDeviceId: String = "",
    val connected: Boolean = false,
    /** Caller's role is `controller` — gates rename/revoke/invite/reset. */
    val canAdminister: Boolean = false,
    /** A registry endpoint exists to name in the invitation link. */
    val canInvite: Boolean = false,
    /** First `device_list` still in flight — the list area shows a spinner. */
    val loading: Boolean = false,
    /** Any `device_list` in flight — drives the refresh affordance. */
    val refreshing: Boolean = false,
    /** A dialog action (rename/revoke/invite/reset/forget) is in flight. */
    val actionBusy: Boolean = false,
    /** `device_list` answered at least once — empty-list copy only then. */
    val fetched: Boolean = false,
    /** Oracle `status` line — success text or failure detail. */
    val status: String? = null,
    val statusIsError: Boolean = false,
    /** Open invitation block — link (and QR when offered) + copy affordance. */
    val invitation: InvitationUi? = null,
)

/**
 * Devices card state owner — the `DeviceSettings.svelte` port.
 *
 * Wire calls (`SessionRepository.request`, payload in `command_result.data`):
 * - `device_list` → `{current_device_id, role, devices[]}` — each device is
 *   `device_id/credential_id/name/role/locale/paired_at/last_seen_at/
 *   version/revoked/current?` (RFC3339 timestamps).
 * - `rename_device` (`device_id`, `name`) → `{device}` — then re-list.
 * - `create_device_invitation` (`name`, `role`) → `{invitation}` — strict
 *   validation like the oracle before the link is composed.
 * - `revoke_device` (`device_id`) → `{device}` — then re-list; revoking our
 *   own row kills this session ~250 ms after the reply (deferred sweep).
 * - `reset_devices` → no data — wipes every credential incl. ours and
 *   re-arms bootstrap pairing on the relay; the list clears locally.
 * - `qr_code` (`text`) → `{size, modules}` when the relay advertises
 *   `invitation_qr`; failure leaves the copyable link standing alone.
 *
 * `refreshDevices` parity: a failed fetch only surfaces when the relay is
 * still connected — offline errors keep the stale list silently.
 */
class DevicesViewModel(
    private val relayId: String,
    private val sessions: SessionRepository,
) : ViewModel() {

    private val _uiState = MutableStateFlow(DevicesUiState())
    val uiState: StateFlow<DevicesUiState> = _uiState.asStateFlow()

    init {
        // Connection state + QR capability.
        viewModelScope.launch {
            sessions.connection(relayId).collect { connection ->
                val was = _uiState.value.connected
                val now = connection?.status == RelayStatus.CONNECTED
                _uiState.update { it.copy(connected = now) }
                if (now && !was) refresh()
            }
        }
        // Caller identity from the handshake (device_list refines it later).
        viewModelScope.launch {
            (sessions.sessionState(relayId) ?: flowOf(null)).collect { state ->
                val finish = (state as? RelaySession.SessionState.Connected)?.finish
                _uiState.update {
                    it.copy(
                        currentDeviceId = finish?.deviceId ?: it.currentDeviceId,
                        canAdminister = if (finish != null) {
                            DeviceRole.fromWireName(finish.role) == DeviceRole.CONTROLLER
                        } else {
                            it.canAdminister
                        },
                    )
                }
            }
        }
        // Relay endpoint → label + socket origin for the invitation link.
        viewModelScope.launch {
            sessions.relays.collect { relays ->
                val endpoint = relays.firstOrNull { it.id == relayId }
                _uiState.update {
                    it.copy(
                        relayLabel = endpoint?.label ?: relayId,
                        canInvite = endpoint != null,
                    )
                }
            }
        }
        refresh()
    }

    /** Header refresh affordance + post-mutation re-list (oracle `refreshDevices`). */
    fun refresh() {
        if (_uiState.value.refreshing) return
        viewModelScope.launch {
            _uiState.update { it.copy(refreshing = true, loading = !it.fetched) }
            try {
                applyList(sessions.request(relayId, Inbound(type = "device_list")).data)
            } catch (failure: Exception) {
                // Offline fetches keep the stale list quietly — the oracle
                // only rethrows while the connection is live.
                if (sessions.connectionNow(relayId)?.status == RelayStatus.CONNECTED) {
                    setStatus(failure.displayMessage(), isError = true)
                }
            } finally {
                _uiState.update { it.copy(refreshing = false, loading = false) }
            }
        }
    }

    /** `renameDevice` — `{type, device_id, name}` then re-list. */
    fun renameDevice(deviceId: String, name: String) {
        val trimmed = name.trim().take(MAX_DEVICE_NAME_CHARS)
        if (trimmed.isEmpty()) return
        runAction("Device name saved.") {
            sessions.request(
                relayId,
                Inbound(type = "rename_device", deviceId = deviceId, name = trimmed),
            )
            refreshDevices()
        }
    }

    /** `revokeDevice` — `{type, device_id}` then re-list. */
    fun revokeDevice(device: DeviceUi) {
        runAction("${device.name} was revoked.") {
            sessions.request(
                relayId,
                Inbound(type = "revoke_device", deviceId = device.deviceId),
            )
            refreshDevices()
        }
    }

    /**
     * `forgetCurrentDevice` — self-revoke. The relay sweeps this session
     * after answering; the local credential record erasure is the relay
     * row's Forget action (`SessionRepository.removeRelay`) — the revoked
     * credential can never authenticate again either way.
     */
    fun forgetCurrentDevice() {
        val deviceId = _uiState.value.currentDeviceId
        if (deviceId.isEmpty()) return
        runAction("This device was revoked at the relay.") {
            sessions.request(
                relayId,
                Inbound(type = "revoke_device", deviceId = deviceId),
            )
            refreshDevices()
        }
    }

    /**
     * `resetDevices` — `{type}` only. Success wipes every credential on the
     * relay (ours included — the sweep closes this session) and re-arms
     * bootstrap pairing; the local list clears like the oracle's.
     */
    fun resetDevices() {
        runAction("All device credentials were reset.") {
            sessions.request(relayId, Inbound(type = "reset_devices"))
            // The wiped invitation must not keep rendering a dead link.
            _uiState.update {
                it.copy(
                    devices = emptyList(),
                    fetched = true,
                    currentDeviceId = "",
                    invitation = null,
                )
            }
        }
    }

    /**
     * `createDeviceInvitation` — `{type, name, role}`; the reply's
     * `{invitation}` validates exactly like the oracle before it becomes a
     * `lerdr://pair#…` link. Then `qr_code` draws it when the relay
     * advertises `invitation_qr`.
     */
    fun createInvitation(name: String, role: DeviceRole) {
        val trimmed = name.trim().take(MAX_DEVICE_NAME_CHARS)
        if (trimmed.isEmpty()) return
        viewModelScope.launch {
            _uiState.update { it.copy(actionBusy = true, status = null, statusIsError = false) }
            try {
                val endpoint = sessions.relays.value.firstOrNull { it.id == relayId }
                    ?: throw IllegalStateException("Relay configuration is unavailable")
                val result = sessions.request(
                    relayId,
                    Inbound(type = "create_device_invitation", name = trimmed, role = role.wireName),
                )
                val invitation = parseInvitation(result.data)
                val link = invitationLink(endpoint.socketOrigin, endpoint.label, invitation)
                _uiState.update {
                    it.copy(
                        invitation = InvitationUi(
                            link = link,
                            deviceName = trimmed,
                            role = role,
                            expiresAtEpochMs = invitation.expiresAtEpochMs,
                            qr = null,
                        ),
                    )
                }
                setStatus(
                    "Invitation created. Share the one-use link below before it expires.",
                    isError = false,
                )
                // Detached like the oracle's `void onQrCode(link)` — the QR
                // pops in when the relay answers, busy is already clear.
                viewModelScope.launch { fetchQr(link) }
            } catch (failure: Exception) {
                setStatus(failure.displayMessage(), isError = true)
            } finally {
                _uiState.update { it.copy(actionBusy = false) }
            }
        }
    }

    /** Copy succeeded — oracle's `'Invitation link copied.'` status line. */
    fun invitationCopied() = setStatus("Invitation link copied.", isError = false)

    /** Clipboard write refused — the link stays selectable for manual copy. */
    fun invitationCopyFailed() = setStatus(
        "Copy failed. Select and copy the invitation link manually.",
        isError = true,
    )

    fun dismissInvitation() = _uiState.update { it.copy(invitation = null) }

    fun dismissStatus() = _uiState.update { it.copy(status = null, statusIsError = false) }

    // ── internals ─────────────────────────────────────────────────────

    /** Oracle `run` — busy latch, status/error plumbing, success text. */
    private fun runAction(success: String, action: suspend () -> Unit) {
        if (_uiState.value.actionBusy) return
        viewModelScope.launch {
            _uiState.update { it.copy(actionBusy = true, status = null, statusIsError = false) }
            try {
                action()
                setStatus(success, isError = false)
            } catch (failure: Exception) {
                setStatus(failure.displayMessage(), isError = true)
            } finally {
                _uiState.update { it.copy(actionBusy = false) }
            }
        }
    }

    /** `refreshDevices` for post-mutation re-listing — silent while offline. */
    private suspend fun refreshDevices() {
        try {
            applyList(sessions.request(relayId, Inbound(type = "device_list")).data)
        } catch (failure: Exception) {
            if (sessions.connectionNow(relayId)?.status == RelayStatus.CONNECTED) throw failure
        }
    }

    /** `qr_code` — best-effort; the link stands alone when it fails. */
    private suspend fun fetchQr(link: String) {
        val capable = sessions.connectionNow(relayId)
            ?.capabilities
            ?.contains(QR_CAPABILITY) == true
        if (!capable) return
        try {
            val qr = parseQr(sessions.request(relayId, Inbound(type = "qr_code", text = link)).data)
            _uiState.update { state ->
                state.copy(
                    invitation = state.invitation
                        ?.takeIf { it.link == link }
                        ?.copy(qr = qr),
                )
            }
        } catch (ignored: Exception) {
            // The link itself is still shown and copyable (oracle parity).
        }
    }

    private fun applyList(data: JsonElement?) {
        val obj = data as? JsonObject
        val devices = (obj?.get("devices") as? JsonArray)
            .orEmpty()
            .mapNotNull { (it as? JsonObject)?.toDeviceUi() }
        // Oracle sort: current first, last-seen desc, then name asc.
        val currentId = obj?.string("current_device_id") ?: _uiState.value.currentDeviceId
        _uiState.update {
            it.copy(
                devices = devices.sortedWith(
                    compareByDescending<DeviceUi> { device ->
                        device.current || device.deviceId == currentId
                    }
                        .thenByDescending { device -> device.lastSeenAtEpochMs ?: 0L }
                        .thenBy { device -> device.name },
                ),
                fetched = true,
                currentDeviceId = currentId,
                canAdminister = obj?.string("role")
                    ?.let(DeviceRole::fromWireName) == DeviceRole.CONTROLLER || it.canAdminister,
            )
        }
    }

    private fun setStatus(text: String, isError: Boolean) {
        _uiState.update { it.copy(status = text, statusIsError = isError) }
    }

    private fun JsonObject.toDeviceUi(): DeviceUi? {
        val deviceId = string("device_id") ?: return null
        val credentialId = string("credential_id") ?: return null
        val role = string("role")?.let(DeviceRole::fromWireName) ?: return null
        val pairedAt = parseWireTime(string("paired_at")) ?: return null
        return DeviceUi(
            deviceId = deviceId,
            credentialId = credentialId,
            name = (string("name") ?: "Device").take(MAX_SUMMARY_NAME_CHARS),
            role = role,
            pairedAtEpochMs = pairedAt,
            lastSeenAtEpochMs = parseWireTime(string("last_seen_at"))?.takeIf { it > 0L },
            current = this["current"].asBoolean() == true,
            revoked = this["revoked"].asBoolean() == true,
        )
    }

    private fun JsonObject.string(name: String): String? =
        (this[name] as? JsonPrimitive)?.takeIf { it.isString }?.content

    /** Null-safe boolean read — malformed types never throw here. */
    private fun JsonElement?.asBoolean(): Boolean? =
        (this as? JsonPrimitive)?.booleanOrNull

    private fun JsonElement?.asString(): String? =
        (this as? JsonPrimitive)?.takeIf { it.isString }?.content

    private fun JsonElement?.asLong(): Long? =
        (this as? JsonPrimitive)?.longOrNull

    private fun JsonElement?.asInt(): Int? =
        (this as? JsonPrimitive)?.intOrNull

    private fun parseWireTime(value: String?): Long? {
        if (value.isNullOrBlank()) return null
        return try {
            Instant.parse(value).toEpochMilli()
        } catch (invalid: java.time.format.DateTimeParseException) {
            try {
                OffsetDateTime.parse(value, DateTimeFormatter.ISO_OFFSET_DATE_TIME)
                    .toInstant()
                    .toEpochMilli()
            } catch (ignored: java.time.format.DateTimeParseException) {
                null
            }
        }
    }

    // ── invitation + qr parsing (oracle-strict) ───────────────────────

    /** The relay's `invitation` payload after validation. */
    @Immutable
    private data class ParsedInvitation(
        val id: String,
        val version: Long,
        val secret: String,
        val expiresAtEpochMs: Long,
    )

    /**
     * `createDeviceInvitation`'s field checks (`store.ts:1818-1829`): the
     * id/secret shapes are exact, the version is a positive safe integer,
     * and the expiry parses. Anything else is a relay bug worth flagging.
     */
    private fun parseInvitation(data: JsonElement?): ParsedInvitation {
        val invitation = (data as? JsonObject)?.get("invitation") as? JsonObject
            ?: throw InvalidInvitation()
        val id = invitation["invitation_id"].asString().orEmpty()
        val version = invitation["version"].asLong() ?: -1L
        val secret = invitation["secret"].asString().orEmpty()
        val expiresAt = parseWireTime(invitation["expires_at"].asString()) ?: -1L
        if (!INVITATION_ID.matches(id) || version < 1 ||
            !INVITATION_SECRET.matches(secret) || expiresAt < 0
        ) {
            throw InvalidInvitation()
        }
        return ParsedInvitation(id, version, secret, expiresAt)
    }

    private class InvalidInvitation :
        Exception("Relay returned an invalid device invitation")

    /**
     * The oracle's `link.hash` params on a `lerdr://pair` deep link —
     * `SetupLink.parse` round-trips exactly these fields.
     */
    private fun invitationLink(
        socketOrigin: String,
        relayLabel: String,
        invitation: ParsedInvitation,
    ): String = buildString {
        append("lerdr://pair#setup=").append(urlEncode(invitation.secret))
        append("&invite=").append(urlEncode(invitation.id))
        append("&invite_version=").append(invitation.version)
        append("&invite_expires=").append(invitation.expiresAtEpochMs)
        append("&label=").append(urlEncode(relayLabel))
        append("&relay=").append(urlEncode(socketOrigin))
    }

    /** `qrBitmap` — packed row-major MSB-first bits → per-module flags. */
    private fun parseQr(data: JsonElement?): QrBitmapUi {
        val obj = data as? JsonObject ?: throw InvalidQr()
        val size = obj["size"].asInt() ?: throw InvalidQr()
        if (size < QR_MIN_MODULES || size > QR_MAX_MODULES) throw InvalidQr()
        val encoded = obj["modules"].asString() ?: throw InvalidQr()
        val bytes = try {
            java.util.Base64.getDecoder().decode(encoded)
        } catch (invalid: IllegalArgumentException) {
            throw InvalidQr()
        }
        val expected = (size * size + 7) / 8
        if (bytes.size != expected) throw InvalidQr()
        val dark = List(size * size) { index ->
            bytes[index / 8].toInt() and (1 shl (7 - index % 8)) != 0
        }
        return QrBitmapUi(size = size, darkModules = dark)
    }

    private class InvalidQr : Exception("Relay returned an invalid QR bitmap")

    private fun Exception.displayMessage(): String =
        message?.takeIf { it.isNotBlank() } ?: "The device action failed."

    private fun urlEncode(value: String): String =
        URLEncoder.encode(value, Charsets.UTF_8.name())

    private companion object {
        /** Oracle `maxlength` on the name fields. */
        const val MAX_DEVICE_NAME_CHARS = 64

        /** The oracle's `.slice(0, 80)` clamp on the parsed summary name. */
        const val MAX_SUMMARY_NAME_CHARS = 80

        /** Relay capability that backs the `qr_code` action. */
        const val QR_CAPABILITY = "invitation_qr"

        /** `qrBitmap` bounds — a real QR symbol is 21..177 modules square. */
        const val QR_MIN_MODULES = 21
        const val QR_MAX_MODULES = 177
        val INVITATION_ID = Regex("^[A-Za-z0-9_-]{16,128}$")
        val INVITATION_SECRET = Regex("^[A-Za-z0-9_-]{43}$")
    }
}
