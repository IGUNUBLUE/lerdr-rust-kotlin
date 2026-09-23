package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.SessionRepository
import java.time.Instant
import java.time.OffsetDateTime
import java.time.format.DateTimeParseException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import lerdr.core.model.Inbound
import lerdr.core.model.PushPolicyMessage
import lerdr.core.model.PushPolicyResultMessage
import lerdr.core.model.PushPolicyView
import lerdr.core.model.PushTestResultMessage
import lerdr.core.store.RelayStatus
import lerdr.core.transport.CommandException

/**
 * The relay's per-device push policy in editable form — the oracle's
 * `DevicePushPolicy` / the relay's `pushPolicyResponse`. [snoozeUntil] keeps
 * the wire's RFC3339 string verbatim so it round-trips without clock skew.
 */
@Immutable
data class PushPolicyUi(
    val deviceId: String = "",
    val locale: String = "en",
    /** Wire `categories` map — all six keys ride every `push_policy_set`. */
    val categories: Map<String, Boolean> = DEFAULT_CATEGORIES,
    val settleMs: Long = DEFAULT_SETTLE_MS,
    val cooldownMs: Long = DEFAULT_COOLDOWN_MS,
    val snoozed: Boolean = false,
    val snoozeUntil: String? = null,
    val updateOnce: Boolean = true,
) {
    companion object {
        const val CATEGORY_ATTENTION = "attention"
        const val CATEGORY_QUESTION = "question"
        const val CATEGORY_BRIEF = "brief"
        const val CATEGORY_FINISHED = "finished"
        const val CATEGORY_UPDATE = "update"
        const val CATEGORY_TEST = "test"

        const val DEFAULT_SETTLE_MS = 2_000L
        const val DEFAULT_COOLDOWN_MS = 30_000L

        /** Oracle `DEFAULT_PUSH_CATEGORIES` — finished starts opted out. */
        val DEFAULT_CATEGORIES: Map<String, Boolean> = mapOf(
            CATEGORY_ATTENTION to true,
            CATEGORY_QUESTION to true,
            CATEGORY_BRIEF to true,
            CATEGORY_FINISHED to false,
            CATEGORY_UPDATE to true,
            CATEGORY_TEST to true,
        )
    }
}

/** Oracle `PushTestState` — what the "Send test" row reports. */
@Immutable
sealed interface PushTestUi {
    data object Idle : PushTestUi
    data object Sending : PushTestUi

    /** `accepted` | `queued` — relay-side acceptance, never handset display. */
    data class Accepted(val result: String) : PushTestUi

    /** The wire stage that refused the test (`dropped`, `rate_limited`, …). */
    data class Rejected(val code: String) : PushTestUi
}

@Immutable
data class PushPolicyUiState(
    val relayId: String = "",
    val relayLabel: String = "",
    val connected: Boolean = false,
    val connecting: Boolean = false,
    /** The relay's `push_config` frame arrived — capabilities are final. */
    val capabilitiesKnown: Boolean = false,
    /** `push_config.capabilities` contains `push_policy`. */
    val supported: Boolean = false,
    /** Connected + capable + no policy yet + no failure — get in flight. */
    val loading: Boolean = false,
    /** The `push_policy_get` request failed — the card offers Retry. */
    val loadFailed: Boolean = false,
    val policy: PushPolicyUi? = null,
    /** One in-flight `push_policy_set` — edits serialize like the oracle. */
    val saving: Boolean = false,
    val policyError: String? = null,
    val test: PushTestUi = PushTestUi.Idle,
)

