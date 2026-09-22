package com.lerdr.app.settings

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import java.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.data.DeviceRole
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

/**
 * `DevicesViewModel` against the real [SessionRepository] over a
 * [FakeRelaySessionHandle]: requests land in `sentRaw` as wire JSON, the
 * test answers them with `command_result` frames emitted on `incoming`.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class DevicesViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    /** viewModelScope rides Dispatchers.Main — redirect it into the test. */
    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
    ) {
        private val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val dataStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "devices.preferences_pb")
        }
        val registry = RelayRegistry(dataStore, scope)
        val connections = ConnectionStore(clock = { 0L })
        val factory = FakeRelaySessionFactory(scope)
        val repository = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = AgentStore(scope),
            workspaceStore = WorkspaceStore(),
            connectionStore = connections,
            sessionFactory = factory,
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(endpoint.socketOrigin) ?: error("no session for ${endpoint.socketOrigin}")

        fun viewModel() = DevicesViewModel("r1", repository)

        /** Registry entry + live session in the Connected handshake state. */
        fun connect(finish: com.lerdr.core.e2ee.E2EEServerFinish = FakeRelaySessionHandle.testFinish()) {
            repository.connect(endpoint)
            pump()
            handle().connect(finish)
            pump()
        }

        /** The wire JSON of the most recent request the VM sent. */
        fun lastRequest(): JsonObject = json(handle().sentRaw.last())

        fun requestTypes(): List<String> = handle().sentRaw.map {
            json(it)["type"]!!.jsonPrimitive.content
        }

        /** Answers the pending request with an `ok` `command_result`. */
        suspend fun answerOk(data: String? = null) {
            val request = lastRequest()
            val id = request["request_id"]!!.jsonPrimitive.content
            val dataField = data?.let { ",\"data\":$it" } ?: ""
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"$id","ok":true,""" +
                        """"phase":"completed","action":"${request["type"]!!.jsonPrimitive.content}"""" +
                        """$dataField}""",
                ),
            )
            pump()
        }

        /** Answers the pending request with a failed `command_result`. */
        suspend fun answerFailed(error: String) {
            val request = lastRequest()
            val id = request["request_id"]!!.jsonPrimitive.content
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"$id","ok":false,""" +
                        """"phase":"failed","error":"$error"}""",
                ),
            )
            pump()
        }

        /** Poll on a real clock — DataStore IO is off the test scheduler. */
        fun await(condition: () -> Boolean) {
            val deadline = System.currentTimeMillis() + 5_000
            while (!condition() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(condition()) { "condition not met within deadline" }
        }
    }

    private fun deviceList(vararg devices: String): String =
        """{"current_device_id":"dev-1","role":"controller","devices":[${devices.joinToString(",")}]}"""

    private fun device(
        id: String,
        name: String,
        role: String = "reader",
        pairedAt: String = "2026-01-01T00:00:00Z",
        lastSeenAt: String? = null,
        current: Boolean = false,
    ): String = buildString {
        append("""{"device_id":"$id","credential_id":"cred-$id","name":"$name",""")
        append(""""role":"$role","locale":"en","paired_at":"$pairedAt",""")
        append(""""last_seen_at":${lastSeenAt?.let { "\"$it\"" } ?: "\"0001-01-01T00:00:00Z\""},""")
        append(""""version":1,"revoked":false""")
        if (current) append(""","current":true""")
        append("}")
    }

    @Test
    fun `device_list parses devices, marks the caller, and sorts current first`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()

        val viewModel = h.viewModel()
        // init → connection collector → refresh() sent device_list.
        h.await { h.handle().sentRaw.isNotEmpty() }
        assertThat(h.lastRequest()["type"]!!.jsonPrimitive.content).isEqualTo("device_list")

        h.answerOk(
            deviceList(
                device("dev-2", "Laptop browser", lastSeenAt = "2026-03-01T10:00:00Z"),
                device("dev-1", "This phone", role = "controller",
                    lastSeenAt = "2026-03-02T10:00:00Z", current = true),
                device("dev-3", "Tablet", lastSeenAt = "2026-03-03T10:00:00Z"),
            ),
        )
        h.pump()

        val state = viewModel.uiState.value
        assertThat(state.fetched).isTrue()
        assertThat(state.connected).isTrue()
        assertThat(state.canAdminister).isTrue()
        assertThat(state.currentDeviceId).isEqualTo("dev-1")
        assertThat(state.relayLabel).isEqualTo("workstation")
        // Oracle order: current first, then last-seen desc.
        assertThat(state.devices.map { it.deviceId })
            .containsExactly("dev-1", "dev-3", "dev-2").inOrder()
        val current = state.devices.first()
        assertThat(current.current).isTrue()
        assertThat(current.role).isEqualTo(DeviceRole.CONTROLLER)
        assertThat(current.pairedAtEpochMs).isEqualTo(1_767_225_600_000L)
        assertThat(current.lastSeenAtEpochMs).isEqualTo(1_772_445_600_000L)
        // The zero-time last_seen collapses to "never" (null).
        val tablet = state.devices.last { it.deviceId == "dev-3" }
        assertThat(tablet.lastSeenAtEpochMs).isEqualTo(1_772_532_000_000L)
        val laptop = state.devices.first { it.deviceId == "dev-2" }
        assertThat(laptop.name).isEqualTo("Laptop browser")
        assertThat(laptop.role).isEqualTo(DeviceRole.READER)
    }

    @Test
    fun `rows missing required fields are dropped like the oracle's flatMap`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }

        h.answerOk(
            """{"current_device_id":"dev-1","role":"controller","devices":[""" +
                device("dev-1", "This phone", role = "controller", current = true) + "," +
                """{"device_id":"dev-x","name":"No credential","role":"reader","paired_at":"2026-01-01T00:00:00Z"},""" +
                """{"device_id":"dev-y","credential_id":"cred-y","name":"Bad role","role":"owner","paired_at":"2026-01-01T00:00:00Z"},""" +
                """{"device_id":"dev-z","credential_id":"cred-z","name":"No paired_at","role":"reader"}""" +
                "]}",
        )
        h.pump()

        assertThat(viewModel.uiState.value.devices.map { it.deviceId })
            .containsExactly("dev-1")
    }

    @Test
    fun `rename sends rename_device with device_id and trimmed name, then re-lists`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true),
            device("dev-2", "Old name", lastSeenAt = "2026-03-01T10:00:00Z")))

        viewModel.renameDevice("dev-2", "  Kitchen hub  ")
        val request = h.lastRequest()
        assertThat(request["type"]!!.jsonPrimitive.content).isEqualTo("rename_device")
        assertThat(request["device_id"]!!.jsonPrimitive.content).isEqualTo("dev-2")
        assertThat(request["name"]!!.jsonPrimitive.content).isEqualTo("Kitchen hub")
        assertThat(viewModel.uiState.value.actionBusy).isTrue()

        h.answerOk()
        // The post-rename refresh.
        h.await { h.lastRequest()["type"]!!.jsonPrimitive.content == "device_list" }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true),
            device("dev-2", "Kitchen hub", lastSeenAt = "2026-03-01T10:00:00Z")))

        val state = viewModel.uiState.value
        assertThat(state.actionBusy).isFalse()
        assertThat(state.status).isEqualTo("Device name saved.")
        assertThat(state.statusIsError).isFalse()
        assertThat(state.devices.first { it.deviceId == "dev-2" }.name).isEqualTo("Kitchen hub")
    }

    @Test
    fun `revoke sends revoke_device with device_id then re-lists`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true),
            device("dev-2", "Laptop", lastSeenAt = "2026-03-01T10:00:00Z")))
        val laptop = viewModel.uiState.value.devices.first { it.deviceId == "dev-2" }

        viewModel.revokeDevice(laptop)
        val request = h.lastRequest()
        assertThat(request["type"]!!.jsonPrimitive.content).isEqualTo("revoke_device")
        assertThat(request["device_id"]!!.jsonPrimitive.content).isEqualTo("dev-2")

        h.answerOk()
        h.await { h.lastRequest()["type"]!!.jsonPrimitive.content == "device_list" }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        val state = viewModel.uiState.value
        assertThat(state.status).isEqualTo("Laptop was revoked.")
        assertThat(state.devices.map { it.deviceId }).containsExactly("dev-1")
    }

    @Test
    fun `forgetCurrentDevice self-revokes with the caller's device_id`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        viewModel.forgetCurrentDevice()
        val request = h.lastRequest()
        assertThat(request["type"]!!.jsonPrimitive.content).isEqualTo("revoke_device")
        assertThat(request["device_id"]!!.jsonPrimitive.content).isEqualTo("dev-1")

        h.answerOk()
        // The follow-up list is best-effort — the sweep may have closed us.
        if (h.lastRequest()["type"]!!.jsonPrimitive.content == "device_list") {
            h.answerOk(deviceList())
        }
        assertThat(viewModel.uiState.value.status).isEqualTo("This device was revoked at the relay.")
    }

    @Test
    fun `reset_devices sends the bare action and clears the list`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        viewModel.resetDevices()
        val request = h.lastRequest()
        assertThat(request["type"]!!.jsonPrimitive.content).isEqualTo("reset_devices")
        assertThat(request.containsKey("device_id")).isFalse()

        h.answerOk()
        val state = viewModel.uiState.value
        assertThat(state.devices).isEmpty()
        assertThat(state.fetched).isTrue()
        assertThat(state.status).isEqualTo("All device credentials were reset.")
        // No follow-up list — the credential is dead.
        assertThat(h.requestTypes().count { it == "device_list" }).isEqualTo(1)
    }

    @Test
    fun `failed command_result surfaces the relay's error text`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true),
            device("dev-2", "Laptop", lastSeenAt = "2026-03-01T10:00:00Z")))
        val laptop = viewModel.uiState.value.devices.first { it.deviceId == "dev-2" }

        viewModel.revokeDevice(laptop)
        h.answerFailed("Device credential was not found")

        val state = viewModel.uiState.value
        assertThat(state.actionBusy).isFalse()
        assertThat(state.statusIsError).isTrue()
        assertThat(state.status).isEqualTo("Device credential was not found")
        assertThat(state.devices).hasSize(2)
    }

    @Test
    fun `refresh failure while connected surfaces an error`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }

        h.answerFailed("store unavailable")
        val state = viewModel.uiState.value
        assertThat(state.statusIsError).isTrue()
        assertThat(state.status).isEqualTo("store unavailable")
        assertThat(state.refreshing).isFalse()
    }

    @Test
    fun `refresh failure while offline keeps the stale list silently`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        h.handle().disconnect()
        h.pump()
        assertThat(viewModel.uiState.value.connected).isFalse()

        val sentBefore = h.handle().sentRaw.size
        viewModel.refresh()
        h.pump()
        // NotConnected threw before any write — and no error status lands.
        assertThat(h.handle().sentRaw.size).isEqualTo(sentBefore)
        assertThat(viewModel.uiState.value.status).isNull()
        assertThat(viewModel.uiState.value.devices).hasSize(1)
        assertThat(viewModel.uiState.value.refreshing).isFalse()
    }

    @Test
    fun `createInvitation validates the payload and builds the lerdr link`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        viewModel.createInvitation("  Kitchen tablet  ", DeviceRole.READER)
        val request = h.lastRequest()
        assertThat(request["type"]!!.jsonPrimitive.content).isEqualTo("create_device_invitation")
        assertThat(request["name"]!!.jsonPrimitive.content).isEqualTo("Kitchen tablet")
        assertThat(request["role"]!!.jsonPrimitive.content).isEqualTo("reader")

        h.answerOk(
            """{"invitation":{"invitation_id":"inv_0123456789abcdef","version":2,""" +
                """"secret":"${"A".repeat(43)}","expires_at":"2030-01-01T00:00:00Z",""" +
                """"name":"Kitchen tablet","role":"reader","locale":"en"}}""",
        )

        val state = viewModel.uiState.value
        assertThat(state.actionBusy).isFalse()
        assertThat(state.status).isEqualTo(
            "Invitation created. Share the one-use link below before it expires.",
        )
        val invitation = state.invitation!!
        assertThat(invitation.deviceName).isEqualTo("Kitchen tablet")
        assertThat(invitation.role).isEqualTo(DeviceRole.READER)
        assertThat(invitation.expiresAtEpochMs).isEqualTo(1_893_456_000_000L)
        assertThat(invitation.link).isEqualTo(
            "lerdr://pair#setup=" + "A".repeat(43) +
                "&invite=inv_0123456789abcdef&invite_version=2" +
                "&invite_expires=1893456000000&label=workstation" +
                "&relay=ws%3A%2F%2F192.168.1.5%3A7474",
        )
        // No invitation_qr capability advertised → link stands alone.
        assertThat(invitation.qr).isNull()
        // And the whole thing round-trips through the app's strict parser.
        assertThat(lerdr.core.data.SetupLink.parse(invitation.link))
            .isInstanceOf(lerdr.core.data.SetupLinkResult.Parsed::class.java)
    }

    @Test
    fun `invitation_qr capability fetches and decodes the qr_code bitmap`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        // push_config advertises the relay-side QR encoder.
        h.handle().emit(
            json("""{"type":"push_config","capabilities":["invitation_qr"]}"""),
        )
        h.pump()

        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        viewModel.createInvitation("Phone 2", DeviceRole.CONTROLLER)
        h.answerOk(
            """{"invitation":{"invitation_id":"inv_0123456789abcdef","version":1,""" +
                """"secret":"${"B".repeat(43)}","expires_at":"2030-01-01T00:00:00Z",""" +
                """"name":"Phone 2","role":"controller","locale":"en"}}""",
        )
        // The QR request went out carrying the link text.
        h.await { h.lastRequest()["type"]!!.jsonPrimitive.content == "qr_code" }
        val qrRequest = h.lastRequest()
        assertThat(qrRequest["text"]!!.jsonPrimitive.content).startsWith("lerdr://pair#setup=" + "B".repeat(43))

        // A 21-module QR — the packed bitfield is ceil(21²/8) = 56 bytes.
        val packed = ByteArray(56) { index -> if (index == 0) 0x80.toByte() else 0 }
        h.answerOk(
            """{"size":21,"modules":"${Base64.getEncoder().encodeToString(packed)}"}""",
        )

        val qr = viewModel.uiState.value.invitation?.qr
        assertThat(qr).isNotNull()
        assertThat(qr!!.size).isEqualTo(21)
        assertThat(qr.darkModules).hasSize(441)
        assertThat(qr.darkModules[0]).isTrue()
        assertThat(qr.darkModules[1]).isFalse()
    }

    @Test
    fun `invalid invitation payload surfaces the oracle's error`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect()
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(deviceList(device("dev-1", "Phone", role = "controller", current = true)))

        viewModel.createInvitation("Phone 2", DeviceRole.CONTROLLER)
        h.answerOk(
            """{"invitation":{"invitation_id":"inv_0123456789abcdef","version":1,""" +
                """"secret":"too-short","expires_at":"2030-01-01T00:00:00Z"}}""",
        )

        val state = viewModel.uiState.value
        assertThat(state.invitation).isNull()
        assertThat(state.statusIsError).isTrue()
        assertThat(state.status).isEqualTo("Relay returned an invalid device invitation")
    }

    @Test
    fun `reader session lists devices but cannot administer`() = runTest {
        val h = Harness(this, tmp.root)
        h.registry.upsert(h.endpoint)
        h.await { h.registry.relays.value.isNotEmpty() }
        h.connect(finish = readerFinish())
        val viewModel = h.viewModel()
        h.await { h.handle().sentRaw.isNotEmpty() }
        h.answerOk(
            """{"current_device_id":"dev-9","role":"reader","devices":[""" +
                device("dev-9", "Reader tablet", role = "reader", current = true) + "]}",
        )

        val state = viewModel.uiState.value
        assertThat(state.canAdminister).isFalse()
        assertThat(state.currentDeviceId).isEqualTo("dev-9")
        assertThat(state.devices).hasSize(1)
    }

    /**
     * `FakeRelaySessionHandle.testFinish` with `role = "reader"` — the same
     * reflective construction (ctor is internal to core:e2ee).
     */
    private fun readerFinish(): com.lerdr.core.e2ee.E2EEServerFinish {
        val type = Class.forName("com.lerdr.core.e2ee.E2EEServerFinish")
        val ctor = type.declaredConstructors.single { it.parameterCount == 7 }
        ctor.isAccessible = true
        return ctor.newInstance(
            byteArrayOf(0),
            "dev-9",
            "cred-9",
            "reader",
            "en",
            1L,
            null,
        ) as com.lerdr.core.e2ee.E2EEServerFinish
    }
}
