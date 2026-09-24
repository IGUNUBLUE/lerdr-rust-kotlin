package com.lerdr.app.session

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import lerdr.core.model.Inbound
import lerdr.core.model.TargetRef
import lerdr.core.protocol.InboundCodec
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.Agent
import lerdr.core.terminal.AckGate
import lerdr.core.terminal.PaneSurface

/**
 * Wire-shaping helpers for pane/session commands. The oracle spreads
 * `agentTargetPayload(agent)` — `{target, server_session_id}` — plus the raw
 * `pane_id` onto every pane-directed frame; several fields the relay reads
 * (`content_fingerprint`, `interval_ms`, `activity_label`) never enter the
 * typed [Inbound] struct, so frames needing them are built as raw JSON here
 * and sent through `sendRaw`.
 */

private val RESOURCE_ID = Regex("^[A-Za-z0-9._:@%+-]{1,160}$")

/**
 * `targetRefForAgent` — the exact terminal identity a pane command needs.
 * Null when the relay has not reported a complete target tuple yet; the
 * oracle rejects the command ("no exact terminal identity") in that case.
 */
internal fun Agent.wireTarget(): TargetRef? {
    val serverSessionId = serverSessionId ?: return null
    val terminalId = terminalId ?: return null
    val generation = generation ?: return null
    if (!RESOURCE_ID.matches(serverSessionId) || !RESOURCE_ID.matches(rawPaneId) ||
        !RESOURCE_ID.matches(terminalId) || generation < 0
    ) {
        return null
    }
    return TargetRef(
        serverSessionId = serverSessionId,
        paneId = rawPaneId,
        terminalId = terminalId,
        generation = generation,
        agentSessionId = agentSessionId.orEmpty(),
        workspaceId = workspaceId,
        tabId = tabId,
    )
}

/** `{target, server_session_id}` — the shared suffix of every pane frame. */
private fun JsonObjectBuilder.putTarget(agent: Agent, target: TargetRef) {
    putJsonObject("target") {
        put("server_session_id", target.serverSessionId)
        put("pane_id", target.paneId)
        put("terminal_id", target.terminalId)
        put("generation", target.generation)
        if (target.agentSessionId.isNotEmpty()) {
            put("agent_session_id", target.agentSessionId)
        }
    }
    put("server_session_id", target.serverSessionId)
}

/**
 * Encode one [AckGate.Intent] to its wire frame for [agent]'s pane.
 * Returns null when the frame must not be sent (missing identity, or a
 * watch-frame while the relay lacks `pane_realtime_delta`) — the caller
 * then rolls the gate's optimistic mark back via [PaneSurface.onSendFailed].
 */
internal fun encodePaneIntent(
    intent: AckGate.Intent,
    agent: Agent?,
    surface: PaneSurface,
    budget: PaneBudget,
    realtimeDeltaCapable: Boolean,
): JsonObject? {
    val target = agent?.wireTarget() ?: return null
    return buildJsonObject {
        when (intent) {
            is AckGate.Intent.Applied -> {
                put("type", "pane_applied")
                put("pane_id", agent.rawPaneId)
                put("content_fingerprint", intent.contentFingerprint)
            }
            is AckGate.Intent.ReadPane -> {
                put("type", "read_pane")
                put("pane_id", agent.rawPaneId)
                put("lines", budget.lines)
                put("format", budget.format)
                put(
                    "content_fingerprint",
                    if (intent.force) "" else surface.fingerprint.orEmpty(),
                )
            }
            AckGate.Intent.Watch -> {
                if (!realtimeDeltaCapable) return null
                put("type", "watch_pane")
                put("pane_id", agent.rawPaneId)
                put("lines", budget.lines)
                put("interval_ms", budget.intervalMs)
                put("format", budget.format)
                put("content_fingerprint", surface.fingerprint.orEmpty())
            }
            AckGate.Intent.Unwatch -> {
                if (!realtimeDeltaCapable) return null
                put("type", "unwatch_pane")
                put("pane_id", agent.rawPaneId)
            }
        }
        putTarget(agent, target)
    }
}

/** `paneBudget()` — the read/watch geometry the user (later: settings) picks. */
data class PaneBudget(
    val lines: Int = DEFAULT_LINES,
    val intervalMs: Long = DEFAULT_INTERVAL_MS,
    val format: String = "ansi",
) {
    companion object {
        const val DEFAULT_LINES = 400
        const val DEFAULT_INTERVAL_MS = 500L
    }
}

/** `sendToAgent` — Inbound with the target identity filled in. */
internal fun Inbound.withPaneTarget(agent: Agent, target: TargetRef): Inbound = copy(
    paneId = agent.rawPaneId,
    target = target,
    serverSessionId = target.serverSessionId,
)

/**
 * Merge [extras] into the encoded [message] — for fields the flat `Inbound`
 * does not declare but the relay reads from the raw map (`activity_label`,
 * `source`, `speech_request_id`, `language`).
 */
internal fun withExtras(
    message: Inbound,
    extras: Map<String, kotlinx.serialization.json.JsonElement>,
): JsonObject {
    val base = LerdrJson.parseToJsonElement(InboundCodec.encode(message)) as? JsonObject
        ?: JsonObject(emptyMap())
    if (extras.isEmpty()) return base
    return JsonObject(base + extras)
}

internal fun stringExtras(
    vararg pairs: Pair<String, String>,
): Map<String, kotlinx.serialization.json.JsonElement> =
    pairs.associate { (k, v) -> k to JsonPrimitive(v) }