/**
 * Per-relay push-notification policy — the port of the oracle's
 * `NotificationSettings.svelte` + `push.ts` semantics:
 *
 * - on connect the oracle fires `push_policy_get` when the relay advertises
 *   the `push_policy` capability (`store.ts`); [bind] mirrors that — the
 *   rising edge of `connected && capable` fetches once per connection;
 * - edits apply optimistically through one serialized `push_policy_set`
 *   (`{categories, settle_ms, cooldown_ms, snoozed, update_once,
 *   snooze_until?}` — the whole editable map, no `device_id`/`locale`; the
 *   relay binds both from the authenticated identity). A rejected set
 *   restores the pre-edit policy and surfaces the `push_policy_result`
 *   code;
 * - snooze rides the same `push_policy_set` path (the oracle never calls
 *   `push_snooze` from this UI): off → `snoozed:false`; a duration →
 *   `snoozed:true` + `snooze_until = now + duration` RFC3339; indefinite →
 *   `snoozed:true` with no `snooze_until`;
 * - "Send test" sends `push_test_device`; the `push_test_result` stage maps
 *   like the oracle (`service_accepted`/`queued`/`retrying` are accepted,
 *   everything else is a rejected code).
 *
 * The card's composable calls [bind] from `LaunchedEffect`; nothing else
 * pokes at lifecycle.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PushPolicyViewModel(
    private val sessions: SessionRepository,
    private val clock: () -> Long = System::currentTimeMillis,
) : ViewModel() {

    /** Relay this VM serves — empty until the section composes. */
    private val boundRelay = MutableStateFlow("")

    @Immutable
    private data class Local(
        val relayId: String = "",
        val policy: PushPolicyUi? = null,
        val loadFailed: Boolean = false,
        val saving: Boolean = false,
        val policyError: String? = null,
        val test: PushTestUi = PushTestUi.Idle,
    )

    private val local = MutableStateFlow(Local())

    /** Pre-edit snapshot — the oracle restores `previous` on a rejected set. */
    private var pendingRevert: PushPolicyUi? = null

    val uiState: StateFlow<PushPolicyUiState> = combine(
        boundRelay.flatMapLatest { relayId ->
            if (relayId.isEmpty()) flowOf(null) else sessions.connection(relayId)
        },
        local,
    ) { connection, local ->
        val connected = connection?.status == RelayStatus.CONNECTED
        // `applyPushConfig` always leaves protocol > 0 — it is the marker
        // that the capability list is final for this connection.
        val capabilitiesKnown = connection != null && connection.protocol > 0
        val supported = connected &&
            connection.capabilities.contains(PUSH_POLICY_CAPABILITY)
        PushPolicyUiState(
            relayId = local.relayId,
            relayLabel = connection?.relayLabel.orEmpty().ifEmpty { local.relayId },
            connected = connected,
            connecting = connection?.status == RelayStatus.CONNECTING,
            capabilitiesKnown = capabilitiesKnown,
            supported = supported,
            loading = connected && (!capabilitiesKnown || supported) &&
                local.policy == null && !local.loadFailed,
            loadFailed = local.loadFailed,
            policy = local.policy,
            saving = local.saving,
            policyError = local.policyError,
            test = local.test,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), PushPolicyUiState())

    init {
        viewModelScope.launch {
            boundRelay.collectLatest { relayId ->
                if (relayId.isEmpty()) return@collectLatest
                coroutineScope {
                    launch { collectFrames(relayId) }
                    launch { collectPolicyTrigger(relayId) }
                }
            }
        }
    }

    /**
     * Points this VM at [relayId] — the section's `LaunchedEffect(relayId)`
     * calls it. Re-binding swaps the frame/connection collectors and drops
     * the previous relay's policy state.
     */
    fun bind(relayId: String) {
        if (boundRelay.value == relayId) return
        pendingRevert = null
        local.value = Local(relayId = relayId)
        boundRelay.value = relayId
    }

    /** Retry hook for the card's Retry action after a failed get. */
    fun refreshPolicy() {
        val relayId = boundRelay.value
        if (relayId.isEmpty()) return
        local.update { it.copy(loadFailed = false) }
        viewModelScope.launch { requestPolicy(relayId) }
    }

    // ── edits → push_policy_set ───────────────────────────────────────

    fun setCategory(category: String, enabled: Boolean) = applyEdit { policy ->
        if (policy.categories[category] == enabled) {
            policy
        } else {
            policy.copy(categories = policy.categories + (category to enabled))
        }
    }

    fun setSettleMs(ms: Long) = applyEdit { it.copy(settleMs = ms.coerceAtLeast(0)) }

    fun setCooldownMs(ms: Long) = applyEdit { it.copy(cooldownMs = ms.coerceAtLeast(0)) }

    fun setUpdateOnce(enabled: Boolean) = applyEdit { it.copy(updateOnce = enabled) }

    /** Oracle `clearSnooze` — `snoozed:false`, `snooze_until` dropped. */
    fun clearSnooze() = applyEdit { it.copy(snoozed = false, snoozeUntil = null) }

    /** Oracle `withTimedSnooze` — `snooze_until = now + duration` (RFC3339). */
    fun snoozeFor(durationMs: Long) = applyEdit {
        if (durationMs <= 0) {
            it.copy(snoozed = false, snoozeUntil = null)
        } else {
            it.copy(
                snoozed = true,
                snoozeUntil = Instant.ofEpochMilli(clock())
                    .plusMillis(durationMs)
                    .toString(),
            )
        }
    }

    /** Oracle `withGlobalSnooze` — `snoozed:true`, no `snooze_until`. */
    fun snoozeIndefinitely() = applyEdit { it.copy(snoozed = true, snoozeUntil = null) }

    /**
     * Oracle `sendTargetedPushTest` — fire `push_test_device`, then let the
     * `push_test_result` frame on [SessionRepository.frames] land the
     * outcome. A refused send is the oracle's `rejectPushTest('disconnected')`.
     */
    fun sendTest() {
        val relayId = boundRelay.value
        if (relayId.isEmpty() || local.value.test == PushTestUi.Sending) return
        if (sessions.connectionNow(relayId)?.status != RelayStatus.CONNECTED) {
            local.update { it.copy(test = PushTestUi.Rejected("disconnected")) }
            return
        }
        local.update { it.copy(test = PushTestUi.Sending) }
        viewModelScope.launch {
            try {
                sessions.request(relayId, Inbound(type = ACTION_TEST_DEVICE))
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                local.update {
                    it.copy(
                        test = PushTestUi.Rejected(
                            when (failure) {
                                is CommandException ->
                                    failure.code ?: failure.phase ?: "failed"
                                else -> "disconnected"
                            },
                        ),
                    )
                }
            }
        }
    }

    // ── plumbing ──────────────────────────────────────────────────────

    /** The oracle's rising edge: `push_policy_get` once per live connection. */
    private suspend fun collectPolicyTrigger(relayId: String) = coroutineScope {
        sessions.connection(relayId)
            .map { connection ->
                connection?.status == RelayStatus.CONNECTED &&
                    connection.capabilities.contains(PUSH_POLICY_CAPABILITY)
            }
            .distinctUntilChanged()
            .collect { ready ->
                // The next connect edge refetches — the oracle re-sends the
                // get on every `push_config`.
                local.update { it.copy(loadFailed = false) }
                if (ready) launch { requestPolicy(relayId) }
            }
    }

    private suspend fun requestPolicy(relayId: String) {
        try {
            sessions.request(relayId, Inbound(type = ACTION_POLICY_GET))
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            local.update { it.copy(loadFailed = true) }
        }
    }

    /** Optimistic edit + serialized send — the oracle's `applyPolicy`. */
    private fun applyEdit(edit: (PushPolicyUi) -> PushPolicyUi) {
        val relayId = boundRelay.value
        val snapshot = local.value
        val current = snapshot.policy ?: return
        if (relayId.isEmpty() || snapshot.saving) return
        if (sessions.connectionNow(relayId)?.status != RelayStatus.CONNECTED) return
        val next = edit(current)
        if (next == current) return
        pendingRevert = current
        local.update { it.copy(policy = next, saving = true, policyError = null) }
        viewModelScope.launch {
            try {
                sessions.request(
                    relayId,
                    Inbound(type = ACTION_POLICY_SET, policy = next.toWire()),
                )
                pendingRevert = null
                local.update { it.copy(saving = false) }
            } catch (cancelled: CancellationException) {
                pendingRevert = null
                local.update { it.copy(saving = false) }
                throw cancelled
            } catch (failure: Exception) {
                failSave(null)
            }
        }
    }

    /**
     * Rejected set — restore the pre-edit snapshot and surface the failure.
     * Runs from the `push_policy_result(ok:false)` frame and/or the failed
     * `command_result` throw; both are idempotent, the coded frame wins.
     */
    private fun failSave(code: String?) {
        val previous = pendingRevert
        pendingRevert = null
        local.update {
            it.copy(
                policy = previous ?: it.policy,
                saving = false,
                policyError = when {
                    code != null ->
                        "The relay did not save this notification policy ($code)."
                    it.policyError != null -> it.policyError
                    else -> "The relay did not save this notification policy."
                },
            )
        }
    }

    private suspend fun collectFrames(relayId: String) {
        sessions.frames.collect { frame ->
            if (frame.relayId != relayId) return@collect
            when (val message = frame.message) {
                is PushPolicyMessage -> adoptPolicy(message.policy)
                is PushPolicyResultMessage -> {
                    // Oracle: `(push_policy|push_policy_result) && ok !== false`
                    // updates the store; a failed result only reports.
                    if (message.ok == false) {
                        failSave(message.code)
                    } else {
                        adoptPolicy(message.policy)
                    }
                }
                is PushTestResultMessage -> local.update {
                    it.copy(test = mapTestStage(message.stage))
                }
                else -> Unit
            }
        }
    }

    private fun adoptPolicy(view: PushPolicyView?) {
        val policy = normalizePolicy(view) ?: return
        local.update {
            it.copy(policy = policy, loadFailed = false, policyError = null)
        }
    }

    /**
     * `PushPolicyPayload.policy` — the editable subset the relay replaces
     * wholesale (`boundPushPolicy`); `snooze_until` rides only when set.
     */
    private fun PushPolicyUi.toWire(): JsonObject = buildJsonObject {
        putJsonObject("categories") {
            categories.forEach { (key, value) -> put(key, value) }
        }
        put("settle_ms", settleMs)
        put("cooldown_ms", cooldownMs)
        put("snoozed", snoozed)
        put("update_once", updateOnce)
        snoozeUntil?.let { put("snooze_until", it) }
    }

    /** Oracle `store.ts` `push_test_result` stage mapping. */
    private fun mapTestStage(stage: String?): PushTestUi =
        when (stage.orEmpty()) {
            "service_accepted" -> PushTestUi.Accepted("accepted")
            "queued", "retrying" -> PushTestUi.Accepted("queued")
            else -> PushTestUi.Rejected(
                stage?.takeIf { it.isNotEmpty() } ?: "dropped",
            )
        }

    /** Oracle `normalizePushPolicy` — `device_id` is required, rest defaults. */
    private fun normalizePolicy(view: PushPolicyView?): PushPolicyUi? {
        if (view == null) return null
        val deviceId = view.deviceId?.trim().orEmpty()
        if (deviceId.isEmpty()) return null
        val categories = PushPolicyUi.DEFAULT_CATEGORIES.toMutableMap()
        view.categories?.forEach { (key, value) ->
            if (key in categories) categories[key] = value
        }
        return PushPolicyUi(
            deviceId = deviceId,
            locale = view.locale?.takeIf { it.isNotEmpty() } ?: "en",
            categories = categories,
            settleMs = view.settleMs?.takeIf { it >= 0 } ?: PushPolicyUi.DEFAULT_SETTLE_MS,
            cooldownMs = view.cooldownMs?.takeIf { it >= 0 } ?: PushPolicyUi.DEFAULT_COOLDOWN_MS,
            snoozed = view.snoozed == true,
            updateOnce = view.updateOnce != false,
            snoozeUntil = view.snoozeUntil?.takeIf(::isRfc3339Instant),
        )
    }

    /** `Date.parse` on the wire's RFC3339 — accepts `Z` and `±hh:mm`. */
    private fun isRfc3339Instant(value: String): Boolean = try {
        OffsetDateTime.parse(value)
        true
    } catch (invalid: DateTimeParseException) {
        false
    }

    companion object {
        /** `push_config.capabilities` entry that gates `push_policy_get`. */
        const val PUSH_POLICY_CAPABILITY = "push_policy"
        const val ACTION_POLICY_GET = "push_policy_get"
        const val ACTION_POLICY_SET = "push_policy_set"
        const val ACTION_TEST_DEVICE = "push_test_device"
    }
}
