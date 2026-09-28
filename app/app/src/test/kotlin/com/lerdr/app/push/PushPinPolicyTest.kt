package com.lerdr.app.push

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * The keep-alive pin predicate — `shouldPin` gates RelaySyncService:
 * pin iff a socket is live AND push cannot reach a dead process. Once
 * `push_subscribed` lands ([PushUiState.deliversWhileDead]) the pin must
 * stay off — that is the battery-warning fix: no permanent FGS while
 * UnifiedPush carries dead-app delivery.
 */
class PushPinPolicyTest {

    private fun push(stage: PushStage, relays: Set<String> = emptySet()) =
        PushUiState(stage = stage, subscribedRelays = relays)

    @Test
    fun `pin arms while connected and push not subscribed`() {
        listOf(
            PushStage.NO_DISTRIBUTOR,
            PushStage.NEEDS_PICK,
            PushStage.REGISTERING,
            PushStage.ENDPOINT_READY,
            PushStage.FAILED,
        ).forEach { stage ->
            assertThat(push(stage).deliversWhileDead).isFalse()
        }
        assertThat(
            PushSubscriptionManager.shouldPin(connected = true, deliversWhileDead = false),
        ).isTrue()
    }

    @Test
    fun `pin stays off once a relay acked the endpoint`() {
        val subscribed = push(PushStage.SUBSCRIBED, relays = setOf("r1"))
        assertThat(subscribed.deliversWhileDead).isTrue()
        assertThat(
            PushSubscriptionManager.shouldPin(
                connected = true,
                deliversWhileDead = subscribed.deliversWhileDead,
            ),
        ).isFalse()
    }

    @Test
    fun `subscribed stage without any acked relay is not coverage`() {
        // Defensive — the UI can stage-flap; only a real ack frees the pin.
        val hollow = push(PushStage.SUBSCRIBED, relays = emptySet())
        assertThat(hollow.deliversWhileDead).isFalse()
    }

    @Test
    fun `nothing pins while no relay is connected`() {
        assertThat(
            PushSubscriptionManager.shouldPin(connected = false, deliversWhileDead = false),
        ).isFalse()
        assertThat(
            PushSubscriptionManager.shouldPin(connected = false, deliversWhileDead = true),
        ).isFalse()
    }
}
