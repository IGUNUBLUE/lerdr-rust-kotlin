package com.lerdr.app.push

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/**
 * The `push_subscribe` retry schedule — a refused or lost subscribe must
 * re-arm instead of wedging until a disconnect edge. The relay restart /
 * socket-churn case loses the request on a dying socket while the mark
 * stays set, so the scheduled backoff is the only thing that ever retries.
 */
class PushSubscribeRetryTest {

    @Test
    fun `backoff doubles from the base delay`() {
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(1)).isEqualTo(5_000L)
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(2)).isEqualTo(10_000L)
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(3)).isEqualTo(20_000L)
    }

    @Test
    fun `backoff saturates at the ceiling without overflowing`() {
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(7)).isEqualTo(300_000L)
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(40)).isEqualTo(300_000L)
    }

    @Test
    fun `defensive attempt values never go below the base`() {
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(0)).isEqualTo(5_000L)
        assertThat(PushSubscriptionManager.subscribeRetryDelayMs(-3)).isEqualTo(5_000L)
    }
}
