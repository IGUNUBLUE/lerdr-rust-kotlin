package com.lerdr.app.settings

import com.google.common.truth.Truth.assertThat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import lerdr.core.data.RelayInvitation
import lerdr.core.protocol.LerdrJson
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

@OptIn(ExperimentalCoroutinesApi::class)
class RelayDetailViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    /** viewModelScope rides Dispatchers.Main — redirect it into the test. */
    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

    @Test
    fun `detail row reflects the live connection`() = runTest {
        val h = SettingsViewModelTest.Harness(this, tmp.root)
        val viewModel = h.detailViewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }

        h.registry.upsert(h.endpoint)
        h.await { viewModel.uiState.value.relay != null }
        assertThat(viewModel.uiState.value.relay?.statusLabel).isEqualTo("offline")

        h.repository.connect(h.endpoint)
        h.handle().connect()
        h.handle().emit(
            json(
                """{"type":"push_config","version":"0.4.2","protocol":3,"inventory":{"state":"ready"}}""",
            ),
        )
        h.pump()

        val row = viewModel.uiState.value.relay!!
        assertThat(row.statusLabel).isEqualTo("connected")
        assertThat(row.detailLabel).contains("websocket")
        assertThat(row.detailLabel).contains("0.4.2")
    }

    @Test
    fun `reconnect dials a registry relay that has no session yet`() = runTest {
        val h = SettingsViewModelTest.Harness(this, tmp.root)
        val viewModel = h.detailViewModel()
        backgroundScope.launch { viewModel.uiState.collect { } }

        // Repository not started — registry row with no session runtime.
        h.registry.upsert(h.endpoint)
        h.await { viewModel.uiState.value.relay != null }
        assertThat(h.factory.created).isEmpty()

        viewModel.reconnect()
        h.pump()
        assertThat(h.factory.created).containsKey("ws://192.168.1.5:7474/ws")
        assertThat(viewModel.uiState.value.relay?.statusLabel)
            .isEqualTo("connecting…")
    }

    @Test
    fun `reconnect on a live session runs the revalidate probe`() = runTest {
        val h = SettingsViewModelTest.Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.repository.connect(h.endpoint)
        h.pump()

        val viewModel = h.detailViewModel()
        viewModel.reconnect()
        assertThat(h.handle().revalidateCount).isEqualTo(1)
    }

    @Test
    fun `forget drops the registry entry, credential, and live session`() = runTest {
        val h = SettingsViewModelTest.Harness(this, tmp.root)
        h.credentials.seed("r1", RelayInvitation(ByteArray(32)))
        h.repository.start()
        h.registry.upsert(h.endpoint)
        h.await { h.factory.handleFor(h.origin) != null }
        val handle = h.handle()

        var forgotten = false
        h.detailViewModel().forget { forgotten = true }
        h.await { handle.closed }

        assertThat(h.registry.snapshot()).isEmpty()
        assertThat(h.credentials.get("r1")).isNull()
        assertThat(forgotten).isTrue()
    }

    @Test
    fun `reconnecting an unknown relay is a no-op`() = runTest {
        val h = SettingsViewModelTest.Harness(this, tmp.root)
        h.detailViewModel("ghost").reconnect()
        h.pump()
        assertThat(h.factory.created).isEmpty()
    }
}
