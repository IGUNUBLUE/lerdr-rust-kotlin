package com.lerdr.app.session

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.AgentsMessage
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.asPatch
import lerdr.core.store.normalizeAgent
import org.junit.Test

/**
 * Regression: a Rust-shaped `agents` payload — `server_session_id`,
 * `terminal_id`, and `generation` inline on the row — must survive
 * `normalizeAgent` as a complete target tuple. If any of those fields is
 * dropped in normalization, `wireTarget()` returns null and every
 * pane-directed command against a Rust relay silently refuses to send.
 */
class AgentWireTargetTest {

    private val rustAgentsFrame = """
        {
          "type": "agents",
          "agents": [
            {
              "pane_id": "wE:p1",
              "raw_pane_id": "wE:p1",
              "terminal_id": "term-1",
              "server_session_id": "primary",
              "generation": 0,
              "agent_session_id": "",
              "tab_id": "wE:t1",
              "tab_label": "main",
              "tab_number": 1,
              "workspace_id": "wE",
              "agent": "claude",
              "name": "claude-main",
              "status": "idle",
              "_focused": true,
              "cwd": "/home/relay/project",
              "session": "",
              "session_name": "",
              "updated_at": 0
            }
          ]
        }
    """.trimIndent()

    @Test
    fun rustAgentRowNormalizesIntoWireTarget() {
        val message = LerdrJson.decodeFromString(AgentsMessage.serializer(), rustAgentsFrame)
        val patch = message.agents!!.single().asPatch()
        val agent = normalizeAgent("relay-1", "workstation", patch)

        // Target fields survive normalization verbatim.
        assertThat(agent.serverSessionId).isEqualTo("primary")
        assertThat(agent.terminalId).isEqualTo("term-1")
        assertThat(agent.generation).isEqualTo(0)
        assertThat(agent.rawPaneId).isEqualTo("wE:p1")

        // …and assemble into a complete TargetRef (generation 0 is valid).
        val target = agent.wireTarget()
        assertThat(target).isNotNull()
        assertThat(target!!.serverSessionId).isEqualTo("primary")
        assertThat(target.paneId).isEqualTo("wE:p1")
        assertThat(target.terminalId).isEqualTo("term-1")
        assertThat(target.generation).isEqualTo(0)
        assertThat(target.agentSessionId).isEmpty()
    }

    @Test
    fun missingServerSessionIdIsNotTargetable() {
        val raw = rustAgentsFrame.replace(
            "\"server_session_id\": \"primary\",",
            "",
        )
        val message = LerdrJson.decodeFromString(AgentsMessage.serializer(), raw)
        val agent = normalizeAgent("relay-1", "workstation", message.agents!!.single().asPatch())

        assertThat(agent.serverSessionId).isNull()
        assertThat(agent.wireTarget()).isNull()
    }
}
