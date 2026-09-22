package com.lerdr.navigation

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class LerdrDeepLinksTest {

    @Test
    fun agentLinkRoutesToAgentFeed() {
        val key = LerdrDeepLinks.match("lerdr://agent?pane_id=pane-42")
        assertThat(key).isEqualTo(LerdrKey.AgentFeed("pane-42"))
    }

    @Test
    fun agentLinkDecodesPaneId() {
        val key = LerdrDeepLinks.match("lerdr://agent?pane_id=ws%3A1%3Apane%3A7")
        assertThat(key).isEqualTo(LerdrKey.AgentFeed("ws:1:pane:7"))
    }

    @Test
    fun agentLinkWithoutPaneIdDrops() {
        assertThat(LerdrDeepLinks.match("lerdr://agent")).isNull()
        assertThat(LerdrDeepLinks.match("lerdr://agent?pane_id=")).isNull()
    }

    @Test
    fun agentsLinkRoutesHome() {
        assertThat(LerdrDeepLinks.match("lerdr://agents")).isEqualTo(LerdrKey.Home)
    }

    @Test
    fun pairLinkStillRoutes() {
        val key = LerdrDeepLinks.match("lerdr://pair")
        assertThat(key).isEqualTo(LerdrKey.Pairing(null))
    }

    @Test
    fun foreignSchemeDrops() {
        assertThat(LerdrDeepLinks.match("https://agent?pane_id=x")).isNull()
    }
}
