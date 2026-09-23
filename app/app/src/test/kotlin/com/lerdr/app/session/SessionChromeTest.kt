package com.lerdr.app.session

import com.google.common.truth.Truth.assertThat
import org.junit.Test

/** `statusVariantOf` — the chip-variant derivation behind `SessionTopBar`. */
class SessionChromeTest {

    @Test
    fun `terminal lease labels map to the amber lease variant`() {
        assertThat(statusVariantOf("lease 92×42")).isEqualTo(SessionStatusVariant.LEASE)
        assertThat(statusVariantOf("lease 80 cols")).isEqualTo(SessionStatusVariant.LEASE)
        assertThat(statusVariantOf("Lease 24x80")).isEqualTo(SessionStatusVariant.LEASE)
    }

    @Test
    fun `blocked and waiting labels map to the waiting variant`() {
        assertThat(statusVariantOf("blocked")).isEqualTo(SessionStatusVariant.WAITING)
        assertThat(statusVariantOf("Waiting for approval"))
            .isEqualTo(SessionStatusVariant.WAITING)
        assertThat(statusVariantOf("needs attention"))
            .isEqualTo(SessionStatusVariant.WAITING)
    }

    @Test
    fun `failure and disconnect labels map to the error variant`() {
        assertThat(statusVariantOf("offline")).isEqualTo(SessionStatusVariant.ERROR)
        assertThat(statusVariantOf("error")).isEqualTo(SessionStatusVariant.ERROR)
        assertThat(statusVariantOf("relay disconnected"))
            .isEqualTo(SessionStatusVariant.ERROR)
        assertThat(statusVariantOf("failed")).isEqualTo(SessionStatusVariant.ERROR)
        assertThat(statusVariantOf("unauthorized")).isEqualTo(SessionStatusVariant.ERROR)
    }

    @Test
    fun `working, done, idle, and live labels stay neutral`() {
        assertThat(statusVariantOf("working")).isEqualTo(SessionStatusVariant.NEUTRAL)
        assertThat(statusVariantOf("done")).isEqualTo(SessionStatusVariant.NEUTRAL)
        assertThat(statusVariantOf("idle")).isEqualTo(SessionStatusVariant.NEUTRAL)
        assertThat(statusVariantOf("live")).isEqualTo(SessionStatusVariant.NEUTRAL)
        assertThat(statusVariantOf("connected")).isEqualTo(SessionStatusVariant.NEUTRAL)
        assertThat(statusVariantOf("")).isEqualTo(SessionStatusVariant.NEUTRAL)
    }
}
