package lerdr.core.data

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runTest
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class RelayRegistryTest {

    @get:Rule
    val tmp: TemporaryFolder = TemporaryFolder()

    private fun TestScope.store(): Pair<androidx.datastore.core.DataStore<androidx.datastore.preferences.core.Preferences>, RelayRegistry> {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "relays.preferences_pb")
        }
        return dataStore to RelayRegistry(dataStore, backgroundScope)
    }

    private fun endpoint(
        id: String,
        label: String = "label-$id",
        host: String = "$id.example.com",
        port: Int = 443,
        transport: RelayTransport = RelayTransport.WEBSOCKET_TLS,
        paired: Boolean = false,
    ) = RelayEndpoint(id, label, host, port, transport, paired)

    // ── state flow ───────────────────────────────────────────────────

    @Test
    fun `starts empty`() = runTest {
        val (_, registry) = store()
        registry.relays.test {
            assertThat(awaitItem()).isEmpty()
            cancelAndIgnoreRemainingEvents()
        }
    }

    @Test
    fun `upsert appends new relays in order`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        registry.upsert(endpoint("r2"))
        assertThat(registry.snapshot().map { it.id }).containsExactly("r1", "r2").inOrder()
        assertThat(registry.relays.first().map { it.id }).containsExactly("r1", "r2").inOrder()
    }

    @Test
    fun `upsert replaces same id in place`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        registry.upsert(endpoint("r2"))
        registry.upsert(endpoint("r1", label = "renamed"))
        val relays = registry.snapshot()
        assertThat(relays.map { it.id }).containsExactly("r1", "r2").inOrder()
        assertThat(relays[0].label).isEqualTo("renamed")
    }

    @Test
    fun `remove drops the entry and reports true`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        registry.upsert(endpoint("r2"))
        assertThat(registry.remove("r1")).isTrue()
        assertThat(registry.snapshot().map { it.id }).containsExactly("r2")
    }

    @Test
    fun `remove reports false for unknown id`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        assertThat(registry.remove("nope")).isFalse()
        assertThat(registry.snapshot()).hasSize(1)
    }

    @Test
    fun `reorder applies the requested order`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        registry.upsert(endpoint("r2"))
        registry.upsert(endpoint("r3"))
        registry.reorder(listOf("r3", "r1", "r2"))
        assertThat(registry.snapshot().map { it.id }).containsExactly("r3", "r1", "r2").inOrder()
    }

    @Test
    fun `reorder keeps unlisted relays at the end and drops unknown ids`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        registry.upsert(endpoint("r2"))
        registry.upsert(endpoint("r3"))
        registry.reorder(listOf("r3", "ghost"))
        assertThat(registry.snapshot().map { it.id }).containsExactly("r3", "r1", "r2").inOrder()
    }

    @Test
    fun `clear empties the registry`() = runTest {
        val (_, registry) = store()
        registry.upsert(endpoint("r1"))
        registry.clear()
        assertThat(registry.snapshot()).isEmpty()
    }

    @Test
    fun `records survive a fresh registry on the same store`() = runTest {
        val (dataStore, registry) = store()
        registry.upsert(endpoint("r1", paired = true, port = 8443))
        val restored = RelayRegistry(dataStore, backgroundScope)
        val relays = restored.relays.first { it.isNotEmpty() }
        assertThat(relays).hasSize(1)
        assertThat(relays[0]).isEqualTo(endpoint("r1", paired = true, port = 8443))
    }

    @Test
    fun `malformed stored json reads as empty`() = runTest {
        val (dataStore, registry) = store()
        dataStore.edit { it[RelayRegistry.RELAYS_KEY] = "not json" }
        assertThat(registry.snapshot()).isEmpty()
    }

    @Test
    fun `malformed entries are dropped while valid ones survive`() = runTest {
        val (dataStore, registry) = store()
        dataStore.edit {
            it[RelayRegistry.RELAYS_KEY] = """
                [{"id":"ok","label":"l","host":"h","port":443,"transport":"wss"},
                 {"id":42},
                 {"id":"badport","label":"l","host":"h","port":70000,"transport":"wss"}]
            """.trimIndent()
        }
        assertThat(registry.snapshot().map { it.id }).containsExactly("ok")
    }

    // ── endpoint model ───────────────────────────────────────────────

    @Test
    fun `socketOrigin elides default ports`() {
        assertThat(endpoint("a", host = "h", port = 443).socketOrigin).isEqualTo("wss://h")
        assertThat(endpoint("a", host = "h", port = 80, transport = RelayTransport.WEBSOCKET).socketOrigin)
            .isEqualTo("ws://h")
        assertThat(endpoint("a", host = "h", port = 8443).socketOrigin).isEqualTo("wss://h:8443")
    }

    @Test
    fun `fromSocketOrigin parses strict bare origins`() {
        val parsed = RelayEndpoint.fromSocketOrigin("wss://relay.example.com:8443", "desk")!!
        assertThat(parsed.host).isEqualTo("relay.example.com")
        assertThat(parsed.port).isEqualTo(8443)
        assertThat(parsed.transport).isEqualTo(RelayTransport.WEBSOCKET_TLS)
        assertThat(parsed.label).isEqualTo("desk")

        assertThat(RelayEndpoint.fromSocketOrigin("wss://user@host", "x")).isNull()
        assertThat(RelayEndpoint.fromSocketOrigin("wss://host/path", "x")).isNull()
        assertThat(RelayEndpoint.fromSocketOrigin("wss://host?q=1", "x")).isNull()
        assertThat(RelayEndpoint.fromSocketOrigin("https://host", "x")).isNull()
        assertThat(RelayEndpoint.fromSocketOrigin("wss://", "x")).isNull()
    }

    @Test
    fun `makeRelayId matches the oracle slug rules`() {
        // The scheme strip is anchored at index 0, so with a label present
        // the "wss://" survives as a slug segment — same as the oracle.
        assertThat(makeRelayId("My Box", "wss://Box.Example.com:8443"))
            .isEqualTo("my-box-wss-box-example-com-8443")
        assertThat(makeRelayId("", "wss://desk.local")).isEqualTo("desk-wss-desk-local")
        assertThat(makeRelayId("", "not a url")).isEqualTo("relay-not-a-url")
    }
}
