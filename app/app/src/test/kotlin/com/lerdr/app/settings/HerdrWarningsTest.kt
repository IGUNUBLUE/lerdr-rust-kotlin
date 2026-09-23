package com.lerdr.app.settings

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.HerdrFeatureStatus
import org.junit.Test

/**
 * `herdrWarnings` — the oracle's `SettingsView.svelte` feature-warning
 * line, ported field-for-field.
 */
class HerdrWarningsTest {

    private fun feature(state: String, reason: String = "") =
        HerdrFeatureStatus(state = state, reason = reason)

    @Test
    fun supportedAndBenignUnknownAreFiltered() {
        val warnings = herdrWarnings(
            mapOf(
                "ordinary_json" to feature("supported"),
                "pane.read" to feature("unknown", "not_checked"),
                "tab.move" to feature("unknown", "not_advertised"),
            ),
        )
        assertThat(warnings).isEmpty()
    }

    @Test
    fun unsupportedMapsToServerMessages() {
        val warnings = herdrWarnings(
            mapOf(
                "workspace.move_block" to feature("unsupported", "method_not_supported"),
                "tab.move" to feature("unsupported", "capability_absent"),
            ),
        )
        assertThat(warnings).isEqualTo(
            "Workspace group reorder: Server upgrade needed" +
                " · Tab reorder: Server feature unavailable",
        )
    }

    @Test
    fun unknownWithoutBenignReasonAndReconnectReason() {
        val warnings = herdrWarnings(
            mapOf(
                "pane.read" to feature("unknown", "probe_timeout"),
                "direct_terminal" to feature("degraded", "reconnect_required"),
            ),
        )
        assertThat(warnings).isEqualTo(
            "Terminal reads: Could not check" +
                " · Direct terminal: Rechecking after Herdr reconnect",
        )
    }

    @Test
    fun unlabeledFeaturesKeepTheirWireName() {
        val warnings = herdrWarnings(
            mapOf("future.feature" to feature("unsupported", "method_not_supported")),
        )
        assertThat(warnings).isEqualTo("future.feature: Server upgrade needed")
    }

    @Test
    fun emptyFeaturesProduceNoLine() {
        assertThat(herdrWarnings(emptyMap())).isEmpty()
    }
}
